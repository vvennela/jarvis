#![cfg(unix)]
mod support;

use jarvis::install::install_host;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
};
use support::TempDir;

const ID: &str = "abcdefghijklmnopabcdefghijklmnop";
const ORIGIN: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop/";

fn binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_jarvis"))
}

#[test]
fn quoted_launcher_paths_execute_real_native_host_without_shell_interpolation() {
    let directory = TempDir::new();
    let copied_binary = directory.join("jarvis ' $(touch PWNED)");
    fs::copy(binary(), &copied_binary).unwrap();
    let database = directory.join("data ' $(touch PWNED).db");
    let installed = install_host(
        &copied_binary,
        &database,
        ID,
        &directory.join("host ' $(touch PWNED)"),
        &directory.join("manifest ' $(touch PWNED)"),
    )
    .unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(&installed.manifest).unwrap()).unwrap();
    assert_eq!(manifest["allowed_origins"], json!([ORIGIN]));
    assert_eq!(manifest["path"], installed.launcher.to_str().unwrap());
    assert_eq!(manifest["type"], "stdio");
    assert_eq!(manifest["name"], "com.jarvis.bridge");
    let mut child = Command::new(&installed.launcher)
        .arg(ORIGIN)
        .current_dir(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let hello = serde_json::to_vec(
        &json!({"type":"hello","protocol_version":1,"client_nonce":"fixture-client-nonce-1234"}),
    )
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&(hello.len() as u32).to_le_bytes())
        .unwrap();
    stdin.write_all(&hello).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() >= 4);
    let length = u32::from_le_bytes(output.stdout[..4].try_into().unwrap()) as usize;
    assert_eq!(output.stdout.len(), length + 4);
    let ack: Value = serde_json::from_slice(&output.stdout[4..]).unwrap();
    assert_eq!(ack["type"], "hello_ack");
    assert_eq!(ack["client_nonce"], "fixture-client-nonce-1234");
    assert!(ack["session_id"].as_str().unwrap().len() >= 16);
    assert!(database.exists());
    assert!(!directory.join("PWNED").exists());
    assert_eq!(
        fs::metadata(&installed.launcher)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&installed.manifest)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(installed.launcher.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(installed.manifest.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn launcher_forwards_origin_instead_of_replacing_it_with_trusted_value() {
    let directory = TempDir::new();
    let database = directory.join("state.db");
    let installed = install_host(
        binary(),
        &database,
        ID,
        &directory.join("host"),
        &directory.join("manifest"),
    )
    .unwrap();
    let output = Command::new(installed.launcher)
        .arg("chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!database.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("origin rejected"));
}

#[test]
fn invalid_id_and_relative_paths_fail_before_creating_install_directories() {
    let directory = TempDir::new();
    let host = directory.join("host");
    let manifests = directory.join("manifest");
    for id in [
        "short",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    ] {
        assert!(install_host(binary(), &directory.join("db"), id, &host, &manifests).is_err());
        assert!(!host.exists());
        assert!(!manifests.exists());
    }
    assert!(install_host(binary(), Path::new("relative.db"), ID, &host, &manifests).is_err());
    assert!(!host.exists());
    assert!(!manifests.exists());
}

#[test]
fn existing_installation_is_never_overwritten() {
    let directory = TempDir::new();
    let host = directory.join("host");
    let manifests = directory.join("manifest");
    let first = install_host(binary(), &directory.join("first.db"), ID, &host, &manifests).unwrap();
    let original_launcher = fs::read(&first.launcher).unwrap();
    let original_manifest = fs::read(&first.manifest).unwrap();
    assert!(
        install_host(
            binary(),
            &directory.join("second.db"),
            ID,
            &host,
            &manifests
        )
        .is_err()
    );
    assert_eq!(fs::read(first.launcher).unwrap(), original_launcher);
    assert_eq!(fs::read(first.manifest).unwrap(), original_manifest);
}

#[test]
fn manifest_collision_removes_only_new_launcher_and_preserves_existing_files() {
    let directory = TempDir::new();
    let host = directory.join("host");
    let manifests = directory.join("manifest");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&manifests).unwrap();
    fs::write(host.join("unrelated"), "keep me").unwrap();
    let existing = manifests.join("com.jarvis.bridge.json");
    fs::write(&existing, "original registration").unwrap();
    assert!(install_host(binary(), &directory.join("state.db"), ID, &host, &manifests).is_err());
    assert!(!host.join("jarvis-chrome-host").exists());
    assert_eq!(
        fs::read_to_string(&existing).unwrap(),
        "original registration"
    );
    assert_eq!(
        fs::read_to_string(host.join("unrelated")).unwrap(),
        "keep me"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn cli_registration_uses_only_isolated_child_home() {
    let directory = TempDir::new();
    let temporary_home = directory.join("isolated-home");
    fs::create_dir(&temporary_home).unwrap();
    let output = Command::new(binary())
        .args(["install-browser-host", "--extension-id", ID, "--db"])
        .arg(directory.join("state.db"))
        .env("HOME", &temporary_home)
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let registration = temporary_home.join(
        "Library/Application Support/Google/Chrome/NativeMessagingHosts/com.jarvis.bridge.json",
    );
    let manifest: Value = serde_json::from_slice(&fs::read(&registration).unwrap()).unwrap();
    assert_eq!(manifest["allowed_origins"], json!([ORIGIN]));
    assert!(Path::new(manifest["path"].as_str().unwrap()).starts_with(&temporary_home));
    assert!(!directory.join("state.db").exists());
}

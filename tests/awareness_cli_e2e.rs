mod support;

use serde_json::Value;
use std::{
    fs,
    process::{Command, Output},
};
use support::TempDir;

const SECRET: &str = "TEST-SECRET-MUST-NEVER-APPEAR-IN-OUTPUT";

fn command(directory: &TempDir, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jarvis"));
    command.current_dir(directory.path()).args(arguments);
    for name in [
        "TYPESAFE_API_KEY",
        "CRUSOE_API_KEY",
        "JARVIS_CONVERSATION_MODEL",
        "JARVIS_WORKER_MODEL",
    ] {
        command.env_remove(name);
    }
    command
}

fn run(directory: &TempDir, arguments: &[&str]) -> Output {
    command(directory, arguments).output().unwrap()
}

fn json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_redacted(output: &Output) {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
}

#[test]
fn index_search_update_delete_are_real_separate_processes_without_provider_keys() {
    let directory = TempDir::new();
    fs::create_dir(directory.join("approved")).unwrap();
    fs::write(directory.join("approved/spec.md"), "first mango").unwrap();
    fs::write(directory.join("outside.txt"), "private outside papaya").unwrap();
    let report = json(run(
        &directory,
        &["index", "--db", "state.db", "--root", "approved"],
    ));
    assert_eq!(report["resources_changed"], 1);
    let first = json(run(&directory, &["search", "--db", "state.db", "mango"]));
    assert_eq!(first.as_array().unwrap().len(), 1);
    assert_eq!(first[0]["version"], 1);
    assert!(
        json(run(&directory, &["search", "--db", "state.db", "papaya"]))
            .as_array()
            .unwrap()
            .is_empty()
    );
    fs::write(directory.join("approved/spec.md"), "second apricot").unwrap();
    json(run(
        &directory,
        &["index", "--db", "state.db", "--root", "approved"],
    ));
    assert!(
        json(run(&directory, &["search", "--db", "state.db", "mango"]))
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        json(run(&directory, &["search", "--db", "state.db", "apricot"]))[0]["version"],
        2
    );
    fs::remove_file(directory.join("approved/spec.md")).unwrap();
    let removed = json(run(
        &directory,
        &["index", "--db", "state.db", "--root", "approved"],
    ));
    assert_eq!(removed["resources_removed"], 1);
    assert!(
        json(run(&directory, &["search", "--db", "state.db", "apricot"]))
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        json(run(&directory, &["recent", "--db", "state.db"]))[0]["kind"],
        "deleted"
    );
}

#[test]
fn doctor_reports_presence_without_exposing_values_or_claiming_verification() {
    let directory = TempDir::new();
    let missing = run(&directory, &["doctor"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stdout).contains("missing"));
    fs::write(
        directory.join(".env"),
        format!(
            "TYPESAFE_API_KEY='{SECRET}'\nCRUSOE_API_KEY=\"{SECRET}\"\nUNRECOGNIZED={SECRET}\n"
        ),
    )
    .unwrap();
    let configured = run(&directory, &["doctor"]);
    assert!(configured.status.success());
    assert!(String::from_utf8_lossy(&configured.stdout).contains("configured (not yet verified)"));
    assert_redacted(&configured);
    let overridden = command(&directory, &["doctor"])
        .env("TYPESAFE_API_KEY", "")
        .output()
        .unwrap();
    assert_eq!(overridden.status.code(), Some(2));
    assert_redacted(&overridden);
}

#[test]
fn local_configuration_byte_limit_and_utf8_errors_are_redacted() {
    let directory = TempDir::new();
    fs::write(
        directory.join(".env"),
        format!("{SECRET}{}", "x".repeat(16_385)),
    )
    .unwrap();
    let oversized = run(&directory, &["doctor"]);
    assert_eq!(oversized.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&oversized.stderr).contains("size limit"));
    assert_redacted(&oversized);
    fs::write(directory.join(".env"), [0xff, 0xfe]).unwrap();
    let invalid = run(&directory, &["doctor"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("UTF-8"));
}

#[test]
fn missing_keys_and_invalid_key_configuration_fail_without_network_or_secret_output() {
    let directory = TempDir::new();
    for arguments in [
        vec!["route", "what now"],
        vec!["ask", "--db", "state.db", "--question", "where was I"],
    ] {
        let output = run(&directory, &arguments);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("TYPESAFE_API_KEY is missing"));
    }
    let invalid = command(&directory, &["route", "what now"])
        .env("TYPESAFE_API_KEY", format!("{SECRET}\ninvalid-header"))
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert_redacted(&invalid);
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("invalid provider configuration"));
}

#[test]
fn explicit_root_and_bounded_duration_are_required() {
    let directory = TempDir::new();
    for arguments in [
        vec!["index", "--db", "state.db"],
        vec!["index", "--db", "state.db", "--root", "missing"],
        vec!["watch", "--db", "state.db", "--root", ".", "--seconds", "0"],
        vec![
            "watch",
            "--db",
            "state.db",
            "--root",
            ".",
            "--seconds",
            "3601",
        ],
        vec![
            "watch",
            "--db",
            "state.db",
            "--root",
            ".",
            "--seconds",
            "-1",
        ],
    ] {
        let output = run(&directory, &arguments);
        assert_eq!(output.status.code(), Some(2));
    }
    let help = run(&directory, &["--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("no desktop UI or autonomous workers yet")
    );
}

#[test]
fn native_host_rejects_wrong_origin_before_reading_stdin_or_creating_database() {
    let directory = TempDir::new();
    let output = run(
        &directory,
        &[
            "native-host",
            "--db",
            "native.db",
            "--extension-id",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!directory.join("native.db").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("origin rejected"));
}

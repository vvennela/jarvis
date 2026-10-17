//! Real signing is opt-in; default tests never create a signing identity.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
#[path = "../../../tests/support/mod.rs"]
mod support;
fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sign-development-app.sh")
}
fn bundle(root: &Path) -> PathBuf {
    let app = root.join("Fixture.app");
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::write(app.join("Contents/Info.plist"), r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.jarvis.panel</string><key>CFBundleExecutable</key><string>fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
    fs::write(
        app.join("Contents/MacOS/fixture"),
        "fixture, never executed",
    )
    .unwrap();
    app
}
#[test]
fn signing_invalid_arguments_leave_no_identity_or_bundle_mutations() {
    let temp = support::TempDir::new();
    for args in [
        vec![],
        vec!["--help"],
        vec!["missing.app"],
        vec!["one", "two"],
    ] {
        let result = Command::new("/bin/bash")
            .arg(script())
            .args(args)
            .env("JARVIS_SIGNING_DIR", temp.join("never-created"))
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&result.stderr).contains("Usage:"));
        assert!(!temp.join("never-created").exists());
    }
}
#[cfg(unix)]
#[test]
fn signing_rejects_symlink_targets_and_identity_directory_before_creating_identity() {
    use std::os::unix::fs::symlink;
    let temp = support::TempDir::new();
    let app = bundle(temp.path());
    let link = temp.join("Alias.app");
    symlink(&app, &link).unwrap();
    let identity = temp.join("never-created");
    let result = Command::new("/bin/bash")
        .arg(script())
        .arg(&link)
        .env("JARVIS_SIGNING_DIR", &identity)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(
        !identity.exists(),
        "symlink target must be rejected before preparing keys"
    );
    let actual = temp.join("actual");
    fs::create_dir(&actual).unwrap();
    symlink(&actual, &identity).unwrap();
    let result = Command::new("/bin/bash")
        .arg(script())
        .arg(&app)
        .env("JARVIS_SIGNING_DIR", &identity)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(fs::read_dir(actual).unwrap().count(), 0);
}
#[test]
fn incomplete_identity_is_preserved_instead_of_silently_rotated() {
    let temp = support::TempDir::new();
    let app = bundle(temp.path());
    let identity = temp.join("identity");
    fs::create_dir(&identity).unwrap();
    let cert = identity.join("development.cer");
    fs::write(&cert, b"synthetic incomplete identity marker").unwrap();
    let result = Command::new("/bin/bash")
        .arg(script())
        .arg(&app)
        .env("JARVIS_SIGNING_DIR", &identity)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("incomplete"));
    assert_eq!(
        fs::read(cert).unwrap(),
        b"synthetic incomplete identity marker"
    );
    assert!(!identity.join("keychain-password").exists());
    assert!(!identity.join("development.keychain-db").exists());
    assert!(!identity.join(".lock").exists());
    assert_eq!(fs::read_dir(identity).unwrap().count(), 1);
}
#[cfg(target_os = "macos")]
#[test]
#[ignore = "uses existing local signing identity; never creates one or launches an app"]
fn opt_in_existing_identity_survives_binary_and_resource_changes() {
    let temp = support::TempDir::new();
    let identity = PathBuf::from(std::env::var_os("HOME").unwrap())
        .join("Library/Application Support/Jarvis/Signing");
    for name in [
        "development.keychain-db",
        "keychain-password",
        "development.cer",
    ] {
        assert!(
            identity.join(name).is_file(),
            "run launcher build first: identity must already exist"
        );
    }
    let cert_before = fs::read(identity.join("development.cer")).unwrap();
    let app = bundle(temp.path());
    let executable = app.join("Contents/MacOS/fixture");
    fs::copy("/usr/bin/true", &executable).unwrap();
    let resources = app.join("Contents/Resources");
    fs::create_dir(&resources).unwrap();
    fs::write(resources.join("version.txt"), "first").unwrap();
    let search_list = || {
        let r = Command::new("/usr/bin/security")
            .args(["list-keychains", "-d", "user"])
            .output()
            .unwrap();
        assert!(r.status.success());
        r.stdout
    };
    let original_search = search_list();
    let sign = || {
        let r = Command::new("/bin/bash")
            .arg(script())
            .arg(&app)
            .env("JARVIS_SIGNING_DIR", &identity)
            .output()
            .unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        assert_eq!(
            search_list(),
            original_search,
            "signing altered user's keychain search list"
        );
        assert!(!identity.join(".lock").exists());
    };
    let inspect = |args: &[&str]| {
        let r = Command::new("/usr/bin/codesign")
            .args(args)
            .arg(&app)
            .output()
            .unwrap();
        assert!(r.status.success());
        format!(
            "{}{}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        )
    };
    sign();
    let first_requirement = inspect(&["-d", "-r-"]);
    let requirement = first_requirement
        .lines()
        .find_map(|line| line.strip_prefix("designated => "))
        .unwrap()
        .to_owned();
    let first_details = inspect(&["-d", "--verbose=4"]);
    let first_hash = first_details
        .lines()
        .find(|line| line.starts_with("CDHash="))
        .unwrap()
        .to_owned();
    fs::copy("/usr/bin/false", &executable).unwrap();
    fs::write(resources.join("version.txt"), "second resource revision").unwrap();
    sign();
    let second_requirement = inspect(&["-d", "-r-"]);
    let second_req = second_requirement
        .lines()
        .find_map(|line| line.strip_prefix("designated => "))
        .unwrap();
    assert_eq!(requirement, second_req);
    let second_details = inspect(&["-d", "--verbose=4"]);
    let second_hash = second_details
        .lines()
        .find(|line| line.starts_with("CDHash="))
        .unwrap();
    assert_ne!(first_hash, second_hash);
    let verify = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "-R", &format!("={requirement}")])
        .arg(&app)
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert_eq!(
        fs::read(identity.join("development.cer")).unwrap(),
        cert_before
    );
}

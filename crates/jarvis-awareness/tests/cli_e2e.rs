//! Actual process tests restricted to argument validation; never invoke native capture.
#[path = "../../../tests/support/mod.rs"]
mod support;
use std::process::Command;
use support::TempDir;

#[test]
fn help_and_invalid_arguments_do_not_create_database_or_collect_native_state() {
    let temp = TempDir::new();
    assert!(temp.path().is_dir());
    let help = Command::new(env!("CARGO_BIN_EXE_jarvis-awareness"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("--seconds")
    );
    for value in ["0", "3601", "-1", "not-a-number"] {
        let output = Command::new(env!("CARGO_BIN_EXE_jarvis-awareness"))
            .arg("--db")
            .arg(temp.join("never.db"))
            .args(["--seconds", value])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!temp.join("never.db").exists());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_jarvis-awareness"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

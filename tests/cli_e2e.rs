mod support;

use std::{
    fs,
    process::{Command, Output},
};
use support::TempDir;

fn cli(dir: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jarvis"))
        .current_dir(dir.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn demo_process_produces_only_a_labeled_review_draft() {
    let dir = TempDir::new();
    let output = cli(&dir, &["demo", "--output", "review folder"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("SCRIPTED FIXTURE"));
    assert!(stdout.contains("READY_FOR_REVIEW"));
    assert!(stdout.contains("Nothing has been sent"));
    assert!(!stdout.to_lowercase().contains("completed the task"));
    let draft = fs::read_to_string(dir.join("review folder/message-review.txt")).unwrap();
    assert!(draft.contains("NOT SENT"));
    assert!(draft.contains("recipient address unresolved"));
    assert!(draft.contains("not a live agent result"));
    assert_eq!(fs::read_dir(dir.join("review folder")).unwrap().count(), 1);
}

#[test]
fn rerunning_demo_refuses_to_overwrite_an_edited_draft() {
    let dir = TempDir::new();
    assert!(cli(&dir, &["demo", "--output", "review"]).status.success());
    let path = dir.join("review/message-review.txt");
    fs::write(&path, b"user edited recipient and body").unwrap();
    let output = cli(&dir, &["demo", "--output", "review"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("READY_FOR_REVIEW"));
    assert_eq!(fs::read(path).unwrap(), b"user edited recipient and body");
}

#[test]
fn final_sensitive_actions_fail_without_creating_files() {
    let dir = TempDir::new();
    for action in ["send", "pay", "delete"] {
        let output = cli(&dir, &["policy", action]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("DenyFinalCommit"));
        assert!(output.stdout.is_empty());
    }
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn invalid_arguments_fail_without_creating_output() {
    let dir = TempDir::new();
    for args in [
        vec![],
        vec!["demo"],
        vec!["demo", "--output", "review", "--send"],
        vec!["policy", "approve"],
        vec!["send"],
    ] {
        let output = cli(&dir, &args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn invalid_output_target_has_no_success_announcement() {
    let dir = TempDir::new();
    fs::write(dir.join("occupied"), b"user data").unwrap();
    let output = cli(&dir, &["demo", "--output", "occupied"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(dir.join("occupied")).unwrap(), b"user data");
}

#[test]
fn help_is_explicit_about_current_fixture_limitations() {
    let dir = TempDir::new();
    let output = cli(&dir, &["--help"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("no live collectors or models yet"));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn demo_refuses_existing_draft_symlink_without_touching_target() {
    let dir = TempDir::new();
    fs::create_dir(dir.join("review")).unwrap();
    fs::write(dir.join("private.txt"), b"private original").unwrap();
    std::os::unix::fs::symlink(
        dir.join("private.txt"),
        dir.join("review/message-review.txt"),
    )
    .unwrap();
    let output = cli(&dir, &["demo", "--output", "review"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        fs::read(dir.join("private.txt")).unwrap(),
        b"private original"
    );
}

//! Full configured conversation → actual local Codex → verified isolated review.
#![cfg(unix)]
use jarvis::{
    coding::{CodingConfig, CodingTask},
    context::{ContextStore, Limits},
    conversation::Conversation,
    tool_runtime::ToolRuntime,
};
use jarvis_panel::{codingworker::CodingWorker, tasks::State};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
struct OwnedWorker(CodingWorker);
impl Drop for OwnedWorker {
    fn drop(&mut self) {
        self.0.control.shutdown();
        let _ = self.0.join();
    }
}
fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("/usr/bin/git")
            .current_dir(root)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "fixture Git setup failed"
    );
}
async fn idle(worker: &CodingWorker, timeout: Duration) {
    tokio::time::timeout(timeout, async {
        while worker.control.busy() {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("coding worker deadline exceeded");
}
#[tokio::test]
#[ignore = "requires JARVIS_LIVE_CODEX_CONVERSATION=1, configured Nano and local Codex sign-in; fictional isolated Rust project"]
async fn real_conversation_delegates_to_codex_and_preserves_original_project() {
    assert_eq!(
        std::env::var("JARVIS_LIVE_CODEX_CONVERSATION").as_deref(),
        Ok("1")
    );
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    std::env::set_current_dir(&repository).unwrap();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = repository.join("runtime").join(format!(
        "conversation-coding-{}-{stamp}",
        std::process::id()
    ));
    let project = root.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let project = fs::canonicalize(project).unwrap();
    let original = "pub fn add(a: i32, b: i32) -> i32 { a - b }\n#[cfg(test)] mod tests { #[test] fn adds() { assert_eq!(super::add(2, 3), 5); } }\n";
    fs::write(project.join("Cargo.toml"),"[package]\nname = \"jarvis_coding_conversation_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
    fs::write(project.join(".gitignore"), "target/\nCargo.lock\n").unwrap();
    fs::write(project.join("src/lib.rs"), original).unwrap();
    git(&project, &["init", "--quiet"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=Jarvis Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "Fictional failing addition",
        ],
    );
    let database = root.join("context.sqlite");
    drop(ContextStore::open(&database, Limits::default()).unwrap());
    let executable = PathBuf::from(std::env::var("HOME").unwrap()).join(".local/bin/codex");
    let config = CodingConfig::new(project.clone(), root.join("jobs"), executable)
        .unwrap()
        .with_timeout(Duration::from_secs(240))
        .unwrap();
    let opened = root.join("explicit-review-path");
    let opener = root.join("silent-reviewer");
    let escaped = opened.to_str().unwrap().replace('\'', "'\\''");
    fs::write(
        &opener,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{escaped}'\n"),
    )
    .unwrap();
    fs::set_permissions(&opener, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = OwnedWorker(
        CodingWorker::with_factory(move || CodingTask::new(config.clone()), opener).unwrap(),
    );
    let mut conversation = Conversation::from_env(database.clone()).unwrap();
    conversation.set_tool_runtime(Arc::new(
        ToolRuntime::read_only(database).with_coding_submitter(worker.0.control.clone()),
    ));
    let started = Instant::now();
    let reply=tokio::time::timeout(Duration::from_secs(60),conversation.ask("Use Codex to fix the add function in the already configured project: add the two integers instead of subtracting. Preserve the existing test assertion, run cargo test --offline, make the smallest change, and leave it ready for review.")).await.expect("conversation deadline exceeded").expect("conversation failed");
    assert_eq!(
        reply.text,
        "Codex is working on it in a separate worktree. I'll bring you the changes for review."
    );
    let delegation_ms = started.elapsed().as_millis();
    idle(&worker.0, Duration::from_secs(245)).await;
    let snapshot = worker.0.control.snapshot();
    assert_eq!(
        snapshot.state,
        State::ReadyForReview,
        "coding state: {}",
        snapshot.text
    );
    let summary = snapshot.path.unwrap();
    let worktree = summary.parent().unwrap().join("worktree");
    assert_eq!(
        fs::read_to_string(project.join("src/lib.rs")).unwrap(),
        original
    );
    let revised = fs::read_to_string(worktree.join("src/lib.rs")).unwrap();
    assert!(revised.contains("a + b") && revised.contains("assert_eq!"));
    let mut test = tokio::process::Command::new("cargo")
        .args(["test", "--offline"])
        .current_dir(&worktree)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), test.wait()).await;
    if result.is_err() {
        let _ = test.kill().await;
        let _ = test.wait().await;
    }
    assert!(
        result
            .expect("independent test deadline")
            .unwrap()
            .success(),
        "independent cargo tests failed"
    );
    assert!(!opened.exists(), "coding result opened automatically");
    worker.0.control.request_review().unwrap();
    idle(&worker.0, Duration::from_secs(20)).await;
    assert_eq!(worker.0.control.snapshot().state, State::ReadyForReview);
    assert_eq!(
        fs::read_to_string(&opened).unwrap(),
        summary.to_str().unwrap()
    );
    println!(
        "live_conversation_codex_verified=true independent_cargo_tests=true original_unchanged=true explicit_verified_review=true automatic_open=false delegation_ms={} total_ms={}",
        delegation_ms,
        started.elapsed().as_millis()
    );
}

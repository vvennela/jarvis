//! Explicit live Codex acceptance on an invented, dependency-free Rust repository.
#![cfg(unix)]
use jarvis::coding::{CodingConfig, CodingOutcome, CodingTask};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

fn git(root: &Path, args: &[&str]) {
    let result = Command::new("/usr/bin/git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(result.status.success(), "fixture Git setup failed");
}

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_CODEX=1, installed Codex and existing ChatGPT sign-in; edits only fictional isolated repository"]
async fn real_codex_fixes_isolated_code_and_independent_tests_confirm_result() {
    assert_eq!(std::env::var("JARVIS_LIVE_CODEX").as_deref(), Ok("1"));
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("runtime")
        .join(format!("coding-live-{suffix}"));
    let project = root.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    let project = fs::canonicalize(project).unwrap();
    let original = "pub fn add(a: i32, b: i32) -> i32 { a - b }\n#[cfg(test)] mod tests { #[test] fn adds() { assert_eq!(super::add(2, 3), 5); } }\n";
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"jarvis_coding_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
    )
    .unwrap();
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
            "Fictional failing addition fixture",
        ],
    );
    let executable = Path::new(&std::env::var("HOME").unwrap()).join(".local/bin/codex");
    let config = CodingConfig::new(
        project.clone(),
        fs::canonicalize(&root).unwrap().join("jobs"),
        executable,
    )
    .unwrap()
    .with_timeout(Duration::from_secs(240))
    .unwrap();
    let mut task = CodingTask::new(config).unwrap();
    let started = Instant::now();
    let outcome = task.run("Use Codex to fix the add function in this project: it must add its two integers, not subtract them. Preserve the test assertion and run cargo test --offline. Make the smallest code change and leave it ready for review.", Arc::new(AtomicBool::new(false))).await.expect("live Codex runner failed");
    let CodingOutcome::ReadyForReview { summary, worktree } = outcome else {
        panic!("live coding was not ready: {outcome:?}")
    };
    assert_eq!(
        fs::read_to_string(project.join("src/lib.rs")).unwrap(),
        original
    );
    let revised = fs::read_to_string(worktree.join("src/lib.rs")).unwrap();
    assert!(revised.contains("a + b"));
    assert!(revised.contains("assert_eq!"));
    let test = Command::new("cargo")
        .args(["test", "--offline"])
        .current_dir(&worktree)
        .output()
        .unwrap();
    assert!(
        test.status.success(),
        "independent fixture cargo test failed"
    );
    assert!(matches!(
        task.review().await.unwrap(),
        CodingOutcome::ReadyForReview { .. }
    ));
    assert!(summary.is_file());
    println!(
        "real_codex=true isolated_edit=true independent_cargo_tests=true original_unchanged=true ready_for_review=true elapsed_ms={} fixture={}",
        started.elapsed().as_millis(),
        root.display()
    );
}

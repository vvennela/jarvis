#![cfg(unix)]
//! Actual isolated Git worktrees and owned executable fixtures; no Codex service calls.
mod support;
use jarvis::coding::{CodingConfig, CodingOutcome, CodingTask};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use support::TempDir;

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fixture Git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
struct Fixture {
    dir: TempDir,
    project: PathBuf,
    executable: PathBuf,
}
impl Fixture {
    fn new(body: &str) -> Self {
        let dir = TempDir::new();
        let root = fs::canonicalize(dir.path()).unwrap();
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        git(&project, &["init", "-q"]);
        git(
            &project,
            &["config", "user.email", "fixture@example.invalid"],
        );
        git(&project, &["config", "user.name", "Fixture Tester"]);
        fs::write(project.join("app.txt"), "original application\n").unwrap();
        git(&project, &["add", "app.txt"]);
        git(&project, &["commit", "-qm", "fixture initial"]);
        let executable = root.join("codex-fixture");
        let header = r#"#!/bin/sh
fixture_dir=${0%/*}
printf '%s\n' "$@" > "$fixture_dir/argv"
printf '%s\n' "$$" > "$fixture_dir/process.pid"
worktree=$(pwd)
last=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -C) worktree=$2; shift 2 ;;
    --output-last-message) last=$2; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' "$worktree" > "$fixture_dir/worktree"
cat > "$fixture_dir/stdin"
"#;
        fs::write(&executable, format!("{header}\n{body}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            dir,
            project,
            executable,
        }
    }
    fn task(&self, timeout: Duration) -> CodingTask {
        let root = fs::canonicalize(self.dir.path()).unwrap();
        CodingTask::new(
            CodingConfig::new(
                self.project.clone(),
                root.join("coding"),
                self.executable.clone(),
            )
            .unwrap()
            .with_timeout(timeout)
            .unwrap(),
        )
        .unwrap()
    }
    fn assert_original(&self) {
        assert_eq!(
            fs::read_to_string(self.project.join("app.txt")).unwrap(),
            "original application\n"
        );
        assert!(git(&self.project, &["status", "--porcelain"]).is_empty());
    }
}
const SUCCESS: &str = r#"printf 'revised application\n' > "$worktree/app.txt"
printf 'Implemented the fixture change. Tests passed.\n' > "$last"
printf '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}\n'
"#;
fn ready(outcome: CodingOutcome) -> (PathBuf, PathBuf) {
    match outcome {
        CodingOutcome::ReadyForReview { summary, worktree } => (summary, worktree),
        other => panic!("expected verified result, got {other:?}"),
    }
}
async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn literal_instruction_runs_in_isolated_git_and_requires_explicit_review() {
    let f = Fixture::new(SUCCESS);
    let head = git(&f.project, &["rev-parse", "HEAD"]);
    let mut task = f.task(Duration::from_secs(5));
    let instruction = "Implement a literal note containing $(touch SHOULD_NOT_EXIST), `touch ALSO_NOT_CREATED`, and '; exit 0' in the project.";
    let (summary, worktree) = ready(
        task.run(instruction, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap(),
    );
    f.assert_original();
    assert_eq!(git(&f.project, &["rev-parse", "HEAD"]), head);
    assert_ne!(worktree, f.project);
    assert_eq!(
        fs::read_to_string(worktree.join("app.txt")).unwrap(),
        "revised application\n"
    );
    assert!(
        fs::read_to_string(f.dir.join("stdin"))
            .unwrap()
            .contains(instruction)
    );
    assert!(!f.project.join("SHOULD_NOT_EXIST").exists());
    assert!(!worktree.join("SHOULD_NOT_EXIST").exists());
    assert!(!worktree.join("ALSO_NOT_CREATED").exists());
    let argv = fs::read_to_string(f.dir.join("argv")).unwrap();
    assert!(!argv.contains(instruction));
    assert!(argv.contains("--sandbox\nworkspace-write\n"));
    assert!(argv.contains("sandbox_workspace_write.network_access=false"));
    assert!(summary.is_file());
    assert!(matches!(
        task.review().await.unwrap(),
        CodingOutcome::ReadyForReview { .. }
    ));
}

#[tokio::test]
async fn initial_dirty_project_never_launches_codex_against_stale_head() {
    let f = Fixture::new(SUCCESS);
    fs::write(f.project.join("app.txt"), "user unsaved branch change\n").unwrap();
    let mut task = f.task(Duration::from_secs(3));
    assert!(matches!(
        task.run("Fix the project", Arc::new(AtomicBool::new(false)))
            .await
            .unwrap(),
        CodingOutcome::Blocked { .. }
    ));
    assert!(!f.dir.join("argv").exists());
    assert_eq!(
        fs::read_to_string(f.project.join("app.txt")).unwrap(),
        "user unsaved branch change\n"
    );
}

#[tokio::test]
async fn exit_success_without_turn_completion_or_failed_turn_cannot_be_ready() {
    for body in [
        "printf 'claimed success' > \"$last\"; printf '{\"type\":\"item.completed\"}\\n'",
        "printf 'claimed success' > \"$last\"; printf '{\"type\":\"turn.completed\"}\\n'; exit 9",
        "printf 'claimed success' > \"$last\"; printf '{\"type\":\"turn.failed\"}\\n{\"type\":\"turn.completed\"}\\n'",
        "printf 'claimed success' > \"$last\"; printf 'not-json\\n'",
    ] {
        let f = Fixture::new(body);
        let mut task = f.task(Duration::from_secs(3));
        let result = task
            .run("Fix the project", Arc::new(AtomicBool::new(false)))
            .await;
        assert!(
            !matches!(result, Ok(CodingOutcome::ReadyForReview { .. })),
            "unverified process was marked ready"
        );
        assert!(!matches!(
            task.review().await,
            Ok(CodingOutcome::ReadyForReview { .. })
        ));
        f.assert_original();
    }
}

#[tokio::test]
async fn source_worktree_or_report_mutation_revokes_review() {
    for changed in ["source", "worktree", "report"] {
        let f = Fixture::new(SUCCESS);
        let mut task = f.task(Duration::from_secs(4));
        let (summary, worktree) = ready(
            task.run("Fix the project", Arc::new(AtomicBool::new(false)))
                .await
                .unwrap(),
        );
        match changed {
            "source" => fs::write(f.project.join("app.txt"), "changed by user").unwrap(),
            "worktree" => {
                fs::write(worktree.join("app.txt"), "changed after verification").unwrap()
            }
            _ => fs::write(summary, "tampered report").unwrap(),
        }
        assert!(
            !matches!(
                task.review().await,
                Ok(CodingOutcome::ReadyForReview { .. })
            ),
            "changed {changed} retained readiness"
        );
    }
}

#[tokio::test]
async fn cancellation_and_timeout_reap_the_owned_runner_without_late_ready() {
    for timeout in [false, true] {
        let f = Fixture::new("printf 'started\\n' > \"$fixture_dir/started\"; exec sleep 30");
        let mut task = f.task(if timeout {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let mut run = Box::pin(task.run("Fix the project", token));
        let started = f.dir.join("started");
        tokio::select! {result=&mut run=>panic!("run completed before cancellation: {result:?}"),_=wait_file(&started)=>{}}
        if !timeout {
            cancel.store(true, Ordering::Release);
        }
        let result = tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .unwrap();
        assert!(!matches!(result, Ok(CodingOutcome::ReadyForReview { .. })));
        let pid = fs::read_to_string(f.dir.join("process.pid")).unwrap();
        assert!(
            !Command::new("kill")
                .args(["-0", pid.trim()])
                .output()
                .unwrap()
                .status
                .success(),
            "owned runner survived cancellation/timeout"
        );
        f.assert_original();
    }
}

#[tokio::test]
async fn oversized_process_output_or_final_report_never_becomes_reviewable() {
    for body in [
        "head -c 1100000 /dev/zero; printf '{\"type\":\"turn.completed\"}\\n'",
        "head -c 1100000 /dev/zero >&2; printf 'done' > \"$last\"; printf '{\"type\":\"turn.completed\"}\\n'",
        "head -c 66000 /dev/zero > \"$last\"; printf '{\"type\":\"turn.completed\"}\\n'",
    ] {
        let f = Fixture::new(body);
        let mut task = f.task(Duration::from_secs(4));
        assert!(!matches!(
            task.run("Fix the project", Arc::new(AtomicBool::new(false)))
                .await,
            Ok(CodingOutcome::ReadyForReview { .. })
        ));
        f.assert_original();
    }
}

#[tokio::test]
async fn invalid_instruction_or_precancel_does_not_start_runner() {
    for instruction in ["".to_string(), "x".repeat(8193), "fix\0this".into()] {
        let f = Fixture::new(SUCCESS);
        let mut task = f.task(Duration::from_secs(3));
        assert!(!matches!(
            task.run(&instruction, Arc::new(AtomicBool::new(false)))
                .await,
            Ok(CodingOutcome::ReadyForReview { .. })
        ));
        assert!(!f.dir.join("argv").exists());
        f.assert_original();
    }
    let f = Fixture::new(SUCCESS);
    let mut task = f.task(Duration::from_secs(3));
    assert!(matches!(
        task.run("Fix project", Arc::new(AtomicBool::new(true)))
            .await
            .unwrap(),
        CodingOutcome::Cancelled { .. }
    ));
    assert!(!f.dir.join("argv").exists());
}

#[tokio::test]
async fn original_project_changes_while_codex_runs_block_publication() {
    let f = Fixture::new(&format!(
        "touch \"$fixture_dir/started\"\nwhile [ ! -f \"$fixture_dir/release\" ]; do sleep 0.01; done\n{SUCCESS}"
    ));
    let mut task = f.task(Duration::from_secs(4));
    let mut run = Box::pin(task.run("Fix the project", Arc::new(AtomicBool::new(false))));
    let started = f.dir.join("started");
    tokio::select! {result=&mut run=>panic!("run finished before source change: {result:?}"),_=wait_file(&started)=>{}}
    fs::write(f.project.join("app.txt"), "new user change\n").unwrap();
    fs::write(f.dir.join("release"), "").unwrap();
    assert!(matches!(run.await.unwrap(), CodingOutcome::Blocked { .. }));
    assert_eq!(
        fs::read_to_string(f.project.join("app.txt")).unwrap(),
        "new user change\n"
    );
}

#[tokio::test]
async fn actual_diff_bypasses_repository_external_diff_and_text_conversion_helpers() {
    let f = Fixture::new(SUCCESS);
    let helper = fs::canonicalize(f.dir.path())
        .unwrap()
        .join("untrusted-diff-helper");
    fs::write(
        &helper,
        "#!/bin/sh\ntouch \"${0%/*}/HELPER_EXECUTED\"\nprintf 'fabricated diff'\n",
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &f.project,
        &["config", "diff.external", helper.to_str().unwrap()],
    );
    git(
        &f.project,
        &["config", "diff.fixture.textconv", helper.to_str().unwrap()],
    );
    fs::write(f.project.join(".gitattributes"), "app.txt diff=fixture\n").unwrap();
    git(&f.project, &["add", ".gitattributes"]);
    git(&f.project, &["commit", "-qm", "fixture attributes"]);
    let hook = f.project.join(".git/hooks/post-checkout");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\n",
            f.dir.join("HOOK_EXECUTED").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let mut task = f.task(Duration::from_secs(5));
    let (summary, _) = ready(
        task.run("Fix the project", Arc::new(AtomicBool::new(false)))
            .await
            .unwrap(),
    );
    assert!(matches!(
        task.review().await.unwrap(),
        CodingOutcome::ReadyForReview { .. }
    ));
    assert!(!f.dir.join("HELPER_EXECUTED").exists());
    assert!(!f.dir.join("HOOK_EXECUTED").exists());
    let diff = fs::read_to_string(summary.parent().unwrap().join("changes.diff")).unwrap();
    assert!(diff.contains("-original application") && diff.contains("+revised application"));
    f.assert_original();
}

#[tokio::test]
async fn repository_tool_configuration_cannot_enable_external_integrations() {
    for name in [".codex/config.toml", ".mcp.json", ".codex.toml"] {
        let f = Fixture::new(SUCCESS);
        let config = f.project.join(name);
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "fixture configuration must not be executed\n").unwrap();
        git(&f.project, &["add", name]);
        git(&f.project, &["commit", "-qm", "fixture configuration"]);
        let mut task = f.task(Duration::from_secs(4));
        assert!(matches!(
            task.run("Fix the project", Arc::new(AtomicBool::new(false)))
                .await
                .unwrap(),
            CodingOutcome::Blocked { .. }
        ));
        assert!(
            !f.dir.join("argv").exists(),
            "repository configuration reached Codex"
        );
    }
}

#[tokio::test]
async fn owned_codex_process_does_not_inherit_provider_secrets() {
    const CHILD_FLAG: &str = "JARVIS_CODING_ENV_FIXTURE_CHILD";
    if std::env::var(CHILD_FLAG).as_deref() != Ok("1") {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "owned_codex_process_does_not_inherit_provider_secrets",
                "--nocapture",
            ])
            .env(CHILD_FLAG, "1");
        for key in [
            "CRUSOE_API_KEY",
            "TYPESAFE_API_KEY",
            "YOU_API_KEY",
            "ELEVENLABS_API_KEY",
            "OPENAI_API_KEY",
        ] {
            child.env(key, "fixture-only-secret-must-not-reach-runner");
        }
        let result = child.output().unwrap();
        assert!(
            result.status.success(),
            "isolated environment fixture failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&result.stdout)
                .contains("fixture-only-secret-must-not-reach-runner")
        );
        return;
    }
    let guard = r#"if [ "${CRUSOE_API_KEY+x}${TYPESAFE_API_KEY+x}${YOU_API_KEY+x}${ELEVENLABS_API_KEY+x}${OPENAI_API_KEY+x}" != "" ]; then exit 19; fi"#;
    let f = Fixture::new(&format!("{guard}\n{SUCCESS}"));
    let mut task = f.task(Duration::from_secs(5));
    ready(
        task.run("Fix the project", Arc::new(AtomicBool::new(false)))
            .await
            .unwrap(),
    );
    f.assert_original();
}

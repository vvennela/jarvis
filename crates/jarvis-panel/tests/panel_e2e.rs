//! Process-level panel checks. The smoke case exercises a real AppKit loop on macOS.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "jarvis-panel-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    #[cfg(target_os = "macos")]
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_bounded(dir: &TempDir, args: &[&str], deadline: Duration) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_jarvis-panel"))
        .current_dir(dir.path())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if started.elapsed() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "panel did not exit within {deadline:?}; stdout={}, stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn invalid_cli_requests_fail_without_default_collection_or_database_creation() {
    let dir = TempDir::new();
    for args in [
        vec![],
        vec!["--smoke-test"],
        vec!["--root"],
        vec!["--db"],
        vec!["--root", "root"],
        vec!["--db", "state.db"],
        vec!["--root", "root", "--root", "other", "--db", "state.db"],
        vec!["--root", "root", "--db", "state.db", "--send"],
        vec![
            "--root",
            "root",
            "--db",
            "state.db",
            "--voice-diagnostics",
            "--voice-diagnostics",
        ],
        vec![
            "--root",
            "root",
            "--db",
            "state.db",
            "--smoke-test",
            "--smoke-test",
        ],
    ] {
        let output = run_bounded(&dir, &args, Duration::from_secs(3));
        assert!(
            !output.status.success(),
            "invalid arguments succeeded: {args:?}"
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            0,
            "invalid arguments caused writes"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn missing_file_and_symlink_roots_are_rejected_before_creating_database() {
    let dir = TempDir::new();
    fs::write(dir.join("ordinary-file"), "user original").unwrap();
    fs::create_dir(dir.join("approved")).unwrap();
    std::os::unix::fs::symlink(dir.join("approved"), dir.join("alias")).unwrap();
    for root in ["missing", "ordinary-file", "alias"] {
        let output = run_bounded(
            &dir,
            &["--root", root, "--db", "state.db", "--smoke-test"],
            Duration::from_secs(4),
        );
        assert!(!output.status.success(), "invalid root succeeded: {root}");
        assert!(
            !dir.join("state.db").exists(),
            "invalid root created database"
        );
    }
    assert_eq!(
        fs::read_to_string(dir.join("ordinary-file")).unwrap(),
        "user original"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn real_appkit_smoke_exercises_awareness_pause_resume_and_quit() {
    let dir = TempDir::new();
    fs::create_dir(dir.join("approved")).unwrap();
    fs::write(dir.join("approved/spec.md"), "panel mango fixture").unwrap();
    let output = run_bounded(
        &dir,
        &["--root", "approved", "--db", "state.db", "--smoke-test"],
        Duration::from_secs(12),
    );
    assert!(
        output.status.success(),
        "AppKit smoke failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    for field in [
        "\"panel_created\":true",
        "\"active_observed\":true",
        "\"paused_observed\":true",
        "\"resumed_observed\":true",
        "\"microphone_active\":false",
        "\"compact_width\":360",
        "\"compact_height\":140",
        "\"expanded_observed\":true",
        "\"collapsed_observed\":true",
        "\"translucent\":true",
        "\"nonactivating\":true",
        "\"initial_hidden\":true",
        "\"menu_bar_present\":true",
        "\"wake_revealed\":true",
        "\"dismissed_hidden\":true",
        "\"workers_alive_after_dismiss\":true",
        "\"voice_unchanged_after_dismiss\":true",
        "\"menu_toggle_revealed\":true",
        "\"review_initially_disabled\":true",
        "\"task_worker_stopped\":true",
    ] {
        assert!(
            stdout.contains(field),
            "missing lifecycle outcome {field}: {stdout}"
        );
    }
    assert!(
        !stdout.contains("panel mango fixture"),
        "panel must not print indexed content"
    );
    let store =
        jarvis::context::ContextStore::open(&dir.join("state.db"), Default::default()).unwrap();
    assert_eq!(store.search("mango", 10).unwrap().len(), 1);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit short native smoke; coordinate with running app; never opens microphone"]
fn opt_in_voice_diagnostics_flag_emits_health_without_fixture_or_configuration_contents() {
    let dir = TempDir::new();
    fs::create_dir(dir.join("approved")).unwrap();
    fs::write(dir.join("approved/private.md"), "PRIVATE_FILE_SENTINEL").unwrap();
    fs::write(
        dir.join(".env"),
        "CRUSOE_API_KEY=SYNTHETIC_KEY_SENTINEL\nELEVENLABS_API_KEY=SYNTHETIC_KEY_SENTINEL\n",
    )
    .unwrap();
    let output = run_bounded(
        &dir,
        &[
            "--root",
            "approved",
            "--db",
            "state.db",
            "--smoke-test",
            "--voice-diagnostics",
        ],
        Duration::from_secs(12),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("jarvis_voice "))
        .collect();
    assert!(!lines.is_empty(), "requested diagnostics were not emitted");
    assert!(lines.len() <= 6, "diagnostics exceeded two-second cadence");
    for line in lines {
        for field in [
            "state=",
            "samples=",
            "rms=",
            "peak=",
            "decodes=",
            "wakes=",
            "commands=",
            "discontinuities=",
            "capture_errors=",
        ] {
            assert!(line.contains(field), "missing diagnostic field {field}");
        }
        assert!(line.len() < 400);
    }
    assert!(!stderr.contains("PRIVATE_FILE_SENTINEL"));
    assert!(!stderr.contains("SYNTHETIC_KEY_SENTINEL"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SYNTHETIC_KEY_SENTINEL"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"microphone_active\":false"));
}

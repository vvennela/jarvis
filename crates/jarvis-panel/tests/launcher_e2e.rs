//! Launcher CLI validation is portable. Signed native bundle smoke is opt-in.
use std::{path::PathBuf, process::Command};
#[path = "../../../tests/support/mod.rs"]
mod support;
fn launcher() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/launch-jarvis.command")
}
#[test]
fn launcher_help_and_invalid_arguments_return_without_build_or_launch() {
    let output = Command::new("/bin/bash")
        .arg(launcher())
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--build-only"));
    for arguments in [vec!["--unknown"], vec!["--root"], vec!["--db"]] {
        let output = Command::new("/bin/bash")
            .arg(launcher())
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires explicit generated app bundle; smoke never opens microphone"]
fn opt_in_signed_bundle_contains_native_executable_and_runs_hardware_free_smoke() {
    use std::{
        fs,
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let bundle = PathBuf::from(
        std::env::var_os("JARVIS_TEST_APP_BUNDLE")
            .expect("set JARVIS_TEST_APP_BUNDLE after launcher --build-only"),
    );
    assert!(bundle.is_absolute());
    assert!(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict"])
            .arg(&bundle)
            .status()
            .unwrap()
            .success()
    );
    let plist = bundle.join("Contents/Info.plist");
    let get = |key: &str| {
        let output = Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-"])
            .arg(&plist)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(get("CFBundleIdentifier"), "com.jarvis.panel");
    assert_eq!(get("CFBundlePackageType"), "APPL");
    assert_eq!(get("CFBundleExecutable"), "jarvis-panel");
    assert!(!get("NSMicrophoneUsageDescription").is_empty());
    let executable = bundle.join("Contents/MacOS/jarvis-panel");
    let file = Command::new("/usr/bin/file")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&file.stdout).contains("Mach-O"));
    assert!(!bundle.join(".env").exists());
    assert!(!bundle.join("Contents/.env").exists());
    let temp = std::env::temp_dir().join(format!("jarvis-bundle-smoke-{}", std::process::id()));
    fs::create_dir(&temp).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(temp.clone());
    let root = temp.join("approved");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("fixture.md"), "generated smoke fixture").unwrap();
    let mut child = Command::new(executable)
        .arg("--root")
        .arg(&root)
        .arg("--db")
        .arg(temp.join("state.db"))
        .arg("--smoke-test")
        .arg("--project-dir")
        .arg(&temp)
        .current_dir("/")
        .env_remove("CRUSOE_API_KEY")
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("ELEVENLABS_API_KEY")
        .env_remove("JARVIS_WHISPER_MODEL_PATH")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("signed bundle smoke timed out");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let flags: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(flags["panel_created"], true);
    assert_eq!(flags["microphone_active"], false);
    assert_eq!(flags["active_observed"], true);
    assert_eq!(flags["paused_observed"], true);
    assert_eq!(flags["resumed_observed"], true);
    for field in [
        "initial_hidden",
        "menu_bar_present",
        "wake_revealed",
        "dismissed_hidden",
        "workers_alive_after_dismiss",
        "voice_unchanged_after_dismiss",
        "menu_toggle_revealed",
    ] {
        assert_eq!(
            flags[field], true,
            "bundle failed visibility lifecycle: {field}"
        );
    }
}

#[test]
fn invalid_project_directory_is_rejected_before_database_or_worker_start() {
    let dir = support::TempDir::new();
    let root = dir.join("approved");
    std::fs::create_dir(&root).unwrap();
    let invalid_file = dir.join("not-a-directory");
    std::fs::write(&invalid_file, "fixture").unwrap();
    for project in [dir.join("missing-project"), invalid_file] {
        let output = Command::new(env!("CARGO_BIN_EXE_jarvis-panel"))
            .arg("--root")
            .arg(&root)
            .arg("--db")
            .arg(dir.join("never.db"))
            .arg("--project-dir")
            .arg(project)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("--project-dir must be an accessible directory")
        );
        assert!(output.stdout.is_empty());
        assert!(!dir.join("never.db").exists());
    }
    for project_args in [
        vec!["--project-dir"],
        vec!["--project-dir", ".", "--project-dir", "."],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_jarvis-panel"))
            .arg("--root")
            .arg(&root)
            .arg("--db")
            .arg(dir.join("never.db"))
            .args(project_args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!dir.join("never.db").exists());
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires explicit signed bundle; observes normal hardware-off app for 30 seconds"]
fn opt_in_normal_bundle_stays_alive_until_explicit_termination() {
    use std::{
        fs,
        process::{Child, Stdio},
        thread,
        time::{Duration, Instant},
    };
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    let bundle = PathBuf::from(
        std::env::var_os("JARVIS_TEST_APP_BUNDLE").expect("set JARVIS_TEST_APP_BUNDLE"),
    );
    let dir = support::TempDir::new();
    let root = dir.join("approved");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("fixture.md"), "lifetime fixture").unwrap();
    let mut command = Command::new(bundle.join("Contents/MacOS/jarvis-panel"));
    command
        .arg("--root")
        .arg(root)
        .arg("--db")
        .arg(dir.join("state.db"))
        .arg("--project-dir")
        .arg(dir.path())
        .current_dir(dir.path())
        .env_remove("CRUSOE_API_KEY")
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("ELEVENLABS_API_KEY")
        .env_remove("JARVIS_WHISPER_MODEL_PATH")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = OwnedChild(command.spawn().unwrap());
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "normal app exited before explicit termination after {:?}",
            started.elapsed()
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &child.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "explicit termination did not stop app"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

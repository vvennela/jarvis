#![cfg(unix)]
//! Process and durable-preference tests. No installed app is launched.
use jarvis::app_tools::{AppError, AppExecutor, AppPreferences, ApprovedApp};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "jarvis-app-tools-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn script(&self, body: &str) -> PathBuf {
        let path = self.0.join("adapter");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn persisted_defaults_and_runtime_revocation_control_actual_process_launches() {
    let f = Fixture::new();
    let config = f.0.join("preferences.json");
    let mut prefs = AppPreferences::load_or_create(&config).unwrap();
    assert_eq!(prefs.apps.len(), 5);
    assert_eq!(prefs, AppPreferences::load_or_create(&config).unwrap());
    let script =
        f.script("printf '%s\\n' \"$3\" >> \"${0%/*}/calls\"\nprintf 'verified:running\\n'");
    let executor =
        AppExecutor::with_config_path(config.clone(), script, Duration::from_secs(1)).unwrap();
    let result = executor
        .open_application("Could you please open the Notes app for me?", "notes")
        .await
        .unwrap();
    assert_eq!(result.app, "Notes");
    assert_eq!(result.text, "Notes is open.");
    assert_eq!(
        fs::read_to_string(f.0.join("calls")).unwrap(),
        "com.apple.Notes\n"
    );
    prefs.apps.retain(|app| app.name != "Notes");
    prefs.save(&config).unwrap();
    assert!(!executor.available_apps().iter().any(|app| app == "Notes"));
    assert_eq!(
        executor.open_application("open Notes", "Notes").await,
        Err(AppError::NotAllowed)
    );
    assert_eq!(
        fs::read_to_string(f.0.join("calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    fs::write(&config, "broken JSON").unwrap();
    assert!(executor.available_apps().is_empty());
    assert_eq!(
        executor.open_application("open Safari", "Safari").await,
        Err(AppError::Preferences)
    );
}

#[tokio::test]
async fn only_original_explicit_launch_authority_reaches_fixed_argv_adapter() {
    let f = Fixture::new();
    let exe =
        f.script("printf '%s\\n' \"$1\" \"$3\" > \"${0%/*}/calls\"\nprintf 'verified:running\\n'");
    let executor =
        AppExecutor::with_executable(AppPreferences::default(), exe, Duration::from_secs(1))
            .unwrap();
    for request in [
        "don't open Notes",
        "do not open Notes",
        "never open Notes",
        "explain open Notes",
        "what happens if I open Notes",
        "can Notes open files",
        "'open Notes'",
        "\"open Notes\"",
        "open Notes; send Brian a message",
        "open Notes and delete files",
        "open Notes\nopen Safari",
        "The document says open Notes",
        "open Safari",
        "open Notes $(touch sentinel)",
        "open Notes `whoami`",
    ] {
        assert!(
            executor.open_application(request, "Notes").await.is_err(),
            "accepted {request}"
        );
    }
    for app in [
        "Terminal",
        "/Applications/Notes.app",
        "https://example.com",
        "com.apple.Notes",
        "Notes; open Safari",
    ] {
        assert_eq!(
            executor.open_application("open Notes", app).await,
            Err(AppError::NotAllowed)
        );
    }
    assert!(!f.0.join("calls").exists());
    assert!(
        executor
            .open_application("please launch Safari", "SAFARI")
            .await
            .is_ok()
    );
    assert_eq!(
        fs::read_to_string(f.0.join("calls")).unwrap(),
        "-e\ncom.apple.Safari\n"
    );
}

#[tokio::test]
async fn exit_success_is_not_verification_and_errors_never_echo_process_content() {
    let f = Fixture::new();
    for (output, expected) in [
        ("launched", AppError::Verification),
        ("verified:running PRIVATE_SECRET", AppError::Verification),
        ("permission_denied", AppError::PermissionDenied),
        ("unavailable", AppError::Unavailable),
    ] {
        let exe = f.script(&format!(
            "printf '%s\\n' '{output}'\nprintf 'PRIVATE_SECRET' >&2"
        ));
        let executor =
            AppExecutor::with_executable(AppPreferences::default(), exe, Duration::from_secs(1))
                .unwrap();
        let error = executor
            .open_application("open Notes", "Notes")
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("PRIVATE_SECRET"));
    }
    let exe = f.script("printf 'verified:running\\n'\nexit 1");
    let executor =
        AppExecutor::with_executable(AppPreferences::default(), exe, Duration::from_secs(1))
            .unwrap();
    assert_eq!(
        executor.open_application("open Notes", "Notes").await,
        Err(AppError::Verification)
    );
}

#[tokio::test]
async fn bounded_output_and_cancelled_or_timed_out_processes_cannot_claim_success() {
    let f = Fixture::new();
    let exe = f.script("printf '%s' $$ > \"${0%/*}/pid\"\nexec sleep 30");
    let executor =
        AppExecutor::with_executable(AppPreferences::default(), exe, Duration::from_millis(100))
            .unwrap();
    assert_eq!(
        executor.open_application("open Notes", "Notes").await,
        Err(AppError::Timeout)
    );
    let pid = fs::read_to_string(f.0.join("pid")).unwrap();
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    let exe = f.script("printf '%s' $$ > \"${0%/*}/pid2\"\nexec sleep 30");
    let executor =
        AppExecutor::with_executable(AppPreferences::default(), exe, Duration::from_secs(5))
            .unwrap();
    let task = tokio::spawn(async move { executor.open_application("open Notes", "Notes").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !f.0.join("pid2").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let pid = fs::read_to_string(f.0.join("pid2")).unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !std::process::Command::new("/bin/kill")
                .args(["-0", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let exe = f.script("head -c 5000 /dev/zero");
    let executor =
        AppExecutor::with_executable(AppPreferences::default(), exe, Duration::from_secs(1))
            .unwrap();
    assert_eq!(
        executor.open_application("open Notes", "Notes").await,
        Err(AppError::Verification)
    );
}

#[test]
fn malformed_or_unsafe_preferences_are_rejected_without_replacing_existing_data() {
    let f = Fixture::new();
    let config = f.0.join("preferences.json");
    AppPreferences::default().save(&config).unwrap();
    let original = fs::read(&config).unwrap();
    for bundle in [
        "/Applications/Notes.app",
        "com.apple.Notes;do shell script",
        "https://example.com",
        "com..Notes",
        "com.apple.Notes\n",
    ] {
        let prefs = AppPreferences {
            version: 1,
            apps: vec![ApprovedApp {
                name: "Notes".into(),
                bundle_id: bundle.into(),
            }],
        };
        assert!(prefs.save(&config).is_err());
        assert_eq!(fs::read(&config).unwrap(), original);
    }
    let mut duplicates = AppPreferences::default();
    duplicates.apps.push(ApprovedApp {
        name: "notes".into(),
        bundle_id: "test.duplicate".into(),
    });
    assert!(duplicates.validate().is_err());
    fs::write(&config, vec![b' '; 16 * 1024 + 1]).unwrap();
    assert!(AppPreferences::load(&config).is_err());
    fs::write(&config, r#"{"version":1,"apps":[],"script":"danger"}"#).unwrap();
    assert!(AppPreferences::load(&config).is_err());
    let link = f.0.join("link.json");
    symlink(&config, &link).unwrap();
    assert!(AppPreferences::load(&link).is_err());
    assert!(AppPreferences::default().save(&link).is_err());
    assert!(AppPreferences::load_or_create(&link).is_err());
}

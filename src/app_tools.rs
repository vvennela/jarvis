//! Durable app allowlist and a verified native launch adapter.
//! User/model text never becomes AppleScript, shell code, paths, or URLs.
use serde::{Deserialize, Serialize};
use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

const MAX_PREFERENCES_BYTES: usize = 16 * 1024;
const MAX_APPS: usize = 16;
const MAX_OUTPUT: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovedApp {
    pub name: String,
    pub bundle_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPreferences {
    pub version: u32,
    pub apps: Vec<ApprovedApp>,
}
impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            apps: [
                ("Spotify", "com.spotify.client"),
                ("Notes", "com.apple.Notes"),
                ("Safari", "com.apple.Safari"),
                ("Calendar", "com.apple.iCal"),
                ("Calculator", "com.apple.calculator"),
            ]
            .into_iter()
            .map(|(name, bundle_id)| ApprovedApp {
                name: name.into(),
                bundle_id: bundle_id.into(),
            })
            .collect(),
        }
    }
}
impl AppPreferences {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.version != 1 || self.apps.len() > MAX_APPS {
            return Err(AppError::Preferences);
        }
        for (index, app) in self.apps.iter().enumerate() {
            if app.name.is_empty()
                || app.name.len() > 64
                || app.name.trim() != app.name
                || !app
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'-')
                || app.bundle_id.len() > 255
                || !app.bundle_id.contains('.')
                || app.bundle_id.split('.').any(|part| {
                    part.is_empty()
                        || !part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                || self.apps[..index].iter().any(|other| {
                    other.name.eq_ignore_ascii_case(&app.name) || other.bundle_id == app.bundle_id
                })
            {
                return Err(AppError::Preferences);
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self, AppError> {
        let file = open_regular(path)?;
        let mut bytes = Vec::with_capacity(MAX_PREFERENCES_BYTES + 1);
        file.take((MAX_PREFERENCES_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| AppError::Preferences)?;
        if bytes.len() > MAX_PREFERENCES_BYTES {
            return Err(AppError::Preferences);
        }
        let preferences: Self =
            serde_json::from_slice(&bytes).map_err(|_| AppError::Preferences)?;
        preferences.validate()?;
        Ok(preferences)
    }
    pub fn load_or_create(path: &Path) -> Result<Self, AppError> {
        if !path.is_absolute() {
            return Err(AppError::Preferences);
        }
        match fs::symlink_metadata(path) {
            Ok(_) => Self::load(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let preferences = Self::default();
                let parent = path.parent().ok_or(AppError::Preferences)?;
                fs::create_dir_all(parent).map_err(|_| AppError::Preferences)?;
                let bytes =
                    serde_json::to_vec_pretty(&preferences).map_err(|_| AppError::Preferences)?;
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                {
                    Ok(mut file) => {
                        file.write_all(&bytes)
                            .and_then(|()| file.sync_all())
                            .map_err(|_| AppError::Preferences)?;
                        Ok(preferences)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        Self::load(path)
                    }
                    Err(_) => Err(AppError::Preferences),
                }
            }
            Err(_) => Err(AppError::Preferences),
        }
    }
    /// Atomically replace validated preferences; a revoked app disappears from
    /// subsequent execution checks even when the runtime is already running.
    pub fn save(&self, path: &Path) -> Result<(), AppError> {
        self.validate()?;
        if !path.is_absolute() {
            return Err(AppError::Preferences);
        }
        if let Ok(metadata) = fs::symlink_metadata(path)
            && !metadata.is_file()
        {
            return Err(AppError::Preferences);
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| AppError::Preferences)?;
        if bytes.len() > MAX_PREFERENCES_BYTES {
            return Err(AppError::Preferences);
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| AppError::Preferences)?;
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let temp = path.with_extension(format!("jarvis-{suffix}.tmp"));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|_| AppError::Preferences)?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| AppError::Preferences)?;
            fs::rename(&temp, path).map_err(|_| AppError::Preferences)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}
fn open_regular(path: &Path) -> Result<fs::File, AppError> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags, open};
        fs::File::from(
            open(
                path,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|_| AppError::Preferences)?,
        )
    };
    #[cfg(not(unix))]
    let file = {
        if fs::symlink_metadata(path)
            .map_err(|_| AppError::Preferences)?
            .file_type()
            .is_symlink()
        {
            return Err(AppError::Preferences);
        }
        fs::File::open(path).map_err(|_| AppError::Preferences)?
    };
    let metadata = file.metadata().map_err(|_| AppError::Preferences)?;
    if !metadata.is_file() || metadata.len() > MAX_PREFERENCES_BYTES as u64 {
        return Err(AppError::Preferences);
    }
    Ok(file)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppError {
    Preferences,
    NotAllowed,
    Clarification,
    PermissionDenied,
    Unavailable,
    Timeout,
    Verification,
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Preferences => "Check Jarvis's app preferences before opening apps.",
            Self::NotAllowed => "That app isn't in your approved app list.",
            Self::Clarification => "Tell me which approved app you'd like me to open.",
            Self::PermissionDenied => "Allow Jarvis to control this app in System Settings, Privacy & Security, Automation, then try again.",
            Self::Unavailable => "That app isn't available. Check that it's installed on this Mac.",
            Self::Timeout => "The app took too long to respond. Check it before trying again.",
            Self::Verification => "I couldn't verify that the app opened. Check it before trying again.",
        })
    }
}
impl std::error::Error for AppError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppResult {
    pub text: String,
    pub app: String,
}
enum PreferencesSource {
    Durable(PathBuf),
    Static(AppPreferences),
}
pub struct AppExecutor {
    source: PreferencesSource,
    executable: PathBuf,
    timeout: Duration,
}
impl AppExecutor {
    pub fn native() -> Result<Self, AppError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(AppError::Preferences)?;
        let path = home.join("Library/Application Support/Jarvis/app-preferences.json");
        AppPreferences::load_or_create(&path)?;
        Self::with_config_path(
            path,
            PathBuf::from("/usr/bin/osascript"),
            Duration::from_secs(20),
        )
    }
    pub fn from_preferences(preferences: AppPreferences) -> Result<Self, AppError> {
        Self::with_executable(
            preferences,
            PathBuf::from("/usr/bin/osascript"),
            Duration::from_secs(20),
        )
    }
    pub fn with_executable(
        preferences: AppPreferences,
        executable: PathBuf,
        timeout: Duration,
    ) -> Result<Self, AppError> {
        preferences.validate()?;
        Self::build(PreferencesSource::Static(preferences), executable, timeout)
    }
    pub fn with_config_path(
        path: PathBuf,
        executable: PathBuf,
        timeout: Duration,
    ) -> Result<Self, AppError> {
        if !path.is_absolute() {
            return Err(AppError::Preferences);
        }
        AppPreferences::load(&path)?;
        Self::build(PreferencesSource::Durable(path), executable, timeout)
    }
    fn build(
        source: PreferencesSource,
        executable: PathBuf,
        timeout: Duration,
    ) -> Result<Self, AppError> {
        if !executable.is_absolute() || timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(AppError::Preferences);
        }
        Ok(Self {
            source,
            executable,
            timeout,
        })
    }
    fn preferences(&self) -> Result<AppPreferences, AppError> {
        match &self.source {
            PreferencesSource::Static(prefs) => Ok(prefs.clone()),
            PreferencesSource::Durable(path) => AppPreferences::load(path),
        }
    }
    pub fn available_apps(&self) -> Vec<String> {
        self.preferences()
            .map(|prefs| prefs.apps.into_iter().map(|app| app.name).collect())
            .unwrap_or_default()
    }
    pub async fn open_application(
        &self,
        user_request: &str,
        app: &str,
    ) -> Result<AppResult, AppError> {
        let started = Instant::now();
        let preferences = self.preferences()?;
        let approved = preferences
            .apps
            .iter()
            .find(|allowed| allowed.name.eq_ignore_ascii_case(app.trim()))
            .ok_or(AppError::NotAllowed)?;
        authorize_open(user_request, &approved.name)?;
        if crate::gate(
            crate::Operation::OpenApplication,
            crate::EvidenceStatus::Ready,
            started.elapsed().as_millis() as u64,
            self.timeout.as_millis().max(1) as u64,
        ) != crate::GateDecision::Allow
        {
            return Err(AppError::Timeout);
        }
        let mut child = tokio::process::Command::new(&self.executable)
            .args(["-e", OPEN_APP_SCRIPT, &approved.bundle_id])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| AppError::Unavailable)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or(AppError::Verification)?
            .take((MAX_OUTPUT + 1) as u64);
        let mut stderr = child
            .stderr
            .take()
            .ok_or(AppError::Verification)?
            .take((MAX_OUTPUT + 1) as u64);
        let mut out = Vec::with_capacity(MAX_OUTPUT + 1);
        let mut err = Vec::with_capacity(MAX_OUTPUT + 1);
        let result = tokio::time::timeout(self.timeout, async {
            tokio::try_join!(
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err),
                child.wait()
            )
        })
        .await;
        let status = match result {
            Ok(Ok((_, _, status))) => status,
            Ok(Err(_)) => {
                let _ = child.kill().await;
                return Err(AppError::Verification);
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(AppError::Timeout);
            }
        };
        if !status.success() || out.len() > MAX_OUTPUT || err.len() > MAX_OUTPUT {
            return Err(AppError::Verification);
        }
        match std::str::from_utf8(&out)
            .map_err(|_| AppError::Verification)?
            .trim()
        {
            "verified:running" => Ok(AppResult {
                text: format!("{} is open.", approved.name),
                app: approved.name.clone(),
            }),
            "permission_denied" => Err(AppError::PermissionDenied),
            "unavailable" => Err(AppError::Unavailable),
            _ => Err(AppError::Verification),
        }
    }
}
pub fn authorize_open(request: &str, approved_name: &str) -> Result<(), AppError> {
    if request.is_empty()
        || request.len() > 8192
        || request
            .chars()
            .any(|c| c.is_control() || matches!(c, '\'' | '"' | '`' | ';' | '$'))
    {
        return Err(AppError::Clarification);
    }
    let normalized = request
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    let normalized = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let request = normalized.strip_prefix("please ").unwrap_or(&normalized);
    let request = request
        .strip_prefix("could you ")
        .or_else(|| request.strip_prefix("can you "))
        .or_else(|| request.strip_prefix("would you "))
        .unwrap_or(request);
    let request = request.strip_prefix("please ").unwrap_or(request);
    let request = request.strip_suffix(" please").unwrap_or(request);
    let request = request.strip_suffix(" for me").unwrap_or(request);
    let target = request
        .strip_prefix("open ")
        .or_else(|| request.strip_prefix("launch "))
        .or_else(|| request.strip_prefix("start "))
        .or_else(|| request.strip_prefix("bring up "))
        .ok_or(AppError::Clarification)?;
    let target = target.strip_prefix("the ").unwrap_or(target);
    let target = target
        .strip_suffix(" application")
        .or_else(|| target.strip_suffix(" app"))
        .unwrap_or(target);
    if !target.eq_ignore_ascii_case(approved_name) {
        return Err(AppError::Clarification);
    }
    Ok(())
}
const OPEN_APP_SCRIPT: &str = r#"on run argv
set approvedBundle to item 1 of argv
try
tell application id approvedBundle to activate
repeat 30 times
if application id approvedBundle is running then return "verified:running"
delay 0.1
end repeat
return "unavailable"
on error messageText number errorNumber
if errorNumber is -1743 then return "permission_denied"
return "unavailable"
end try
end run"#;

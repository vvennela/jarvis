//! Structured foreground awareness. The native collector polls only app identity
//! and focused-window metadata; it never reads screenshots, selection or fields.
#![deny(unsafe_op_in_unsafe_fn)]
use jarvis::context::{ContextStore, ObservationResult, Source};
use serde::Serialize;
use std::fmt;

pub const RESOURCE_ID: &str = "os:foreground";
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
pub const MAX_APP_BYTES: usize = 256;
pub const MAX_TITLE_BYTES: usize = 2048;
pub const MAX_WINDOW_ID_BYTES: usize = 256;

#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct AppInfo {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub name: Option<String>,
}
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: Option<String>,
    pub title: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Available,
    PermissionMissing,
    NoFocusedWindow,
    Unavailable,
    Excluded,
}
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct Observation {
    pub app: Option<AppInfo>,
    pub window: Option<WindowInfo>,
    pub status: Status,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AwarenessError {
    InvalidObservation,
    Storage,
    NativeUnavailable,
}
impl fmt::Display for AwarenessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidObservation => "invalid or oversized foreground metadata",
            Self::Storage => "foreground context storage unavailable",
            Self::NativeUnavailable => "native foreground awareness requires macOS",
        })
    }
}
impl std::error::Error for AwarenessError {}

/// Known browser families are excluded because AX cannot identify private tabs.
/// Unidentified apps are also excluded; extension origin grants govern browsers.
pub fn window_content_allowed(bundle_id: &str) -> bool {
    if bundle_id.is_empty() || bundle_id.len() > MAX_APP_BYTES {
        return false;
    }
    let id = bundle_id.to_ascii_lowercase();
    ![
        "com.google.chrome",
        "com.apple.safari",
        "com.apple.safaritechnologypreview",
        "org.mozilla",
        "com.microsoft.edgemac",
        "com.brave.browser",
        "com.operasoftware",
        "com.vivaldi",
        "com.kagi",
        "company.thebrowser",
        "org.chromium",
        "app.zen-browser",
        "com.duckduckgo",
        "com.apple.passwords",
        "com.agilebits",
        "com.1password",
        "com.bitwarden",
        "com.lastpass",
        "com.dashlane",
        "com.keepersecurity",
        "com.keepass",
        "org.keepass",
        "com.protonpass",
        "me.proton.pass",
        "proton.pass",
        "in.sinew.enpass",
        "com.apple.keychainaccess",
    ]
    .iter()
    .any(|prefix| id == *prefix || id.starts_with(&format!("{prefix}.")))
}
fn valid(value: &Option<String>, cap: usize) -> bool {
    value
        .as_ref()
        .is_none_or(|s| s.len() <= cap && !s.contains('\0'))
}
/// Store one current-focus resource. Timestamp stays outside serialized content,
/// so unchanged polls refresh freshness without generating artificial versions.
pub fn publish(
    store: &mut ContextStore,
    mut observation: Observation,
    observed_at_ms: i64,
) -> Result<ObservationResult, AwarenessError> {
    if observed_at_ms < 0 {
        return Err(AwarenessError::InvalidObservation);
    }
    if let Some(app) = &observation.app {
        if app.pid <= 0 || !valid(&app.bundle_id, MAX_APP_BYTES) || !valid(&app.name, MAX_APP_BYTES)
        {
            return Err(AwarenessError::InvalidObservation);
        }
        if !app.bundle_id.as_deref().is_some_and(window_content_allowed) {
            observation.window = None;
            observation.status = Status::Excluded;
        }
    } else {
        observation.window = None;
        observation.status = Status::Unavailable;
    }
    if observation.status != Status::Available {
        observation.window = None;
    }
    if let Some(window) = &observation.window
        && (!valid(&window.id, MAX_WINDOW_ID_BYTES) || !valid(&window.title, MAX_TITLE_BYTES))
    {
        return Err(AwarenessError::InvalidObservation);
    }
    #[derive(Serialize)]
    struct Evidence<'a> {
        actor: &'static str,
        trust: &'static str,
        collector: &'static str,
        observation: &'a Observation,
    }
    let content = serde_json::to_string(&Evidence {
        actor: "unknown",
        trust: "untrusted_app_metadata",
        collector: "macos_foreground",
        observation: &observation,
    })
    .map_err(|_| AwarenessError::InvalidObservation)?;
    store
        .observe(RESOURCE_ID, &content, Source::Accessibility, observed_at_ms)
        .map_err(|_| AwarenessError::Storage)
}
#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
pub use native::capture;
#[cfg(not(target_os = "macos"))]
pub fn capture() -> Result<Observation, AwarenessError> {
    Err(AwarenessError::NativeUnavailable)
}

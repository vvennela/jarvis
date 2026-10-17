//! User-authorized, allowlisted desktop actions. All scripts are static code;
//! user text and model proposals never become shell or AppleScript source.
use std::{fmt, path::PathBuf, process::Stdio, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::{EvidenceStatus, GateDecision, Operation, gate};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    OpenSpotify,
    PlaySpotify,
    PauseSpotify,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    pub action: Action,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionError {
    InvalidConfiguration,
    NotAuthorized,
    Clarification,
    PermissionDenied,
    Unavailable,
    Timeout,
    Verification,
    NoSelection,
}
impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "Spotify controls need a quick setup fix.",
            Self::NotAuthorized | Self::Clarification => "Do you want me to open Spotify, resume it, or pause it?",
            Self::PermissionDenied => "I need permission to control Spotify. Enable Jarvis under System Settings, Privacy & Security, Automation.",
            Self::Unavailable => "I couldn't reach the Spotify desktop app. Is it installed?",
            Self::Timeout => "Spotify isn't responding yet. I couldn't confirm the change.",
            Self::Verification => "I couldn't confirm Spotify's playback state.",
            Self::NoSelection => "Spotify's open, but nothing started playing. It may need a track selected first.",
        })
    }
}
impl std::error::Error for ActionError {}

/// Request-aware copy is for the user only, never an executable proposal. A
/// named selection must not silently become resume-current-selection.
pub fn friendly_error(request: &str, error: &ActionError) -> String {
    if matches!(
        error,
        ActionError::NotAuthorized | ActionError::Clarification
    ) && let Some(selection) = named_selection(request)
    {
        return format!("I can resume Spotify, but I can't pick {selection} yet.");
    }
    error.to_string()
}

fn named_selection(request: &str) -> Option<String> {
    let normalized = normalize(request)?;
    let normalized = normalized
        .strip_suffix(" in the background")
        .unwrap_or(&normalized);
    let selection = normalized.strip_prefix("play ")?;
    let selection = selection
        .strip_suffix(" on spotify")
        .or_else(|| selection.strip_suffix(" in spotify"))
        .or_else(|| selection.strip_suffix(" using spotify"))?;
    if selection.is_empty()
        || selection.len() > 80
        || matches!(selection, "music" | "some music" | "my music" | "playback")
        || !selection
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '/' | '&'))
        || selection.split_whitespace().any(|word| {
            matches!(
                word,
                "and"
                    | "then"
                    | "send"
                    | "pay"
                    | "delete"
                    | "not"
                    | "never"
                    | "open"
                    | "launch"
                    | "resume"
                    | "pause"
                    | "stop"
            )
        })
    {
        return None;
    }
    // ASCII case normalization preserves byte offsets, including Unicode names.
    let original = request.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = original.to_ascii_lowercase();
    let start = lower.find("play ")? + "play ".len();
    Some(original[start..start + selection.len()].to_owned())
}

fn normalize(request: &str) -> Option<String> {
    if request.is_empty() || request.len() > 8192 || request.chars().any(char::is_control) {
        return None;
    }
    let request = request
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    let request = request.strip_prefix("please ").unwrap_or(&request);
    let request = request
        .strip_prefix("can you ")
        .or_else(|| request.strip_prefix("could you "))
        .or_else(|| request.strip_prefix("would you "))
        .unwrap_or(request);
    let request = request.strip_prefix("please ").unwrap_or(request);
    let request = request.strip_suffix(" please").unwrap_or(request);
    Some(request.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The fast path recognizes complete requests, never substrings in documents,
/// explanations, negations, named-song requests, or chains of commands.
pub fn recognize(request: &str) -> Option<Action> {
    let normalized = normalize(request)?;
    let playback = normalized
        .strip_suffix(" in the background")
        .unwrap_or(&normalized);
    if [
        "open spotify and ",
        "launch spotify and ",
        "start spotify and ",
    ]
    .iter()
    .filter_map(|prefix| playback.strip_prefix(prefix))
    .any(|tail| {
        matches!(
            tail,
            "play"
                | "play music"
                | "play some music"
                | "resume"
                | "resume music"
                | "resume playback"
        )
    }) || matches!(
        playback,
        "play spotify"
            | "resume spotify"
            | "play music on spotify"
            | "play some music on spotify"
            | "play music in spotify"
            | "resume music on spotify"
            | "resume playback on spotify"
            | "start playing music on spotify"
    ) {
        return Some(Action::PlaySpotify);
    }
    match normalized.as_str() {
        "open spotify" | "launch spotify" | "start spotify" => Some(Action::OpenSpotify),
        "pause spotify"
        | "pause music on spotify"
        | "pause playback on spotify"
        | "stop spotify"
        | "stop music on spotify" => Some(Action::PauseSpotify),
        _ => None,
    }
}

fn request_phrase(request: &str) -> bool {
    let imperative = request
        .strip_prefix("would you ")
        .or_else(|| request.strip_prefix("i want to "))
        .or_else(|| request.strip_prefix("i would like you to "))
        .or_else(|| request.strip_prefix("i would like to "))
        .or_else(|| request.strip_prefix("spotify "))
        .unwrap_or(request);
    matches!(
        imperative.split_whitespace().next(),
        Some("open" | "launch" | "start" | "play" | "resume" | "pause" | "stop" | "put")
    )
}

/// Broader Spotify requests may ask the planner for a closed action choice.
/// Execution still checks every token and the action's compatible intent.
pub fn is_action_request(request: &str) -> bool {
    let Some(request) = normalize(request) else {
        return false;
    };
    let words: Vec<_> = request.split_whitespace().collect();
    request_phrase(&request)
        && words.contains(&"spotify")
        && words.iter().any(|w| {
            matches!(
                *w,
                "open" | "launch" | "start" | "play" | "resume" | "pause" | "stop" | "put"
            )
        })
        && !words.iter().any(|w| {
            matches!(
                *w,
                "explain"
                    | "describe"
                    | "why"
                    | "how"
                    | "what"
                    | "don't"
                    | "dont"
                    | "not"
                    | "never"
            )
        })
}

/// The only admitted app is the fixed Spotify bundle ID. A planner may select
/// an action but cannot broaden the user's scope or the native tool catalog.
pub fn gate_proposal(request: &str, action: Action) -> Result<(), ActionError> {
    if let Some(authorized) = recognize(request) {
        return if action == authorized {
            Ok(())
        } else {
            Err(ActionError::NotAuthorized)
        };
    }
    let request = normalize(request).ok_or(ActionError::NotAuthorized)?;
    let words: Vec<_> = request.split_whitespace().collect();
    if !request_phrase(&request)
        || !words.contains(&"spotify")
        || words.iter().any(|word| {
            !matches!(
                *word,
                "spotify"
                    | "open"
                    | "launch"
                    | "start"
                    | "play"
                    | "resume"
                    | "pause"
                    | "stop"
                    | "music"
                    | "playback"
                    | "playing"
                    | "some"
                    | "my"
                    | "the"
                    | "app"
                    | "application"
                    | "on"
                    | "in"
                    | "using"
                    | "with"
                    | "up"
                    | "for"
                    | "me"
                    | "please"
                    | "would"
                    | "you"
                    | "can"
                    | "could"
                    | "i"
                    | "want"
                    | "like"
                    | "to"
                    | "put"
            )
        })
    {
        return Err(ActionError::NotAuthorized);
    }
    let pause = words.iter().any(|w| matches!(*w, "pause" | "stop"));
    let play = words
        .iter()
        .any(|w| matches!(*w, "play" | "resume" | "playing" | "put"))
        || (words.contains(&"start") && words.iter().any(|w| matches!(*w, "music" | "playback")));
    let open = words.iter().any(|w| matches!(*w, "open" | "launch"))
        || (words.contains(&"start") && !play);
    let compatible = match action {
        Action::OpenSpotify => open && !play && !pause,
        Action::PlaySpotify => play && !open && !pause,
        Action::PauseSpotify => pause && !open && !play,
    };
    if !compatible {
        return Err(ActionError::NotAuthorized);
    }
    Ok(())
}

pub struct ActionExecutor {
    executable: PathBuf,
    timeout: Duration,
    jev: Option<crate::providers::JevClient>,
}
impl ActionExecutor {
    pub fn native() -> Self {
        let jev = crate::conversation::load_provider_settings()
            .ok()
            .and_then(|settings| settings.get("TYPESAFE_API_KEY").cloned())
            .and_then(|key| crate::providers::JevClient::production(key).ok());
        Self {
            executable: PathBuf::from("/usr/bin/osascript"),
            timeout: Duration::from_secs(20),
            jev,
        }
    }
    pub fn with_executable(executable: PathBuf, timeout: Duration) -> Result<Self, ActionError> {
        if !executable.is_absolute() || timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(ActionError::InvalidConfiguration);
        }
        Ok(Self {
            executable,
            timeout,
            jev: None,
        })
    }
    pub fn with_jev(mut self, jev: crate::providers::JevClient) -> Self {
        self.jev = Some(jev);
        self
    }
    pub async fn handle(&self, request: &str) -> Option<Result<ActionResult, ActionError>> {
        if let Some(action) = recognize(request) {
            return Some(self.execute(request, action).await);
        }
        if !is_action_request(request) {
            return None;
        }
        let Some(jev) = &self.jev else {
            return Some(Err(ActionError::Clarification));
        };
        let Ok(Ok(proposal)) =
            tokio::time::timeout(Duration::from_millis(1500), jev.propose_action(request)).await
        else {
            return Some(Err(ActionError::Clarification));
        };
        if proposal.confidence < 0.9 {
            return Some(Err(ActionError::Clarification));
        }
        use crate::providers::ActionChoice;
        let action = match proposal.action {
            ActionChoice::OpenSpotify => Action::OpenSpotify,
            ActionChoice::PlaySpotify => Action::PlaySpotify,
            ActionChoice::PauseSpotify => Action::PauseSpotify,
            ActionChoice::Clarify | ActionChoice::None => {
                return Some(Err(ActionError::Clarification));
            }
        };
        Some(self.execute(request, action).await)
    }
    pub async fn execute(
        &self,
        request: &str,
        action: Action,
    ) -> Result<ActionResult, ActionError> {
        let started = std::time::Instant::now();
        gate_proposal(request, action)?;
        let script = match action {
            Action::OpenSpotify => OPEN_SCRIPT,
            Action::PlaySpotify => PLAY_SCRIPT,
            Action::PauseSpotify => PAUSE_SCRIPT,
        };
        let operation = match action {
            Action::OpenSpotify => Operation::OpenApplication,
            Action::PlaySpotify => Operation::PlayMedia,
            Action::PauseSpotify => Operation::PauseMedia,
        };
        if gate(
            operation,
            EvidenceStatus::Ready,
            started.elapsed().as_millis() as u64,
            self.timeout.as_millis().max(1) as u64,
        ) != GateDecision::Allow
        {
            return Err(ActionError::Timeout);
        }
        let mut child = tokio::process::Command::new(&self.executable)
            .arg("-e")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| ActionError::Unavailable)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or(ActionError::Verification)?
            .take(4097);
        let mut stderr = child
            .stderr
            .take()
            .ok_or(ActionError::Verification)?
            .take(4097);
        let mut out = Vec::with_capacity(4097);
        let mut err = Vec::with_capacity(4097);
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
                return Err(ActionError::Verification);
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(ActionError::Timeout);
            }
        };
        if !status.success() || out.len() > 4096 || err.len() > 4096 {
            return Err(ActionError::Verification);
        }
        let output = std::str::from_utf8(&out)
            .map_err(|_| ActionError::Verification)?
            .trim();
        let text = match (action, output) {
            (Action::OpenSpotify, "verified:running") => "Spotify's open.",
            (Action::PlaySpotify, "verified:playing") => "On it—Spotify's playing.",
            (Action::PauseSpotify, "verified:paused") => "Paused.",
            (Action::PauseSpotify, "verified:stopped" | "not_running") => "Spotify isn't playing.",
            (_, "permission_denied") => return Err(ActionError::PermissionDenied),
            (_, "unavailable") => return Err(ActionError::Unavailable),
            (Action::PlaySpotify, "not_playing") => return Err(ActionError::NoSelection),
            _ => return Err(ActionError::Verification),
        };
        Ok(ActionResult {
            action,
            text: text.into(),
        })
    }
}

const OPEN_SCRIPT: &str = r#"try
tell application id "com.spotify.client" to activate
repeat 30 times
if application id "com.spotify.client" is running then return "verified:running"
delay 0.1
end repeat
return "unavailable"
on error messageText number errorNumber
if errorNumber is -1743 then return "permission_denied"
return "unavailable"
end try"#;
const PLAY_SCRIPT: &str = r#"try
tell application id "com.spotify.client"
launch
play
repeat 30 times
if (player state as text) is "playing" then return "verified:playing"
delay 0.1
end repeat
end tell
return "not_playing"
on error messageText number errorNumber
if errorNumber is -1743 then return "permission_denied"
return "unavailable"
end try"#;
const PAUSE_SCRIPT: &str = r#"try
if application id "com.spotify.client" is not running then return "not_running"
tell application id "com.spotify.client"
pause
repeat 30 times
set playbackState to player state as text
if playbackState is "paused" then return "verified:paused"
if playbackState is "stopped" then return "verified:stopped"
delay 0.1
end repeat
end tell
return "unverified"
on error messageText number errorNumber
if errorNumber is -1743 then return "permission_denied"
return "unavailable"
end try"#;

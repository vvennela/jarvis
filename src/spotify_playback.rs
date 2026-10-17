//! Resolved-artist playback through Spotify's fixed native scripting interface.
//! Artist resolution and user authorization precede the native side effect.
use crate::{EvidenceStatus, GateDecision, Operation, gate, music_catalog::ResolvedArtist};
use serde::Deserialize;
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackError {
    InvalidConfiguration,
    InvalidArtist,
    NotAuthorized,
    Unavailable,
    PermissionDenied,
    Timeout,
    Verification,
}
impl fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "Spotify playback needs a setup fix.",
            Self::InvalidArtist => "I couldn't verify that artist's Spotify identity.",
            Self::NotAuthorized => "Which artist would you like me to play?",
            Self::Unavailable => "I couldn't reach the Spotify desktop app. Is it installed?",
            Self::PermissionDenied => "I need permission to control Spotify. Enable Jarvis under System Settings, Privacy & Security, Automation.",
            Self::Timeout => "Spotify took too long to respond. I couldn't confirm playback.",
            Self::Verification => "I couldn't confirm that Spotify started the artist you asked for.",
        })
    }
}
impl std::error::Error for PlaybackError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackResult {
    pub text: String,
    pub artist: String,
    pub track_uri: String,
}

fn normalized(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn artist_candidates(request: &str) -> Option<Vec<String>> {
    if request.is_empty()
        || request.len() > 8192
        || request
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '`' | '$' | ';' | '“' | '”'))
    {
        return None;
    }
    let original = normalized(request.trim().trim_end_matches(['.', '!', '?']));
    let mut request = original.as_str();
    for _ in 0..4 {
        let Some(rest) = [
            "please ",
            "can you ",
            "could you ",
            "would you ",
            "i want you to ",
            "i would like you to ",
        ]
        .iter()
        .find_map(|prefix| request.strip_prefix(prefix)) else {
            break;
        };
        request = rest;
    }
    for _ in 0..3 {
        let Some(rest) = [" please", " for me", " in the background", " in background"]
            .iter()
            .find_map(|suffix| request.strip_suffix(suffix))
        else {
            break;
        };
        request = rest;
    }
    let explicit_spotify = [
        " on spotify",
        " in spotify",
        " using spotify",
        " with spotify",
    ]
    .iter()
    .find_map(|suffix| request.strip_suffix(suffix));
    request = explicit_spotify.unwrap_or(request);
    request = request
        .strip_prefix("open spotify and ")
        .or_else(|| request.strip_prefix("launch spotify and "))
        .unwrap_or(request);
    let mut candidates: Vec<String> = [
        "play music by ",
        "play songs by ",
        "play something by ",
        "play music from ",
        "play some ",
        "play ",
        "listen to ",
        "put on ",
    ]
    .iter()
    .filter_map(|prefix| request.strip_prefix(prefix))
    .filter(|target| valid_artist_target(target))
    .map(str::to_owned)
    .collect();
    // In a named music destination, opening an artist selects their music.
    // Keep a bare "open NAME" ambiguous; it may refer to an app or document.
    if explicit_spotify.is_some()
        && let Some(target) = request.strip_prefix("open ")
        && valid_artist_target(target)
    {
        candidates.push(target.to_owned());
    }
    (!candidates.is_empty()).then_some(candidates)
}
fn valid_artist_target(target: &str) -> bool {
    if target.is_empty()
        || target.len() > 256
        || target.starts_with('\'')
        || target.ends_with('\'')
        || matches!(
            target,
            "music"
                | "some music"
                | "a song"
                | "some songs"
                | "songs"
                | "song"
                | "something"
                | "anything"
                | "whatever"
                | "current selection"
                | "the current selection"
                | "this"
                | "that"
                | "it"
                | "my music"
                | "the music"
                | "a track"
                | "track"
        )
        || target.split_whitespace().any(|word| {
            matches!(
                word,
                "if" | "unless" | "when" | "then" | "not" | "never" | "don't" | "dont"
            )
        })
    {
        return false;
    }
    for separator in [" and ", " or "] {
        if target.split(separator).skip(1).any(|tail| {
            let tail = tail.strip_prefix("please ").unwrap_or(tail);
            matches!(
                tail.split_whitespace().next(),
                Some(
                    "open"
                        | "launch"
                        | "play"
                        | "pause"
                        | "send"
                        | "delete"
                        | "pay"
                        | "purchase"
                        | "run"
                        | "execute"
                        | "write"
                        | "create"
                        | "ignore"
                )
            )
        }) {
            return false;
        }
    }
    true
}

/// A clear artist target from the original user's playback request. Modifier
/// forms with multiple exact interpretations remain for catalog/model planning;
/// never truncate an artist name such as "Some Girls" to "Girls".
pub fn requested_artist(request: &str) -> Option<String> {
    let mut candidates = artist_candidates(request)?;
    (candidates.len() == 1).then(|| candidates.remove(0))
}

/// Original utterance authority is independent of catalog/model data. Compare
/// complete parsed targets; a modifier can itself be part of an artist's name.
pub fn authorize_artist_request(request: &str, artist_name: &str) -> Result<(), PlaybackError> {
    if artist_name.trim().is_empty()
        || artist_name.len() > 256
        || artist_name.chars().any(char::is_control)
    {
        return Err(PlaybackError::NotAuthorized);
    }
    let target = normalized(artist_name);
    let candidates = artist_candidates(request).ok_or(PlaybackError::NotAuthorized)?;
    if !candidates.iter().any(|candidate| candidate == &target) {
        return Err(PlaybackError::NotAuthorized);
    }
    Ok(())
}

pub struct SpotifyPlayback {
    executable: PathBuf,
    timeout: Duration,
    native: bool,
}
impl SpotifyPlayback {
    pub fn native() -> Result<Self, PlaybackError> {
        if !installed() {
            return Err(PlaybackError::Unavailable);
        }
        Ok(Self {
            executable: PathBuf::from("/usr/bin/osascript"),
            timeout: Duration::from_secs(20),
            native: true,
        })
    }
    pub fn with_executable(executable: PathBuf, timeout: Duration) -> Result<Self, PlaybackError> {
        if !executable.is_absolute() || timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(PlaybackError::InvalidConfiguration);
        }
        Ok(Self {
            executable,
            timeout,
            native: false,
        })
    }
    pub async fn play_artist(
        &self,
        user_request: &str,
        artist: &ResolvedArtist,
    ) -> Result<PlaybackResult, PlaybackError> {
        let started = Instant::now();
        authorize_artist_request(user_request, artist.name())?;
        if artist.name().len() > 256
            || artist.name().chars().any(char::is_control)
            || !spotify_id(artist.id())
            || artist.uri() != format!("spotify:artist:{}", artist.id())
        {
            return Err(PlaybackError::InvalidArtist);
        }
        if self.native && !installed() {
            return Err(PlaybackError::Unavailable);
        }
        if gate(
            Operation::PlayMedia,
            EvidenceStatus::Ready,
            started.elapsed().as_millis() as u64,
            self.timeout.as_millis().max(1) as u64,
        ) != GateDecision::Allow
        {
            return Err(PlaybackError::Timeout);
        }
        let mut child = tokio::process::Command::new(&self.executable)
            .args([
                "-e",
                PLAY_ARTIST_SCRIPT,
                artist.uri(),
                artist.name(),
                artist.track_uri().unwrap_or(artist.uri()),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| PlaybackError::Unavailable)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or(PlaybackError::Verification)?
            .take(4097);
        let mut stderr = child
            .stderr
            .take()
            .ok_or(PlaybackError::Verification)?
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
                return Err(PlaybackError::Verification);
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(PlaybackError::Timeout);
            }
        };
        if !status.success() || out.len() > 4096 || err.len() > 4096 {
            return Err(PlaybackError::Verification);
        }
        let observed: Observation =
            serde_json::from_slice(&out).map_err(|_| PlaybackError::Verification)?;
        match observed.status.as_str() {
            "permission_denied" => return Err(PlaybackError::PermissionDenied),
            "unavailable" => return Err(PlaybackError::Unavailable),
            "observed" => {}
            _ => return Err(PlaybackError::Verification),
        }
        let track_id = observed
            .track_id
            .as_deref()
            .and_then(track_identifier)
            .ok_or(PlaybackError::Verification)?;
        let url_id = observed
            .track_url
            .as_deref()
            .filter(|value| {
                value.starts_with("spotify:track:")
                    || value.starts_with("https://open.spotify.com/track/")
            })
            .and_then(track_identifier)
            .ok_or(PlaybackError::Verification)?;
        if observed.state.as_deref() != Some("playing")
            || track_id != url_id
            || artist
                .track_uri()
                .is_some_and(|uri| track_identifier(uri) != Some(track_id))
            || observed.artist.as_deref().is_none_or(|name| {
                name.chars().any(char::is_control) || normalized(name) != normalized(artist.name())
            })
        {
            return Err(PlaybackError::Verification);
        }
        Ok(PlaybackResult {
            text: format!("Playing {}.", artist.name()),
            artist: artist.name().to_owned(),
            track_uri: format!("spotify:track:{track_id}"),
        })
    }
}
fn installed() -> bool {
    let system = Path::new("/Applications/Spotify.app/Contents/Info.plist");
    system.is_file()
        || std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .is_some_and(|home| {
                home.join("Applications/Spotify.app/Contents/Info.plist")
                    .is_file()
            })
}
fn spotify_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}
fn track_identifier(value: &str) -> Option<&str> {
    let id = value
        .strip_prefix("spotify:track:")
        .or_else(|| value.strip_prefix("https://open.spotify.com/track/"))
        .unwrap_or(value);
    spotify_id(id).then_some(id)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    status: String,
    state: Option<String>,
    artist: Option<String>,
    track_id: Option<String>,
    track_url: Option<String>,
}

// Spotify's installed scripting dictionary exposes `play track` for a URI and
// current-track artist/id/URL. Artist-context acceptance is verified by the
// actual artist readback, never inferred from successful command dispatch.
const PLAY_ARTIST_SCRIPT: &str = r#"use framework "Foundation"
use scripting additions
on run argv
set artistURI to item 1 of argv
set expectedArtist to item 2 of argv
set requestedTrackURI to item 3 of argv
try
tell application id "com.spotify.client"
launch
if requestedTrackURI is artistURI then
play track artistURI
else
play track requestedTrackURI in context artistURI
end if
repeat 60 times
set playbackState to player state as text
if playbackState is "playing" then
set trackID to id of current track as text
set actualArtist to artist of current track as text
set trackURL to spotify url of current track as text
ignoring case
set matched to actualArtist is expectedArtist
end ignoring
set stableTrack to trackID is (id of current track as text)
set stillPlaying to (player state as text) is "playing"
set matchedTrack to requestedTrackURI is artistURI or trackURL is requestedTrackURI or trackID is requestedTrackURI
if matched and matchedTrack and stableTrack and stillPlaying then
set payload to current application's NSDictionary's dictionaryWithObjects:{"observed", playbackState, actualArtist, trackID, trackURL} forKeys:{"status", "state", "artist", "track_id", "track_url"}
set jsonData to current application's NSJSONSerialization's dataWithJSONObject:payload options:0 |error|:(missing value)
set jsonText to current application's NSString's alloc()'s initWithData:jsonData encoding:(current application's NSUTF8StringEncoding)
return jsonText as text
end if
end if
delay 0.1
end repeat
end tell
return "{\"status\":\"unverified\"}"
on error messageText number errorNumber
if errorNumber is -1743 then return "{\"status\":\"permission_denied\"}"
if errorNumber is -10814 or errorNumber is -600 then return "{\"status\":\"unavailable\"}"
return "{\"status\":\"unverified\"}"
end try
end run"#;

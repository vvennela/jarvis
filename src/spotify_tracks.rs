//! Specific-track discovery and playback. Search hits are suggestions; Spotify's
//! inert public embed metadata verifies title, artist and identity before action.
use crate::{EvidenceStatus, GateDecision, Operation, gate};
use reqwest::{Client, Url, header::HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fmt,
    net::IpAddr,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

const MAX_BODY: usize = 512 * 1024;
const MAX_EMBED: usize = 256 * 1024;
const MAX_CANDIDATES: usize = 5;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackError {
    Configuration,
    NotAuthorized,
    Authentication,
    RateLimited,
    Unavailable,
    Timeout,
    InvalidResponse,
    ResponseTooLarge,
    NotFound,
    Ambiguous,
    Verification,
}
impl fmt::Display for TrackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
        Self::Configuration => "Song lookup isn't connected yet.",
        Self::NotAuthorized => "Tell me the song title and artist you'd like me to play.",
        Self::Authentication => "Song search couldn't sign in. Check its configuration.",
        Self::RateLimited => "Song search is busy. Try again shortly.",
        Self::Unavailable => "I couldn't reach song search or Spotify.",
        Self::Timeout => "That took too long. I couldn't confirm the song started.",
        Self::InvalidResponse | Self::ResponseTooLarge => "I couldn't verify that song's details.",
        Self::NotFound => "I couldn't verify that exact song and artist together on Spotify.",
        Self::Ambiguous => "I found multiple recordings with that exact title and artist. Which version did you want?",
        Self::Verification => "I couldn't confirm that Spotify started the song you requested.",
    })
    }
}
impl std::error::Error for TrackError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackRequest {
    pub title: String,
    pub artist: String,
}
fn normalized(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn name(s: &str) -> bool {
    !s.trim().is_empty()
        && s.len() <= 128
        && !s.chars().any(|c| {
            c.is_control() || matches!(c, '"' | '`' | '$' | ';' | '|' | '<' | '>' | '“' | '”')
        })
}
fn id(s: &str) -> bool {
    s.len() == 22 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}
/// Parse explicit song/artist authority only. A guessed transcription of "by"
/// is not accepted, and model or search content cannot supply either slot.
pub fn requested_track(request: &str) -> Option<TrackRequest> {
    if request.is_empty() || request.len() > 8192 || request.chars().any(char::is_control) {
        return None;
    }
    let mut text = normalized(request.trim().trim_end_matches(['.', '!', '?']));
    if text
        .chars()
        .any(|c| matches!(c, '"' | '`' | '$' | ';' | '|' | '<' | '>' | '“' | '”'))
        || text.split_whitespace().any(|w| {
            matches!(
                w,
                "not"
                    | "never"
                    | "don't"
                    | "dont"
                    | "if"
                    | "unless"
                    | "then"
                    | "before"
                    | "after"
                    | "instead"
            )
        })
    {
        return None;
    }
    for _ in 0..4 {
        if let Some(rest) = [
            "please ",
            "can you ",
            "could you ",
            "would you ",
            "i want you to ",
        ]
        .iter()
        .find_map(|p| text.strip_prefix(p))
        {
            text = rest.to_owned();
        } else {
            break;
        }
    }
    for _ in 0..2 {
        if let Some(rest) = [" please", " for me"]
            .iter()
            .find_map(|p| text.strip_suffix(p))
        {
            text = rest.to_owned();
        }
    }
    let body = ["play ", "open ", "put on ", "listen to "]
        .iter()
        .find_map(|p| text.strip_prefix(p))?;
    let mut body = body.to_owned();
    for provider in [" on spotify", " in spotify", " using spotify"] {
        if body.matches(provider).count() > 1 {
            return None;
        }
        if let Some((before, after)) = body.split_once(provider) {
            if !after.is_empty() && !after.starts_with(" by ") && !after.starts_with(", by ") {
                return None;
            }
            body = format!("{before}{}", after.trim_start_matches(','));
        }
    }
    let (title, artist) = body.split_once(" by ")?;
    if artist.contains(" by ") {
        return None;
    }
    let title = title
        .strip_prefix("the song ")
        .or_else(|| title.strip_prefix("the track "))
        .or_else(|| title.strip_prefix("song "))
        .or_else(|| title.strip_prefix("track "))
        .unwrap_or(title)
        .trim();
    let artist = artist.trim();
    if !name(title)
        || !name(artist)
        || [title, artist].iter().any(|part| {
            part.split(" and ").skip(1).any(|tail| {
                [
                    "open ", "play ", "send ", "delete ", "pay ", "run ", "execute ",
                ]
                .iter()
                .any(|p| tail.starts_with(p))
            })
        })
    {
        return None;
    }
    Some(TrackRequest {
        title: title.into(),
        artist: artist.into(),
    })
}
pub fn authorize_track_request(request: &str, title: &str, artist: &str) -> Result<(), TrackError> {
    let target = requested_track(request).ok_or(TrackError::NotAuthorized)?;
    if !name(title)
        || !name(artist)
        || target.title != normalized(title)
        || target.artist != normalized(artist)
    {
        return Err(TrackError::NotAuthorized);
    }
    Ok(())
}
/// A clarification proposal only. Original "my" wording does not authorize playback.
pub fn track_confirmation(request: &str) -> Option<TrackRequest> {
    if request.len() > 8192 || request.chars().any(char::is_control) {
        return None;
    }
    let text = normalized(request.trim().trim_end_matches(['.', '!', '?']));
    for separator in [" on spotify, my ", " on spotify my "] {
        if let Some((title_request, artist)) = text.split_once(separator) {
            if artist.contains(separator) || artist.contains(" my ") {
                return None;
            }
            return requested_track(&format!("{title_request} by {artist} on Spotify"));
        }
    }
    None
}
pub struct TrackConfig {
    search: Url,
    embed: Url,
    timeout: Duration,
}
impl TrackConfig {
    pub fn production() -> Self {
        Self {
            search: Url::parse("https://ydc-index.io/v1/search").unwrap(),
            embed: Url::parse("https://open.spotify.com/embed/track/").unwrap(),
            timeout: Duration::from_secs(30),
        }
    }
    pub fn loopback_for_tests(
        search: &str,
        embed_base: &str,
        timeout: Duration,
    ) -> Result<Self, TrackError> {
        fn endpoint(raw: &str) -> Result<Url, TrackError> {
            let u = Url::parse(raw).map_err(|_| TrackError::Configuration)?;
            if u.scheme() != "http"
                || !u
                    .host_str()
                    .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback())
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
            {
                return Err(TrackError::Configuration);
            }
            Ok(u)
        }
        let embed = endpoint(embed_base)?;
        if !embed.path().ends_with('/') || timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(TrackError::Configuration);
        }
        Ok(Self {
            search: endpoint(search)?,
            embed,
            timeout,
        })
    }
}
pub struct SpotifyTracks {
    client: Client,
    key: HeaderValue,
    config: TrackConfig,
    executable: PathBuf,
    permits: tokio::sync::Semaphore,
}
impl SpotifyTracks {
    pub fn production(key: impl Into<String>) -> Result<Self, TrackError> {
        Self::new(
            key,
            TrackConfig::production(),
            PathBuf::from("/usr/bin/osascript"),
        )
    }
    pub fn new(
        key: impl Into<String>,
        config: TrackConfig,
        executable: PathBuf,
    ) -> Result<Self, TrackError> {
        let key = key.into();
        if key.is_empty()
            || key.len() > 4096
            || key
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            || !executable.is_absolute()
        {
            return Err(TrackError::Configuration);
        }
        let mut key = HeaderValue::from_str(&key).map_err(|_| TrackError::Configuration)?;
        key.set_sensitive(true);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| TrackError::Configuration)?;
        Ok(Self {
            client,
            key,
            config,
            executable,
            permits: tokio::sync::Semaphore::new(1),
        })
    }
    pub async fn play(
        &self,
        request: &str,
        title: &str,
        artist: &str,
    ) -> Result<String, TrackError> {
        authorize_track_request(request, title, artist)?;
        tokio::time::timeout(self.config.timeout, async {
            let _permit = self
                .permits
                .acquire()
                .await
                .map_err(|_| TrackError::Unavailable)?;
            let started = Instant::now();
            let track = self.resolve(title, artist).await?;
            if gate(
                Operation::PlayMedia,
                EvidenceStatus::Ready,
                started.elapsed().as_millis() as u64,
                self.config.timeout.as_millis() as u64,
            ) != GateDecision::Allow
            {
                return Err(TrackError::Timeout);
            }
            self.execute(&track).await?;
            Ok(format!("Playing {} by {}.", track.title, track.artist))
        })
        .await
        .map_err(|_| TrackError::Timeout)?
    }
    async fn resolve(&self, title: &str, artist: &str) -> Result<VerifiedTrack, TrackError> {
        let query = format!(
            "\"{}\" \"{}\" Spotify track",
            title.replace('\\', "\\\\"),
            artist.replace('\\', "\\\\")
        );
        let response=self.client.post(self.config.search.clone()).header("X-API-Key",self.key.clone()).json(&json!({"query":query,"count":MAX_CANDIDATES,"include_domains":["open.spotify.com"],"strict_operators":true})).send().await.map_err(|_|TrackError::Unavailable)?;
        let body = bounded(response, MAX_BODY).await?;
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| TrackError::InvalidResponse)?;
        let hits = value
            .pointer("/results/web")
            .and_then(Value::as_array)
            .ok_or(TrackError::InvalidResponse)?;
        if hits.len() > MAX_CANDIDATES {
            return Err(TrackError::InvalidResponse);
        }
        let mut candidates = HashSet::new();
        let mut checks = tokio::task::JoinSet::new();
        for hit in hits {
            let Some(candidate) = hit.get("url").and_then(Value::as_str).and_then(track_id) else {
                continue;
            };
            if !candidates.insert(candidate.clone()) {
                continue;
            }
            let endpoint = self
                .config
                .embed
                .join(&candidate)
                .map_err(|_| TrackError::InvalidResponse)?;
            let client = self.client.clone();
            let title = title.to_owned();
            let artist = artist.to_owned();
            checks.spawn(async move {
                let response = client
                    .get(endpoint)
                    .send()
                    .await
                    .map_err(|_| TrackError::Unavailable)?;
                let body = bounded(response, MAX_EMBED).await?;
                verify_embed(&body, &title, &artist)
            });
        }
        let mut matches = Vec::new();
        let mut ids = HashSet::new();
        while let Some(result) = checks.join_next().await {
            match result.map_err(|_| TrackError::Unavailable)?? {
                Some(track) if ids.insert(track.uri.clone()) => matches.push(track),
                _ => {}
            }
        }
        match matches.len() {
            0 => Err(TrackError::NotFound),
            1 => Ok(matches.remove(0)),
            _ => Err(TrackError::Ambiguous),
        }
    }
    async fn execute(&self, track: &VerifiedTrack) -> Result<(), TrackError> {
        let mut child = tokio::process::Command::new(&self.executable)
            .args(["-e", PLAY_TRACK_SCRIPT, &track.uri])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| TrackError::Unavailable)?;
        let mut out = child
            .stdout
            .take()
            .ok_or(TrackError::Verification)?
            .take(4097);
        let mut err = child
            .stderr
            .take()
            .ok_or(TrackError::Verification)?
            .take(4097);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let (_, _, status) = tokio::try_join!(
            out.read_to_end(&mut stdout),
            err.read_to_end(&mut stderr),
            child.wait()
        )
        .map_err(|_| TrackError::Verification)?;
        if !status.success() || stdout.len() > 4096 || stderr.len() > 4096 {
            return Err(TrackError::Verification);
        }
        let observed: Observed =
            serde_json::from_slice(&stdout).map_err(|_| TrackError::Verification)?;
        if observed.state != "playing" || observed.track_uri != track.uri {
            return Err(TrackError::Verification);
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observed {
    state: String,
    track_uri: String,
}
struct VerifiedTrack {
    title: String,
    artist: String,
    uri: String,
}
fn track_id(raw: &str) -> Option<String> {
    if raw.len() > 2048 {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("open.spotify.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let parts: Vec<_> = url.path_segments()?.collect();
    let value = match parts.as_slice() {
        ["track", id] => *id,
        ["embed", "track", id] => *id,
        [locale, "track", id] if locale.starts_with("intl-") => *id,
        _ => return None,
    };
    id(value).then(|| value.to_owned())
}
async fn bounded(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>, TrackError> {
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err(TrackError::Authentication),
        429 => return Err(TrackError::RateLimited),
        _ => return Err(TrackError::Unavailable),
    }
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(TrackError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| TrackError::Unavailable)?
    {
        if chunk.len() > max - body.len() {
            return Err(TrackError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
fn verify_embed(
    body: &[u8],
    title: &str,
    artist: &str,
) -> Result<Option<VerifiedTrack>, TrackError> {
    let html = std::str::from_utf8(body).map_err(|_| TrackError::InvalidResponse)?;
    let mut remaining = html;
    let mut bootstrap = None;
    while let Some(start) = remaining.find("<script") {
        remaining = &remaining[start + 7..];
        let end = remaining.find('>').ok_or(TrackError::InvalidResponse)?;
        let attrs = &remaining[..end];
        let content = &remaining[end + 1..];
        let close = content
            .find("</script>")
            .ok_or(TrackError::InvalidResponse)?;
        if attrs
            .split_ascii_whitespace()
            .any(|a| matches!(a, "id=\"__NEXT_DATA__\"" | "id='__NEXT_DATA__'"))
        {
            if bootstrap.is_some() {
                return Err(TrackError::InvalidResponse);
            }
            bootstrap = Some(&content[..close]);
        }
        remaining = &content[close + 9..];
    }
    let value: Value = serde_json::from_str(bootstrap.ok_or(TrackError::InvalidResponse)?)
        .map_err(|_| TrackError::InvalidResponse)?;
    let e = value
        .pointer("/props/pageProps/state/data/entity")
        .ok_or(TrackError::InvalidResponse)?;
    let field = |key: &str| {
        e.get(key)
            .and_then(Value::as_str)
            .ok_or(TrackError::InvalidResponse)
    };
    let name_value = field("name")?;
    let title_value = field("title")?;
    let identifier = field("id")?;
    let uri = field("uri")?;
    if field("type")? != "track"
        || !id(identifier)
        || uri != format!("spotify:track:{identifier}")
        || !name(name_value)
        || !name(title_value)
    {
        return Err(TrackError::InvalidResponse);
    }
    let artists = e
        .get("artists")
        .and_then(Value::as_array)
        .ok_or(TrackError::InvalidResponse)?;
    if artists.is_empty() || artists.len() > 16 {
        return Err(TrackError::InvalidResponse);
    }
    let mut matched = None;
    for entry in artists {
        let n = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or(TrackError::InvalidResponse)?;
        let u = entry
            .get("uri")
            .and_then(Value::as_str)
            .ok_or(TrackError::InvalidResponse)?;
        if !name(n) || !u.strip_prefix("spotify:artist:").is_some_and(id) {
            return Err(TrackError::InvalidResponse);
        }
        if normalized(n) == normalized(artist) {
            matched = Some(n.to_owned());
        }
    }
    if normalized(name_value) != normalized(title)
        || normalized(title_value) != normalized(title)
        || matched.is_none()
        || e.get("isPlayable") != Some(&Value::Bool(true))
        || field("playabilityReason")? != "PLAYABLE"
    {
        return Ok(None);
    }
    Ok(Some(VerifiedTrack {
        title: title_value.into(),
        artist: matched.unwrap(),
        uri: uri.into(),
    }))
}
const PLAY_TRACK_SCRIPT: &str = r#"use framework "Foundation"
on run argv
 set targetURI to item 1 of argv
 tell application "Spotify"
  play track targetURI
  repeat 40 times
   if player state is playing then
    if id of current track is targetURI then exit repeat
   end if
   delay 0.25
  end repeat
  set observedState to player state as text
  set observedURI to id of current track
 end tell
 set resultObject to current application's NSMutableDictionary's dictionary()
 resultObject's setObject:observedState forKey:"state"
 resultObject's setObject:observedURI forKey:"track_uri"
 set dataObject to current application's NSJSONSerialization's dataWithJSONObject:resultObject options:0 |error|:(missing value)
 return (current application's NSString's alloc()'s initWithData:dataObject encoding:4) as text
end run
"#;

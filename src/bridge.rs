//! Observation-only Chrome Native Messaging transport. The caller must verify
//! Chrome's supplied origin before invoking this session and install a manifest
//! with the exact extension allowed_origin. No frame can invoke an action.

use std::{
    fmt,
    io::{Read, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::context::{ContextStore, MAX_CONTENT_BYTES, ObservationResult, Source};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 512 * 1024;
const MAX_URL_BYTES: usize = 8192;
const MAX_TITLE_BYTES: usize = 2048;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_JS_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeError {
    InvalidOrigin,
    InvalidFrame,
    FrameTooLarge,
    InvalidHandshake,
    InvalidObservation,
    InvalidSession,
    InvalidSequence,
    Io,
    Randomness,
    Clock,
    Store,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidOrigin => "native bridge extension origin rejected",
            Self::InvalidFrame => "native bridge frame is malformed or truncated",
            Self::FrameTooLarge => "native bridge frame exceeds its limit",
            Self::InvalidHandshake => "native bridge handshake rejected",
            Self::InvalidObservation => "native bridge observation rejected",
            Self::InvalidSession => "native bridge session rejected",
            Self::InvalidSequence => "native bridge observation sequence rejected",
            Self::Io => "native bridge transport failed",
            Self::Randomness => "native bridge session randomness unavailable",
            Self::Clock => "native bridge trusted clock unavailable",
            Self::Store => "native bridge evidence persistence failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for BridgeError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionReport {
    pub observations_accepted: u64,
    pub resources_changed: u64,
}

pub fn validate_extension_origin(expected_id: &str, origin: &str) -> Result<(), BridgeError> {
    if expected_id.len() != 32
        || !expected_id
            .bytes()
            .all(|byte| (b'a'..=b'p').contains(&byte))
        || origin != format!("chrome-extension://{expected_id}/")
    {
        return Err(BridgeError::InvalidOrigin);
    }
    Ok(())
}

pub fn run_session<R: Read, W: Write>(
    input: R,
    output: W,
    store: &mut ContextStore,
) -> Result<SessionReport, BridgeError> {
    run_session_with_clock(input, output, store, || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .unwrap_or(-1)
    })
}

/// Clock values come from the native collector, never from page event claims.
/// Generic blocking readers cannot enforce an IO deadline; the hosting process
/// must supply that lifecycle if used outside Chrome's Native Messaging pipe.
pub fn run_session_with_clock<R: Read, W: Write, C: FnMut() -> i64>(
    mut input: R,
    mut output: W,
    store: &mut ContextStore,
    mut clock: C,
) -> Result<SessionReport, BridgeError> {
    let hello = read_frame(&mut input)?.ok_or(BridgeError::InvalidHandshake)?;
    let Incoming::Hello {
        protocol_version,
        client_nonce,
    } = hello
    else {
        return Err(BridgeError::InvalidHandshake);
    };
    if protocol_version != PROTOCOL_VERSION || !valid_nonce(&client_nonce) {
        return Err(BridgeError::InvalidHandshake);
    }
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|_| BridgeError::Randomness)?;
    let session_id = hex(&random);
    write_ack(&mut output, &client_nonce, &session_id)?;
    let mut report = SessionReport::default();
    let mut last_sequence = 0;
    let mut last_clock = 0;

    while let Some(message) = read_frame(&mut input)? {
        let Incoming::Observation {
            protocol_version,
            session_id: observed_session,
            sequence,
            event,
        } = message
        else {
            return Err(BridgeError::InvalidHandshake);
        };
        if protocol_version != PROTOCOL_VERSION || observed_session != session_id {
            return Err(BridgeError::InvalidSession);
        }
        if sequence <= last_sequence || sequence > MAX_JS_INTEGER {
            return Err(BridgeError::InvalidSequence);
        }
        event.validate()?;
        let now = clock();
        if now < 0 {
            return Err(BridgeError::Clock);
        }
        // System clock adjustments must not make later messages appear older
        // within this session. Session-to-session freshness remains store-owned.
        let now = now.max(last_clock);
        let changes = apply_event(store, &event, now)?;
        report.observations_accepted = report
            .observations_accepted
            .checked_add(1)
            .ok_or(BridgeError::InvalidSequence)?;
        report.resources_changed = report
            .resources_changed
            .checked_add(changes)
            .ok_or(BridgeError::InvalidSequence)?;
        last_sequence = sequence;
        last_clock = now;
    }
    Ok(report)
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Incoming {
    #[serde(rename = "hello")]
    Hello {
        protocol_version: u32,
        client_nonce: String,
    },
    #[serde(rename = "observation")]
    Observation {
        protocol_version: u32,
        session_id: String,
        sequence: u64,
        event: Box<BrowserEvent>,
    },
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
enum EventKind {
    #[serde(rename = "tab.activated")]
    Activated,
    #[serde(rename = "tab.updated")]
    Updated,
    #[serde(rename = "tab.closed")]
    Closed,
    #[serde(rename = "document.changed")]
    DocumentChanged,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserEvent {
    kind: EventKind,
    tab_id: i64,
    window_id: i64,
    url: String,
    title: String,
    observed_at_ms: i64,
    actor: String,
    owner: String,
    document_id: Option<String>,
    navigation_id: Option<String>,
    source: Option<String>,
    trust: Option<String>,
    content: Option<String>,
}

impl BrowserEvent {
    fn validate(&self) -> Result<(), BridgeError> {
        if !(0..=i32::MAX as i64).contains(&self.tab_id)
            || !(0..=i32::MAX as i64).contains(&self.window_id)
            || self.observed_at_ms < 0
            || self.actor != "unknown"
            || self.owner != "unknown"
            || self.url.is_empty()
            || self.url.len() > MAX_URL_BYTES
            || self
                .url
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
            || self.title.len() > MAX_TITLE_BYTES
            || self.title.contains('\0')
            || self
                .document_id
                .as_ref()
                .is_some_and(|id| !valid_identifier(id))
            || self
                .navigation_id
                .as_ref()
                .is_some_and(|id| !valid_identifier(id))
            || self.source.as_deref().is_some_and(|source| {
                !matches!(source, "browser_metadata" | "browser_content_script")
            })
            || self
                .trust
                .as_deref()
                .is_some_and(|trust| trust != "untrusted_page")
            || self
                .content
                .as_ref()
                .is_some_and(|content| content.len() > MAX_CONTENT_BYTES || content.contains('\0'))
            || (self.kind == EventKind::DocumentChanged && self.content.is_none())
            || (self.kind == EventKind::Closed && self.content.is_some())
        {
            return Err(BridgeError::InvalidObservation);
        }
        let parsed = reqwest::Url::parse(&self.url).map_err(|_| BridgeError::InvalidObservation)?;
        if !matches!(parsed.scheme(), "https" | "http")
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(BridgeError::InvalidObservation);
        }
        if let Some(navigation_id) = &self.navigation_id {
            let Some((tab, timestamp)) = navigation_id.split_once(':') else {
                return Err(BridgeError::InvalidObservation);
            };
            if tab != self.tab_id.to_string()
                || timestamp.is_empty()
                || !timestamp
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
            {
                return Err(BridgeError::InvalidObservation);
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct TabMetadata<'a> {
    kind: EventKind,
    tab_id: i64,
    window_id: i64,
    url: &'a str,
    title: &'a str,
    document_id: Option<&'a str>,
    navigation_id: Option<&'a str>,
}

fn apply_event(
    store: &mut ContextStore,
    event: &BrowserEvent,
    now: i64,
) -> Result<u64, BridgeError> {
    let metadata_id = format!("browser-tab:{}", event.tab_id);
    if event.kind == EventKind::Closed {
        return store
            .forget(&metadata_id, now)
            .map(|result| u64::from(matches!(result, Some(ObservationResult::Changed { .. }))))
            .map_err(|_| BridgeError::Store);
    }
    let metadata = serde_json::to_string(&TabMetadata {
        kind: event.kind,
        tab_id: event.tab_id,
        window_id: event.window_id,
        url: &event.url,
        title: &event.title,
        document_id: event.document_id.as_deref(),
        navigation_id: event.navigation_id.as_deref(),
    })
    .map_err(|_| BridgeError::InvalidObservation)?;
    let results = if let Some(content) = &event.content {
        let url_hash = blake3::hash(event.url.as_bytes());
        let page_id = format!("browser-page:{}:{}", event.tab_id, url_hash.to_hex());
        store.observe_many(&[
            (&page_id, content, Source::Browser, now),
            (&metadata_id, &metadata, Source::Browser, now),
        ])
    } else {
        store.observe_many(&[(&metadata_id, &metadata, Source::Browser, now)])
    }
    .map_err(|_| BridgeError::Store)?;
    Ok(results
        .iter()
        .filter(|result| matches!(result, ObservationResult::Changed { .. }))
        .count() as u64)
}

fn valid_nonce(nonce: &str) -> bool {
    (16..=128).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= MAX_IDENTIFIER_BYTES
        && !identifier.chars().any(char::is_control)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    output
}

fn read_frame<R: Read>(input: &mut R) -> Result<Option<Incoming>, BridgeError> {
    let mut header = [0u8; 4];
    // Distinguish clean EOF between frames from a partial header.
    loop {
        match input.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(BridgeError::Io),
        }
    }
    input.read_exact(&mut header[1..]).map_err(read_error)?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 {
        return Err(BridgeError::InvalidFrame);
    }
    if length > MAX_FRAME_BYTES {
        return Err(BridgeError::FrameTooLarge);
    }
    let mut bytes = vec![0u8; length];
    input.read_exact(&mut bytes).map_err(read_error)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| BridgeError::InvalidFrame)
}

fn read_error(error: std::io::Error) -> BridgeError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        BridgeError::InvalidFrame
    } else {
        BridgeError::Io
    }
}

fn write_ack<W: Write>(
    output: &mut W,
    client_nonce: &str,
    session_id: &str,
) -> Result<(), BridgeError> {
    #[derive(Serialize)]
    struct Ack<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        protocol_version: u32,
        client_nonce: &'a str,
        session_id: &'a str,
    }
    let bytes = serde_json::to_vec(&Ack {
        kind: "hello_ack",
        protocol_version: PROTOCOL_VERSION,
        client_nonce,
        session_id,
    })
    .map_err(|_| BridgeError::InvalidHandshake)?;
    output
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|()| output.write_all(&bytes))
        .and_then(|()| output.flush())
        .map_err(|_| BridgeError::Io)
}

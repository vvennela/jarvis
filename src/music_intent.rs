//! Caller-scoped music intent. Semantic decisions select original text; they
//! never create a command string or authorize a different artist.
use std::time::{Duration, Instant};

use crate::providers::{JevClient, MusicChoice};

pub(crate) const SEMANTIC_DEADLINE: Duration = Duration::from_secs(3);
const MIN_CONFIDENCE: f64 = 0.95;

pub(crate) struct ArtistIntent {
    original: String,
    artist: String,
    issued: Instant,
}

fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl ArtistIntent {
    pub(crate) fn explicit(original: &str, artist: &str) -> Option<Self> {
        crate::spotify_playback::authorize_artist_request(original, artist).ok()?;
        Some(Self::new(original, artist))
    }

    fn new(original: &str, artist: &str) -> Self {
        Self {
            original: original.to_owned(),
            artist: normalized(artist),
            issued: Instant::now(),
        }
    }

    pub(crate) fn confirmed(original: &str, artist: &str) -> Option<Self> {
        (eligible(original) && contains_target(original, artist))
            .then(|| Self::new(original, artist))
    }

    pub(crate) fn matches(&self, original: &str, artist: &str) -> bool {
        self.original == original
            && self.artist == normalized(artist)
            && self.issued.elapsed() < Duration::from_secs(35)
    }

    pub(crate) async fn semantic(original: &str, artist: &str, jev: &JevClient) -> Option<Self> {
        if !eligible(original) || !contains_target(original, artist) {
            return None;
        }
        let decision = tokio::time::timeout(
            SEMANTIC_DEADLINE,
            jev.authorize_music_request(original, artist),
        )
        .await
        .ok()?
        .ok()?;
        (decision.choice == MusicChoice::PlayArtist && decision.confidence >= MIN_CONFIDENCE)
            .then(|| Self::new(original, artist))
    }

    pub(crate) async fn resolve(original: &str, jev: &JevClient) -> Option<(Self, String)> {
        if !eligible(original) {
            return None;
        }
        let candidates = candidates(original);
        if candidates.is_empty() {
            return None;
        }
        tokio::time::timeout(SEMANTIC_DEADLINE, async {
            let (index, confidence) = jev
                .resolve_music_request(original, &candidates)
                .await
                .ok()??;
            let artist = candidates.get(index)?.clone();
            if !contains_target(original, &artist) {
                return None;
            }
            // Overlapping spans divide extraction confidence. A selected span
            // is only a proposal until a high-confidence intent decision binds it.
            let intent = if confidence >= MIN_CONFIDENCE {
                Self::new(original, &artist)
            } else {
                Self::semantic(original, &artist, jev).await?
            };
            Some((intent, artist))
        })
        .await
        .ok()?
    }
}

/// Coarse trust/intent guards, not an English command-prefix grammar. Jev still
/// sees every original word and must affirm the exact selected artist.
pub(crate) fn eligible(original: &str) -> bool {
    if original.is_empty()
        || original.len() > 8192
        || original.chars().any(|c| {
            c.is_control() || matches!(c, '"' | '“' | '”' | '`' | '$' | ';' | '|' | '<' | '>')
        })
        || original.trim().starts_with(['\'', '‘'])
    {
        return false;
    }
    let lower = original.to_lowercase().replace('’', "'");
    let words: Vec<_> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|s| !s.is_empty())
        .collect();
    if words.iter().any(|word| {
        matches!(
            *word,
            "not"
                | "never"
                | "don't"
                | "dont"
                | "cannot"
                | "can't"
                | "avoid"
                | "stop"
                | "if"
                | "unless"
                | "when"
                | "then"
                | "before"
                | "after"
                | "instead"
                | "without"
                | "says"
                | "said"
                | "quote"
                | "quoted"
                | "document"
                | "webpage"
                | "website"
                | "instructions"
                | "send"
                | "delete"
                | "purchase"
                | "pay"
                | "buy"
                | "email"
                | "message"
                | "explain"
                | "describe"
                | "summarize"
                | "write"
                | "translate"
                | "analyze"
                | "how"
                | "whether"
                | "about"
                | "example"
                | "hypothetically"
                | "song"
                | "track"
                | "album"
                | "playlist"
        )
    }) {
        return false;
    }
    words.iter().any(|word| {
        matches!(
            *word,
            "play" | "open" | "listen" | "put" | "start" | "hear" | "queue" | "get" | "switch"
        )
    })
}

pub(crate) fn contains_target(original: &str, artist: &str) -> bool {
    let artist = normalized(artist);
    if artist.is_empty() || artist.len() > 128 || artist.chars().any(char::is_control) {
        return false;
    }
    let original = normalized(original);
    original.match_indices(&artist).any(|(start, _)| {
        let end = start + artist.len();
        original[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
            && original[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric())
    })
}

/// Bound recall to 32 words and eight-word names (228 spans). IDs map back to
/// slices of the caller's text; neither the model nor page evidence adds names.
fn candidates(original: &str) -> Vec<String> {
    let words: Vec<(usize, usize)> = original
        .split_whitespace()
        .map(|word| {
            let start = word.as_ptr() as usize - original.as_ptr() as usize;
            (start, start + word.len())
        })
        .collect();
    if words.len() > 32 {
        return Vec::new();
    }
    let mut spans = Vec::new();
    for i in 0..words.len() {
        for j in i..(i + 8).min(words.len()) {
            let text = original[words[i].0..words[j].1]
                .trim_matches(|c: char| c.is_ascii_punctuation() && c != '\'');
            if !text.is_empty() && text.len() <= 128 && !spans.iter().any(|span| span == text) {
                spans.push(text.to_owned());
            }
        }
    }
    spans
}

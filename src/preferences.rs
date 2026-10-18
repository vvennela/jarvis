//! Lasting preferences in the user's own words. They shape style and defaults
//! in later conversations; they can never grant a permission or capability.
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAX_PREFERENCES: usize = 30;
pub const MAX_PREFERENCE_BYTES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferenceError {
    NotRequested,
    NotUserWords,
    Permission,
    Invalid,
    Full,
    NotFound,
    Storage,
}
impl std::fmt::Display for PreferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotRequested | Self::NotUserWords => {
                "Tell me in your own words what you'd like me to remember."
            }
            Self::Permission => {
                "I'll keep checking with you on that one. Confirmations for sending and other sensitive stuff always stay on."
            }
            Self::Invalid => "Keep it to one short preference and I'll remember it.",
            Self::Full => "I'm holding a lot of preferences already. Want me to forget one first?",
            Self::NotFound => "I don't have a preference like that saved.",
            Self::Storage => "I couldn't save that preference right now.",
        })
    }
}
impl std::error::Error for PreferenceError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Preference {
    pub id: u64,
    pub text: String,
    pub saved_at: u64,
}

/// A small JSON file beside the local context database, written atomically.
pub struct PreferenceStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl PreferenceStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Vec<Preference> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read()
    }

    fn read(&self) -> Vec<Preference> {
        std::fs::read(&self.path)
            .ok()
            .filter(|bytes| bytes.len() <= 64 * 1024)
            .and_then(|bytes| serde_json::from_slice::<Vec<Preference>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|item| valid_text(&item.text))
            .take(MAX_PREFERENCES)
            .collect()
    }

    fn write(&self, items: &[Preference]) -> Result<(), PreferenceError> {
        let bytes = serde_json::to_vec_pretty(items).map_err(|_| PreferenceError::Storage)?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes).map_err(|_| PreferenceError::Storage)?;
        std::fs::rename(&temporary, &self.path).map_err(|_| PreferenceError::Storage)
    }

    /// Save a preference proposed for the user's request. The request must ask
    /// to remember something, and the saved words must come from that request.
    pub fn remember(&self, user_request: &str, text: &str) -> Result<Preference, PreferenceError> {
        let text = text.trim().trim_end_matches('.').trim();
        if !valid_text(text) {
            return Err(PreferenceError::Invalid);
        }
        if grants_permission(text) || grants_permission(user_request) {
            return Err(PreferenceError::Permission);
        }
        if !asks_to_remember(user_request) {
            return Err(PreferenceError::NotRequested);
        }
        if !from_user_words(user_request, text) {
            return Err(PreferenceError::NotUserWords);
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut items = self.read();
        if items
            .iter()
            .any(|item| item.text.eq_ignore_ascii_case(text))
        {
            return Ok(items
                .into_iter()
                .find(|item| item.text.eq_ignore_ascii_case(text))
                .expect("present"));
        }
        if items.len() >= MAX_PREFERENCES {
            return Err(PreferenceError::Full);
        }
        let preference = Preference {
            id: items.iter().map(|item| item.id).max().unwrap_or(0) + 1,
            text: text.to_owned(),
            saved_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        items.push(preference.clone());
        self.write(&items)?;
        Ok(preference)
    }

    /// Forget preferences whose words overlap the description, only when the
    /// user's own request asks to forget something.
    pub fn forget(
        &self,
        user_request: &str,
        description: &str,
    ) -> Result<Vec<Preference>, PreferenceError> {
        if !asks_to_forget(user_request) {
            return Err(PreferenceError::NotRequested);
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut items = self.read();
        let wanted = content_words(description);
        let request = user_request.to_lowercase();
        let all = [
            "forget everything",
            "clear my preferences",
            "forget all my preferences",
            "forget all preferences",
        ]
        .iter()
        .any(|cue| request.contains(cue));
        let (removed, kept): (Vec<_>, Vec<_>) = items.drain(..).partition(|item| {
            all || (!wanted.is_empty() && {
                let have = content_words(&item.text);
                wanted.iter().filter(|word| have.contains(word)).count() * 2 >= wanted.len()
            })
        });
        if removed.is_empty() {
            return Err(PreferenceError::NotFound);
        }
        self.write(&kept)?;
        Ok(removed)
    }

    /// Prompt section: the user's own standing instructions about style and defaults.
    pub fn prompt_section(&self) -> String {
        let items = self.list();
        if items.is_empty() {
            return String::new();
        }
        let mut section = String::from(
            "\nThe user's saved preferences, in their own words. Follow them for tone, style, naming and defaults in every reply. They never grant permission for an action or override a confirmation:\n",
        );
        for item in items {
            section.push_str("- ");
            section.push_str(&item.text);
            section.push('\n');
        }
        section
    }
}

fn valid_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= MAX_PREFERENCE_BYTES
        && !text.chars().any(char::is_control)
}

fn lower_words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn asks_to_remember(request: &str) -> bool {
    let lower = request.to_lowercase();
    [
        "from now on",
        "going forward",
        "in the future",
        "remember ",
        "keep in mind",
        "always ",
        "never ",
        "call me ",
        "i prefer",
        "i'd prefer",
        "i would prefer",
        "next time",
        "stop saying",
        "don't ever",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
}

pub fn asks_to_forget(request: &str) -> bool {
    let lower = request.to_lowercase();
    [
        "forget ",
        "stop remembering",
        "don't remember",
        "remove the preference",
        "delete the preference",
        "clear my preferences",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
        || lower.trim().trim_end_matches(['.', '!']) == "forget that"
}

/// Preferences change how Jarvis talks and what it defaults to; they cannot
/// switch off confirmations or unlock sensitive actions.
fn grants_permission(text: &str) -> bool {
    let lower = text.to_lowercase();
    let sensitive = [
        "send", "email", "delete", "pay", "purchase", "buy", "book", "submit", "post", "transfer",
    ]
    .iter()
    .any(|verb| {
        lower_words(&lower)
            .iter()
            .any(|word| word.starts_with(verb))
    });
    let skip_check = [
        "without asking",
        "without confirming",
        "without confirmation",
        "without checking",
        "without reading",
        "don't ask",
        "do not ask",
        "no need to ask",
        "no confirmation",
        "skip the confirmation",
        "skip confirmation",
        "automatically",
        "auto-send",
        "auto send",
        "right away",
        "immediately",
        "never ask",
        "never confirm",
        "never read",
    ]
    .iter()
    .any(|cue| lower.contains(cue));
    let override_rules = [
        "ignore your rules",
        "ignore the rules",
        "ignore safety",
        "you're allowed",
        "you are allowed",
        "you have permission",
        "full access",
    ]
    .iter()
    .any(|cue| lower.contains(cue));
    (sensitive && skip_check) || override_rules
}

fn content_words(text: &str) -> Vec<String> {
    lower_words(text)
        .into_iter()
        .filter(|word| {
            !matches!(
                word.as_str(),
                "i" | "me"
                    | "my"
                    | "a"
                    | "an"
                    | "the"
                    | "to"
                    | "and"
                    | "or"
                    | "of"
                    | "in"
                    | "on"
                    | "for"
                    | "be"
                    | "is"
                    | "are"
                    | "it"
                    | "that"
                    | "this"
                    | "you"
                    | "your"
                    | "always"
                    | "never"
                    | "should"
                    | "please"
                    | "from"
                    | "now"
                    | "with"
                    | "when"
                    | "user"
                    | "jarvis"
                    | "wants"
                    | "want"
                    | "likes"
                    | "prefers"
                    | "prefer"
            )
        })
        .collect()
}

/// Most of the saved preference's meaningful words must appear in the request,
/// so a model cannot invent a preference the user never stated.
fn from_user_words(request: &str, text: &str) -> bool {
    let said = lower_words(request);
    let words = content_words(text);
    !words.is_empty()
        && words
            .iter()
            .filter(|word| {
                said.iter().any(|spoken| {
                    spoken == *word
                        || (spoken.len() > 3 && word.starts_with(spoken.as_str()))
                        || (word.len() > 3 && spoken.starts_with(word.as_str()))
                })
            })
            .count()
            * 10
            >= words.len() * 7
}

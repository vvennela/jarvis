//! Learned skills: verified model-routed actions become templates that later
//! requests can reuse without a model call. A skill only chooses a tool and
//! its arguments from the user's words; the runtime still authorizes every
//! execution against the original request.
use crate::providers::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Verified successes before a template is used without the model.
pub const PROMOTE_AFTER: u32 = 2;
pub const MAX_SKILLS: usize = 100;
const MAX_TOKENS: usize = 14;
const MAX_SLOT_TOKENS: usize = 6;
/// Tools whose arguments can be recovered from the user's words.
/// Only tools whose authorization can accept loose phrasing (via Jev) gain
/// from learning: page, app and control requests that the model could
/// complete are already matched by the strict direct parsers.
const LEARNABLE: &[&str] = &["spotify_play_artist", "spotify_play_track"];
/// Arguments that may be fixed by a template rather than taken from speech.
const FIXABLE: &[&str] = &[];
const FILLERS: &[&str] = &[
    "hey", "jarvis", "please", "can", "could", "would", "will", "you", "uh", "um", "okay", "ok",
];
const BLOCKING: &[&str] = &[
    "not", "no", "never", "dont", "don't", "cannot", "cant", "can't", "if", "unless", "when",
    "before", "after", "until", "instead", "then",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub template: String,
    pub tool: String,
    pub fixed: Map<String, Value>,
    pub successes: u32,
    pub failures: u32,
    pub last_used: u64,
}
impl Skill {
    pub fn active(&self) -> bool {
        self.successes >= PROMOTE_AFTER && self.failures * 2 < self.successes
    }
    fn key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.tool,
            self.template,
            Value::Object(self.fixed.clone())
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stats {
    pub model_routed: u64,
    pub model_ms: u64,
    pub skill_hits: u64,
    pub skill_ms: u64,
    pub skill_failures: u64,
}
impl Stats {
    pub fn average_model_ms(&self) -> Option<u64> {
        (self.model_routed > 0).then(|| self.model_ms / self.model_routed)
    }
    pub fn average_skill_ms(&self) -> Option<u64> {
        (self.skill_hits > 0).then(|| self.skill_ms / self.skill_hits)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
    skills: Vec<Skill>,
    stats: Stats,
}

/// A matched skill: its identity for outcome reporting and the proposal.
pub struct SkillMatch {
    pub key: String,
    pub call: ToolCall,
}

pub struct ExperienceStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ExperienceStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Book {
        std::fs::read(&self.path)
            .ok()
            .filter(|bytes| bytes.len() <= 512 * 1024)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write(&self, book: &Book) {
        let Ok(bytes) = serde_json::to_vec_pretty(book) else {
            return;
        };
        let temporary = self.path.with_extension("json.tmp");
        if std::fs::write(&temporary, bytes).is_ok() {
            let _ = std::fs::rename(&temporary, &self.path);
        }
    }

    pub fn skills(&self) -> Vec<Skill> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read().skills
    }

    pub fn stats(&self) -> Stats {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read().stats
    }

    /// A model-routed request finished; `verified` actions may teach a skill.
    pub fn record_model_turn(&self, request: &str, call: Option<&ToolCall>, elapsed: Duration) {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut book = self.read();
        book.stats.model_routed += 1;
        book.stats.model_ms += elapsed.as_millis() as u64;
        if let Some(call) = call
            && let Some(skill) = derive(request, call)
        {
            let key = skill.key();
            match book
                .skills
                .iter_mut()
                .find(|existing| existing.key() == key)
            {
                Some(existing) => {
                    existing.successes = existing.successes.saturating_add(1);
                    existing.last_used = now();
                }
                None => {
                    if book.skills.len() >= MAX_SKILLS {
                        // Evict the least useful skill.
                        if let Some(index) = book
                            .skills
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, s)| (s.successes, s.last_used))
                            .map(|(i, _)| i)
                        {
                            book.skills.remove(index);
                        }
                    }
                    book.skills.push(skill);
                }
            }
        }
        self.write(&book);
    }

    /// The best active skill whose template matches this request exactly.
    pub fn match_request(&self, request: &str) -> Option<SkillMatch> {
        let tokens = tokens(request)?;
        if tokens
            .iter()
            .any(|(word, _)| BLOCKING.contains(&word.as_str()))
        {
            return None;
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut skills = self.read().skills;
        skills.retain(Skill::active);
        skills.sort_by_key(|skill| std::cmp::Reverse(skill.successes));
        for skill in skills {
            if let Some(slots) = fill(&skill.template, &tokens) {
                let mut arguments = skill.fixed.clone();
                for (name, value) in slots {
                    arguments.insert(name, Value::String(value));
                }
                return Some(SkillMatch {
                    key: skill.key(),
                    call: ToolCall {
                        id: "learned-skill".into(),
                        name: skill.tool.clone(),
                        arguments: Value::Object(arguments),
                    },
                });
            }
        }
        None
    }

    /// Outcome of executing a matched skill; failures demote it.
    pub fn report(&self, key: &str, verified: bool, elapsed: Duration) {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut book = self.read();
        if let Some(skill) = book.skills.iter_mut().find(|skill| skill.key() == key) {
            if verified {
                skill.successes = skill.successes.saturating_add(1);
            } else {
                skill.failures = skill.failures.saturating_add(1);
            }
            skill.last_used = now();
        }
        if verified {
            book.stats.skill_hits += 1;
            book.stats.skill_ms += elapsed.as_millis() as u64;
        } else {
            book.stats.skill_failures += 1;
        }
        self.write(&book);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Lowercased comparison words paired with original-case words, without
/// punctuation or leading fillers.
fn tokens(request: &str) -> Option<Vec<(String, String)>> {
    if request.is_empty() || request.len() > 1024 || request.chars().any(char::is_control) {
        return None;
    }
    let mut out: Vec<(String, String)> = request
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’')
                .replace('’', "'")
        })
        .filter(|word| !word.is_empty())
        .map(|word| (word.to_lowercase(), word))
        .collect();
    while out
        .first()
        .is_some_and(|(word, _)| FILLERS.contains(&word.as_str()))
    {
        out.remove(0);
    }
    (!out.is_empty() && out.len() <= MAX_TOKENS).then_some(out)
}

/// Template from a verified request: argument values found verbatim in the
/// user's words become `{slot}`s. Anything the model supplied that the user
/// never said (besides a fixed site/action) means nothing is learned.
fn derive(request: &str, call: &ToolCall) -> Option<Skill> {
    if !LEARNABLE.contains(&call.name.as_str()) {
        return None;
    }
    let words = tokens(request)?;
    if words
        .iter()
        .any(|(word, _)| BLOCKING.contains(&word.as_str()))
    {
        return None;
    }
    let arguments = call.arguments.as_object()?;
    let mut template: Vec<String> = words.iter().map(|(word, _)| word.clone()).collect();
    let mut fixed = Map::new();
    let mut slots = 0;
    for (name, value) in arguments {
        let text = value.as_str()?;
        let target: Vec<String> = text
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect();
        let position = (!target.is_empty() && target.len() <= MAX_SLOT_TOKENS)
            .then(|| {
                template
                    .windows(target.len())
                    .position(|window| window == target.as_slice())
            })
            .flatten();
        match position {
            Some(start) => {
                template.splice(start..start + target.len(), [format!("{{{name}}}")]);
                slots += 1;
            }
            None if FIXABLE.contains(&name.as_str()) => {
                fixed.insert(name.clone(), value.clone());
            }
            None => return None,
        }
    }
    let literals = template
        .iter()
        .filter(|word| !word.starts_with('{'))
        .count();
    let adjacent = template
        .windows(2)
        .any(|pair| pair[0].starts_with('{') && pair[1].starts_with('{'));
    if literals == 0 || adjacent || slots == 0 {
        return None;
    }
    Some(Skill {
        template: template.join(" "),
        tool: call.name.clone(),
        fixed,
        successes: 1,
        failures: 0,
        last_used: now(),
    })
}

/// Match literal words exactly; each slot takes 1..=MAX_SLOT_TOKENS words
/// (original casing) up to the next literal or the end.
fn fill(template: &str, words: &[(String, String)]) -> Option<Vec<(String, String)>> {
    let parts: Vec<&str> = template.split(' ').collect();
    let mut slots = Vec::new();
    let mut index = 0;
    for (position, part) in parts.iter().enumerate() {
        if let Some(name) = part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
            let end = match parts.get(position + 1) {
                Some(next) => {
                    let mut end = index + 1;
                    while end < words.len() && words[end].0 != *next {
                        end += 1;
                    }
                    if end >= words.len() {
                        return None;
                    }
                    end
                }
                None => words.len(),
            };
            if end <= index || end - index > MAX_SLOT_TOKENS {
                return None;
            }
            let value = words[index..end]
                .iter()
                .map(|(_, original)| original.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            slots.push((name.to_owned(), value));
            index = end;
        } else {
            if words.get(index)?.0 != *part {
                return None;
            }
            index += 1;
        }
    }
    (index == words.len()).then_some(slots)
}

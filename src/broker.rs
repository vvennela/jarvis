//! Bounded, source-balanced shared context from a single SQLite read snapshot.
//! This captures retrieval-time evidence, not a wake-time focus anchor.
use crate::context::{ContextError, ContextStore, MAX_QUERY_BYTES, Resource, Source};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAX_EVIDENCE: usize = 8;
pub const MAX_EXCERPT_BYTES: usize = 4096;
const PER_SOURCE: usize = 3;
const FOREGROUND_ID: &str = "os:foreground";
const WORK_CANDIDATES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestIntent {
    General,
    NextWork,
    ResumeWork,
}

#[derive(Serialize)]
pub struct Evidence {
    pub resource_id: String,
    pub version: i64,
    pub source: Source,
    pub observed_at_ms: i64,
    pub excerpt: String,
    pub truncated: bool,
    pub trust: &'static str,
    pub selection_reason: &'static str,
    pub age_ms: Option<u64>,
}
#[derive(Serialize)]
pub struct Coverage {
    pub foreground: usize,
    pub files: usize,
    pub browser: usize,
    pub limits_note: &'static str,
}
impl Coverage {
    pub fn from_evidence(evidence: &[Evidence]) -> Self {
        Self {
            foreground: evidence
                .iter()
                .filter(|e| e.source == Source::Accessibility && e.resource_id == FOREGROUND_ID)
                .count(),
            files: evidence.iter().filter(|e| e.source == Source::File).count(),
            browser: evidence
                .iter()
                .filter(|e| e.source == Source::Browser)
                .count(),
            limits_note: "Counts describe selected indexed evidence only. Zero means no evidence selected, not no activity. Collection permissions, current active browser tab, and wake-time focus are not inferred. Browser URL-cache associations do not establish the same or current document.",
        }
    }
}
#[derive(Serialize)]
pub struct ContextPacket {
    pub captured_at_ms: i64,
    pub request_intent: RequestIntent,
    pub evidence: Vec<Evidence>,
    pub coverage: Coverage,
}
pub fn retrieve(database: PathBuf, query: &str) -> Result<ContextPacket, ContextError> {
    let store = ContextStore::open_readonly(&database)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ContextError::InvalidInput)?
        .as_millis()
        .min(i64::MAX as u128) as i64;
    retrieve_at(&store, query, now)
}
pub fn retrieve_at(
    store: &ContextStore,
    query: &str,
    captured_at_ms: i64,
) -> Result<ContextPacket, ContextError> {
    if captured_at_ms < 0 {
        return Err(ContextError::InvalidInput);
    }
    let request_intent = request_intent(query);
    if general_chat(query) {
        return Ok(ContextPacket {
            captured_at_ms,
            request_intent,
            evidence: Vec::new(),
            coverage: Coverage::from_evidence(&[]),
        });
    }
    let query = search_terms(query);
    store.read_snapshot(|snapshot| {
        let mut chosen: Vec<(Resource, &'static str)> = Vec::with_capacity(MAX_EVIDENCE);
        let mut seen = HashSet::new();
        if let Some(resource) = snapshot.get(FOREGROUND_ID)?
            && resource.source == Source::Accessibility
        {
            seen.insert(resource.id.clone());
            chosen.push((resource, "foreground_snapshot"));
        }
        for source in [Source::File, Source::Browser] {
            let mut count = 0;
            if source == Source::File && request_intent != RequestIntent::General {
                for resource in work_files(snapshot, &query)? {
                    if seen.insert(resource.id.clone()) {
                        chosen.push((resource, "work_priority"));
                        count += 1;
                    }
                }
            }
            if count < PER_SOURCE && !query.is_empty() {
                for resource in snapshot.search_source(&query, source, PER_SOURCE)? {
                    if count >= PER_SOURCE {
                        break;
                    }
                    if seen.insert(resource.id.clone()) {
                        chosen.push((resource, "query_match"));
                        count += 1;
                    }
                }
            }
            if count < PER_SOURCE {
                // Fetch a few extra because query matches may also be newest.
                for resource in snapshot.latest_source(source, PER_SOURCE * 2)? {
                    if count >= PER_SOURCE {
                        break;
                    }
                    if seen.insert(resource.id.clone()) {
                        chosen.push((resource, "recent_fallback"));
                        count += 1;
                    }
                }
            }
        }
        // One enrichment slot associates a selected tab URL with a cached page, or a
        // page with matching tab URL metadata. It does not prove document freshness or browser focus.
        if chosen.len() < MAX_EVIDENCE {
            let mut paired = None;
            for (resource, _) in &chosen {
                if let Some(id) = paired_page_id(resource) {
                    if !seen.contains(&id)
                        && let Some(page) = snapshot.get(&id)?
                        && page.source == Source::Browser
                    {
                        paired = Some((page, "cached_url_pair"));
                        break;
                    }
                } else if resource.source == Source::Browser
                    && let Some(rest) = resource.id.strip_prefix("browser-page:")
                    && let Some((tab, _)) = rest.split_once(':')
                    && let Ok(tab) = tab.parse::<i32>()
                    && tab >= 0
                {
                    let id = format!("browser-tab:{tab}");
                    if !seen.contains(&id)
                        && let Some(metadata) = snapshot.get(&id)?
                        && paired_page_id(&metadata).as_deref() == Some(&resource.id)
                    {
                        paired = Some((metadata, "cached_url_pair"));
                        break;
                    }
                }
            }
            if let Some(pair) = paired {
                chosen.push(pair);
            }
        }
        let evidence = chosen
            .into_iter()
            .take(MAX_EVIDENCE)
            .map(|(resource, reason)| {
                let content = excerpt(&resource.content, MAX_EXCERPT_BYTES);
                Evidence {
                    resource_id: resource.id.clone(),
                    version: resource.version,
                    source: resource.source,
                    observed_at_ms: resource.observed_at_ms,
                    truncated: content.len() < resource.content.len(),
                    excerpt: content.to_owned(),
                    trust: "untrusted_content",
                    selection_reason: reason,
                    age_ms: captured_at_ms
                        .checked_sub(resource.observed_at_ms)
                        .and_then(|age| u64::try_from(age).ok()),
                }
            })
            .collect::<Vec<_>>();
        let coverage = Coverage::from_evidence(&evidence);
        Ok(ContextPacket {
            captured_at_ms,
            request_intent,
            evidence,
            coverage,
        })
    })
}

fn request_intent(query: &str) -> RequestIntent {
    let text = excerpt(query, MAX_QUERY_BYTES)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    if text.split_whitespace().any(|word| {
        matches!(
            word,
            "setup" | "install" | "installation" | "configure" | "configuration"
        )
    }) {
        return RequestIntent::General;
    }
    if [
        "where was i",
        "resume my work",
        "resume work",
        "what was i working",
        "pick up where",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
    {
        RequestIntent::ResumeWork
    } else if [
        "work on next",
        "do next",
        "next task",
        "next step",
        "prioritize",
        "priorities",
        "focus on next",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
    {
        RequestIntent::NextWork
    } else {
        RequestIntent::General
    }
}

/// Select from bounded indexed candidates only. Filenames and explicit task
/// headings are relevance hints, never proof of authorship, authority or action.
fn work_files(store: &ContextStore, query: &str) -> Result<Vec<Resource>, ContextError> {
    let mut selected: Vec<Resource> = Vec::with_capacity(PER_SOURCE + 1);
    let mut seen = HashSet::new();
    let searches = [
        Some(
            "priority priorities task tasks todo blocker blocked current status milestone unfinished remaining resume deadline",
        ),
        (!query.is_empty()).then_some(query),
        None,
    ];
    for search in searches {
        let candidates = match search {
            Some(terms) => store.search_source(terms, Source::File, WORK_CANDIDATES)?,
            None => store.latest_source(Source::File, WORK_CANDIDATES)?,
        };
        for resource in candidates {
            if !seen.insert(resource.id.clone()) {
                continue;
            }
            selected.push(resource);
            selected.sort_by(|a, b| {
                work_rank(a)
                    .cmp(&work_rank(b))
                    .then_with(|| b.observed_at_ms.cmp(&a.observed_at_ms))
                    .then_with(|| a.id.cmp(&b.id))
            });
            selected.truncate(PER_SOURCE);
        }
    }
    Ok(selected)
}

fn work_rank(resource: &Resource) -> u8 {
    let name = resource.id.rsplit('/').next().unwrap_or(&resource.id);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let stem = stem.to_ascii_lowercase().replace('_', "-");
    if matches!(
        stem.as_str(),
        "current-work" | "work-status" | "project-status" | "current-status"
    ) {
        return 0;
    }
    // Setup guides often have "next steps" too; their title keeps them below
    // actual work records unless the user explicitly asks a setup question.
    if stem
        .split('-')
        .any(|word| matches!(word, "setup" | "install" | "installation" | "configuration"))
    {
        return 5;
    }
    if matches!(
        stem.as_str(),
        "todo" | "todos" | "tasks" | "priorities" | "worklog" | "notes"
    ) {
        return 1;
    }
    let task_heading = excerpt(&resource.content, MAX_EXCERPT_BYTES)
        .lines()
        .any(|line| {
            let line = line.trim_start_matches(|c: char| c.is_whitespace() || c == '#');
            let lower = line.to_ascii_lowercase();
            [
                "current task",
                "current work",
                "next steps",
                "priorities",
                "remaining work",
                "unfinished",
                "- [ ]",
            ]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        });
    if task_heading {
        return 1;
    }
    if matches!(
        stem.as_str(),
        "plan" | "build-plan" | "roadmap" | "implementation-plan"
    ) {
        return 2;
    }
    if stem == "readme" {
        return 3;
    }
    4
}
fn paired_page_id(resource: &Resource) -> Option<String> {
    if resource.source != Source::Browser || !resource.id.starts_with("browser-tab:") {
        return None;
    }
    #[derive(Deserialize)]
    struct Tab {
        tab_id: i64,
        url: String,
    }
    let tab: Tab = serde_json::from_str(&resource.content).ok()?;
    if !(0..=i32::MAX as i64).contains(&tab.tab_id)
        || resource.id != format!("browser-tab:{}", tab.tab_id)
        || tab.url.len() > 8192
    {
        return None;
    }
    let url = reqwest::Url::parse(&tab.url).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(format!(
        "browser-page:{}:{}",
        tab.tab_id,
        blake3::hash(tab.url.as_bytes()).to_hex()
    ))
}
fn excerpt(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
fn search_terms(query: &str) -> String {
    // Common conversational words otherwise dominate OR-ranked lexical search.
    // No model calls or semantics are inferred from this small retrieval hint.
    const STOP: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "been", "can", "could", "do", "does", "for",
        "from", "have", "hey", "how", "i", "in", "is", "it", "jarvis", "me", "my", "of", "on",
        "or", "please", "should", "show", "tell", "that", "the", "then", "there", "these", "this",
        "to", "was", "were", "what", "when", "where", "which", "who", "with", "would", "you",
        "your",
    ];
    excerpt(query, MAX_QUERY_BYTES)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .filter(|term| !STOP.iter().any(|word| term.eq_ignore_ascii_case(word)))
        .take(16)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Known social/readiness requests should not inherit unrelated setup guides.
/// Explicit work-context cues retain normal retrieval, including mixed greetings.
fn general_chat(query: &str) -> bool {
    let words = excerpt(query, MAX_QUERY_BYTES)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !w.eq_ignore_ascii_case("jarvis"))
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if words.iter().any(|w| {
        matches!(
            w.as_str(),
            "file"
                | "files"
                | "project"
                | "projects"
                | "document"
                | "documents"
                | "notes"
                | "tab"
                | "window"
                | "context"
                | "draft"
                | "email"
                | "summarize"
                | "resume"
                | "continue"
                | "setup"
                | "configure"
        )
    }) {
        return false;
    }
    let text = words.join(" ");
    let readiness = [
        "are you working",
        "are you there",
        "are you connected",
        "can you hear",
        "is this working",
        "how are you",
        "tell me a joke",
    ]
    .iter()
    .any(|phrase| text.contains(phrase));
    let key_complaint =
        text.contains("why") && (text.contains("api key") || text.contains("credentials"));
    let greeting = words
        .first()
        .is_some_and(|w| matches!(w.as_str(), "hi" | "hello" | "thanks"))
        && !words
            .iter()
            .any(|w| matches!(w.as_str(), "work" | "where" | "next" | "earlier"));
    readiness
        || key_complaint
        || greeting
        || matches!(text.as_str(), "thank you" | "good morning" | "good evening")
}

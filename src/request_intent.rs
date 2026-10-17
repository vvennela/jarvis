//! Bounded, conservative parsing of trusted user requests. These helpers identify
//! clauses and explicit missing slots; they never authorize tools by themselves.
const MAX_REQUEST: usize = 8192;
const ACTIONS: &[&str] = &[
    "open",
    "launch",
    "play",
    "pause",
    "draft",
    "write",
    "create",
    "update",
    "fix",
    "build",
    "implement",
    "search",
    "summarize",
];

fn word(text: &str) -> &str {
    text.split_ascii_whitespace().next().unwrap_or("")
}
fn imperative(text: &str) -> &str {
    let text = text.strip_prefix("please ").unwrap_or(text);
    let text = ["can you ", "could you ", "would you ", "will you "]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
        .unwrap_or(text);
    text.strip_prefix("please ").unwrap_or(text)
}
fn quoted(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c, '\'' | '"' | '`' | '‘' | '’' | '“' | '”' | '«' | '»'))
}
fn restrictive(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric()).any(|token| {
        matches!(
            token,
            "not"
                | "no"
                | "never"
                | "dont"
                | "cannot"
                | "cant"
                | "if"
                | "unless"
                | "when"
                | "before"
                | "after"
                | "until"
                | "instead"
        )
    })
}
fn documentary(text: &str) -> bool {
    // A conjunction inside described content does not become a new instruction.
    text.split(|c: char| !c.is_alphanumeric()).any(|token| {
        matches!(
            token,
            "says"
                | "said"
                | "quote"
                | "quoted"
                | "example"
                | "instruction"
                | "instructions"
                | "prompt"
                | "sentence"
                | "paragraph"
                | "following"
                | "say"
                | "tell"
                | "explain"
                | "about"
                | "describing"
                | "containing"
                | "mentioning"
        )
    })
}

/// Separate at most three explicit actions, keeping borrowed original text.
/// Ordinary questions and ambiguous wording remain one untouched request, so
/// callers can answer them normally and apply their existing whole-request gate.
#[allow(clippy::result_unit_err)]
pub fn command_clauses(request: &str) -> Result<Vec<&str>, ()> {
    if request.is_empty() || request.len() > MAX_REQUEST || request.chars().any(char::is_control) {
        return Err(());
    }
    let lower = request.to_ascii_lowercase();
    let body = imperative(lower.trim());
    if quoted(request)
        || request
            .chars()
            .any(|c| matches!(c, ';' | '|' | '&' | '$' | '<' | '>'))
        || lower.contains(" then ")
        || restrictive(&lower)
        || documentary(&lower)
        || !ACTIONS.contains(&word(body))
        || matches!(
            word(body),
            "search" | "summarize" | "draft" | "write" | "create"
        )
    {
        return Ok(vec![request]);
    }
    let mut cuts = Vec::new();
    // Only "and" describes independent actions here. "Then" stays intact
    // above; this helper must not turn a dependency into parallel execution.
    for (index, matched) in lower.match_indices(" and ") {
        let next = index + matched.len();
        if ACTIONS.contains(&word(lower[next..].trim_start())) {
            cuts.push((index, next));
        }
    }
    cuts.sort_unstable();
    // Content requests consume their remaining text. A later conjunction could
    // belong to the text being summarized rather than to the user's commands.
    if cuts
        .iter()
        .take(cuts.len().saturating_sub(1))
        .any(|(_, next)| {
            matches!(
                word(lower[*next..].trim_start()),
                "search" | "summarize" | "draft" | "write" | "create"
            )
        })
    {
        return Ok(vec![request]);
    }
    if cuts.len() > 2 {
        return Err(());
    }
    if cuts.is_empty() {
        return Ok(vec![request]);
    }
    let mut clauses = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    for (end, next) in cuts {
        let clause = request[start..end].trim().trim_end_matches(',').trim_end();
        if clause.is_empty() {
            return Ok(vec![request]);
        }
        clauses.push(clause);
        start = next;
    }
    clauses.push(request[start..].trim());
    Ok(clauses)
}

fn plain_name(answer: &str) -> Option<&str> {
    if answer.len() > 128 || answer.chars().any(char::is_control) {
        return None;
    }
    let answer = answer.trim();
    if answer.is_empty()
        || answer.len() > 128
        || quoted(answer)
        || !answer
            .chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || c == '-')
    {
        return None;
    }
    let lower = answer.to_ascii_lowercase();
    if restrictive(&lower)
        || lower.split_ascii_whitespace().any(|token| {
            ACTIONS.contains(&token)
                || matches!(
                    token,
                    "and"
                        | "then"
                        | "or"
                        | "send"
                        | "delete"
                        | "pay"
                        | "purchase"
                        | "execute"
                        | "run"
                        | "ignore"
                        | "instead"
                )
        })
    {
        return None;
    }
    Some(answer)
}

/// Fill only a supported explicit user-request slot. A model's question cannot
/// turn a summary or general conversation into permission to perform an action.
pub fn resolve_action_followup(original: &str, answer: &str) -> Option<String> {
    if original.len() > MAX_REQUEST || original.chars().any(char::is_control) || quoted(original) {
        return None;
    }
    let lower = original
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    if restrictive(&lower) {
        return None;
    }
    let body = imperative(&lower);
    let answer = plain_name(answer)?;
    match body {
        "open the browser" | "open a browser" | "launch the browser" | "launch a browser" => {
            // The shipped app preferences approve Safari. Execution still checks
            // the current durable allowlist, including revocations.
            answer
                .eq_ignore_ascii_case("safari")
                .then(|| "open Safari".into())
        }
        "play some music"
        | "play music"
        | "play some music on spotify"
        | "play music on spotify" => Some(format!("play {answer} on Spotify")),
        "open wikipedia" | "open the wikipedia page" | "open a wikipedia page" => {
            Some(format!("open the Wikipedia page for {answer}"))
        }
        _ => None,
    }
}

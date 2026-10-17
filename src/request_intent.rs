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

// A possessive in a target ("Napoleon's Wikipedia page") is not quoted
// speech. All other quotation marks remain a boundary against splitting
// described/quoted instructions into independently executable commands.
fn quoted_command(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(index, &c)| {
        if matches!(c, '\'' | '’')
            && index > 0
            && chars[index - 1].is_alphanumeric()
            && chars.get(index + 1).is_some_and(|c| matches!(c, 's' | 'S'))
            && chars.get(index + 2).is_none_or(|c| c.is_whitespace())
        {
            return false;
        }
        matches!(c, '\'' | '"' | '`' | '‘' | '’' | '“' | '”' | '«' | '»')
    })
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
    if quoted_command(request)
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
    let answer = answer.trim().trim_end_matches(['.', '!', '?']).trim_end();
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
    if matches!(
        lower.as_str(),
        "cancel" | "stop" | "wait" | "nevermind" | "never mind"
    ) || matches!(
        word(&lower),
        "what" | "who" | "why" | "how" | "where" | "when" | "tell" | "explain"
    ) || restrictive(&lower)
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

/// Recognize a trusted request to open an article. Wikipedia is the installed
/// article provider when the user says only "page" or "article". An empty topic
/// is a missing slot, not an executable target.
pub fn wikipedia_page_intent(request: &str) -> Option<String> {
    if request.is_empty()
        || request.len() > MAX_REQUEST
        || request
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '`' | '$' | ';' | '|' | '<' | '>'))
    {
        return None;
    }
    let normalized = request
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut body = normalized.as_str();
    for _ in 0..4 {
        let Some(rest) = [
            "please ",
            "can you ",
            "could you ",
            "would you ",
            "will you ",
            "i want you to ",
        ]
        .iter()
        .find_map(|prefix| body.strip_prefix(prefix)) else {
            break;
        };
        body = rest;
    }
    for _ in 0..2 {
        let Some(rest) = [" please", " for me"]
            .iter()
            .find_map(|suffix| body.strip_suffix(suffix))
        else {
            break;
        };
        body = rest;
    }
    let rest = body.strip_prefix("open ")?;
    let rest = rest
        .strip_prefix("the ")
        .or_else(|| rest.strip_prefix("a "))
        .unwrap_or(rest);
    // The topic may precede the provider and noun in ordinary speech:
    // "open the Ada Lovelace Wikipedia page". Preserve the whole supplied
    // topic; this parser does not invent a narrower article or resolve aliases.
    for suffix in [" wikipedia page", " wikipedia article"] {
        if let Some(topic) = rest.strip_suffix(suffix) {
            let topic = topic
                .strip_suffix("'s")
                .or_else(|| topic.strip_suffix("’s"))
                .unwrap_or(topic);
            return (!topic.is_empty()).then(|| topic.to_owned());
        }
    }
    if let Some(topic) = rest.strip_suffix(" on wikipedia") {
        if matches!(topic, "page" | "article") {
            return Some(String::new());
        }
        let topic = topic
            .strip_suffix(" page")
            .or_else(|| topic.strip_suffix(" article"))
            .unwrap_or(topic);
        return (!topic.is_empty()).then(|| topic.to_owned());
    }
    for prefix in ["page on wikipedia", "article on wikipedia"] {
        if rest == prefix {
            return Some(String::new());
        }
        if let Some(tail) = rest.strip_prefix(prefix) {
            for link in [" for ", " about ", " on "] {
                if let Some(topic) = tail.strip_prefix(link) {
                    return (!topic.is_empty()).then(|| topic.to_owned());
                }
            }
        }
    }
    for noun in [
        "wikipedia's page",
        "wikipedia's article",
        "wikipedia’s page",
        "wikipedia’s article",
        "wikipedia page",
        "wikipedia article",
        "wikipedia",
        "page",
        "article",
    ] {
        if rest == noun {
            return Some(String::new());
        }
        if let Some(tail) = rest.strip_prefix(noun) {
            for link in [" for ", " about ", " on "] {
                if let Some(topic) = tail.strip_prefix(link) {
                    return (!topic.is_empty()).then(|| topic.to_owned());
                }
            }
        }
    }
    None
}

/// A named tab and explicit destination from human speech/text. Document text
/// and implicit focus cannot supply either part of a navigation authorization.
pub fn tab_navigation_intent(request: &str) -> Option<(String, String)> {
    if request.len() > MAX_REQUEST || request.chars().any(char::is_control) || quoted(request) {
        return None;
    }
    let text = request.trim();
    let lower = text.to_ascii_lowercase();
    let body = imperative(&lower);
    let offset = lower.len() - body.len();
    let original_body = &text[offset..];
    let (query, destination) = if let Some(rest) = ["navigate ", "change ", "open "]
        .iter()
        .find_map(|prefix| body.strip_prefix(prefix))
        && let Some((query, _)) = rest.split_once(" tab to ")
    {
        let destination_start = body.find(" tab to ")? + " tab to ".len();
        (query, &original_body[destination_start..])
    } else if let Some(rest) = body.strip_prefix("open ")
        && let Some((_, query)) = rest.split_once(" in ")
        && let Some(query) = query.strip_suffix(" tab")
    {
        let destination_end = body.find(" in ")?;
        (query, &original_body["open ".len()..destination_end])
    } else {
        return None;
    };
    let query = query
        .strip_prefix("the ")
        .or_else(|| query.strip_prefix("my "))
        .unwrap_or(query)
        .trim();
    if query.len() < 3
        || query.len() > 128
        || restrictive(query)
        || matches!(query, "this" | "that" | "current" | "active" | "any")
    {
        return None;
    }
    let url = reqwest::Url::parse(destination).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || destination.len() > 2048
        || destination.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some((query.to_owned(), url.to_string()))
}

/// Fill only a supported explicit user-request slot. A model's question cannot
/// turn a summary or general conversation into permission to perform an action.
pub fn resolve_action_followup(original: &str, answer: &str) -> Option<String> {
    if original.len() > MAX_REQUEST
        || original.chars().any(char::is_control)
        || quoted_command(original)
        || restrictive(&original.to_ascii_lowercase())
    {
        return None;
    }
    let continuation = answer
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    let clauses = command_clauses(original).ok()?;
    if clauses.len() == 1
        && (matches!(
            continuation.as_str(),
            "next" | "the next one" | "do the remaining one"
        ) || (matches!(
            continuation.as_str(),
            "now the music" | "now the song" | "do the music next" | "do the song next"
        ) && (crate::spotify_tracks::requested_track(original).is_some()
            || crate::spotify_playback::requested_artist(original).is_some()))
            || (matches!(
                continuation.as_str(),
                "now the page" | "now the wikipedia page"
            ) && wikipedia_page_intent(original).is_some()))
    {
        return Some(original.to_owned());
    }
    // Choosing an action from a pending compound request retains its original
    // target, rather than treating "Wikipedia first" as a new missing-topic
    // request. Only an unambiguous existing clause can be selected.
    if let Ok(clauses) = command_clauses(original)
        && clauses.len() > 1
    {
        let answer = answer
            .trim()
            .trim_end_matches(['.', '!', '?'])
            .to_ascii_lowercase();
        if answer.len() > 128
            || answer.chars().any(char::is_control)
            || quoted(&answer)
            || restrictive(&answer)
        {
            return None;
        }
        let selection = answer
            .strip_prefix("do ")
            .or_else(|| answer.strip_prefix("open "))
            .unwrap_or(&answer);
        let selection = selection.strip_prefix("the ").unwrap_or(selection);
        let selection = selection.strip_suffix(" first").unwrap_or(selection);
        let ordinal = match selection {
            "first" | "first one" => Some(0),
            "second" | "second one" => Some(1),
            "third" | "third one" => Some(2),
            _ => None,
        };
        if let Some(index) = ordinal {
            return clauses.get(index).map(|clause| (*clause).to_owned());
        }
        let mut matches = clauses.iter().filter(|clause| match selection {
            "wikipedia" | "wikipedia page" | "page" | "article" => {
                wikipedia_page_intent(clause).is_some()
            }
            "music" | "song" | "spotify" => {
                crate::spotify_tracks::requested_track(clause).is_some()
                    || crate::spotify_playback::requested_artist(clause).is_some()
            }
            _ => false,
        });
        let selected = matches.next()?;
        return matches.next().is_none().then(|| (*selected).to_owned());
    }
    let lower = original
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    if restrictive(&lower) {
        return None;
    }
    if let Some(track) = crate::spotify_tracks::track_confirmation(original) {
        let answer = answer
            .trim()
            .trim_end_matches(['.', '!', '?'])
            .to_lowercase();
        return matches!(
            answer.as_str(),
            "yes" | "yes please" | "correct" | "that's right"
        )
        .then(|| format!("play {} by {} on Spotify", track.title, track.artist));
    }
    let body = imperative(&lower);
    let answer = plain_name(answer)?;
    if let Some(topic) = wikipedia_page_intent(original) {
        let named = answer.to_lowercase();
        // A missing topic can be filled; an existing topic can be repeated or
        // narrowed ("Napoleon III"), but a bare reply cannot replace it wholesale.
        if topic.is_empty() || named == topic || named.starts_with(&format!("{topic} ")) {
            return Some(format!("open the Wikipedia page for {answer}"));
        }
        return None;
    }
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
        _ => None,
    }
}

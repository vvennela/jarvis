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
        // An apostrophe between letters is a contraction ("I'm", "there's").
        if matches!(c, '\'' | '’')
            && index > 0
            && chars[index - 1].is_alphabetic()
            && chars.get(index + 1).is_some_and(|c| c.is_alphabetic())
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

/// Verbs that may begin a later clause of a multi-step request.
const CLAUSE_STARTS: &[&str] = &[
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
    "email",
    "send",
    "shoot",
    "check",
    "read",
    "show",
    "pull",
    "bring",
    "visit",
    "look",
    "switch",
    "jump",
    "focus",
];
/// Clauses whose text is content: it runs to the end of the request unless a
/// comma clearly ends it ("email Brian that I'll open it and play the demo").
const CONTENT_STARTS: &[&str] = &[
    "draft",
    "write",
    "create",
    "search",
    "summarize",
    "email",
    "send",
    "shoot",
];
/// Read-only clauses may contain conditions ("check my texts and tell me if
/// anything is new") without making the request ambiguous.
const READ_STARTS: &[&str] = &["check", "read"];

/// Separate at most four explicit actions, keeping borrowed original text.
/// Ordinary questions and ambiguous wording remain one untouched request, so
/// callers can answer them normally and apply their existing whole-request gate.
#[allow(clippy::result_unit_err)]
pub fn command_clauses(request: &str) -> Result<Vec<&str>, ()> {
    if request.is_empty() || request.len() > MAX_REQUEST || request.chars().any(char::is_control) {
        return Err(());
    }
    let lower = request.to_ascii_lowercase();
    let whole = Ok(vec![request]);
    let body = imperative(lower.trim());
    if quoted_command(request)
        || request
            .chars()
            .any(|c| matches!(c, ';' | '|' | '&' | '$' | '<' | '>'))
        || lower.contains(" then ")
        || !CLAUSE_STARTS.contains(&word(body))
    {
        return whole;
    }
    // Candidate boundaries: ", and verb", ", verb", or " and verb".
    let mut cuts: Vec<(usize, usize, bool)> = Vec::new();
    for (index, _) in lower.match_indices(' ') {
        let before = &lower[..index];
        let after = &lower[index..];
        let (end, next, comma) = if let Some(rest) = after.strip_prefix(" and ") {
            let comma = before.ends_with(',');
            (
                if comma { index - 1 } else { index },
                lower.len() - rest.len(),
                comma,
            )
        } else if before.ends_with(',') && !after.starts_with(" and ") {
            (index - 1, index + 1, true)
        } else {
            continue;
        };
        if CLAUSE_STARTS.contains(&word(lower[next..].trim_start())) {
            cuts.push((end, next, comma));
        }
    }
    cuts.sort_unstable();
    cuts.dedup_by_key(|cut| cut.1);
    if cuts.is_empty() {
        return whole;
    }
    let mut clauses = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    let mut content = CONTENT_STARTS.contains(&word(body));
    for (end, next, comma) in cuts {
        if content && !comma {
            // A bare "and verb" inside content is ambiguous: keep it whole.
            return whole;
        }
        let clause = request[start..end].trim().trim_end_matches(',').trim_end();
        if clause.is_empty() {
            return whole;
        }
        clauses.push(clause);
        start = next;
        content = CONTENT_STARTS.contains(&word(&lower[next..]));
    }
    clauses.push(request[start..].trim());
    if clauses.len() > 4 {
        return Err(());
    }
    // Conditions and described instructions in an action clause keep the
    // request whole, as do any in a clause that is not content or a read.
    for clause in &clauses {
        let lower = clause.to_ascii_lowercase();
        let verb = word(imperative(&lower));
        if CONTENT_STARTS.contains(&verb) || READ_STARTS.contains(&verb) {
            continue;
        }
        if restrictive(&lower) || documentary(&lower) {
            return whole;
        }
    }
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

/// A named tab to bring to the front ("switch to the Docs tab"), optionally
/// followed by an explicit destination ("... and navigate it to https://...").
pub fn tab_switch_intent(request: &str) -> Option<(String, Option<String>)> {
    if request.len() > MAX_REQUEST || request.chars().any(char::is_control) || quoted(request) {
        return None;
    }
    let text = request.trim().trim_end_matches(['.', '!', '?']);
    let lower = text.to_ascii_lowercase();
    let body = imperative(&lower);
    let rest = [
        "switch to ",
        "switch over to ",
        "go to ",
        "go back to ",
        "jump to ",
        "flip to ",
        "take me to ",
        "show me ",
        "bring up ",
        "pull up ",
        "open ",
        "focus ",
        "focus on ",
    ]
    .iter()
    .find_map(|prefix| body.strip_prefix(prefix))?;
    // "tab" must be its own word, and a page request is never a tab.
    if rest.contains("wikipedia") || rest.contains(" page") || rest.contains(" article") {
        return None;
    }
    let (named, destination) = rest
        .match_indices(" tab")
        .map(|(index, matched)| (&rest[..index], &rest[index + matched.len()..]))
        .find(|(_, after)| after.is_empty() || after.starts_with(' '))?;
    let destination = destination.trim();
    let url = if destination.is_empty() {
        None
    } else {
        let tail = [
            "and navigate it to ",
            "and navigate to ",
            "and go to ",
            "and take it to ",
            "and open ",
        ]
        .iter()
        .find_map(|prefix| destination.strip_prefix(prefix))?;
        let start = lower.rfind(tail)?;
        let original = &text[start..];
        let parsed = reqwest::Url::parse(original).ok()?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || original.chars().any(char::is_whitespace)
        {
            return None;
        }
        Some(parsed.to_string())
    };
    let query = named
        .strip_prefix("the ")
        .or_else(|| named.strip_prefix("my "))
        .unwrap_or(named)
        .trim();
    if query.len() < 3
        || query.len() > 128
        || restrictive(query)
        || matches!(
            query,
            "this" | "that" | "current" | "active" | "any" | "new" | "next" | "last"
        )
    {
        return None;
    }
    Some((query.to_owned(), url))
}

/// A named tab and explicit destination from human speech/text. Document text
/// and implicit focus cannot supply either part of a navigation authorization.
pub fn tab_navigation_intent(request: &str) -> Option<(String, String)> {
    if let Some((query, Some(url))) = tab_switch_intent(request) {
        return Some((query, url));
    }
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

/// "Play some music", "put something on", "I want to listen to music": a
/// music request with no artist, song or playlist named.
pub fn generic_music_request(request: &str) -> bool {
    if request.len() > 200 || request.chars().any(char::is_control) || quoted(request) {
        return false;
    }
    let lower = request
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_lowercase();
    if restrictive(&lower) {
        return false;
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    if words.len() > 10 {
        return false;
    }
    let verb = words.iter().any(|w| {
        matches!(
            *w,
            "play" | "listen" | "hear" | "put" | "throw" | "queue" | "start"
        )
    });
    let generic = words.iter().any(|w| {
        matches!(
            *w,
            "music" | "song" | "songs" | "something" | "tunes" | "anything" | "jams" | "track"
        )
    });
    // Every other word must be filler, so a named target is never ignored.
    let rest_is_filler = words.iter().all(|w| {
        matches!(
            *w,
            "play"
                | "listen"
                | "hear"
                | "put"
                | "throw"
                | "queue"
                | "start"
                | "music"
                | "song"
                | "songs"
                | "something"
                | "tunes"
                | "anything"
                | "jams"
                | "track"
                | "some"
                | "a"
                | "an"
                | "to"
                | "on"
                | "me"
                | "i"
                | "want"
                | "wanna"
                | "would"
                | "like"
                | "i'd"
                | "can"
                | "could"
                | "you"
                | "please"
                | "jarvis"
                | "hey"
                | "spotify"
                | "up"
                | "good"
                | "let's"
                | "lets"
                | "us"
                | "for"
                | "the"
        )
    });
    verb && generic && rest_is_filler
}

/// A spoken name answer with conversational padding removed.
fn answer_name(answer: &str) -> Option<String> {
    let mut text = answer
        .replace(',', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!', '?'])
        .trim()
        .to_owned();
    for _ in 0..3 {
        let lower = text.to_lowercase();
        let Some(prefix) = [
            "um ",
            "uh ",
            "uhh ",
            "hmm ",
            "oh ",
            "how about ",
            "what about ",
            "let's do ",
            "lets do ",
            "let's go with ",
            "go with ",
            "maybe ",
            "i'd say ",
            "i guess ",
            "play ",
            "put on ",
            "some ",
            "definitely ",
            "probably ",
        ]
        .iter()
        .find(|prefix| lower.starts_with(*prefix)) else {
            break;
        };
        text = text[prefix.len()..]
            .trim_start_matches([' ', ','])
            .to_owned();
    }
    for _ in 0..2 {
        let lower = text.to_lowercase();
        let Some(suffix) = [
            " please",
            " i guess",
            " thanks",
            " thank you",
            " on spotify",
        ]
        .iter()
        .find(|suffix| lower.ends_with(*suffix)) else {
            break;
        };
        text.truncate(text.len() - suffix.len());
        text = text.trim_end_matches([' ', ',']).to_owned();
    }
    plain_name(&text).map(str::to_owned)
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
    if generic_music_request(original) {
        return answer_name(answer).map(|name| format!("play {name} on Spotify"));
    }
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

/// The request says not to, or only conditionally ("don't", "never", "if").
pub fn negated(request: &str) -> bool {
    let lower = request.to_lowercase().replace('’', "'");
    restrictive(&lower.replace("n't", " not"))
        || lower
            .split(|c: char| !c.is_alphanumeric() && c != '\'')
            .any(|word| matches!(word, "don't" | "dont" | "won't" | "shouldn't" | "stop"))
}

/// Negation or condition in the words before the first of `targets` ("don't
/// read my messages"), ignoring the content that follows ("email Brian that
/// I'll be there after 5").
pub fn negated_before(request: &str, targets: &[&str]) -> bool {
    let lower = request.to_lowercase();
    let mut cut = lower.len();
    let mut start = None;
    for (index, c) in lower.char_indices().chain([(lower.len(), ' ')]) {
        let part = c.is_alphanumeric() || c == '-';
        match (start, part) {
            (None, true) => start = Some(index),
            (Some(begin), false) => {
                if targets.contains(&&lower[begin..index]) {
                    cut = begin;
                    break;
                }
                start = None;
            }
            _ => {}
        }
    }
    // Lowercasing can change byte lengths; cut the lowercase text itself.
    negated(&lower[..cut])
}

/// One complete, deterministic action: a named page, song, or artist.
pub fn resolved_action(clause: &str) -> bool {
    tab_switch_intent(clause).is_some()
        || crate::web_visit::website_intent(clause).is_some()
        || wikipedia_page_intent(clause).is_some_and(|topic| !topic.is_empty())
        || crate::spotify_tracks::requested_track(clause).is_some()
        || crate::spotify_playback::requested_artist(clause).is_some()
}

fn resolved_count(request: &str) -> usize {
    command_clauses(request).map_or(0, |clauses| {
        clauses
            .iter()
            .filter(|clause| resolved_action(clause))
            .count()
    })
}

/// Speech recognition often changes a spoken command's verb ("opened",
/// "playing") or keeps a filler word. Rewrite only each clause's leading
/// words, and return the rewrite only when it yields more complete actions,
/// so questions and ordinary conversation are never altered.
pub fn canonical_request(request: &str) -> Option<String> {
    if request.is_empty() || request.len() > MAX_REQUEST || request.chars().any(char::is_control) {
        return None;
    }
    let text = request.split_whitespace().collect::<Vec<_>>().join(" ");
    let rewritten = text
        .split(" and ")
        .map(canonical_clause)
        .collect::<Vec<_>>()
        .join(" and ");
    (rewritten != text && resolved_count(&rewritten) > resolved_count(request)).then_some(rewritten)
}

fn canonical_clause(clause: &str) -> String {
    let mut rest = clause.trim();
    // Fillers and recognized wake residue before the command itself.
    for _ in 0..4 {
        let lower = rest.to_ascii_lowercase();
        let Some(filler) = [
            "hey", "hi", "okay", "ok", "so", "um", "uh", "and", "jarvis", "to", "please",
        ]
        .iter()
        .find(|filler| lower.starts_with(*filler) && lower[filler.len()..].starts_with([' ', ','])) else {
            break;
        };
        rest = rest[filler.len()..].trim_start_matches([' ', ',']);
    }
    let lower = rest.to_ascii_lowercase();
    let page = lower.contains("wikipedia") || lower.contains(" page") || lower.contains(" article");
    let verbs: &[(&str, &str)] = &[
        ("opened ", "open "),
        ("opens ", "open "),
        ("opening ", "open "),
        ("played ", "play "),
        ("plays ", "play "),
        ("playing ", "play "),
        ("start playing ", "play "),
    ];
    if let Some((from, to)) = verbs.iter().find(|(from, _)| lower.starts_with(*from)) {
        return format!("{to}{}", &rest[from.len()..]);
    }
    if page {
        for from in [
            "pull up ",
            "bring up ",
            "show me ",
            "show ",
            "go to ",
            "take me to ",
            "load ",
            "look up ",
            "find ",
            "get ",
        ] {
            if lower.starts_with(from) {
                return format!("open {}", &rest[from.len()..]);
            }
        }
        if lower.starts_with("the wikipedia ") || lower.starts_with("wikipedia ") {
            return format!("open {rest}");
        }
    }
    rest.to_owned()
}

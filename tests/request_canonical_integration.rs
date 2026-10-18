//! Recognizer verb/filler normalization: complete actions are recovered, while
//! questions and ambiguous or restricted requests are never rewritten.
use jarvis::request_intent::{canonical_request, command_clauses, resolved_action};

#[test]
fn recognizer_verb_forms_and_fillers_become_complete_actions() {
    for (heard, expected) in [
        (
            "opened the Wikipedia page for Napoleon.",
            "open the Wikipedia page for Napoleon.",
        ),
        (
            "playing Metallica on Spotify.",
            "play Metallica on Spotify.",
        ),
        ("Hey, Jarvis, play Taylor Swift.", "play Taylor Swift."),
        (
            "the Wikipedia page for Napoleon",
            "open the Wikipedia page for Napoleon",
        ),
        (
            "opened the Wikipedia page for Napoleon and playing Taylor Swift on Spotify",
            "open the Wikipedia page for Napoleon and play Taylor Swift on Spotify",
        ),
    ] {
        let rewritten = canonical_request(heard).unwrap_or_else(|| panic!("{heard}"));
        assert_eq!(rewritten, expected);
        assert!(
            command_clauses(&rewritten)
                .unwrap()
                .iter()
                .all(|clause| resolved_action(clause)),
            "{rewritten}"
        );
    }
}

#[test]
fn questions_complete_requests_and_restrictions_are_never_rewritten() {
    for request in [
        "open the Wikipedia page for Napoleon",
        "play Taylor Swift on Spotify",
        "what was I working on?",
        "show me what changed in the Beacon notes",
        "so what should I do next",
        "don't play Taylor Swift",
        "playing Taylor Swift is not what I want",
        "nice",
        "",
    ] {
        assert_eq!(canonical_request(request), None, "{request}");
    }
}

#[test]
fn bounded_input_is_rejected_without_rewriting() {
    assert_eq!(canonical_request(&"opened ".repeat(2000)), None);
    assert_eq!(canonical_request("opened the page\u{0} for Napoleon"), None);
}

#[test]
fn a_leading_to_from_the_recognizer_still_opens_the_page() {
    let rewritten = canonical_request("to open the Wikipedia page for Napoleon").unwrap();
    assert_eq!(rewritten, "open the Wikipedia page for Napoleon");
    assert_eq!(
        jarvis::request_intent::wikipedia_page_intent(&rewritten).as_deref(),
        Some("napoleon")
    );
    // "to" inside a request is untouched.
    assert_eq!(canonical_request("how to open a jar"), None);
}

#[test]
fn every_opening_phrase_opens_the_napoleon_page() {
    for request in [
        "open up the Wikipedia page for Napoleon",
        "Open up the Wikipedia page for Napoleon.",
        "pull up the Wikipedia page for Napoleon",
        "bring up Napoleon's Wikipedia page",
        "show me the Wikipedia article about Napoleon",
        "look up Napoleon on Wikipedia",
        "go to the Wikipedia page for Napoleon",
        "can you open up the Wikipedia page for Napoleon",
        "to open up the Wikipedia page for Napoleon",
    ] {
        let canonical = canonical_request(request);
        let text = canonical.as_deref().unwrap_or(request);
        assert_eq!(
            jarvis::request_intent::wikipedia_page_intent(text).as_deref(),
            Some("napoleon"),
            "{request}"
        );
        assert!(
            jarvis::web_open::authorize_page_request(text, "wikipedia", "napoleon").is_ok(),
            "{request}"
        );
    }
}

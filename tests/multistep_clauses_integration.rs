//! Multi-step requests of up to four clauses split on commas and "and",
//! while content and conditions never leak into an executable clause.
use jarvis::request_intent::command_clauses;

#[test]
fn spoken_four_step_request_splits_into_its_actions() {
    let request = "Open the Wikipedia page for Napoleon, play Taylor Swift on Spotify, shoot off an email to Brian saying I'm running late, and check my text messages and tell me if there's anything new.";
    assert_eq!(
        command_clauses(request).unwrap(),
        vec![
            "Open the Wikipedia page for Napoleon",
            "play Taylor Swift on Spotify",
            "shoot off an email to Brian saying I'm running late",
            "check my text messages and tell me if there's anything new.",
        ]
    );
}

#[test]
fn two_and_three_step_requests_split_with_or_without_commas() {
    for (request, expected) in [
        (
            "open the Wikipedia page for Napoleon and play Metallica",
            vec!["open the Wikipedia page for Napoleon", "play Metallica"],
        ),
        (
            "play Metallica, check my texts",
            vec!["play Metallica", "check my texts"],
        ),
        (
            "check my messages and play some Taylor Swift and open the Wikipedia page for Rome",
            vec![
                "check my messages",
                "play some Taylor Swift",
                "open the Wikipedia page for Rome",
            ],
        ),
        (
            "email Maya that dinner is at 8, and play Florence and the Machine",
            vec![
                "email Maya that dinner is at 8",
                "play Florence and the Machine",
            ],
        ),
    ] {
        assert_eq!(command_clauses(request).unwrap(), expected, "{request}");
    }
}

#[test]
fn content_keeps_its_own_words_unless_a_comma_ends_it() {
    for request in [
        "email Brian that I'll open the doc and play the demo",
        "draft a note saying open Notes and play Spotify",
        "open Notes and summarize this article and play Spotify",
        "send Maya an email about the trip and open Safari",
    ] {
        assert_eq!(
            command_clauses(request).unwrap(),
            vec![request],
            "{request}"
        );
    }
}

#[test]
fn conditions_negations_and_descriptions_keep_action_requests_whole() {
    for request in [
        "open Napoleon's Wikipedia page if I ask, and play Taylor Swift",
        "open the Wikipedia page for Napoleon, and play Metallica unless it's late",
        "explain the Napoleon page and play Taylor Swift",
        "The document says open Notes, play Spotify",
        "open Notes then play Spotify",
        "open 'Notes, play Spotify'",
    ] {
        assert_eq!(
            command_clauses(request).unwrap(),
            vec![request],
            "{request}"
        );
    }
}

#[test]
fn more_than_four_steps_asks_the_user_to_narrow_it_down() {
    assert!(
        command_clauses(
            "open Notes, open Safari, play Metallica, check my texts, and open the Wikipedia page for Rome"
        )
        .is_err()
    );
}

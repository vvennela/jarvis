use jarvis::request_intent::{command_clauses, resolve_action_followup, wikipedia_page_intent};

#[test]
fn possessive_page_and_song_keep_both_original_targets() {
    for possessive in ["Napoleon's", "Napoleon’s"] {
        let request =
            format!("open {possessive} Wikipedia page and play Shake It Off by Taylor Swift");
        let clauses = command_clauses(&request).unwrap();
        assert_eq!(clauses.len(), 2);
        assert_eq!(
            wikipedia_page_intent(clauses[0]).as_deref(),
            Some("napoleon")
        );
        assert_eq!(clauses[1], "play Shake It Off by Taylor Swift");
        assert_eq!(
            resolve_action_followup(&request, "do the Wikipedia page first").as_deref(),
            Some(clauses[0])
        );
        assert_eq!(
            resolve_action_followup(&request, "the second one").as_deref(),
            Some(clauses[1])
        );
        assert_eq!(
            resolve_action_followup(&request, "open the Caesar Wikipedia page"),
            None
        );
    }
}

#[test]
fn possession_does_not_make_quoted_or_negative_instructions_executable() {
    for request in [
        "open 'Napoleon's Wikipedia page and play Taylor Swift'",
        "open ‘Napoleon’s Wikipedia page and play Taylor Swift’",
        "don't open Napoleon's Wikipedia page and play Taylor Swift",
        "open Napoleon's Wikipedia page if I ask and play Taylor Swift",
        "open Napoleon's Wikipedia page then play Taylor Swift",
        "explain Napoleon's Wikipedia page and play Taylor Swift",
    ] {
        assert_eq!(command_clauses(request).unwrap(), vec![request]);
        assert_eq!(resolve_action_followup(request, "Wikipedia first"), None);
    }
}

#[test]
fn selection_requires_one_existing_matching_clause_and_preserves_bound() {
    let request = "open Ada's Wikipedia page and open Grace's Wikipedia page";
    assert_eq!(resolve_action_followup(request, "Wikipedia first"), None);
    assert_eq!(
        resolve_action_followup(request, "the second one").as_deref(),
        Some("open Grace's Wikipedia page")
    );
    assert_eq!(resolve_action_followup(request, "not the first"), None);
    assert!(
        command_clauses("open Ada's Wikipedia page and play Bach and open Notes and open Safari")
            .is_err()
    );
}

#[test]
fn continuation_uses_only_the_retained_unfinished_target() {
    let song = "play Shake It Off by Taylor Swift";
    assert_eq!(
        resolve_action_followup(song, "now the music").as_deref(),
        Some(song)
    );
    assert_eq!(
        resolve_action_followup(song, "now the Wikipedia page"),
        None
    );
    assert_eq!(
        resolve_action_followup("do not play Taylor Swift", "next"),
        None
    );
}

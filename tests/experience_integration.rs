//! Skill learning rules: only verified tool choices whose arguments the user
//! actually said become templates, and failing skills are demoted.
mod support;
use jarvis::{
    experience::{ExperienceStore, PROMOTE_AFTER},
    providers::ToolCall,
};
use serde_json::json;
use std::time::Duration;
use support::TempDir;

fn artist(name: &str) -> ToolCall {
    ToolCall {
        id: "m".into(),
        name: "spotify_play_artist".into(),
        arguments: json!({ "artist": name }),
    }
}
fn store(dir: &TempDir) -> ExperienceStore {
    ExperienceStore::new(dir.join("experience.json"))
}
fn teach(store: &ExperienceStore, request: &str, call: &ToolCall) {
    store.record_model_turn(request, Some(call), Duration::from_millis(2400));
}

#[test]
fn a_template_is_promoted_only_after_repeated_verified_success() {
    let dir = TempDir::new();
    let store = store(&dir);
    teach(
        &store,
        "Hey Jarvis, crank up some Metallica!",
        &artist("Metallica"),
    );
    assert!(
        store.match_request("crank up some Bach").is_none(),
        "promoted after one success"
    );
    teach(
        &store,
        "crank up some Taylor Swift",
        &artist("Taylor Swift"),
    );
    assert_eq!(PROMOTE_AFTER, 2);
    let matched = store
        .match_request("Crank up some Florence and the Machine.")
        .unwrap();
    assert_eq!(matched.call.name, "spotify_play_artist");
    assert_eq!(
        matched.call.arguments,
        json!({"artist":"Florence and the Machine"})
    );
    assert!(
        store
            .match_request("crank up the volume please now")
            .is_none()
    );
    assert!(store.match_request("crank up some").is_none());
}

#[test]
fn nothing_is_learned_from_invented_arguments_or_unlearnable_tools() {
    let dir = TempDir::new();
    let store = store(&dir);
    for _ in 0..3 {
        // The model chose an artist the user never said.
        teach(&store, "play something to focus to", &artist("Brian Eno"));
        // Page and app tools are authorized by strict parsers already.
        teach(
            &store,
            "pull up Rome on Wikipedia",
            &ToolCall {
                id: "w".into(),
                name: "open_webpage".into(),
                arguments: json!({"site":"wikipedia","topic":"Rome"}),
            },
        );
        // Conditions and negations are never templated.
        teach(
            &store,
            "if it's late put on some Metallica",
            &artist("Metallica"),
        );
        teach(&store, "don't put on some Metallica", &artist("Metallica"));
    }
    assert!(store.skills().is_empty(), "{:?}", store.skills());
    assert_eq!(store.stats().model_routed, 12);
}

#[test]
fn restricted_requests_never_match_a_learned_skill() {
    let dir = TempDir::new();
    let store = store(&dir);
    teach(&store, "crank up some Metallica", &artist("Metallica"));
    teach(&store, "crank up some Bach", &artist("Bach"));
    for request in [
        "don't crank up some Bach",
        "crank up some Bach if I ask",
        "crank up some Bach then pause it",
    ] {
        assert!(store.match_request(request).is_none(), "{request}");
    }
}

#[test]
fn failures_demote_a_skill_and_stats_compare_routes() {
    let dir = TempDir::new();
    let store = store(&dir);
    teach(&store, "crank up some Metallica", &artist("Metallica"));
    teach(&store, "crank up some Bach", &artist("Bach"));
    let key = store.match_request("crank up some Muse").unwrap().key;
    store.report(&key, true, Duration::from_millis(400));
    assert_eq!(store.stats().average_skill_ms(), Some(400));
    assert_eq!(store.stats().average_model_ms(), Some(2400));
    // 3 verified successes: one failure keeps it, a second demotes it.
    store.report(&key, false, Duration::from_millis(100));
    assert!(store.match_request("crank up some Muse").is_some());
    store.report(&key, false, Duration::from_millis(100));
    assert!(
        store.match_request("crank up some Muse").is_none(),
        "a failing skill stayed active: {:?}",
        store.skills()
    );
    assert_eq!(store.stats().skill_failures, 2);
}

#[test]
fn corrupt_or_oversized_memory_starts_fresh() {
    let dir = TempDir::new();
    std::fs::write(dir.join("experience.json"), b"{broken").unwrap();
    let store = store(&dir);
    assert!(store.skills().is_empty());
    teach(&store, "crank up some Metallica", &artist("Metallica"));
    assert_eq!(store.skills().len(), 1);
    assert!(store.match_request(&"crank up some ".repeat(100)).is_none());
}

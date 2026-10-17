//! Adversarial tests use a real on-disk SQLite database, not a mocked index.
mod support;

use jarvis::context::{
    Actor, ContextError, ContextStore, Limits, MAX_CONTENT_BYTES, MAX_QUERY_BYTES, MAX_RESULTS,
    ObservationResult, Source,
};
use rusqlite::Connection;
use support::TempDir;

fn store(directory: &TempDir, limits: Limits) -> ContextStore {
    // Also exercise the shared support accessor to keep test targets warning-free.
    assert!(directory.path().is_dir());
    ContextStore::open(&directory.join("context.db"), limits).unwrap()
}

fn changed(result: ObservationResult, expected_version: i64) -> Actor {
    match result {
        ObservationResult::Changed { version, actor } => {
            assert_eq!(version, expected_version);
            actor
        }
        other => panic!("expected changed observation, got {other:?}"),
    }
}

fn candidate(job: &str, action: &str) -> Actor {
    Actor::AgentCandidate {
        job_id: job.to_owned(),
        action_id: action.to_owned(),
    }
}

#[test]
fn persisted_resource_and_fts_survive_reopen_and_replace_old_content() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    assert_eq!(
        changed(
            context
                .observe("file:spec", "obsolete kiwi", Source::File, 10)
                .unwrap(),
            1
        ),
        Actor::Unknown
    );
    context
        .observe("file:spec", "current mango", Source::File, 11)
        .unwrap();
    context
        .observe("tab:1", "browser banana", Source::Browser, 12)
        .unwrap();
    drop(context);

    let reopened = store(&directory, Limits::default());
    let resource = reopened.get("file:spec").unwrap().unwrap();
    assert_eq!(resource.version, 2);
    assert_eq!(resource.content, "current mango");
    assert_eq!(resource.source, Source::File);
    assert!(reopened.search("kiwi", 10).unwrap().is_empty());
    let results = reopened.search("mango", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].version, 2);
    assert_eq!(
        reopened.search("banana", 10).unwrap()[0].source,
        Source::Browser
    );
    assert_eq!(reopened.recent(10).unwrap().len(), 3);
}

#[test]
fn unchanged_observation_refreshes_freshness_without_new_event_or_version() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    context
        .observe("file:a", "unchanged content", Source::File, 10)
        .unwrap();
    assert_eq!(
        context
            .observe("file:a", "unchanged content", Source::File, 20)
            .unwrap(),
        ObservationResult::Unchanged { version: 1 }
    );
    let resource = context.get("file:a").unwrap().unwrap();
    assert_eq!(resource.observed_at_ms, 20);
    assert_eq!(context.recent(10).unwrap().len(), 1);
    assert_eq!(context.search("unchanged", 10).unwrap().len(), 1);
    assert_eq!(
        context
            .observe("file:a", "late corrupted value", Source::File, 19)
            .unwrap(),
        ObservationResult::IgnoredOlder
    );
    assert_eq!(
        context.get("file:a").unwrap().unwrap().content,
        "unchanged content"
    );
    assert!(context.search("corrupted", 10).unwrap().is_empty());
}

#[test]
fn resource_quota_rejects_new_content_atomically_but_allows_existing_updates() {
    let directory = TempDir::new();
    let limits = Limits {
        max_resources: 1,
        max_events: 4,
        max_actions: 2,
    };
    let mut context = store(&directory, limits);
    context
        .observe("a", "retained pear", Source::File, 10)
        .unwrap();
    assert!(matches!(
        context.observe("b", "forbidden melon", Source::Browser, 11),
        Err(ContextError::Capacity)
    ));
    assert!(context.get("b").unwrap().is_none());
    assert!(context.search("melon", 10).unwrap().is_empty());
    assert_eq!(context.recent(10).unwrap().len(), 1);
    changed(
        context
            .observe("a", "updated pear", Source::File, 12)
            .unwrap(),
        2,
    );
    assert_eq!(context.search("updated", 10).unwrap().len(), 1);
}

#[test]
fn oversized_or_invalid_observations_do_not_mutate_resource_or_index() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    context
        .observe("a", "original plum", Source::File, 10)
        .unwrap();
    for (id, content, timestamp) in [
        ("a".to_owned(), "x".repeat(MAX_CONTENT_BYTES + 1), 11),
        ("a".to_owned(), "replacement".to_owned(), -1),
        ("".to_owned(), "replacement".to_owned(), 11),
        ("a".repeat(2049), "replacement".to_owned(), 11),
    ] {
        assert!(matches!(
            context.observe(&id, &content, Source::File, timestamp),
            Err(ContextError::InvalidInput)
        ));
    }
    let current = context.get("a").unwrap().unwrap();
    assert_eq!(current.version, 1);
    assert_eq!(current.content, "original plum");
    assert_eq!(current.observed_at_ms, 10);
    assert_eq!(context.recent(10).unwrap().len(), 1);
    assert_eq!(context.search("plum", 10).unwrap().len(), 1);
    assert!(context.search("replacement", 10).unwrap().is_empty());
}

#[test]
fn event_retention_is_bounded_metadata_and_keeps_newest_changes() {
    let directory = TempDir::new();
    let mut context = store(
        &directory,
        Limits {
            max_resources: 2,
            max_events: 2,
            max_actions: 2,
        },
    );
    for version in 1..=5 {
        context
            .observe(
                "a",
                &format!("sensitive-content-{version}"),
                Source::File,
                version,
            )
            .unwrap();
    }
    let events = context.recent(MAX_RESULTS).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events.iter().map(|event| event.version).collect::<Vec<_>>(),
        vec![5, 4]
    );
    assert!(events.iter().all(|event| event.actor == Actor::Unknown));
    let connection = Connection::open(directory.join("context.db")).unwrap();
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(events)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!columns.iter().any(|column| column == "content"));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn search_is_capped_and_quotes_fts_operators_instead_of_executing_them() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    for id in 0..40 {
        context
            .observe(&format!("file:{id}"), "common fruit", Source::File, id)
            .unwrap();
    }
    assert_eq!(
        context.search("common", MAX_RESULTS).unwrap().len(),
        MAX_RESULTS
    );
    for (query, limit) in [
        ("common".to_owned(), 0),
        ("common".to_owned(), MAX_RESULTS + 1),
        ("x".repeat(MAX_QUERY_BYTES + 1), 1),
        ("   ".to_owned(), 1),
    ] {
        assert!(matches!(
            context.search(&query, limit),
            Err(ContextError::InvalidInput)
        ));
    }
    for query in [
        "\" OR common",
        "common OR missing",
        "common*",
        "content:common",
        "\"; DROP TABLE resources; --",
        "NEAR(common missing)",
    ] {
        // A lexical query may legitimately match tokens (e.g. common*), but it
        // must not become unrestricted SQL or fail as malformed FTS syntax.
        assert!(
            context.search(query, MAX_RESULTS).is_ok(),
            "query rejected as syntax: {query}"
        );
    }
    assert!(
        context
            .search("common OR missing", MAX_RESULTS)
            .unwrap()
            .is_empty()
    );
    assert!(context.get("file:0").unwrap().is_some());
    assert!(context.recent(0).is_err());
    assert!(context.recent(MAX_RESULTS + 1).is_err());
}

#[test]
fn expected_effect_is_exact_scoped_and_one_use() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    context
        .expect_action("action", "job", "target", "expected", 10, 20)
        .unwrap();
    assert_eq!(
        changed(
            context
                .observe("other", "expected", Source::File, 11)
                .unwrap(),
            1
        ),
        Actor::Unknown
    );
    assert_eq!(
        changed(
            context
                .observe("target", "expected", Source::File, 12)
                .unwrap(),
            1
        ),
        candidate("job", "action")
    );
    assert_eq!(
        changed(
            context
                .observe("target", "external edit", Source::File, 13)
                .unwrap(),
            2
        ),
        Actor::Unknown
    );
    assert_eq!(
        changed(
            context
                .observe("target", "expected", Source::File, 14)
                .unwrap(),
            3
        ),
        Actor::Unknown
    );
}

#[test]
fn expired_and_ambiguous_expected_effects_never_claim_actor() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    context
        .expect_action("expired", "job", "a", "value", 1, 5)
        .unwrap();
    assert_eq!(
        changed(context.observe("a", "value", Source::File, 6).unwrap(), 1),
        Actor::Unknown
    );
    for action in ["first", "second"] {
        context
            .expect_action(action, "job", "b", "same", 10, 20)
            .unwrap();
    }
    assert_eq!(
        changed(
            context.observe("b", "same", Source::Browser, 11).unwrap(),
            1
        ),
        Actor::Unknown
    );
    context
        .observe("b", "different", Source::Browser, 12)
        .unwrap();
    assert_eq!(
        changed(
            context.observe("b", "same", Source::Browser, 13).unwrap(),
            3
        ),
        Actor::Unknown
    );
}

#[test]
fn expected_action_quota_is_bounded_and_expiry_frees_capacity() {
    let directory = TempDir::new();
    let mut context = store(
        &directory,
        Limits {
            max_resources: 2,
            max_events: 4,
            max_actions: 1,
        },
    );
    context
        .expect_action("first", "job", "a", "one", 1, 5)
        .unwrap();
    assert!(matches!(
        context.expect_action("second", "job", "b", "two", 2, 6),
        Err(ContextError::Capacity)
    ));
    context
        .expect_action("third", "job", "b", "three", 6, 10)
        .unwrap();
    assert_eq!(
        changed(context.observe("a", "one", Source::File, 7).unwrap(), 1),
        Actor::Unknown
    );
    assert_eq!(
        changed(context.observe("b", "three", Source::File, 8).unwrap(), 1),
        candidate("job", "third")
    );
    context
        .expect_action("fourth", "job", "a", "four", 9, 12)
        .unwrap();
}

#[test]
fn no_op_effect_is_consumed_and_does_not_claim_a_later_external_edit() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    context
        .observe("target", "already there", Source::File, 1)
        .unwrap();
    context
        .expect_action("noop", "job", "target", "already there", 2, 20)
        .unwrap();
    assert_eq!(
        context
            .observe("target", "already there", Source::File, 3)
            .unwrap(),
        ObservationResult::Unchanged { version: 1 }
    );
    context
        .observe("target", "human edit", Source::File, 4)
        .unwrap();
    assert_eq!(
        changed(
            context
                .observe("target", "already there", Source::File, 5)
                .unwrap(),
            3
        ),
        Actor::Unknown
    );
}

#[test]
fn unexpected_target_edit_invalidates_old_expected_effect() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    context
        .observe("target", "initial", Source::File, 1)
        .unwrap();
    context
        .expect_action("stale", "job", "target", "planned output", 2, 20)
        .unwrap();
    assert_eq!(
        changed(
            context
                .observe("target", "intervening human edit", Source::File, 3)
                .unwrap(),
            2
        ),
        Actor::Unknown
    );
    assert_eq!(
        changed(
            context
                .observe("target", "planned output", Source::File, 4)
                .unwrap(),
            3
        ),
        Actor::Unknown
    );
}

#[test]
fn invalid_limits_fail_before_creating_a_database() {
    let directory = TempDir::new();
    for limits in [
        Limits {
            max_resources: 0,
            ..Limits::default()
        },
        Limits {
            max_events: 100001,
            ..Limits::default()
        },
        Limits {
            max_actions: 1025,
            ..Limits::default()
        },
    ] {
        assert!(matches!(
            ContextStore::open(&directory.join("invalid.db"), limits),
            Err(ContextError::InvalidInput)
        ));
        assert!(!directory.join("invalid.db").exists());
    }
}

#[test]
fn rejected_source_switch_preserves_evidence_and_pending_action() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    context
        .observe("same-id", "original", Source::File, 1)
        .unwrap();
    context
        .expect_action("pending", "job", "same-id", "expected", 2, 10)
        .unwrap();
    for content in ["original", "expected"] {
        assert!(matches!(
            context.observe("same-id", content, Source::Browser, 3),
            Err(ContextError::InvalidInput)
        ));
    }
    let original = context.get("same-id").unwrap().unwrap();
    assert_eq!(original.content, "original");
    assert_eq!(original.source, Source::File);
    assert_eq!(original.version, 1);
    assert_eq!(context.recent(10).unwrap().len(), 1);
    assert_eq!(
        changed(
            context
                .observe("same-id", "expected", Source::File, 4)
                .unwrap(),
            2
        ),
        candidate("job", "pending")
    );
}

#[test]
fn old_observations_and_pre_action_changes_do_not_consume_future_candidates() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    context.observe("a", "original", Source::File, 10).unwrap();
    context
        .expect_action("future", "job", "a", "expected", 20, 30)
        .unwrap();
    assert_eq!(
        context.observe("a", "expected", Source::File, 9).unwrap(),
        ObservationResult::IgnoredOlder
    );
    assert_eq!(
        context.forget("a", 9).unwrap(),
        Some(ObservationResult::IgnoredOlder)
    );
    assert_eq!(
        changed(
            context
                .observe("a", "before action", Source::File, 15)
                .unwrap(),
            2
        ),
        Actor::Unknown
    );
    assert_eq!(
        changed(
            context.observe("a", "expected", Source::File, 21).unwrap(),
            3
        ),
        candidate("job", "future")
    );
}

#[test]
fn deletion_tombstone_survives_reopen_and_recreation_preserves_version() {
    let directory = TempDir::new();
    let limits = Limits {
        max_resources: 1,
        max_events: 8,
        max_actions: 2,
    };
    let mut context = store(&directory, limits);
    context
        .observe("a", "private grapefruit", Source::File, 1)
        .unwrap();
    context
        .expect_action("before-delete", "job", "a", "restored", 2, 10)
        .unwrap();
    assert_eq!(context.forget("unknown", 2).unwrap(), None);
    assert_eq!(
        context.forget("a", 3).unwrap(),
        Some(ObservationResult::Changed {
            version: 2,
            actor: Actor::Unknown
        })
    );
    assert!(context.get("a").unwrap().is_none());
    assert!(context.search("grapefruit", 10).unwrap().is_empty());
    assert_eq!(context.recent(10).unwrap()[0].kind, "deleted");
    assert_eq!(
        context.forget("a", 4).unwrap(),
        Some(ObservationResult::Unchanged { version: 2 })
    );
    assert_eq!(context.recent(10).unwrap().len(), 2);
    drop(context);

    let mut reopened = store(&directory, limits);
    assert!(reopened.get("a").unwrap().is_none());
    assert!(matches!(
        reopened.observe("other", "new resource", Source::File, 5),
        Err(ContextError::Capacity)
    ));
    assert_eq!(
        reopened
            .observe("a", "stale resurrection", Source::File, 3)
            .unwrap(),
        ObservationResult::IgnoredOlder
    );
    assert!(matches!(
        reopened.observe("a", "source spoof", Source::Browser, 5),
        Err(ContextError::InvalidInput)
    ));
    assert_eq!(
        changed(
            reopened.observe("a", "restored", Source::File, 6).unwrap(),
            3
        ),
        Actor::Unknown
    );
    assert_eq!(reopened.search("restored", 10).unwrap()[0].version, 3);
    assert!(reopened.search("grapefruit", 10).unwrap().is_empty());
}

#[test]
fn root_enumeration_uses_literal_component_boundary_and_only_file_sources() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    for (id, source) in [
        ("file:/approved/report.md", Source::File),
        ("file:/approved/nested/note.txt", Source::File),
        ("file:/approved-sibling/private.md", Source::File),
        ("file:/approved", Source::File),
        ("file:/approved/browser-spoof", Source::Browser),
        ("file:/special%_/literal.md", Source::File),
        ("file:/specialXX/no.md", Source::File),
    ] {
        context.observe(id, "text", source, 1).unwrap();
    }
    assert_eq!(
        context
            .file_ids_under(std::path::Path::new("/approved"))
            .unwrap(),
        vec!["file:/approved/nested/note.txt", "file:/approved/report.md"]
    );
    assert_eq!(
        context
            .file_ids_under(std::path::Path::new("/approved/"))
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        context
            .file_ids_under(std::path::Path::new("/special%_"))
            .unwrap(),
        vec!["file:/special%_/literal.md"]
    );
    assert!(matches!(
        context.file_ids_under(std::path::Path::new("relative")),
        Err(ContextError::InvalidInput)
    ));
    context.forget("file:/approved/report.md", 2).unwrap();
    assert_eq!(
        context
            .file_ids_under(std::path::Path::new("/approved"))
            .unwrap(),
        vec!["file:/approved/nested/note.txt"]
    );
}

#[test]
fn enumeration_overflow_is_explicit_instead_of_silently_omitting_files() {
    let directory = TempDir::new();
    let mut context = store(&directory, Limits::default());
    for index in 0..1024 {
        context
            .observe(
                &format!("file:/approved/{index}.txt"),
                "text",
                Source::File,
                index,
            )
            .unwrap();
    }
    assert_eq!(
        context
            .file_ids_under(std::path::Path::new("/approved"))
            .unwrap()
            .len(),
        1024
    );
    context
        .observe("file:/approved/extra.txt", "text", Source::File, 1025)
        .unwrap();
    assert!(matches!(
        context.file_ids_under(std::path::Path::new("/approved")),
        Err(ContextError::Capacity)
    ));
}

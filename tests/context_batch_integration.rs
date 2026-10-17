mod support;

use jarvis::context::{Actor, ContextError, ContextStore, Limits, ObservationResult, Source};
use support::TempDir;

#[test]
fn second_resource_capacity_failure_rolls_back_content_fts_history_and_action_consumption() {
    let directory = TempDir::new();
    assert!(directory.path().is_dir());
    let mut store = ContextStore::open(
        &directory.join("batch.db"),
        Limits {
            max_resources: 1,
            max_events: 4,
            max_actions: 2,
        },
    )
    .unwrap();
    store
        .expect_action("expected", "job", "page", "unique mango", 1, 20)
        .unwrap();
    assert!(matches!(
        store.observe_many(&[
            ("page", "unique mango", Source::Browser, 2),
            ("metadata", "title", Source::Browser, 2),
        ]),
        Err(ContextError::Capacity)
    ));
    assert!(store.get("page").unwrap().is_none());
    assert!(store.get("metadata").unwrap().is_none());
    assert!(store.search("mango", 10).unwrap().is_empty());
    assert!(store.recent(10).unwrap().is_empty());
    assert_eq!(
        store
            .observe("page", "unique mango", Source::Browser, 3)
            .unwrap(),
        ObservationResult::Changed {
            version: 1,
            actor: Actor::AgentCandidate {
                job_id: "job".to_owned(),
                action_id: "expected".to_owned()
            }
        }
    );
}

#[test]
fn failed_batch_rolls_back_existing_versions_and_event_retention() {
    let directory = TempDir::new();
    let mut store = ContextStore::open(
        &directory.join("batch.db"),
        Limits {
            max_resources: 2,
            max_events: 1,
            max_actions: 2,
        },
    )
    .unwrap();
    store
        .observe("existing", "original pear", Source::File, 1)
        .unwrap();
    assert!(matches!(
        store.observe_many(&[
            ("existing", "replacement banana", Source::File, 2),
            ("existing", "forged browser identity", Source::Browser, 3),
        ]),
        Err(ContextError::InvalidInput)
    ));
    let original = store.get("existing").unwrap().unwrap();
    assert_eq!(original.version, 1);
    assert_eq!(original.content, "original pear");
    assert_eq!(original.observed_at_ms, 1);
    assert!(store.search("banana", 10).unwrap().is_empty());
    assert_eq!(store.search("pear", 10).unwrap().len(), 1);
    let history = store.recent(10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].version, 1);
}

#[test]
fn successful_batch_is_durable_and_batch_size_is_bounded_before_mutation() {
    let directory = TempDir::new();
    let path = directory.join("batch.db");
    let mut store = ContextStore::open(&path, Limits::default()).unwrap();
    assert!(matches!(
        store.observe_many(&[]),
        Err(ContextError::InvalidInput)
    ));
    assert!(matches!(
        store.observe_many(&[("oversized", "not committed", Source::File, 1); 9]),
        Err(ContextError::InvalidInput)
    ));
    assert!(store.get("oversized").unwrap().is_none());
    let results = store
        .observe_many(&[
            ("page", "page apricot", Source::Browser, 2),
            ("metadata", "page title", Source::Browser, 2),
        ])
        .unwrap();
    assert_eq!(results.len(), 2);
    drop(store);
    let reopened = ContextStore::open(&path, Limits::default()).unwrap();
    assert_eq!(reopened.search("apricot", 10).unwrap()[0].id, "page");
    assert_eq!(reopened.get("metadata").unwrap().unwrap().version, 1);
    assert_eq!(reopened.recent(10).unwrap().len(), 2);
}

//! Real independent SQLite connections reproduce the collector contention path.
mod support;
use jarvis::context::{ContextError, ContextStore, Limits, Source};
use std::{
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};
use support::TempDir;

#[test]
fn independent_writers_reserve_before_reading_and_keep_atomic_batches() {
    let directory = TempDir::new();
    assert!(directory.path().is_dir());
    let db = directory.join("context.sqlite");
    ContextStore::open(&db, Limits::default()).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|writer| {
            let db = db.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut store = ContextStore::open(&db, Limits::default()).unwrap();
                let id = format!("writer:{writer}");
                let peer = format!("peer:{writer}");
                barrier.wait();
                for version in 1..=400 {
                    let content = format!("version {version}");
                    if writer == 0 {
                        store
                            .observe_many(&[
                                (&id, &content, Source::File, version),
                                (&peer, &content, Source::Accessibility, version),
                            ])
                            .unwrap();
                    } else {
                        store
                            .observe(&id, &content, Source::Browser, version)
                            .unwrap();
                    }
                }
            })
        })
        .collect();
    barrier.wait();
    for handle in handles {
        handle.join().unwrap();
    }
    let store = ContextStore::open(&db, Limits::default()).unwrap();
    for id in ["writer:0", "peer:0", "writer:1"] {
        let row = store.get(id).unwrap().unwrap();
        assert_eq!(row.version, 400);
        assert_eq!(row.content, "version 400");
    }
}

#[test]
fn failed_nested_batch_rolls_back_and_releases_the_writer_for_next_connection() {
    let directory = TempDir::new();
    let db = directory.join("context.sqlite");
    let mut first = ContextStore::open(&db, Limits::default()).unwrap();
    let mut second = ContextStore::open(&db, Limits::default()).unwrap();
    first
        .observe("immutable", "browser", Source::Browser, 1)
        .unwrap();
    assert!(matches!(
        first.observe_many(&[
            ("must-rollback", "new", Source::File, 2),
            ("immutable", "wrong-source", Source::File, 2)
        ]),
        Err(ContextError::InvalidInput)
    ));
    assert!(second.get("must-rollback").unwrap().is_none());
    second
        .observe("next", "committed", Source::File, 3)
        .unwrap();
    assert_eq!(first.get("next").unwrap().unwrap().version, 1);
}

#[test]
fn writer_waits_for_reservation_while_reads_remain_available() {
    let directory = TempDir::new();
    let db = directory.join("context.sqlite");
    let mut store = ContextStore::open(&db, Limits::default()).unwrap();
    store
        .observe("existing", "readable", Source::File, 1)
        .unwrap();
    let blocker = rusqlite::Connection::open(&db).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(store.get("existing").unwrap().unwrap().content, "readable");
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (complete_tx, complete_rx) = mpsc::sync_channel(1);
    let writer = thread::spawn(move || {
        ready_tx.send(()).unwrap();
        store.observe("new", "write", Source::File, 2).unwrap();
        complete_tx.send(()).unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        complete_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    blocker.execute_batch("COMMIT").unwrap();
    complete_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    writer.join().unwrap();
    let store = ContextStore::open(&db, Limits::default()).unwrap();
    assert_eq!(store.get("existing").unwrap().unwrap().content, "readable");
    assert_eq!(store.get("new").unwrap().unwrap().content, "write");
}

#[cfg(unix)]
#[test]
fn live_file_watcher_and_foreground_writer_share_database_without_stopping() {
    use jarvis::awareness::watch_root_with_ready;
    let directory = TempDir::new();
    let root = directory.join("approved");
    std::fs::create_dir(&root).unwrap();
    for n in 0..80 {
        std::fs::write(root.join(format!("file-{n}.md")), "initial context").unwrap();
    }
    let db = directory.join("context.sqlite");
    ContextStore::open(&db, Limits::default()).unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    let watch_db = db.clone();
    let watch_root = root.clone();
    let watcher = thread::spawn(move || {
        let mut store = ContextStore::open(&watch_db, Limits::default()).unwrap();
        watch_root_with_ready(&mut store, &watch_root, Duration::from_secs(1), || {
            tx.send(()).unwrap();
        })
        .unwrap()
    });
    let writer_db = db.clone();
    let writer = thread::spawn(move || {
        let mut store = ContextStore::open(&writer_db, Limits::default()).unwrap();
        for n in 0..300 {
            store
                .observe(
                    "os:foreground",
                    &format!("synthetic app {n}"),
                    Source::Accessibility,
                    n,
                )
                .unwrap();
            thread::sleep(Duration::from_millis(2));
        }
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    std::fs::write(root.join("file-0.md"), "updated handoff").unwrap();
    let report = watcher.join().unwrap();
    writer.join().unwrap();
    assert!(report.resources_changed >= 80);
    let store = ContextStore::open(&db, Limits::default()).unwrap();
    assert_eq!(store.search("updated handoff", 4).unwrap().len(), 1);
    assert_eq!(store.get("os:foreground").unwrap().unwrap().version, 300);
}

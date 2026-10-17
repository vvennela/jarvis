#![cfg(unix)]
//! Real temporary files and the platform watcher; no synthetic collector events.
mod support;

use jarvis::{
    awareness::{
        AwarenessError, MAX_DEPTH, MAX_FILES, index_root, watch_root, watch_root_with_ready,
    },
    context::{Actor, ContextStore, Limits, MAX_CONTENT_BYTES, Source},
};
use std::{fs, os::unix::fs::symlink, path::Path, sync::mpsc, thread, time::Duration};
use support::TempDir;

fn setup(directory: &TempDir) -> (std::path::PathBuf, ContextStore) {
    let root = directory.join("approved");
    fs::create_dir(&root).unwrap();
    let context = ContextStore::open(&directory.join("context.db"), Limits::default()).unwrap();
    assert!(directory.path().is_dir());
    (root, context)
}

fn file_id(path: &Path) -> String {
    format!("file:{}", fs::canonicalize(path).unwrap().display())
}

#[test]
fn initial_scan_indexes_supported_text_with_unknown_actor_only() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    fs::write(root.join("requirements.md"), "mango specification").unwrap();
    fs::write(root.join("NOTES.TXT"), "banana decisions").unwrap();
    fs::write(root.join("ignored.json"), "secret papaya").unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("nested/accepted.txt"), "nested apricot").unwrap();
    let report = index_root(&mut context, &root).unwrap();
    assert_eq!(report.resources_changed, 3);
    assert_eq!(context.search("mango", 10).unwrap()[0].source, Source::File);
    assert_eq!(context.search("apricot", 10).unwrap().len(), 1);
    assert!(context.search("papaya", 10).unwrap().is_empty());
    assert!(
        context
            .recent(10)
            .unwrap()
            .iter()
            .all(|event| event.actor == Actor::Unknown)
    );
}

#[test]
fn rescans_do_not_duplicate_versions_and_reconcile_restart_deletions() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    let path = root.join("note.md");
    fs::write(&path, "original kiwi").unwrap();
    let id = file_id(&path);
    index_root(&mut context, &root).unwrap();
    let same = index_root(&mut context, &root).unwrap();
    assert_eq!(same.resources_changed, 0);
    assert_eq!(context.get(&id).unwrap().unwrap().version, 1);
    assert_eq!(context.recent(10).unwrap().len(), 1);
    fs::write(&path, "revised pear").unwrap();
    index_root(&mut context, &root).unwrap();
    assert_eq!(context.get(&id).unwrap().unwrap().version, 2);
    assert!(context.search("kiwi", 10).unwrap().is_empty());
    drop(context);
    fs::remove_file(&path).unwrap();
    let mut reopened =
        ContextStore::open(&directory.join("context.db"), Limits::default()).unwrap();
    let removed = index_root(&mut reopened, &root).unwrap();
    assert_eq!(removed.resources_removed, 1);
    assert!(reopened.get(&id).unwrap().is_none());
    assert!(reopened.search("pear", 10).unwrap().is_empty());
    fs::write(&path, "restored grape").unwrap();
    index_root(&mut reopened, &root).unwrap();
    assert_eq!(reopened.get(&id).unwrap().unwrap().version, 4);
}

#[test]
fn hidden_invalid_utf8_and_oversized_files_are_not_ingested() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    fs::write(root.join(".hidden.txt"), "hidden-secret-marker").unwrap();
    fs::create_dir(root.join(".private")).unwrap();
    fs::write(root.join(".private/token.md"), "nested-secret-marker").unwrap();
    fs::write(root.join("binary.txt"), [0xff, 0xfe, 0xfd]).unwrap();
    fs::write(
        root.join("too-large.txt"),
        "x".repeat(MAX_CONTENT_BYTES + 1),
    )
    .unwrap();
    fs::write(root.join("boundary.txt"), "b".repeat(MAX_CONTENT_BYTES)).unwrap();
    let report = index_root(&mut context, &root).unwrap();
    assert_eq!(report.resources_changed, 1);
    assert!(report.skipped >= 4);
    assert!(context.search("secret", 10).unwrap().is_empty());
    assert!(
        context
            .get(&file_id(&root.join("binary.txt")))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        context
            .get(&file_id(&root.join("boundary.txt")))
            .unwrap()
            .unwrap()
            .content
            .len(),
        MAX_CONTENT_BYTES
    );
}

#[test]
fn symlink_roots_files_and_intermediate_directories_do_not_escape() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    let external = directory.join("outside");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("secret.txt"), "outside-secret-marker").unwrap();
    symlink(external.join("secret.txt"), root.join("alias.txt")).unwrap();
    symlink(&external, root.join("linked-dir")).unwrap();
    let report = index_root(&mut context, &root).unwrap();
    assert_eq!(report.resources_changed, 0);
    assert!(context.search("outside", 10).unwrap().is_empty());
    let root_alias = directory.join("root-alias");
    symlink(&root, &root_alias).unwrap();
    assert!(matches!(
        index_root(&mut context, &root_alias),
        Err(AwarenessError::InvalidRoot)
    ));
    assert!(matches!(
        index_root(&mut context, &external.join("secret.txt")),
        Err(AwarenessError::InvalidRoot)
    ));
}

#[test]
fn replacing_previously_indexed_file_with_symlink_removes_searchable_old_evidence() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    let target = root.join("note.md");
    fs::write(&target, "formerly visible").unwrap();
    let id = file_id(&target);
    index_root(&mut context, &root).unwrap();
    let outside = directory.join("outside.md");
    fs::write(&outside, "outside credentials").unwrap();
    fs::remove_file(&target).unwrap();
    symlink(&outside, &target).unwrap();
    let report = index_root(&mut context, &root).unwrap();
    assert_eq!(report.resources_removed, 1);
    assert!(context.get(&id).unwrap().is_none());
    assert!(context.search("credentials", 10).unwrap().is_empty());
}

#[test]
fn depth_truncation_is_explicit_and_does_not_invent_deletions() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    let old_path = root.join("previous.md");
    fs::write(&old_path, "preserved on incomplete scan").unwrap();
    let old_id = file_id(&old_path);
    index_root(&mut context, &root).unwrap();
    fs::remove_file(&old_path).unwrap();
    let mut nested = root.clone();
    for depth in 0..=MAX_DEPTH {
        nested.push(format!("level-{depth}"));
        fs::create_dir(&nested).unwrap();
    }
    fs::write(nested.join("beyond.txt"), "beyond depth marker").unwrap();
    let report = index_root(&mut context, &root).unwrap();
    assert!(report.truncated);
    assert_eq!(report.resources_removed, 0);
    assert!(context.get(&old_id).unwrap().is_some());
    assert!(context.search("beyond", 10).unwrap().is_empty());
}

#[test]
fn file_count_is_bounded_and_reported_as_incomplete() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    for id in 0..=MAX_FILES {
        fs::write(root.join(format!("{id}.txt")), "bounded evidence").unwrap();
    }
    let report = index_root(&mut context, &root).unwrap();
    assert_eq!(report.resources_changed, MAX_FILES);
    assert!(report.truncated);
}

#[test]
fn bounded_platform_watch_observes_a_real_edit_and_keeps_version_stable_afterward() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    let path = root.join("live.md");
    fs::write(&path, "before editing").unwrap();
    let id = file_id(&path);
    let (ready_send, ready_recv) = mpsc::channel();
    let writer = thread::spawn(move || {
        ready_recv.recv_timeout(Duration::from_secs(10)).unwrap();
        fs::write(path, "after live edit").unwrap();
    });
    let report = watch_root_with_ready(&mut context, &root, Duration::from_secs(2), || {
        ready_send.send(()).unwrap();
    })
    .unwrap();
    writer.join().unwrap();
    assert_eq!(
        context.get(&id).unwrap().unwrap().content,
        "after live edit"
    );
    assert_eq!(context.get(&id).unwrap().unwrap().version, 2);
    assert_eq!(report.resources_changed, 2);
    assert!(report.resources_unchanged > 0);
    assert!(report.reconciliations >= 3);
}

#[test]
fn changed_root_identity_stops_collection_without_following_replacement() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    fs::write(root.join("initial.md"), "initial allowed evidence").unwrap();
    let replacement_root = root.clone();
    let old_root = directory.join("moved-root");
    let (ready_send, ready_recv) = mpsc::channel();
    let writer = thread::spawn(move || {
        ready_recv.recv_timeout(Duration::from_secs(10)).unwrap();
        fs::rename(&replacement_root, old_root).unwrap();
        fs::create_dir(&replacement_root).unwrap();
        fs::write(
            replacement_root.join("secret.md"),
            "replacement secret marker",
        )
        .unwrap();
    });
    let result = watch_root_with_ready(&mut context, &root, Duration::from_secs(2), || {
        ready_send.send(()).unwrap();
    });
    writer.join().unwrap();
    assert!(matches!(result, Err(AwarenessError::RootChanged)));
    assert!(context.search("replacement", 10).unwrap().is_empty());
}

#[test]
fn invalid_watch_duration_is_rejected_before_collection() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    fs::write(root.join("never-read.txt"), "not ingested").unwrap();
    for duration in [Duration::ZERO, Duration::from_secs(3601)] {
        assert!(matches!(
            watch_root(&mut context, &root, duration),
            Err(AwarenessError::InvalidDuration)
        ));
    }
    assert!(context.search("ingested", 10).unwrap().is_empty());
}

#[test]
fn live_watcher_publishes_edits_and_deletion_before_shutdown() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    let file = root.join("live-delete.md");
    fs::write(&file, "initial fixture").unwrap();
    let id = file_id(&file);
    let db = directory.join("context.db");
    let worker = thread::spawn(move || watch_root(&mut context, &root, Duration::from_secs(3)));
    let observer = ContextStore::open(&db, Limits::default()).unwrap();
    let wait_until = |condition: &mut dyn FnMut() -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(
                std::time::Instant::now() < deadline,
                "watcher did not publish state while active"
            );
            thread::sleep(Duration::from_millis(10));
        }
    };
    wait_until(&mut || observer.get(&id).unwrap().is_some());
    fs::write(&file, "edited mango").unwrap();
    wait_until(&mut || {
        observer
            .get(&id)
            .unwrap()
            .is_some_and(|resource| resource.content == "edited mango")
    });
    fs::remove_file(&file).unwrap();
    wait_until(&mut || observer.get(&id).unwrap().is_none());
    let report = worker.join().unwrap().unwrap();
    assert!(report.resources_changed >= 2);
    assert_eq!(report.resources_removed, 1);
    assert!(observer.search("mango", 10).unwrap().is_empty());
}

#[test]
fn readiness_callback_is_not_called_when_root_validation_fails() {
    let directory = TempDir::new();
    let (_, mut context) = setup(&directory);
    let mut announced_ready = false;
    let result = jarvis::awareness::watch_root_with_ready(
        &mut context,
        &directory.join("missing"),
        Duration::from_millis(50),
        || announced_ready = true,
    );
    assert!(matches!(result, Err(AwarenessError::InvalidRoot)));
    assert!(
        !announced_ready,
        "invalid root must not produce an active-awareness indicator"
    );
}

#[test]
fn controlled_watcher_announces_ready_only_after_initial_evidence_is_persisted() {
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    let file = root.join("ready.md");
    fs::write(&file, "ready mango").unwrap();
    let id = file_id(&file);
    let observer = ContextStore::open(&directory.join("context.db"), Limits::default()).unwrap();
    let ready = std::cell::Cell::new(false);
    let started = std::time::Instant::now();
    let report = jarvis::awareness::watch_root_controlled(
        &mut context,
        &root,
        Duration::from_secs(3600),
        || {
            assert_eq!(observer.get(&id).unwrap().unwrap().content, "ready mango");
            ready.set(true);
        },
        || ready.get(),
    )
    .unwrap();
    assert!(ready.get());
    assert_eq!(report.resources_changed, 1);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn controlled_idle_watcher_cancels_without_waiting_for_session_deadline() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let directory = TempDir::new();
    let (root, mut context) = setup(&directory);
    fs::write(root.join("initial.md"), "initial").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let (ready_send, ready_recv) = mpsc::channel();
    let (done_send, done_recv) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = jarvis::awareness::watch_root_controlled(
            &mut context,
            &root,
            Duration::from_secs(3600),
            || ready_send.send(()).unwrap(),
            || worker_stop.load(Ordering::Acquire),
        );
        done_send.send(result).unwrap();
    });
    ready_recv.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(done_recv.try_recv().is_err());
    stop.store(true, Ordering::Release);
    done_recv
        .recv_timeout(Duration::from_secs(2))
        .expect("pause/quit did not stop idle watcher")
        .unwrap();
    worker.join().unwrap();
}

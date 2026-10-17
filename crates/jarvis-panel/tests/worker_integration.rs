#![cfg(unix)]
use jarvis::context::{ContextStore, Limits};
use jarvis_panel::worker::{State, Worker};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "jarvis-worker-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "worker did not reach expected state within3s"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn active_means_initial_evidence_exists_and_pause_resume_controls_real_collection() {
    let dir = TempDir::new();
    let root = dir.join("approved");
    fs::create_dir(&root).unwrap();
    let file = root.join("spec.md");
    fs::write(&file, "initial mango").unwrap();
    let id = format!("file:{}", fs::canonicalize(&file).unwrap().display());
    let db = dir.join("state.db");
    let mut worker = Worker::start(root, db.clone()).unwrap();
    wait_until(|| worker.control.snapshot().state == State::Active);
    let store = ContextStore::open(&db, Limits::default()).unwrap();
    assert_eq!(store.get(&id).unwrap().unwrap().content, "initial mango");
    worker.control.pause();
    wait_until(|| worker.control.snapshot().state == State::Paused);
    fs::write(&file, "updated banana").unwrap();
    thread::sleep(Duration::from_millis(250));
    assert_eq!(
        store.get(&id).unwrap().unwrap().content,
        "initial mango",
        "paused observer still collected file changes"
    );
    worker.control.resume();
    wait_until(|| worker.control.snapshot().state == State::Active);
    assert_eq!(store.get(&id).unwrap().unwrap().content, "updated banana");
    worker.control.stop();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
    assert_eq!(worker.control.snapshot().state, State::Stopped);
    worker.control.resume();
    worker.control.pause();
    assert!(worker.control.stop_requested());
    assert_eq!(worker.control.snapshot().state, State::Stopped);
}

#[test]
fn database_failure_is_blocked_and_never_announced_active() {
    let dir = TempDir::new();
    let root = dir.join("approved");
    fs::create_dir(&root).unwrap();
    let database = dir.join("directory-not-db");
    fs::create_dir(&database).unwrap();
    let mut worker = Worker::start(root, database).unwrap();
    wait_until(|| worker.is_finished());
    assert_eq!(worker.control.snapshot().state, State::Blocked);
    assert!(worker.control.snapshot().message.contains("database"));
    worker.join().unwrap();
}

#[test]
fn quitting_while_paused_stops_without_restarting_collection() {
    let dir = TempDir::new();
    let root = dir.join("approved");
    fs::create_dir(&root).unwrap();
    let mut worker = Worker::start(root, dir.join("state.db")).unwrap();
    wait_until(|| worker.control.snapshot().state == State::Active);
    worker.control.pause();
    wait_until(|| worker.control.snapshot().state == State::Paused);
    let before = worker.control.snapshot();
    worker.control.stop();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
    let after = worker.control.snapshot();
    assert_eq!(after.state, State::Stopped);
    assert_eq!(after.reconciliations, before.reconciliations);
}

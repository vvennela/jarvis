//! Synthetic collector + real SQLite. Does not query native applications or AX.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::context::{ContextStore, Limits};
use jarvis_awareness::{AppInfo, Observation, RESOURCE_ID, Status};
use jarvis_panel::osworker::{Collector, OsWorker, State};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use support::TempDir;
#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    hold: AtomicBool,
    release: AtomicBool,
}
struct Fixture(Arc<Probe>);
impl Collector for Fixture {
    fn capture(&mut self) -> Result<Observation, &'static str> {
        self.0.calls.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.0.hold.load(Ordering::Acquire) && !self.0.release.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "test must release fixture collector"
            );
            thread::sleep(Duration::from_millis(2));
        }
        Ok(Observation {
            app: Some(AppInfo {
                pid: 12,
                bundle_id: Some("dev.fixture.Editor".into()),
                name: Some("Fixture".into()),
            }),
            window: None,
            status: Status::PermissionMissing,
        })
    }
}
fn wait(mut test: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !test() {
        assert!(Instant::now() < deadline, "worker lifecycle timeout");
        thread::sleep(Duration::from_millis(5));
    }
}
fn stop(worker: &mut OsWorker) {
    worker.control.stop();
    wait(|| worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn missing_accessibility_permission_preserves_app_awareness_and_pause_stops_polling() {
    let temp = TempDir::new();
    assert!(temp.path().is_dir());
    let probe = Arc::new(Probe::default());
    let p = probe.clone();
    let mut worker =
        OsWorker::with_collector(temp.join("os.db"), move || Ok(Box::new(Fixture(p)))).unwrap();
    wait(|| worker.control.snapshot().state == State::PermissionMissing);
    let db = ContextStore::open(&temp.join("os.db"), Limits::default()).unwrap();
    assert!(
        db.get(RESOURCE_ID)
            .unwrap()
            .unwrap()
            .content
            .contains("dev.fixture.Editor")
    );
    thread::sleep(Duration::from_millis(80));
    assert_eq!(
        probe.calls.load(Ordering::Relaxed),
        1,
        "collector must not busy-poll"
    );
    worker.control.pause();
    wait(|| worker.control.snapshot().state == State::Paused);
    let calls = probe.calls.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(probe.calls.load(Ordering::Relaxed), calls);
    worker.control.resume();
    wait(|| worker.control.snapshot().state == State::PermissionMissing);
    assert_eq!(db.get(RESOURCE_ID).unwrap().unwrap().version, 1);
    stop(&mut worker);
}

#[test]
fn pause_or_stop_during_capture_discards_late_observation_before_sqlite_write() {
    for stopping in [false, true] {
        let temp = TempDir::new();
        let probe = Arc::new(Probe::default());
        probe.hold.store(true, Ordering::Release);
        let p = probe.clone();
        let mut worker =
            OsWorker::with_collector(temp.join("os.db"), move || Ok(Box::new(Fixture(p)))).unwrap();
        wait(|| probe.calls.load(Ordering::Relaxed) == 1);
        if stopping {
            worker.control.stop();
        } else {
            worker.control.pause();
        }
        probe.release.store(true, Ordering::Release);
        wait(|| {
            matches!(
                worker.control.snapshot().state,
                State::Paused | State::Stopped
            )
        });
        let db = ContextStore::open(&temp.join("os.db"), Limits::default()).unwrap();
        assert!(db.get(RESOURCE_ID).unwrap().is_none());
        stop(&mut worker);
    }
}

#[test]
fn disabled_path_has_no_worker_and_initialization_failure_is_redacted() {
    let mut disabled = OsWorker::disabled();
    assert_eq!(disabled.control.snapshot().state, State::Disabled);
    assert_eq!(disabled.control.snapshot().polls, 0);
    assert!(disabled.is_finished());
    disabled.control.resume();
    assert!(disabled.is_finished());
    disabled.join().unwrap();
    let temp = TempDir::new();
    let mut worker =
        OsWorker::with_collector(temp.join("os.db"), || Err("private-window-title-secret"))
            .unwrap();
    wait(|| worker.is_finished());
    assert_eq!(worker.control.snapshot().state, State::Blocked);
    assert!(
        !worker
            .control
            .snapshot()
            .message
            .contains("private-window-title-secret")
    );
    assert!(!temp.join("os.db").exists());
    worker.join().unwrap();
}

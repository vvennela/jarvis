use jarvis::{
    awareness::watch_root_controlled,
    context::{ContextStore, Limits},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const RUN: u8 = 0;
const PAUSE: u8 = 1;
const STOP: u8 = 2;
const SESSION: Duration = Duration::from_secs(3600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Active,
    Pausing,
    Paused,
    Stopping,
    Stopped,
    Blocked,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: State,
    pub changes: usize,
    pub removals: usize,
    pub reconciliations: usize,
    pub message: &'static str,
}

pub struct Control {
    desired: AtomicU8,
    snapshot: Mutex<Snapshot>,
}
impl Control {
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    pub fn pause_requested(&self) -> bool {
        self.desired.load(Ordering::Acquire) == PAUSE
    }
    pub fn stop_requested(&self) -> bool {
        self.desired.load(Ordering::Acquire) == STOP
    }
    pub fn pause(&self) {
        if self
            .desired
            .compare_exchange(RUN, PAUSE, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
            if !matches!(snapshot.state, State::Blocked | State::Stopped) {
                snapshot.state = State::Pausing;
            }
        }
    }
    pub fn resume(&self) {
        if self
            .desired
            .compare_exchange(PAUSE, RUN, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
            if !matches!(snapshot.state, State::Blocked | State::Stopped) {
                snapshot.state = State::Starting;
            }
        }
    }
    pub fn stop(&self) {
        self.desired.store(STOP, Ordering::Release);
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        if !matches!(snapshot.state, State::Blocked | State::Stopped) {
            snapshot.state = State::Stopping;
        }
    }
    fn state(&self, state: State, message: &'static str) {
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        snapshot.state = state;
        snapshot.message = message;
    }
}

pub struct Worker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl Worker {
    /// One worker and one latest-state slot: no unbounded event queue or UI data.
    pub fn start(root: PathBuf, database: PathBuf) -> Result<Self, &'static str> {
        // Reject an invalid scope before creating the database or starting a UI.
        let metadata =
            std::fs::symlink_metadata(&root).map_err(|_| "Awareness root is unavailable")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Choose a real directory, not a symlink");
        }
        let root = std::fs::canonicalize(root).map_err(|_| "Awareness root is unavailable")?;
        let control = Arc::new(Control {
            desired: AtomicU8::new(RUN),
            snapshot: Mutex::new(Snapshot {
                state: State::Starting,
                changes: 0,
                removals: 0,
                reconciliations: 0,
                message: "Starting file observer",
            }),
        });
        let worker_control = Arc::clone(&control);
        let handle = thread::Builder::new()
            .name("jarvis-file-awareness".into())
            .spawn(move || {
                let mut store = match ContextStore::open(&database, Limits::default()) {
                    Ok(store) => store,
                    Err(_) => {
                        worker_control.state(State::Blocked, "Cannot open context database");
                        return;
                    }
                };
                loop {
                    match worker_control.desired.load(Ordering::Acquire) {
                        STOP => break,
                        PAUSE => {
                            worker_control.state(State::Paused, "File collection paused");
                            thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                        _ => {}
                    }
                    worker_control.state(State::Starting, "Starting file observer");
                    let result = watch_root_controlled(
                        &mut store,
                        &root,
                        SESSION,
                        || {
                            let desired = worker_control.desired.load(Ordering::Acquire);
                            worker_control.state(
                                if desired == RUN {
                                    State::Active
                                } else if desired == PAUSE {
                                    State::Pausing
                                } else {
                                    State::Stopping
                                },
                                "File observer registered",
                            );
                        },
                        || worker_control.desired.load(Ordering::Acquire) != RUN,
                    );
                    match result {
                        Ok(report) => {
                            let mut snapshot = worker_control
                                .snapshot
                                .lock()
                                .unwrap_or_else(|p| p.into_inner());
                            snapshot.changes =
                                snapshot.changes.saturating_add(report.resources_changed);
                            snapshot.removals =
                                snapshot.removals.saturating_add(report.resources_removed);
                            snapshot.reconciliations = snapshot
                                .reconciliations
                                .saturating_add(report.reconciliations);
                        }
                        Err(_) => {
                            worker_control.state(
                                State::Blocked,
                                "File observer stopped; check root and database",
                            );
                            return;
                        }
                    }
                }
                worker_control.state(State::Stopped, "File observer stopped");
            })
            .map_err(|_| "Could not start file observer")?;
        Ok(Self {
            control,
            handle: Some(handle),
        })
    }
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(|handle| handle.is_finished())
    }
    pub fn join(&mut self) -> Result<(), &'static str> {
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| "File observer terminated unexpectedly")?;
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        // No detached collector survives an early UI error or dropped handle.
        // Normal UI exit waits responsively, then calls join before reaching Drop.
        self.control.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

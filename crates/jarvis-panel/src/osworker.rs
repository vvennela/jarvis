//! One bounded native foreground observer, independent of files and inference.
use jarvis::context::{ContextStore, Limits};
use jarvis_awareness::{Observation, Status};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const RUN: u8 = 0;
const PAUSE: u8 = 1;
const STOP: u8 = 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Active,
    PermissionMissing,
    NoFocusedWindow,
    Excluded,
    Unavailable,
    Paused,
    Disabled,
    Blocked,
    Stopped,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: State,
    pub message: &'static str,
    pub polls: u64,
}
pub trait Collector {
    fn capture(&mut self) -> Result<Observation, &'static str>;
}
pub struct Control {
    desired: AtomicU8,
    snapshot: Mutex<Snapshot>,
}
impl Control {
    pub fn pause(&self) {
        let _ = self
            .desired
            .compare_exchange(RUN, PAUSE, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn resume(&self) {
        let _ = self
            .desired
            .compare_exchange(PAUSE, RUN, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn stop(&self) {
        self.desired.store(STOP, Ordering::Release);
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    fn publish(&self, state: State, message: &'static str, poll: bool) {
        let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        s.state = state;
        s.message = message;
        if poll {
            s.polls = s.polls.saturating_add(1);
        }
    }
}
pub struct OsWorker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl OsWorker {
    /// Native smoke tests deliberately use this hardware-free path.
    pub fn disabled() -> Self {
        Self {
            control: Arc::new(Control {
                desired: AtomicU8::new(STOP),
                snapshot: Mutex::new(Snapshot {
                    state: State::Disabled,
                    message: "App observer disabled for smoke test",
                    polls: 0,
                }),
            }),
            handle: None,
        }
    }
    pub fn start(database: PathBuf) -> Result<Self, &'static str> {
        Self::with_collector(database, || Ok(Box::new(NativeCollector)))
    }
    pub fn with_collector(
        database: PathBuf,
        factory: impl FnOnce() -> Result<Box<dyn Collector>, &'static str> + Send + 'static,
    ) -> Result<Self, &'static str> {
        let control = Arc::new(Control {
            desired: AtomicU8::new(RUN),
            snapshot: Mutex::new(Snapshot {
                state: State::Starting,
                message: "Starting app awareness",
                polls: 0,
            }),
        });
        let c = control.clone();
        let handle = thread::Builder::new()
            .name("jarvis-app-awareness".into())
            .spawn(move || {
                let mut collector = match factory() {
                    Ok(c) => c,
                    Err(_) => {
                        c.publish(State::Blocked, "App observer unavailable", false);
                        return;
                    }
                };
                let mut store = match ContextStore::open(&database, Limits::default()) {
                    Ok(s) => s,
                    Err(_) => {
                        c.publish(State::Blocked, "App context database unavailable", false);
                        return;
                    }
                };
                let mut last_timestamp = 0_i64;
                loop {
                    match c.desired.load(Ordering::Acquire) {
                        STOP => break,
                        PAUSE => {
                            c.publish(State::Paused, "App awareness paused", false);
                            thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        _ => {}
                    }
                    let began = Instant::now();
                    let observation = match collector.capture() {
                        Ok(o) => o,
                        Err(_) => {
                            c.publish(
                                State::Blocked,
                                "App observer stopped; check Accessibility settings",
                                false,
                            );
                            return;
                        }
                    };
                    // A pause during native capture discards the observation before storage.
                    if c.desired.load(Ordering::Acquire) != RUN {
                        continue;
                    }
                    let status = observation.status;
                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
                        .unwrap_or(0)
                        .max(last_timestamp.saturating_add(1));
                    if jarvis_awareness::publish(&mut store, observation, timestamp).is_err() {
                        c.publish(State::Blocked, "App context update unavailable", false);
                        return;
                    }
                    last_timestamp = timestamp;
                    let (state, message) = match status {
                        Status::Available => (State::Active, "Apps and focused windows observed"),
                        Status::PermissionMissing => (
                            State::PermissionMissing,
                            "Apps observed · enable Accessibility for window titles",
                        ),
                        Status::NoFocusedWindow => {
                            (State::NoFocusedWindow, "Apps observed · no focused window")
                        }
                        Status::Excluded => (
                            State::Excluded,
                            "App identity observed · private window details excluded",
                        ),
                        Status::Unavailable => (State::Unavailable, "Waiting for foreground app"),
                    };
                    c.publish(state, message, true);
                    while began.elapsed() < Duration::from_millis(250)
                        && c.desired.load(Ordering::Acquire) == RUN
                    {
                        thread::sleep(Duration::from_millis(20));
                    }
                }
                c.publish(State::Stopped, "App awareness stopped", false);
            })
            .map_err(|_| "Could not start app awareness worker")?;
        Ok(Self {
            control,
            handle: Some(handle),
        })
    }
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }
    pub fn join(&mut self) -> Result<(), &'static str> {
        if let Some(h) = self.handle.take() {
            h.join()
                .map_err(|_| "App awareness worker stopped unexpectedly")?;
        }
        Ok(())
    }
}
impl Drop for OsWorker {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
struct NativeCollector;
impl Collector for NativeCollector {
    fn capture(&mut self) -> Result<Observation, &'static str> {
        jarvis_awareness::capture().map_err(|_| "Native capture unavailable")
    }
}

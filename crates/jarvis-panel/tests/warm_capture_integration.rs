//! Owned fake capture with the real wake worker; no microphone or provider access.
use jarvis_panel::wakeworker::{State, WakeBackend, WakeWorker};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Default)]
struct Probe {
    audio: Mutex<VecDeque<Vec<f32>>>,
    starts: AtomicUsize,
    stops: AtomicUsize,
    polls: AtomicUsize,
    fast: AtomicUsize,
    command: AtomicUsize,
    fail: AtomicBool,
}
struct Backend(Arc<Probe>);
impl WakeBackend for Backend {
    fn start(&mut self) -> Result<(), &'static str> {
        self.0.starts.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
    fn stop(&mut self) {
        self.0.stops.fetch_add(1, Ordering::AcqRel);
    }
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        self.0.polls.fetch_add(1, Ordering::AcqRel);
        if self.0.fail.swap(false, Ordering::AcqRel) {
            return Err("fixture capture unavailable");
        }
        Ok((
            self.0.audio.lock().unwrap().pop_front().unwrap_or_default(),
            false,
        ))
    }
    fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
        self.0.fast.fetch_add(1, Ordering::AcqRel);
        Ok("Jarvis".into())
    }
    fn transcribe_command(
        &mut self,
        _: &[f32],
        _: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.0.command.fetch_add(1, Ordering::AcqRel);
        Ok("where was I?".into())
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < end, "worker condition deadline");
        thread::sleep(Duration::from_millis(2));
    }
}
fn start(p: &Arc<Probe>) -> WakeWorker {
    let probe = p.clone();
    let w = WakeWorker::with_backend(move || Ok(Box::new(Backend(probe)))).unwrap();
    wait(|| w.control.snapshot().state == State::Muted);
    w.control.set_enabled(true);
    wait(|| w.control.snapshot().state == State::WakeListening);
    let polls = p.polls.load(Ordering::Acquire);
    wait(|| p.polls.load(Ordering::Acquire) > polls);
    w
}
fn utterance(p: &Probe) {
    let mut q = p.audio.lock().unwrap();
    q.push_back(vec![0.1; 8000]);
    q.push_back(vec![0.; 17600]);
}
fn finish(w: &mut WakeWorker) {
    w.control.shutdown();
    wait(|| w.is_finished());
    w.join().unwrap();
}
#[test]
fn wake_decoder_and_command_decoder_have_distinct_production_dispatch() {
    let p = Arc::new(Probe::default());
    let mut w = start(&p);
    utterance(&p);
    wait(|| w.control.snapshot().state == State::CommandListening);
    assert_eq!(p.fast.load(Ordering::Acquire), 1);
    assert_eq!(p.command.load(Ordering::Acquire), 0);
    assert!(w.control.take_command().is_none());
    utterance(&p);
    wait(|| w.control.snapshot().state == State::CommandReady);
    assert_eq!(w.control.take_command().as_deref(), Some("where was I?"));
    assert_eq!(p.fast.load(Ordering::Acquire), 1);
    assert_eq!(p.command.load(Ordering::Acquire), 1);
    finish(&mut w);
}
#[test]
fn speech_suppression_drains_echo_without_reopening_capture_and_mute_still_stops() {
    let p = Arc::new(Probe::default());
    let mut w = start(&p);
    let starts = p.starts.load(Ordering::Acquire);
    let stops = p.stops.load(Ordering::Acquire);
    for _ in 0..3 {
        w.control.suppress(true);
        wait(|| w.control.snapshot().state == State::Suppressed);
        for _ in 0..4 {
            utterance(&p);
        }
        wait(|| p.audio.lock().unwrap().is_empty());
        assert_eq!(p.fast.load(Ordering::Acquire), 0);
        assert_eq!(p.command.load(Ordering::Acquire), 0);
        assert!(w.control.take_command().is_none());
        w.control.suppress(false);
        wait(|| w.control.snapshot().state == State::WakeListening);
        let polls = p.polls.load(Ordering::Acquire);
        wait(|| p.polls.load(Ordering::Acquire) > polls);
    }
    assert_eq!(p.starts.load(Ordering::Acquire), starts);
    assert_eq!(p.stops.load(Ordering::Acquire), stops);
    utterance(&p);
    wait(|| w.control.snapshot().state == State::CommandListening);
    utterance(&p);
    wait(|| w.control.snapshot().state == State::CommandReady);
    assert_eq!(w.control.take_command().as_deref(), Some("where was I?"));
    w.control.set_enabled(false);
    wait(|| w.control.snapshot().state == State::Muted);
    assert!(p.stops.load(Ordering::Acquire) > stops);
    finish(&mut w);
}
#[test]
fn runtime_capture_failure_while_suppressed_is_visible_and_shutdown_closes_owned_stream() {
    let p = Arc::new(Probe::default());
    let mut w = start(&p);
    w.control.suppress(true);
    wait(|| w.control.snapshot().state == State::Suppressed);
    let stops = p.stops.load(Ordering::Acquire);
    p.fail.store(true, Ordering::Release);
    wait(|| w.control.snapshot().state == State::Failed);
    assert!(!w.control.enabled());
    assert!(p.stops.load(Ordering::Acquire) > stops);
    assert!(w.control.take_command().is_none());
    finish(&mut w);
    let p = Arc::new(Probe::default());
    let mut w = start(&p);
    w.control.suppress(true);
    wait(|| w.control.snapshot().state == State::Suppressed);
    let stops = p.stops.load(Ordering::Acquire);
    finish(&mut w);
    assert!(p.stops.load(Ordering::Acquire) > stops);
}

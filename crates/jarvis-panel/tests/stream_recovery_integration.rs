//! Real wake worker with the production zero-stream detector and synthetic
//! timestamped PCM. Reopening is simulated; no native microphone is accessed.
use jarvis_panel::wakeworker::{KeywordStatus, State, WakeBackend, WakeWorker};
use jarvis_voice::capture::{InputInfo, SilentStreamWatchdog};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Frame {
    samples: Vec<f32>,
    at: Duration,
}
#[derive(Default)]
struct Probe {
    frames: Mutex<VecDeque<Frame>>,
    texts: Mutex<VecDeque<String>>,
    decoded: Mutex<Vec<Vec<f32>>>,
    hold: AtomicBool,
    release: AtomicBool,
    reopen: AtomicUsize,
    resets: AtomicUsize,
    runtime_failure: AtomicBool,
    reopen_failures: AtomicUsize,
    attempts: Mutex<Vec<Duration>>,
}
impl Probe {
    fn push(&self, samples: Vec<f32>, seconds: u64) {
        self.frames.lock().unwrap().push_back(Frame {
            samples,
            at: Duration::from_secs(seconds),
        });
    }
}
struct Backend {
    p: Arc<Probe>,
    watchdog: SilentStreamWatchdog,
}
impl WakeBackend for Backend {
    fn start(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn input_info(&self) -> Option<InputInfo> {
        Some(InputInfo {
            name: "Fixture headset".into(),
            sample_rate: if self.p.reopen.load(Ordering::Acquire) == 0 {
                48000
            } else {
                24000
            },
            channels: 1,
        })
    }
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        let Some(frame) = self.p.frames.lock().unwrap().pop_front() else {
            return Ok((Vec::new(), false));
        };
        if self.watchdog.observe(&frame.samples, frame.at) {
            self.p.reopen.fetch_add(1, Ordering::AcqRel);
            // Same contract as replacing an owned native capture stream: discard
            // the stale batch and mark the detector/keyword boundary invalid.
            return Ok((Vec::new(), true));
        }
        Ok((frame.samples, false))
    }
    fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
        Ok(Some(KeywordStatus {
            lookback_samples: (samples.first() == Some(&0.2)).then_some(64000),
        }))
    }
    fn reset_keyword(&mut self) {
        self.p.resets.fetch_add(1, Ordering::AcqRel);
    }
    fn transcribe(
        &mut self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.p.decoded.lock().unwrap().push(samples.to_vec());
        let end = Instant::now() + Duration::from_secs(3);
        while self.p.hold.load(Ordering::Acquire) && !self.p.release.load(Ordering::Acquire) {
            if cancelled.load(Ordering::Acquire) {
                return Err("Fixture cancelled");
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
        Ok(self.p.texts.lock().unwrap().pop_front().unwrap())
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(4);
    while !condition() {
        assert!(Instant::now() < end, "recovery fixture timed out");
        thread::sleep(Duration::from_millis(2));
    }
}
fn start(p: &Arc<Probe>) -> WakeWorker {
    let probe = p.clone();
    let worker = WakeWorker::with_backend(move || {
        Ok(Box::new(Backend {
            p: probe,
            watchdog: SilentStreamWatchdog::new(),
        }))
    })
    .unwrap();
    wait(|| worker.control.snapshot().state == State::Muted);
    worker.control.set_enabled(true);
    wait(|| worker.control.snapshot().state == State::WakeListening);
    worker
}
#[test]
fn recovery_during_decode_discards_old_utterance_and_fresh_stream_can_submit_once() {
    let p = Arc::new(Probe::default());
    p.hold.store(true, Ordering::Release);
    p.texts
        .lock()
        .unwrap()
        .extend(["Jarvis stale request".into(), "Jarvis fresh request".into()]);
    let mut worker = start(&p);
    p.push(vec![0.2; 16000], 0);
    p.push(vec![0.0; 17600], 1);
    wait(|| p.decoded.lock().unwrap().len() == 1);
    // Capture can queue a full zero stream while local decoding is busy. The
    // post-decode poll must invalidate this result before any command is exposed.
    p.push(vec![0.0; 48000], 4);
    p.release.store(true, Ordering::Release);
    wait(|| worker.control.snapshot().diagnostics.discontinuities == 1);
    assert_eq!(p.reopen.load(Ordering::Acquire), 1);
    assert!(worker.control.take_command().is_none());
    assert_eq!(worker.control.snapshot().diagnostics.commands, 0);
    assert_eq!(
        worker
            .control
            .snapshot()
            .diagnostics
            .input
            .unwrap()
            .sample_rate,
        24000
    );
    assert!(p.resets.load(Ordering::Acquire) > 0);
    p.push(vec![0.2; 16000], 5);
    p.push(vec![0.0; 17600], 6);
    wait(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("fresh request")
    );
    assert!(worker.control.take_command().is_none());
    assert_eq!(worker.control.snapshot().diagnostics.commands, 1);
    assert_eq!(p.reopen.load(Ordering::Acquire), 1);
    worker.control.shutdown();
    wait(|| worker.is_finished());
    worker.join().unwrap();
}
#[test]
fn sustained_zero_stream_recovery_is_bounded_and_never_becomes_a_voice_command() {
    let p = Arc::new(Probe::default());
    let mut worker = start(&p);
    for second in 0..31 {
        p.push(vec![0.0; 16000], second);
    }
    wait(|| p.frames.lock().unwrap().is_empty());
    assert_eq!(p.reopen.load(Ordering::Acquire), 3);
    assert!(p.decoded.lock().unwrap().is_empty());
    assert!(worker.control.take_command().is_none());
    assert_eq!(worker.control.snapshot().diagnostics.commands, 0);
    worker.control.shutdown();
    wait(|| worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn subnormal_transport_audio_reopens_without_ambient_command_then_accepts_fresh_wake() {
    let p = Arc::new(Probe::default());
    let mut worker = start(&p);
    for second in 0..4 {
        p.push(vec![f32::from_bits(71); 16000], second);
    }
    wait(|| p.reopen.load(Ordering::Acquire) == 1);
    assert!(p.decoded.lock().unwrap().is_empty());
    assert!(worker.control.take_command().is_none());
    p.texts
        .lock()
        .unwrap()
        .push_back("Jarvis recovered headset request".into());
    p.push(vec![0.2; 16000], 5);
    p.push(vec![0.0; 17600], 6);
    wait(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("recovered headset request")
    );
    assert_eq!(p.reopen.load(Ordering::Acquire), 1);
    assert_eq!(worker.control.snapshot().diagnostics.commands, 1);
    worker.control.shutdown();
    wait(|| worker.is_finished());
    worker.join().unwrap();
}

struct RetryingBackend {
    inner: Backend,
    clock: Instant,
    recovery_started: Option<Duration>,
    backoff: jarvis_voice::capture::ReconnectBackoff,
}
impl WakeBackend for RetryingBackend {
    fn start(&mut self) -> Result<(), &'static str> {
        self.inner.start()
    }
    fn stop(&mut self) {
        self.inner.stop();
    }
    fn input_info(&self) -> Option<InputInfo> {
        self.inner.input_info()
    }
    fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
        self.inner.keyword_spot(samples)
    }
    fn reset_keyword(&mut self) {
        self.inner.reset_keyword();
    }
    fn transcribe(
        &mut self,
        samples: &[f32],
        cancel: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.inner.transcribe(samples, cancel)
    }
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        let now = self.clock.elapsed();
        if self.inner.p.runtime_failure.swap(false, Ordering::AcqRel) {
            self.inner.stop();
            assert!(self.backoff.begin(now));
            self.recovery_started = Some(now);
            return Ok((Vec::new(), true));
        }
        if let Some(started) = self.recovery_started {
            if !self.backoff.due(now) {
                return Ok((Vec::new(), false));
            }
            let attempt = {
                let mut attempts = self.inner.p.attempts.lock().unwrap();
                attempts.push(now.saturating_sub(started));
                attempts.len()
            };
            if attempt <= self.inner.p.reopen_failures.load(Ordering::Acquire) {
                if !self.backoff.failed(now) {
                    return Err("Fixture microphone remains unavailable");
                }
                return Ok((Vec::new(), false));
            }
            self.inner.start()?;
            self.inner.p.reopen.fetch_add(1, Ordering::AcqRel);
            self.recovery_started = None;
            self.backoff.success();
            return Ok((Vec::new(), true));
        }
        self.inner.poll()
    }
}
fn start_retrying(p: &Arc<Probe>) -> WakeWorker {
    let p = p.clone();
    let w = WakeWorker::with_backend(move || {
        Ok(Box::new(RetryingBackend {
            inner: Backend {
                p,
                watchdog: SilentStreamWatchdog::new(),
            },
            clock: Instant::now(),
            recovery_started: None,
            backoff: jarvis_voice::capture::ReconnectBackoff::new(),
        }))
    })
    .unwrap();
    wait(|| w.control.snapshot().state == State::Muted);
    w.control.set_enabled(true);
    wait(|| w.control.snapshot().state == State::WakeListening);
    w
}
#[test]
fn transient_runtime_failure_uses_delayed_reopens_and_rejects_old_decode_before_fresh_wake() {
    let p = Arc::new(Probe::default());
    p.reopen_failures.store(1, Ordering::Release);
    p.hold.store(true, Ordering::Release);
    p.texts.lock().unwrap().extend([
        "Jarvis old interrupted request".into(),
        "Jarvis after reconnect".into(),
    ]);
    let mut w = start_retrying(&p);
    p.push(vec![0.2; 16000], 0);
    p.push(vec![0.0; 17600], 1);
    wait(|| p.decoded.lock().unwrap().len() == 1);
    p.runtime_failure.store(true, Ordering::Release);
    p.release.store(true, Ordering::Release);
    wait(|| p.reopen.load(Ordering::Acquire) == 1);
    let attempts = p.attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), 2);
    assert!(attempts[0] >= Duration::from_millis(250));
    assert!(attempts[1] - attempts[0] >= Duration::from_millis(500));
    assert!(w.control.enabled());
    assert!(w.control.take_command().is_none());
    p.push(vec![0.2; 16000], 5);
    p.push(vec![0.0; 17600], 6);
    wait(|| w.control.snapshot().state == State::CommandReady);
    assert_eq!(w.control.take_command().as_deref(), Some("after reconnect"));
    assert_eq!(w.control.snapshot().diagnostics.commands, 1);
    w.control.shutdown();
    wait(|| w.is_finished());
    w.join().unwrap();
}
#[test]
fn exhausted_runtime_recovery_reports_failure_and_does_not_keep_reopening() {
    let p = Arc::new(Probe::default());
    p.reopen_failures.store(usize::MAX, Ordering::Release);
    let mut w = start_retrying(&p);
    p.runtime_failure.store(true, Ordering::Release);
    wait(|| w.control.snapshot().state == State::Failed);
    let attempts = p.attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), 3);
    assert!(attempts[0] >= Duration::from_millis(250));
    assert!(attempts[1] - attempts[0] >= Duration::from_millis(500));
    assert!(attempts[2] - attempts[1] >= Duration::from_millis(1000));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(p.attempts.lock().unwrap().len(), 3);
    assert!(!w.control.enabled());
    assert!(w.control.take_command().is_none());
    assert!(p.decoded.lock().unwrap().is_empty());
    w.control.shutdown();
    wait(|| w.is_finished());
    w.join().unwrap();
}

#[test]
fn muting_during_runtime_backoff_prevents_further_open_attempts() {
    let p = Arc::new(Probe::default());
    p.reopen_failures.store(usize::MAX, Ordering::Release);
    let mut w = start_retrying(&p);
    p.runtime_failure.store(true, Ordering::Release);
    wait(|| p.attempts.lock().unwrap().len() == 1);
    w.control.set_enabled(false);
    wait(|| w.control.snapshot().state == State::Muted);
    thread::sleep(Duration::from_millis(650));
    assert_eq!(
        p.attempts.lock().unwrap().len(),
        1,
        "mute must not allow a delayed capture reopen"
    );
    assert!(w.control.take_command().is_none());
    w.control.shutdown();
    wait(|| w.is_finished());
    w.join().unwrap();
}

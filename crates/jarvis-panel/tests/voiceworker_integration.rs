//! Synthetic backend only: no microphone device, Whisper weights, or provider calls.
use jarvis_panel::voiceworker::{CommandError, State, VoiceBackend, VoiceFailure, VoiceWorker};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Default)]
struct Probe {
    events: Mutex<Vec<&'static str>>,
    full: AtomicBool,
    failed: AtomicBool,
    release: AtomicBool,
}
struct Backend {
    probe: Arc<Probe>,
    samples: Vec<f32>,
    text: String,
    wait_for_release: bool,
}
impl VoiceBackend for Backend {
    fn start_recording(&mut self) -> Result<(), VoiceFailure> {
        self.probe.events.lock().unwrap().push("start");
        Ok(())
    }
    fn recording_full(&self) -> bool {
        self.probe.full.load(Ordering::Acquire)
    }
    fn recording_failed(&self) -> bool {
        self.probe.failed.load(Ordering::Acquire)
    }
    fn finish_recording(&mut self) -> Result<Vec<f32>, VoiceFailure> {
        self.probe.events.lock().unwrap().push("finish");
        Ok(self.samples.clone())
    }
    fn cancel_recording(&mut self) {
        self.probe.events.lock().unwrap().push("cancel");
    }
    fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, VoiceFailure> {
        self.probe.events.lock().unwrap().push("transcribe");
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.wait_for_release && !self.probe.release.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "test forgot to release synthetic inference"
            );
            thread::sleep(Duration::from_millis(2));
        }
        Ok(self.text.clone())
    }
}
fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "voice lifecycle timed out");
        thread::sleep(Duration::from_millis(5));
    }
}
fn worker(
    probe: &Arc<Probe>,
    samples: Vec<f32>,
    text: &str,
    wait_for_release: bool,
) -> VoiceWorker {
    let backend = Backend {
        probe: probe.clone(),
        samples,
        text: text.into(),
        wait_for_release,
    };
    let worker = VoiceWorker::with_backend(move || Ok(Box::new(backend))).unwrap();
    wait_until(|| worker.control.available());
    worker
}
fn stop(worker: &mut VoiceWorker) {
    worker.control.shutdown();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn microphone_opens_only_on_record_and_transcript_stays_for_explicit_review() {
    let probe = Arc::new(Probe::default());
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let count = factory_calls.clone();
    let p = probe.clone();
    let mut worker = VoiceWorker::with_backend(move || {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Backend {
            probe: p,
            samples: vec![0.1; 100],
            text: "book a flight".into(),
            wait_for_release: false,
        }))
    })
    .unwrap();
    wait_until(|| worker.control.available());
    thread::sleep(Duration::from_millis(30));
    assert!(probe.events.lock().unwrap().is_empty());
    for _ in 0..2 {
        worker.control.record().unwrap();
        wait_until(|| worker.control.snapshot().state == State::Recording);
        for _ in 0..100 {
            assert_eq!(worker.control.record(), Err(CommandError::Busy));
        }
        worker.control.finish_recording();
        wait_until(|| worker.control.snapshot().state == State::Transcript);
        let snapshot = worker.control.snapshot();
        assert_eq!(snapshot.transcript, "book a flight");
        assert!(snapshot.message.contains("review and press Ask"));
        assert!(!worker.control.busy());
    }
    assert_eq!(
        factory_calls.load(Ordering::Relaxed),
        1,
        "backend/model was reloaded each utterance"
    );
    assert_eq!(
        probe
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "start")
            .count(),
        2
    );
    stop(&mut worker);
    assert_eq!(worker.control.snapshot().state, State::Stopped);
    assert_eq!(worker.control.record(), Err(CommandError::Unavailable));
}

#[test]
fn recording_cancellation_discards_audio_without_transcribing() {
    let probe = Arc::new(Probe::default());
    let mut worker = worker(&probe, vec![0.1; 100], "must not appear", false);
    worker.control.record().unwrap();
    wait_until(|| worker.control.snapshot().state == State::Recording);
    worker.control.cancel();
    wait_until(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Ready);
    assert!(worker.control.snapshot().transcript.is_empty());
    assert!(!probe.events.lock().unwrap().contains(&"transcribe"));
    assert!(probe.events.lock().unwrap().contains(&"cancel"));
    stop(&mut worker);
}

#[test]
fn cancelling_inference_remains_responsive_and_discards_a_late_backend_result() {
    let probe = Arc::new(Probe::default());
    let mut worker = worker(&probe, vec![0.1; 100], "late cancelled result", true);
    worker.control.record().unwrap();
    wait_until(|| worker.control.snapshot().state == State::Recording);
    worker.control.finish_recording();
    wait_until(|| probe.events.lock().unwrap().contains(&"transcribe"));
    let begin = Instant::now();
    worker.control.cancel();
    assert!(begin.elapsed() < Duration::from_millis(100));
    assert_eq!(worker.control.snapshot().state, State::Cancelling);
    assert_eq!(worker.control.record(), Err(CommandError::Busy));
    probe.release.store(true, Ordering::Release);
    wait_until(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Ready);
    assert!(worker.control.snapshot().transcript.is_empty());
    stop(&mut worker);
}

#[test]
fn recording_cap_auto_finishes_but_capture_failure_does_not_transcribe() {
    for failed in [false, true] {
        let probe = Arc::new(Probe::default());
        probe.full.store(!failed, Ordering::Release);
        probe.failed.store(failed, Ordering::Release);
        let mut worker = worker(&probe, vec![0.1; 100], "bounded phrase", false);
        worker.control.record().unwrap();
        wait_until(|| !worker.control.busy());
        assert_eq!(
            worker.control.snapshot().state,
            if failed {
                State::Failed
            } else {
                State::Transcript
            }
        );
        assert_eq!(
            probe.events.lock().unwrap().contains(&"transcribe"),
            !failed
        );
        stop(&mut worker);
    }
}

#[test]
fn missing_model_or_invalid_audio_and_oversized_text_do_not_publish_transcripts() {
    let mut missing = VoiceWorker::with_backend(|| Err(VoiceFailure::MissingModel)).unwrap();
    wait_until(|| missing.is_finished());
    assert_eq!(missing.control.snapshot().state, State::Unavailable);
    assert_eq!(missing.control.record(), Err(CommandError::Unavailable));
    missing.join().unwrap();
    for (samples, text) in [
        (vec![], "phrase".into()),
        (vec![0.0; 240001], "phrase".into()),
        (vec![0.1; 100], " ".into()),
        (vec![0.1; 100], "x".repeat(8193)),
    ] {
        let probe = Arc::new(Probe::default());
        probe.full.store(true, Ordering::Release);
        let mut worker = worker(&probe, samples, &text, false);
        worker.control.record().unwrap();
        wait_until(|| !worker.control.busy());
        assert_eq!(worker.control.snapshot().state, State::Failed);
        assert!(worker.control.snapshot().transcript.is_empty());
        stop(&mut worker);
    }
}

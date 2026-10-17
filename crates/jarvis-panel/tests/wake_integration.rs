//! Synthetic samples and backend only. Never accesses a microphone or model provider.
use jarvis_panel::wake::{DecodeRequest, Phase, WakeDetector};
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

fn utterance(detector: &mut WakeDetector) -> DecodeRequest {
    assert!(detector.ingest(&vec![0.1; 16000]).is_none());
    detector
        .ingest(&vec![0.0; 12000])
        .expect("speech plus .75s silence should produce endpoint")
}

#[test]
fn wake_requires_exact_word_boundaries_and_ambient_text_never_becomes_a_command() {
    for text in [
        "ordinary ambient conversation",
        "Jarvises open Spotify",
        "hey jarvisian send email",
        "heyjarvis do it",
        "jarvis123 open it",
        "superjarvis open it",
        "hey jar vis",
    ] {
        let mut detector = WakeDetector::new();
        assert_eq!(utterance(&mut detector).phase, Phase::Wake);
        assert!(
            detector.accept_transcript(text).is_none(),
            "ambient text forwarded: {text}"
        );
        assert_eq!(detector.phase(), Phase::Wake);
    }
    for text in [
        "Jarvis open Spotify",
        "JARVIS open Spotify",
        "jarvis open Spotify",
        "Hey Jarvis open Spotify",
        "Okay, Jarvis: open Spotify",
        "Jarvis! open Spotify",
        "(Jarvis), open Spotify",
    ] {
        let mut detector = WakeDetector::new();
        utterance(&mut detector);
        assert_eq!(
            detector.accept_transcript(text).as_deref(),
            Some("open Spotify")
        );
        assert_eq!(detector.phase(), Phase::Wake);
    }
}

#[test]
fn standalone_wake_followup_is_forwarded_once_and_reset_drops_pending_intent() {
    let mut detector = WakeDetector::new();
    utterance(&mut detector);
    assert!(detector.accept_transcript("Jarvis.").is_none());
    assert_eq!(detector.phase(), Phase::Command);
    let request = utterance(&mut detector);
    assert_eq!(request.phase, Phase::Command);
    assert!(request.endpoint);
    assert_eq!(
        detector
            .accept_transcript("Find my project notes")
            .as_deref(),
        Some("Find my project notes")
    );
    assert_eq!(detector.phase(), Phase::Wake);
    utterance(&mut detector);
    assert!(detector.accept_transcript("Jarvis").is_none());
    detector.reset();
    assert_eq!(detector.phase(), Phase::Wake);
    utterance(&mut detector);
    assert!(
        detector
            .accept_transcript("this was spoken after mute")
            .is_none()
    );
}

#[test]
fn continuous_audio_and_command_windows_remain_bounded() {
    let mut detector = WakeDetector::new();
    let chunk = vec![0.1; 16000];
    for _ in 0..120 {
        if let Some(request) = detector.ingest(&chunk) {
            assert!(request.samples.len() <= 4 * 16000);
            assert_eq!(request.phase, Phase::Wake);
            assert!(detector.accept_transcript("ambient words").is_none());
        }
    }
    detector.reset();
    utterance(&mut detector);
    detector.accept_transcript("Jarvis");
    let mut endpoint_seen = false;
    for _ in 0..20 {
        if let Some(request) = detector.ingest(&chunk) {
            assert!(request.samples.len() <= 15 * 16000);
            if request.endpoint {
                endpoint_seen = true;
                detector.accept_transcript("bounded command");
                break;
            }
        }
    }
    assert!(endpoint_seen, "command did not endpoint by15second cap");
}

#[derive(Default)]
struct Probe {
    chunks: Mutex<VecDeque<(Vec<f32>, bool)>>,
    texts: Mutex<VecDeque<String>>,
    starts: AtomicUsize,
    stops: AtomicUsize,
    decodes: AtomicUsize,
    hold_decode: AtomicBool,
    release: AtomicBool,
}
struct Backend(Arc<Probe>);
impl WakeBackend for Backend {
    fn start(&mut self) -> Result<(), &'static str> {
        self.0.starts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        Ok(self
            .0
            .chunks
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default())
    }
    fn stop(&mut self) {
        self.0.stops.fetch_add(1, Ordering::Relaxed);
    }
    fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
        self.0.decodes.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.0.hold_decode.load(Ordering::Acquire) && !self.0.release.load(Ordering::Acquire)
        {
            assert!(
                Instant::now() < deadline,
                "test did not release synthetic decoding"
            );
            thread::sleep(Duration::from_millis(2));
        }
        Ok(self.0.texts.lock().unwrap().pop_front().unwrap_or_default())
    }
}
fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "wakeworker lifecycle timed out");
        thread::sleep(Duration::from_millis(5));
    }
}
fn worker(probe: &Arc<Probe>) -> WakeWorker {
    let p = probe.clone();
    let worker = WakeWorker::with_backend(move || Ok(Box::new(Backend(p)))).unwrap();
    wait_until(|| worker.control.snapshot().state == State::Muted);
    worker
}
fn feed(probe: &Probe, text: &str) {
    probe.texts.lock().unwrap().push_back(text.into());
    let mut queue = probe.chunks.lock().unwrap();
    queue.push_back((vec![0.1; 16000], false));
    queue.push_back((vec![0.0; 12000], false));
}
fn stop(worker: &mut WakeWorker) {
    worker.control.shutdown();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn worker_stays_muted_until_enabled_and_only_exposes_woken_commands_once() {
    let probe = Arc::new(Probe::default());
    let mut worker = worker(&probe);
    thread::sleep(Duration::from_millis(30));
    assert_eq!(probe.starts.load(Ordering::Relaxed), 0);
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    feed(&probe, "ambient private sentence");
    wait_until(|| probe.decodes.load(Ordering::Relaxed) == 1);
    assert!(worker.control.take_command().is_none());
    feed(&probe, "Jarvis find my notes");
    wait_until(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("find my notes")
    );
    assert!(worker.control.take_command().is_none());
    stop(&mut worker);
}

#[test]
fn mute_and_owned_speech_suppression_discard_late_wake_results() {
    for suppress in [false, true] {
        let probe = Arc::new(Probe::default());
        probe.hold_decode.store(true, Ordering::Release);
        let mut worker = worker(&probe);
        worker.control.set_enabled(true);
        wait_until(|| worker.control.snapshot().state == State::WakeListening);
        feed(&probe, "Jarvis should never escape");
        wait_until(|| probe.decodes.load(Ordering::Relaxed) == 1);
        if suppress {
            worker.control.suppress(true);
        } else {
            worker.control.set_enabled(false);
        }
        probe.release.store(true, Ordering::Release);
        wait_until(|| {
            matches!(
                worker.control.snapshot().state,
                State::Muted | State::Suppressed
            )
        });
        assert!(worker.control.take_command().is_none());
        assert!(probe.stops.load(Ordering::Relaxed) > 0);
        // Fresh ambient text after resuming cannot inherit the prior wake.
        if suppress {
            worker.control.suppress(false);
        } else {
            worker.control.set_enabled(true);
        }
        wait_until(|| worker.control.snapshot().state == State::WakeListening);
        feed(&probe, "followup after mute");
        wait_until(|| probe.decodes.load(Ordering::Relaxed) == 2);
        assert!(worker.control.take_command().is_none());
        stop(&mut worker);
    }
}

#[test]
fn owned_speech_suppression_stops_capture_and_prevents_new_decode_work() {
    let probe = Arc::new(Probe::default());
    let mut worker = worker(&probe);
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    worker.control.suppress(true);
    wait_until(|| worker.control.snapshot().state == State::Suppressed);
    let starts = probe.starts.load(Ordering::Relaxed);
    let decodes = probe.decodes.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(60));
    assert_eq!(probe.starts.load(Ordering::Relaxed), starts);
    assert_eq!(probe.decodes.load(Ordering::Relaxed), decodes);
    worker.control.shutdown();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
    assert_eq!(worker.control.snapshot().state, State::Stopped);
}

#[test]
fn streaming_wake_waits_for_command_endpoint_and_invalid_text_cannot_escape() {
    let mut detector = WakeDetector::new();
    let request = detector.ingest(&vec![0.1; 40000]).unwrap();
    assert!(!request.endpoint);
    assert!(detector.accept_transcript("Jarvis find").is_none());
    assert_eq!(detector.phase(), Phase::Command);
    assert!(detector.ingest(&vec![0.1; 8000]).is_none());
    let request = detector.ingest(&vec![0.0; 12000]).unwrap();
    assert!(request.endpoint);
    assert_eq!(
        detector
            .accept_transcript("Jarvis find my notes")
            .as_deref(),
        Some("find my notes")
    );
    assert!(detector.accept_transcript("duplicate result").is_none());
    for invalid in [
        "Jarvis\0send".to_owned(),
        format!("Jarvis {}", "x".repeat(8192)),
    ] {
        utterance(&mut detector);
        assert!(detector.accept_transcript(&invalid).is_none());
        assert_eq!(detector.phase(), Phase::Wake);
    }
    utterance(&mut detector);
    detector.accept_transcript("Jarvis");
    assert!(detector.ingest(&vec![0.0; 16000 * 11]).is_none());
    assert_eq!(
        detector.phase(),
        Phase::Wake,
        "wake alone must expire without follow-up"
    );
}

#[test]
fn capture_overflow_discards_pending_wake_and_command_mailbox_has_backpressure() {
    let probe = Arc::new(Probe::default());
    let mut worker = worker(&probe);
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    feed(&probe, "Jarvis");
    wait_until(|| worker.control.snapshot().state == State::CommandListening);
    probe
        .chunks
        .lock()
        .unwrap()
        .push_back((vec![0.1; 320], true));
    feed(&probe, "unrelated speech after missing audio");
    wait_until(|| probe.decodes.load(Ordering::Relaxed) == 2);
    assert!(worker.control.take_command().is_none());
    feed(&probe, "Jarvis first request");
    wait_until(|| worker.control.snapshot().state == State::CommandReady);
    feed(&probe, "Jarvis queued request");
    thread::sleep(Duration::from_millis(80));
    assert_eq!(probe.decodes.load(Ordering::Relaxed), 3);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("first request")
    );
    wait_until(|| probe.decodes.load(Ordering::Relaxed) == 4);
    wait_until(|| worker.control.snapshot().state == State::CommandReady);
    worker.control.set_enabled(false);
    assert!(
        worker.control.take_command().is_none(),
        "mute must clear an already queued command"
    );
    stop(&mut worker);
}

#[test]
#[ignore = "requires explicit local model and generated wake-command WAV"]
fn opt_in_real_whisper_audio_reaches_wake_command_without_microphone() {
    let model = std::env::var_os("JARVIS_TEST_WHISPER_MODEL")
        .expect("set JARVIS_TEST_WHISPER_MODEL to an existing trusted local model");
    let wav = std::env::var_os("JARVIS_TEST_WAKE_WAV")
        .expect("set JARVIS_TEST_WAKE_WAV to a generated Jarvis command fixture");
    let mut transcriber = jarvis_voice::Transcriber::load(std::path::Path::new(&model)).unwrap();
    let samples = jarvis_voice::load_wav(std::path::Path::new(&wav)).unwrap();
    let mut detector = WakeDetector::new();
    let mut commands = Vec::new();
    for chunk in samples
        .chunks(320)
        .chain(std::iter::repeat_n(&[0.0; 320][..], 40))
    {
        if let Some(request) = detector.ingest(chunk) {
            let text = transcriber.transcribe(&request.samples).unwrap();
            if let Some(command) = detector.accept_transcript(&text) {
                commands.push(command);
            }
        }
    }
    assert_eq!(
        commands.len(),
        1,
        "expected one command from the generated utterance"
    );
    let command = commands[0].to_lowercase();
    assert!(
        !command.contains("jarvis"),
        "wake token must not reach the conversational model"
    );
    assert!(
        command.contains("what should i work on next"),
        "generated command was not recognized: {command}"
    );
    assert_eq!(detector.phase(), Phase::Wake);
}

#[test]
fn oversized_command_chunk_preserves_beginning_instead_of_rolling_it_away() {
    let mut detector = WakeDetector::new();
    utterance(&mut detector);
    detector.accept_transcript("Jarvis");
    let mut samples = vec![0.1; 16000];
    samples.extend(vec![0.2; 16000 * 15]);
    let request = detector.ingest(&samples).unwrap();
    assert!(request.endpoint);
    assert_eq!(request.samples.len(), 16000 * 15);
    assert_eq!(&request.samples[..16000], &[0.1; 16000]);
}

#[test]
fn microphone_start_failure_stays_visible_without_automatic_retry() {
    struct FailedBackend(Arc<AtomicUsize>);
    impl WakeBackend for FailedBackend {
        fn start(&mut self) -> Result<(), &'static str> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err("synthetic microphone permission denied")
        }
        fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
            panic!("failed microphone must not be polled")
        }
        fn stop(&mut self) {}
        fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
            panic!("failed microphone must not be transcribed")
        }
    }
    let starts = Arc::new(AtomicUsize::new(0));
    let calls = starts.clone();
    let mut worker = WakeWorker::with_backend(move || Ok(Box::new(FailedBackend(calls)))).unwrap();
    wait_until(|| worker.control.snapshot().state == State::Muted);
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::Failed);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(worker.control.snapshot().state, State::Failed);
    assert!(!worker.control.enabled());
    assert_eq!(starts.load(Ordering::Relaxed), 1);
    worker.control.set_enabled(true);
    wait_until(|| starts.load(Ordering::Relaxed) == 2);
    stop(&mut worker);
}

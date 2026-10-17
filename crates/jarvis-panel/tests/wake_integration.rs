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
        .ingest(&vec![0.0; 17600])
        .expect("speech plus 1.1s silence should produce endpoint")
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
    input: Mutex<Option<jarvis_voice::capture::InputInfo>>,
    poll_failure: AtomicBool,
    starts: AtomicUsize,
    stops: AtomicUsize,
    decodes: AtomicUsize,
    hold_decode: AtomicBool,
    release: AtomicBool,
}
struct Backend(Arc<Probe>);
impl WakeBackend for Backend {
    fn input_info(&self) -> Option<jarvis_voice::capture::InputInfo> {
        self.0.input.lock().unwrap().clone()
    }
    fn start(&mut self) -> Result<(), &'static str> {
        self.0.starts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        if self.0.poll_failure.load(Ordering::Acquire) {
            return Err("Synthetic capture failure");
        }
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
    queue.push_back((vec![0.0; 17600], false));
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
        if suppress {
            assert_eq!(
                probe.stops.load(Ordering::Relaxed),
                0,
                "owned speech must keep the stream warm"
            );
        } else {
            assert!(probe.stops.load(Ordering::Relaxed) > 0);
        }
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
fn owned_speech_suppression_drains_capture_and_prevents_new_decode_work() {
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
    let request = detector.ingest(&vec![0.0; 17600]).unwrap();
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
#[cfg(feature = "whisper")]
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
        .chain(std::iter::repeat_n(&[0.0; 320][..], 60))
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

#[test]
fn long_idle_silence_does_not_spend_the_next_utterance_decode_budget() {
    for amplitude in [0.015, 0.1] {
        let mut detector = WakeDetector::new();
        for _ in 0..3000 {
            assert!(
                detector.ingest(&[0.0; 320]).is_none(),
                "idle silence must never request inference"
            );
        }
        for _ in 0..59 {
            assert!(
                detector.ingest(&[amplitude; 320]).is_none(),
                "long-idle counter consumed the fresh 1.2-second probe budget"
            );
        }
        let probe = detector
            .ingest(&[amplitude; 320])
            .expect("first probe at 1.2 seconds");
        assert!(!probe.endpoint);
        assert!(detector.accept_transcript("Jarvis where was I?").is_none());
        let mut endpoint = None;
        for _ in 0..60 {
            if let Some(request) = detector.ingest(&[0.0; 320]) {
                endpoint = Some(request);
                break;
            }
        }
        let request =
            endpoint.expect("quiet or normal speech must reach silence endpoint after idle");
        assert!(request.endpoint);
        assert!(
            request.samples.len() >= 16000,
            "beginning of utterance must survive"
        );
        assert_eq!(
            detector.accept_transcript("Jarvis where was I?").as_deref(),
            Some("where was I?")
        );
    }
}

#[test]
fn worker_ignores_extended_silence_and_ambient_speech_then_accepts_wake() {
    let probe = Arc::new(Probe::default());
    let mut worker = worker(&probe);
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    for _ in 0..6 {
        probe
            .chunks
            .lock()
            .unwrap()
            .push_back((vec![0.0; 160000], false));
    }
    wait_until(|| probe.chunks.lock().unwrap().is_empty());
    assert_eq!(
        probe.decodes.load(Ordering::Relaxed),
        0,
        "silence requested local inference"
    );
    assert!(worker.control.take_command().is_none());
    feed(&probe, "ordinary ambient conversation");
    wait_until(|| probe.decodes.load(Ordering::Relaxed) > 0);
    assert!(
        worker.control.take_command().is_none(),
        "ambient words must never reach remote chat"
    );
    // Any remaining silent frame from the ambient utterance is consumed before wake.
    wait_until(|| probe.chunks.lock().unwrap().is_empty());
    feed(&probe, "Jarvis resume my work");
    wait_until(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("resume my work")
    );
    stop(&mut worker);
}

#[test]
fn conversational_midphrase_pauses_and_quiet_trailing_words_do_not_submit_early() {
    for pause_ms in [700, 800, 900] {
        let mut detector = WakeDetector::new();
        utterance(&mut detector);
        detector.accept_transcript("Jarvis");
        assert_eq!(detector.phase(), Phase::Command);
        assert!(detector.ingest(&vec![0.1; 16000]).is_none());
        assert!(
            detector.ingest(&vec![0.0; pause_ms * 16]).is_none(),
            "{pause_ms}ms midphrase pause submitted prematurely"
        );
        // Quieter words after speech has started must reset endpoint silence.
        assert!(
            detector.ingest(&vec![0.007; 8000]).is_none(),
            "quiet trailing words were treated as silence"
        );
        assert!(
            detector.ingest(&vec![0.0; 16000]).is_none(),
            "endpoint occurred before1.1seconds of silence"
        );
        let request = detector
            .ingest(&vec![0.0; 1600])
            .expect("1.1s finalsilence should endpoint");
        assert!(request.endpoint);
        assert!(
            request
                .samples
                .iter()
                .any(|sample| (*sample - 0.007).abs() < 0.00001)
        );
        assert_eq!(
            detector
                .accept_transcript("find my notes and summarize them")
                .as_deref(),
            Some("find my notes and summarize them")
        );
        assert!(detector.accept_transcript("duplicate").is_none());
    }
}

#[test]
fn resumed_speech_during_slow_decode_replaces_provisional_command_with_complete_utterance() {
    struct ResumeFixture {
        chunks: Mutex<VecDeque<Vec<f32>>>,
        decodes: AtomicUsize,
        release: AtomicBool,
        complete_audio: AtomicBool,
    }
    struct Resuming(Arc<ResumeFixture>);
    impl WakeBackend for Resuming {
        fn start(&mut self) -> Result<(), &'static str> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
            Ok((
                self.0
                    .chunks
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_default(),
                false,
            ))
        }
        fn transcribe(
            &mut self,
            samples: &[f32],
            _: Arc<AtomicBool>,
        ) -> Result<String, &'static str> {
            let number = self.0.decodes.fetch_add(1, Ordering::AcqRel);
            if number == 0 {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !self.0.release.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(2));
                }
                Ok("Jarvis find my notes".into())
            } else {
                let original = samples.iter().any(|sample| (*sample - 0.1).abs() < 0.00001);
                let resumed = samples
                    .iter()
                    .any(|sample| (*sample - 0.05).abs() < 0.00001);
                self.0
                    .complete_audio
                    .store(original && resumed, Ordering::Release);
                Ok("Jarvis find my notes and summarize them".into())
            }
        }
    }
    let probe = Arc::new(ResumeFixture {
        chunks: Mutex::new(VecDeque::new()),
        decodes: AtomicUsize::new(0),
        release: AtomicBool::new(false),
        complete_audio: AtomicBool::new(false),
    });
    let backend = probe.clone();
    let mut worker = WakeWorker::with_backend(move || Ok(Box::new(Resuming(backend)))).unwrap();
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    {
        let mut q = probe.chunks.lock().unwrap();
        q.push_back(vec![0.1; 16000]);
        q.push_back(vec![0.0; 17600]);
    }
    wait_until(|| probe.decodes.load(Ordering::Acquire) == 1);
    {
        let mut q = probe.chunks.lock().unwrap();
        q.push_back(vec![0.05; 16000]);
        q.push_back(vec![0.0; 17600]);
    }
    probe.release.store(true, Ordering::Release);
    wait_until(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("find my notes and summarize them"),
        "provisional decode escaped before checking resumedaudio"
    );
    assert!(
        probe.complete_audio.load(Ordering::Acquire),
        "redecode omitted originalprefix or resumedtail"
    );
    assert!(worker.control.take_command().is_none());
    stop(&mut worker);
}

#[test]
fn endpoint_revalidation_redecodes_already_finished_resumed_tail_from_original_audio() {
    let mut detector = WakeDetector::new();
    let first = utterance(&mut detector);
    assert!(first.endpoint);
    let mut queued = vec![0.05; 16000];
    queued.extend(vec![0.0; 17600]);
    let outcome = detector.accept_transcript_with_audio("Jarvis find my notes", &queued);
    assert!(
        outcome.command.is_none(),
        "stale provisional transcript must not be forwarded"
    );
    let next = outcome
        .next
        .expect("resumed tail already reached endpoint so a full redecode should be ready");
    assert!(next.endpoint);
    assert!(next.samples.iter().any(|v| (*v - 0.1).abs() < 0.00001));
    assert!(next.samples.iter().any(|v| (*v - 0.05).abs() < 0.00001));
    assert!(next.samples.len() <= 15 * 16000);
    let complete =
        detector.accept_transcript_with_audio("Jarvis find my notes and summarize them", &[]);
    assert_eq!(
        complete.command.as_deref(),
        Some("find my notes and summarize them")
    );
    assert!(complete.next.is_none());
}

#[test]
fn audio_overflow_during_decode_discards_provisional_command() {
    let probe = Arc::new(Probe::default());
    probe.hold_decode.store(true, Ordering::Release);
    let mut worker = worker(&probe);
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    feed(&probe, "Jarvis interrupted request must not escape");
    wait_until(|| probe.decodes.load(Ordering::Acquire) == 1);
    probe
        .chunks
        .lock()
        .unwrap()
        .push_back((vec![0.05; 320], true));
    probe.release.store(true, Ordering::Release);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    assert!(worker.control.take_command().is_none());
    feed(&probe, "Jarvis fresh request");
    wait_until(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("fresh request")
    );
    stop(&mut worker);
}

#[test]
fn low_energy_idle_audio_does_not_start_decode_but_active_quiet_tail_is_retained() {
    let mut detector = WakeDetector::new();
    let idle = [0.007; 16000];
    for _ in 0..90 {
        assert!(
            detector.ingest(&idle).is_none(),
            "below-start-gate idle energy must not cause inference"
        );
    }
    assert!(detector.ingest(&vec![0.1; 8000]).is_none());
    assert!(detector.ingest(&vec![0.007; 8000]).is_none());
    let request = detector
        .ingest(&vec![0.0; 17600])
        .expect("activeutterance endsafter silence");
    assert!(request.endpoint);
    assert!(request.samples.len() <= 4 * 16000);
    assert!(
        detector
            .accept_transcript("ambient conversation without the wake word")
            .is_none()
    );
}

#[test]
fn repeating_the_wake_word_waits_for_a_request_instead_of_sending_it_as_a_command() {
    for wake_only in ["Jarvis Jarvis", "Jarvis, Jarvis.", "JARVIS! Jarvis? Jarvis"] {
        let mut detector = WakeDetector::new();
        utterance(&mut detector);
        assert!(
            detector.accept_transcript(wake_only).is_none(),
            "repeated wake words became a remote command: {wake_only}"
        );
        assert_eq!(detector.phase(), Phase::Command);
        utterance(&mut detector);
        assert_eq!(
            detector.accept_transcript("find my notes").as_deref(),
            Some("find my notes")
        );
    }
    let mut detector = WakeDetector::new();
    utterance(&mut detector);
    assert_eq!(
        detector
            .accept_transcript("Jarvis, Jarvis, find my notes")
            .as_deref(),
        Some("find my notes")
    );
    utterance(&mut detector);
    assert_eq!(
        detector
            .accept_transcript("Jarvis tell me about Jarvis")
            .as_deref(),
        Some("tell me about Jarvis"),
        "normalizer must preserve wake token inside actual request"
    );
}

#[test]
fn explicit_wake_accepts_a_quiet_followup_without_requiring_louder_speech_again() {
    let mut detector = WakeDetector::new();
    utterance(&mut detector);
    assert!(detector.accept_transcript("Jarvis").is_none());
    assert_eq!(detector.phase(), Phase::Command);
    assert!(detector.ingest(&vec![0.007; 16000]).is_none());
    let request = detector
        .ingest(&vec![0.0; 17600])
        .expect("armed followup should retain quietspeech and endpoint");
    assert_eq!(request.phase, Phase::Command);
    assert!(request.endpoint);
    assert!(request.samples.len() >= 16000);
    assert_eq!(
        detector.accept_transcript("where was I?").as_deref(),
        Some("where was I?")
    );
    assert_eq!(detector.phase(), Phase::Wake);
}

#[test]
fn repeated_wake_during_followup_rearms_instead_of_sending_a_wake_token_or_disarming() {
    let mut detector = WakeDetector::new();
    utterance(&mut detector);
    detector.accept_transcript("Jarvis");
    for repeated in ["Jarvis", "Jarvis Jarvis", "Jarvis, Jarvis!"] {
        utterance(&mut detector);
        assert!(detector.accept_transcript(repeated).is_none());
        assert_eq!(
            detector.phase(),
            Phase::Command,
            "repeatwake lostthefollowupwindow"
        );
    }
    utterance(&mut detector);
    assert_eq!(
        detector.accept_transcript("summarize my notes").as_deref(),
        Some("summarize my notes")
    );
}

#[test]
fn first_wake_probe_is_prompt_but_repeat_probes_remain_bounded() {
    let mut detector = WakeDetector::new();
    for _ in 0..59 {
        assert!(detector.ingest(&[0.1; 320]).is_none());
    }
    let first = detector.ingest(&[0.1; 320]).unwrap();
    assert!(!first.endpoint);
    assert!(detector.accept_transcript("ambient words").is_none());
    for _ in 0..124 {
        assert!(detector.ingest(&[0.1; 320]).is_none());
    }
    let repeat = detector
        .ingest(&[0.1; 320])
        .expect("repeat after 2.5 seconds");
    assert!(!repeat.endpoint);
    assert!(repeat.samples.len() <= 4 * 16000);
    assert!(detector.accept_transcript("Jarvis find my notes").is_none());
    assert_eq!(detector.phase(), Phase::Command);
    let endpoint = detector.ingest(&[0.0; 17600]).unwrap();
    assert!(endpoint.endpoint);
    assert_eq!(
        detector
            .accept_transcript("Jarvis find my notes")
            .as_deref(),
        Some("find my notes")
    );
}

#[test]
fn diagnostics_distinguish_capture_decode_and_wake_without_storing_transcript() {
    let probe = Arc::new(Probe::default());
    *probe.input.lock().unwrap() = Some(jarvis_voice::capture::InputInfo {
        name: "Synthetic microphone".into(),
        sample_rate: 48000,
        channels: 2,
    });
    let mut worker = worker(&probe);
    assert!(worker.control.snapshot().diagnostics.input.is_none());
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    let input = worker.control.snapshot().diagnostics.input.unwrap();
    assert_eq!(
        (input.name.as_str(), input.sample_rate, input.channels),
        ("Synthetic microphone", 48000, 2)
    );
    probe
        .chunks
        .lock()
        .unwrap()
        .push_back((vec![0.007; 320], false));
    wait_until(|| worker.control.snapshot().diagnostics.captured_samples == 320);
    let d = worker.control.snapshot().diagnostics;
    assert!((d.last_rms - 0.007).abs() < 0.00001);
    assert!((d.last_peak - 0.007).abs() < 0.00001);
    assert_eq!(d.decode_count, 0);
    feed(&probe, "ambient secret fixture words");
    wait_until(|| worker.control.snapshot().diagnostics.decode_count == 1);
    assert_eq!(worker.control.snapshot().diagnostics.commands, 0);
    feed(&probe, "Jarvis synthetic private command");
    wait_until(|| worker.control.snapshot().state == State::CommandReady);
    let snapshot = worker.control.snapshot();
    assert_eq!(snapshot.diagnostics.decode_count, 2);
    assert_eq!(snapshot.diagnostics.wake_matches, 1);
    assert_eq!(snapshot.diagnostics.commands, 1);
    assert!(snapshot.diagnostics.max_rms >= 0.099);
    assert!(!format!("{snapshot:?}").contains("secret fixture"));
    assert!(!format!("{snapshot:?}").contains("synthetic private command"));
    worker.control.suppress(true);
    assert!(worker.control.take_command().is_none());
    stop(&mut worker);
}

#[test]
fn fatal_capture_error_remains_visible_and_requires_explicit_retry() {
    let probe = Arc::new(Probe::default());
    let mut worker = worker(&probe);
    worker.control.set_enabled(true);
    wait_until(|| worker.control.snapshot().state == State::WakeListening);
    probe.poll_failure.store(true, Ordering::Release);
    wait_until(|| worker.control.snapshot().state == State::Failed);
    thread::sleep(Duration::from_millis(60));
    assert_eq!(worker.control.snapshot().state, State::Failed);
    assert_eq!(worker.control.snapshot().diagnostics.capture_errors, 1);
    assert_eq!(probe.starts.load(Ordering::Acquire), 1);
    assert!(!worker.control.enabled());
    probe.poll_failure.store(false, Ordering::Release);
    worker.control.set_enabled(true);
    wait_until(|| probe.starts.load(Ordering::Acquire) == 2);
    feed(&probe, "Jarvis retry succeeds");
    wait_until(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("retry succeeds")
    );
    stop(&mut worker);
}

#[test]
fn opt_in_diagnostic_line_contains_only_fixed_state_and_numeric_health() {
    use jarvis_panel::wakeworker::{Diagnostics, Snapshot};
    let snapshot = Snapshot {
        state: State::WakeListening,
        message: "PRIVATE_TRANSCRIPT_SENTINEL api-key-secret",
        revision: 99,
        diagnostics: Diagnostics {
            input: Some(jarvis_voice::capture::InputInfo {
                name: "PRIVATE_DEVICE_OWNER_SENTINEL\nforged diagnostic".into(),
                sample_rate: 48000,
                channels: 2,
            }),
            captured_samples: 32000,
            last_rms: 0.025,
            last_peak: 0.5,
            max_rms: 0.25,
            decode_count: 7,
            last_decode_ms: 350,
            wake_matches: 2,
            commands: 1,
            discontinuities: 3,
            capture_errors: 4,
            capture: jarvis_voice::capture::CaptureDiagnostics {
                xruns: 6,
                route_changes: 7,
                queue_overruns: 8,
            },
            decode_errors: 5,
            ..Default::default()
        },
    };
    let line = snapshot.diagnostic_line();
    assert!(!line.contains("PRIVATE_"));
    assert!(!line.contains("secret"));
    assert!(!line.contains("forged"));
    assert_eq!(line.lines().count(), 1);
    assert_eq!(
        line,
        "jarvis_voice state=WakeListening samples=32000 rms=2.500e-2 peak=5.000e-1 max_rms=2.500e-1 decodes=7 decode_ms=350 wakes=2 commands=1 discontinuities=3 capture_errors=4 decode_errors=5 xruns=6 route_changes=7 queue_overruns=8 input_rate=48000 input_channels=2"
    );
}

#[test]
fn acoustic_nonspeech_ends_command_even_when_background_rms_exceeds_legacy_gate() {
    let mut detector = WakeDetector::new();
    for _ in 0..100 {
        assert!(
            detector
                .ingest_with_activity(&[0.03; 320], Some(&[false]))
                .is_none(),
            "classified nonspeech caused ambient decode"
        );
    }
    detector.arm_keyword(&[0.1; 1600]);
    for _ in 0..54 {
        assert!(
            detector
                .ingest_with_activity(&[0.03; 320], Some(&[false]))
                .is_none()
        );
    }
    let request = detector
        .ingest_with_activity(&[0.03; 320], Some(&[false]))
        .expect("classified ambient noise must reach1.1second endpoint");
    assert!(request.endpoint);
    assert!(request.samples.len() < 15 * 16000);
    assert_eq!(
        detector.accept_transcript("Jarvis where was I?").as_deref(),
        Some("where was I?")
    );
}

#[test]
fn acoustic_speech_flags_preserve_quiet_trailing_words_and_resume_provisional_endpoint() {
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&[0.1; 1600]);
    assert!(
        detector
            .ingest_with_activity(&[0.001; 16000], Some(&[true; 50]))
            .is_none()
    );
    let endpoint = detector
        .ingest_with_activity(&[0.03; 17600], Some(&[false; 55]))
        .unwrap();
    assert!(endpoint.endpoint);
    let resumed = detector.accept_transcript_with_audio_and_activity(
        "Jarvis find notes",
        &[0.001; 16000],
        Some(&[true; 50]),
    );
    assert!(resumed.command.is_none());
    assert!(resumed.next.is_none());
    let request = detector
        .ingest_with_activity(&[0.03; 17600], Some(&[false; 55]))
        .unwrap();
    assert!(request.endpoint);
    let complete = detector.accept_transcript_with_audio_and_activity(
        "Jarvis find notes and summarize them",
        &[0.03; 320],
        Some(&[false]),
    );
    assert_eq!(
        complete.command.as_deref(),
        Some("find notes and summarize them")
    );
    assert!(complete.next.is_none());
}

#[test]
fn invalid_activity_alignment_clears_wake_authorization_instead_of_guessing_labels() {
    for flags in [Vec::new(), vec![false, false]] {
        let mut detector = WakeDetector::new();
        detector.arm_keyword(&[0.1; 1600]);
        assert!(
            detector
                .ingest_with_activity(&[0.03; 320], Some(&flags))
                .is_none()
        );
        assert_eq!(detector.phase(), Phase::Wake);
        assert!(
            detector
                .accept_transcript("should not become a command")
                .is_none()
        );
    }
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&[0.1; 1600]);
    detector
        .ingest_with_activity(&[0.03; 17600], Some(&[false; 55]))
        .unwrap();
    let invalid = detector.accept_transcript_with_audio_and_activity(
        "Jarvis must not escape",
        &[0.1; 321],
        Some(&[true]),
    );
    assert!(invalid.command.is_none());
    assert!(invalid.next.is_none());
    assert_eq!(detector.phase(), Phase::Wake);
}

#[test]
fn delayed_vad_onset_preserves_quiet_wake_prefix_for_local_recognition() {
    let mut detector = WakeDetector::new();
    // The first 400 ms represent a quiet wake word which the classifier labels
    // nonspeech; the stronger request tail finally crosses its speech gate.
    let quiet_prefix = vec![0.004; 6400];
    for frame in quiet_prefix.chunks(320) {
        assert!(
            detector
                .ingest_with_activity(frame, Some(&[false]))
                .is_none()
        );
    }
    let mut decoded = None;
    for _ in 0..60 {
        if let Some(request) = detector.ingest_with_activity(&[0.1; 320], Some(&[true])) {
            decoded = Some(request);
            break;
        }
    }
    let request = decoded.expect("voiced tail should trigger a bounded local decode");
    let preserved_prefix = request
        .samples
        .iter()
        .take_while(|&&sample| sample == 0.004)
        .count();
    assert_eq!(
        preserved_prefix,
        quiet_prefix.len(),
        "speech-classifier onset clipped the quiet wake word before Whisper could inspect it"
    );
    assert!(request.samples.len() <= 64000);
}

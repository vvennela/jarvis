//! Dedicated keyword backend simulation through the real wake worker, no microphone.
#[cfg(target_os = "macos")]
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis_panel::wakeworker::{KeywordStatus, State, WakeBackend, WakeWorker};
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
    audio: Mutex<VecDeque<(Vec<f32>, bool)>>,
    transcripts: Mutex<VecDeque<String>>,
    decoded: Mutex<Vec<Vec<f32>>>,
    input: Mutex<Option<jarvis_voice::capture::InputInfo>>,
    spots: AtomicUsize,
    resets: AtomicUsize,
    hold: AtomicBool,
    release: AtomicBool,
    fallback: AtomicBool,
}
struct Backend(Arc<Probe>);
impl WakeBackend for Backend {
    fn wake_fallback_enabled(&self) -> bool {
        self.0.fallback.load(Ordering::Acquire)
    }
    fn speech_activity(&mut self, samples: &[f32]) -> Result<Option<Vec<bool>>, &'static str> {
        Ok(self.wake_fallback_enabled().then(|| {
            samples
                .chunks(320)
                .map(|frame| {
                    frame
                        .first()
                        .is_some_and(|sample| *sample == 0.1 || *sample == 0.2)
                })
                .collect()
        }))
    }
    fn input_info(&self) -> Option<jarvis_voice::capture::InputInfo> {
        self.0.input.lock().unwrap().clone()
    }
    fn start(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        Ok(self.0.audio.lock().unwrap().pop_front().unwrap_or_default())
    }
    fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
        if !samples.is_empty() {
            self.0.spots.fetch_add(1, Ordering::AcqRel);
        }
        Ok(Some(KeywordStatus {
            lookback_samples: if samples.first() == Some(&0.2) {
                Some(usize::MAX)
            } else {
                None
            },
        }))
    }
    fn reset_keyword(&mut self) {
        self.0.resets.fetch_add(1, Ordering::AcqRel);
    }
    fn transcribe(&mut self, samples: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
        self.0.decoded.lock().unwrap().push(samples.to_vec());
        let end = Instant::now() + Duration::from_secs(3);
        while self.0.hold.load(Ordering::Acquire) && !self.0.release.load(Ordering::Acquire) {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
        Ok(self
            .0
            .transcripts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default())
    }
}
fn wait(mut check: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(4);
    while !check() {
        assert!(Instant::now() < end, "KWS worker deadline");
        thread::sleep(Duration::from_millis(2));
    }
}
fn start(probe: &Arc<Probe>) -> WakeWorker {
    let p = probe.clone();
    let w = WakeWorker::with_backend(move || Ok(Box::new(Backend(p)))).unwrap();
    wait(|| w.control.snapshot().state == State::Muted);
    w.control.set_enabled(true);
    wait(|| w.control.snapshot().state == State::WakeListening);
    w
}
fn stop(w: &mut WakeWorker) {
    w.control.shutdown();
    wait(|| w.is_finished());
    w.join().unwrap();
}
fn command(p: &Probe, text: &str) {
    p.transcripts.lock().unwrap().push_back(text.into());
    let mut q = p.audio.lock().unwrap();
    q.push_back((vec![0.2; 16000], false));
    q.push_back((vec![0.0; 17600], false));
}
#[test]
fn dedicated_spotter_handles_loud_ambient_without_whisper_then_recovers_bounded_same_breath_command()
 {
    let p = Arc::new(Probe::default());
    let mut w = start(&p);
    for _ in 0..20 {
        p.audio
            .lock()
            .unwrap()
            .push_back((vec![0.025; 8000], false));
    }
    wait(|| p.spots.load(Ordering::Acquire) == 20);
    assert!(p.decoded.lock().unwrap().is_empty());
    assert!(w.control.take_command().is_none());
    command(&p, "Jarvis where was I?");
    wait(|| w.control.snapshot().state == State::CommandReady);
    assert_eq!(w.control.take_command().as_deref(), Some("where was I?"));
    let decoded = p.decoded.lock().unwrap();
    assert_eq!(decoded.len(), 1);
    assert!(decoded[0].len() <= 64000 + 17600);
    assert!(decoded[0].contains(&0.2));
    drop(decoded);
    assert_eq!(w.control.snapshot().diagnostics.wake_matches, 1);
    assert_eq!(w.control.snapshot().diagnostics.decode_count, 1);
    stop(&mut w);
}
#[test]
fn dedicated_spotter_mute_suppression_and_overflow_drop_pending_wake_and_late_results() {
    for suppress in [false, true] {
        let p = Arc::new(Probe::default());
        let mut w = start(&p);
        p.hold.store(true, Ordering::Release);
        command(&p, "Jarvis this result must not escape");
        wait(|| !p.decoded.lock().unwrap().is_empty());
        if suppress {
            w.control.suppress(true)
        } else {
            w.control.set_enabled(false)
        }
        p.release.store(true, Ordering::Release);
        wait(|| matches!(w.control.snapshot().state, State::Muted | State::Suppressed));
        assert!(w.control.take_command().is_none());
        if suppress {
            w.control.suppress(false)
        } else {
            w.control.set_enabled(true)
        }
        wait(|| w.control.snapshot().state == State::WakeListening);
        let before = p.resets.load(Ordering::Acquire);
        p.audio.lock().unwrap().push_back((vec![0.2; 320], true));
        wait(|| p.resets.load(Ordering::Acquire) > before);
        p.audio
            .lock()
            .unwrap()
            .push_back((vec![0.025; 32000], false));
        wait(|| w.control.snapshot().diagnostics.captured_samples >= 16000 + 17600 + 320 + 32000);
        assert_eq!(p.decoded.lock().unwrap().len(), 1);
        assert!(w.control.take_command().is_none());
        stop(&mut w);
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires explicit trusted KWS/Whisper models and generated command WAV; no microphone"]
fn opt_in_real_keyword_and_whisper_worker_preserves_same_breath_request() {
    struct LocalBackend {
        kws: jarvis_voice::kws::LocalKeywordSpotter,
        stt: jarvis_voice::Transcriber,
        vad: jarvis_voice::vad::LocalVoiceActivity,
        audio: VecDeque<Vec<f32>>,
        handoff: jarvis_voice::capture::AudioHandoff,
        resampler: jarvis_voice::capture::CaptureBuffer,
        exhausted: Arc<AtomicBool>,
        force_keyword_miss: bool,
        activity_delay_samples: usize,
    }
    impl WakeBackend for LocalBackend {
        fn wake_fallback_enabled(&self) -> bool {
            true
        }
        fn speech_activity(&mut self, samples: &[f32]) -> Result<Option<Vec<bool>>, &'static str> {
            let mut flags = self.vad.classify(samples)?;
            for (flag, frame) in flags.iter_mut().zip(samples.chunks(320)) {
                if self.activity_delay_samples > 0 {
                    *flag = false;
                }
                self.activity_delay_samples =
                    self.activity_delay_samples.saturating_sub(frame.len());
            }
            Ok(Some(flags))
        }
        fn start(&mut self) -> Result<(), &'static str> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
            if self.audio.is_empty() {
                self.exhausted.store(true, Ordering::Release);
            }
            self.resampler
                .push_f32(&self.audio.pop_front().unwrap_or_default())
                .map_err(|_| "fixture resampling failed")?;
            let (samples, gap) = self.resampler.drain_samples();
            assert!(!gap, "generated fixture overflowed capture");
            self.handoff.push(&samples);
            Ok(self.handoff.drain())
        }
        fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
            let detected = self.kws.accept(samples)?;
            Ok(Some(KeywordStatus {
                lookback_samples: if self.force_keyword_miss {
                    None
                } else {
                    detected.map(|d| d.lookback_samples)
                },
            }))
        }
        fn reset_keyword(&mut self) {
            self.kws.reset();
            self.vad.reset();
        }
        fn transcribe(
            &mut self,
            samples: &[f32],
            cancel: Arc<AtomicBool>,
        ) -> Result<String, &'static str> {
            self.stt
                .transcribe_cancelled(samples, cancel)
                .map_err(|_| "local fixture transcription failed")
        }
    }
    let kws = std::path::PathBuf::from(
        std::env::var_os("JARVIS_TEST_KWS_MODEL").expect("set JARVIS_TEST_KWS_MODEL"),
    );
    let stt = std::path::PathBuf::from(
        std::env::var_os("JARVIS_TEST_WHISPER_MODEL").expect("set JARVIS_TEST_WHISPER_MODEL"),
    );
    let vad = std::path::PathBuf::from(
        std::env::var_os("JARVIS_TEST_VAD_MODEL").expect("set JARVIS_TEST_VAD_MODEL"),
    );
    let wav = std::path::PathBuf::from(
        std::env::var_os("JARVIS_TEST_WAKE_WAV").expect("set JARVIS_TEST_WAKE_WAV"),
    );
    let mut samples = jarvis_voice::load_wav(&wav).unwrap();
    let negative = std::env::var_os("JARVIS_TEST_EXPECT_NO_WAKE").is_some();
    let mut downstream = negative.then(NegativeDownstream::new);
    let gain = std::env::var("JARVIS_TEST_WAKE_GAIN")
        .ok()
        .map(|s| s.parse::<f32>().unwrap())
        .unwrap_or(1.0);
    let noise = std::env::var("JARVIS_TEST_WAKE_NOISE")
        .ok()
        .map(|s| s.parse::<f32>().unwrap())
        .unwrap_or(0.0);
    let mut fixture_seed = 0x4413_u32;
    for sample in &mut samples {
        fixture_seed = fixture_seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *sample = *sample * gain + ((fixture_seed >> 8) as f32 / 16777215.0 * 2.0 - 1.0) * noise;
    }
    let exhausted = Arc::new(AtomicBool::new(false));
    let backend_exhausted = exhausted.clone();
    let mut noise_seed = 0x5413_u32;
    samples.extend((0..32000).map(|_| {
        noise_seed = noise_seed.wrapping_mul(1664525).wrapping_add(1013904223);
        ((noise_seed >> 8) as f32 / 16777215.0 * 2.0 - 1.0) * 0.06
    }));
    let source_rate = std::env::var("JARVIS_TEST_INPUT_RATE")
        .ok()
        .map(|s| s.parse::<u32>().unwrap())
        .unwrap_or(16000);
    assert!([16000, 24000, 48000].contains(&source_rate));
    if source_rate != 16000 {
        // Generate continuous input-rate microphone-like audio from trusted 16kHz
        // speech fixture, then add an independently generated out-of-band tone.
        // The production CaptureBuffer, not the test, must remove that tone.
        let ultrasonic_gain = std::env::var("JARVIS_TEST_HIGH_FREQUENCY_GAIN")
            .ok()
            .map(|s| s.parse::<f32>().unwrap())
            .unwrap_or(0.0);
        let original = std::mem::take(&mut samples);
        samples = (0..original.len() * source_rate as usize / 16000)
            .map(|n| {
                let position = n as f64 * 16000.0 / f64::from(source_rate);
                let index = position.floor() as usize;
                let fraction = position.fract() as f32;
                let next = original.get(index + 1).copied().unwrap_or(original[index]);
                original[index] * (1.0 - fraction)
                    + next * fraction
                    + ultrasonic_gain
                        * (std::f64::consts::TAU * 12000.0 * n as f64 / f64::from(source_rate))
                            .sin() as f32
            })
            .collect();
    }
    let mut worker = WakeWorker::with_backend(move || {
        Ok(Box::new(LocalBackend {
            kws: jarvis_voice::kws::LocalKeywordSpotter::load(&kws)?,
            vad: jarvis_voice::vad::LocalVoiceActivity::load(&vad)?,
            stt: jarvis_voice::Transcriber::load(&stt).map_err(|_| "local Whisper model failed")?,
            audio: samples
                .chunks(source_rate as usize / 50)
                .map(Vec::from)
                .collect(),
            resampler: jarvis_voice::capture::CaptureBuffer::continuous(source_rate, 1).unwrap(),
            handoff: jarvis_voice::capture::AudioHandoff::new(),
            exhausted: backend_exhausted,
            force_keyword_miss: std::env::var_os("JARVIS_TEST_FORCE_KWS_MISS").is_some(),
            activity_delay_samples: std::env::var("JARVIS_TEST_VAD_DELAY_MS")
                .ok()
                .map(|s| s.parse::<usize>().unwrap() * 16)
                .unwrap_or(0),
        }))
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    worker.control.set_enabled(true);
    let command = loop {
        if let Some(downstream) = &mut downstream {
            downstream.tick_and_assert_no_request(&worker);
        }
        if let Some(command) = worker.control.take_command() {
            assert!(!negative, "negative generated audio produced a command");
            break command;
        }
        if negative
            && exhausted.load(Ordering::Acquire)
            && worker.control.snapshot().state == State::WakeListening
        {
            let health = worker.control.snapshot().diagnostics;
            assert_eq!(health.commands, 0);
            assert_eq!(health.wake_matches, 0);
            assert!(
                health.decode_count > 0,
                "negative speech never exercised local transcription"
            );
            stop(&mut worker);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "real local wake path failed: {}",
            worker.control.snapshot().diagnostic_line()
        );
        assert_ne!(worker.control.snapshot().state, State::Failed);
        thread::sleep(Duration::from_millis(5));
    };
    let normalized = command.to_ascii_lowercase();
    assert!(
        normalized.contains("what should i work on next"),
        "generated fixture command was lost or truncated"
    );
    assert!(
        !normalized.contains("jarvis"),
        "wake prefix leaked into user command"
    );
    let health = worker.control.snapshot().diagnostics;
    assert_eq!(health.wake_matches, 1);
    assert_eq!(health.commands, 1);
    assert!(
        health.decode_count <= 2,
        "ambient speech unexpectedly invoked Whisper repeatedly"
    );
    stop(&mut worker);
}

#[test]
fn microphone_route_switch_during_decode_discards_old_command_and_refreshes_selected_input() {
    let p = Arc::new(Probe::default());
    *p.input.lock().unwrap() = Some(jarvis_voice::capture::InputInfo {
        name: "Original microphone".into(),
        sample_rate: 48000,
        channels: 1,
    });
    let mut w = start(&p);
    assert_eq!(
        w.control.snapshot().diagnostics.input.unwrap().name,
        "Original microphone"
    );
    p.hold.store(true, Ordering::Release);
    command(&p, "Jarvis stale request from original microphone");
    wait(|| p.decoded.lock().unwrap().len() == 1);
    let resets = p.resets.load(Ordering::Acquire);
    *p.input.lock().unwrap() = Some(jarvis_voice::capture::InputInfo {
        name: "New headset input".into(),
        sample_rate: 24000,
        channels: 1,
    });
    // Native default-device replacement uses this same discontinuity boundary.
    p.audio.lock().unwrap().push_back((Vec::new(), true));
    p.release.store(true, Ordering::Release);
    wait(|| {
        w.control.snapshot().state == State::WakeListening
            && w.control
                .snapshot()
                .diagnostics
                .input
                .as_ref()
                .is_some_and(|i| i.name == "New headset input")
    });
    assert!(p.resets.load(Ordering::Acquire) > resets);
    assert!(w.control.take_command().is_none());
    assert_eq!(w.control.snapshot().diagnostics.commands, 0);
    command(&p, "Jarvis fresh request on new microphone");
    wait(|| w.control.snapshot().state == State::CommandReady);
    assert_eq!(
        w.control.take_command().as_deref(),
        Some("fresh request on new microphone")
    );
    assert_eq!(
        w.control.snapshot().diagnostics.input.unwrap().sample_rate,
        24000
    );
    assert_eq!(w.control.snapshot().diagnostics.commands, 1);
    stop(&mut w);
}

fn fallback_utterance(probe: &Probe, text: &str) {
    probe.transcripts.lock().unwrap().push_back(text.into());
    let mut queue = probe.audio.lock().unwrap();
    queue.push_back((vec![0.1; 16000], false));
    queue.push_back((vec![0.03; 17600], false));
}
#[test]
fn vad_gated_fallback_ignores_background_and_only_forwards_exact_invocations() {
    let p = Arc::new(Probe::default());
    p.fallback.store(true, Ordering::Release);
    let mut worker = start(&p);
    for _ in 0..10 {
        p.audio
            .lock()
            .unwrap()
            .push_back((vec![0.03; 16000], false));
    }
    wait(|| p.spots.load(Ordering::Acquire) == 10);
    assert!(p.decoded.lock().unwrap().is_empty());
    for (index, text) in [
        "ordinary ambient conversation",
        "The control service is ready",
        "Jarvises open something",
        "Jervis filed the report",
    ]
    .into_iter()
    .enumerate()
    {
        fallback_utterance(&p, text);
        wait(|| p.decoded.lock().unwrap().len() == index + 1);
        wait(|| worker.control.snapshot().state == State::WakeListening);
        assert!(worker.control.take_command().is_none());
    }
    fallback_utterance(&p, "Jarvis find my project notes");
    wait(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("find my project notes")
    );
    assert_eq!(worker.control.snapshot().diagnostics.commands, 1);
    assert_eq!(worker.control.snapshot().diagnostics.wake_matches, 1);
    assert!(
        p.decoded
            .lock()
            .unwrap()
            .iter()
            .all(|audio| audio.len() <= 64000)
    );
    stop(&mut worker);
}
#[test]
fn fallback_decode_stays_in_background_and_cancel_or_route_change_cannot_publish_late_command() {
    for cancellation in ["mute", "suppress", "route"] {
        let p = Arc::new(Probe::default());
        p.fallback.store(true, Ordering::Release);
        p.hold.store(true, Ordering::Release);
        let mut worker = start(&p);
        fallback_utterance(&p, "Jarvis this old command must never escape");
        wait(|| p.decoded.lock().unwrap().len() == 1);
        assert_eq!(
            worker.control.snapshot().state,
            State::WakeListening,
            "ambient fallback exposed active command UI"
        );
        match cancellation {
            "mute" => worker.control.set_enabled(false),
            "suppress" => worker.control.suppress(true),
            _ => p.audio.lock().unwrap().push_back((Vec::new(), true)),
        }
        p.release.store(true, Ordering::Release);
        wait(|| {
            worker.control.snapshot().diagnostics.decode_count == 1
                && match cancellation {
                    "mute" => worker.control.snapshot().state == State::Muted,
                    "suppress" => worker.control.snapshot().state == State::Suppressed,
                    _ => worker.control.snapshot().diagnostics.discontinuities == 1,
                }
        });
        assert!(worker.control.take_command().is_none());
        assert_eq!(worker.control.snapshot().diagnostics.commands, 0);
        stop(&mut worker);
    }
}
#[test]
fn dedicated_keyword_fast_path_does_not_duplicate_fallback_wakes_or_commands() {
    let p = Arc::new(Probe::default());
    p.fallback.store(true, Ordering::Release);
    let mut worker = start(&p);
    command(&p, "Jarvis show my notes");
    wait(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("show my notes")
    );
    assert_eq!(worker.control.snapshot().diagnostics.wake_matches, 1);
    assert_eq!(worker.control.snapshot().diagnostics.commands, 1);
    assert_eq!(p.decoded.lock().unwrap().len(), 1);
    stop(&mut worker);
}

#[test]
fn queued_primary_keyword_supersedes_old_fallback_transcript_without_double_wake() {
    let p = Arc::new(Probe::default());
    p.fallback.store(true, Ordering::Release);
    p.hold.store(true, Ordering::Release);
    let mut worker = start(&p);
    fallback_utterance(&p, "Jarvis old provisional request");
    wait(|| p.decoded.lock().unwrap().len() == 1);
    command(&p, "Jarvis fresh request from primary wake");
    p.release.store(true, Ordering::Release);
    wait(|| worker.control.snapshot().state == State::CommandReady);
    assert_eq!(
        worker.control.take_command().as_deref(),
        Some("fresh request from primary wake")
    );
    assert_eq!(worker.control.snapshot().diagnostics.wake_matches, 1);
    assert_eq!(worker.control.snapshot().diagnostics.commands, 1);
    let decoded = p.decoded.lock().unwrap();
    assert_eq!(decoded.len(), 2);
    assert!(decoded[1].contains(&0.2), "queued wake audio was lost");
    assert!(decoded[1].len() <= 64000 + 17600);
    drop(decoded);
    stop(&mut worker);
}

/// The same production handoff used by AppKit, with a listening loopback socket
/// and real conversation/speech workers. Any ambient forwarding reaches this
/// socket or changes worker state; no external provider can be contacted.
#[cfg(target_os = "macos")]
struct NegativeDownstream {
    _directory: support::TempDir,
    listener: std::net::TcpListener,
    chat: jarvis_panel::chat::ConversationWorker,
    speech: jarvis_panel::speech::SpeechWorker,
    handoff: jarvis_panel::voicehandoff::VoiceHandoff,
}
#[cfg(target_os = "macos")]
impl NegativeDownstream {
    fn new() -> Self {
        use jarvis::{
            context::{ContextStore, Limits},
            conversation::Conversation,
            providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
        };
        let directory = support::TempDir::new();
        assert!(directory.path().is_dir());
        let database = directory.join("context.db");
        drop(ContextStore::open(&database, Limits::default()).unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let client = CrusoeClient::new(
            "synthetic-fixture-key",
            NEMOTRON_MODEL,
            ProviderConfig::loopback_for_tests(&endpoint, Duration::from_secs(1)).unwrap(),
        )
        .unwrap();
        let chat = jarvis_panel::chat::ConversationWorker::with_factory(move || {
            Ok(Conversation::new(database, None, client))
        })
        .unwrap();
        let speech =
            jarvis_panel::speech::SpeechWorker::with_executable("/usr/bin/true".into()).unwrap();
        wait(|| chat.control.available() && speech.control.available());
        Self {
            _directory: directory,
            listener,
            chat,
            speech,
            handoff: Default::default(),
        }
    }
    fn tick_and_assert_no_request(&mut self, wake: &WakeWorker) {
        assert!(
            self.handoff
                .tick(&wake.control, &self.chat.control, &self.speech.control)
                .is_none(),
            "ambient audio reached production handoff"
        );
        assert_eq!(
            self.chat.control.snapshot().state,
            jarvis_panel::chat::State::Ready
        );
        assert!(!self.chat.control.busy());
        assert!(!self.speech.control.busy());
        assert!(
            matches!(self.listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "ambient audio opened a provider connection"
        );
    }
}

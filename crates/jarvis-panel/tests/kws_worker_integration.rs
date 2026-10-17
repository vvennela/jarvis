//! Dedicated keyword backend simulation through the real wake worker, no microphone.
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
    spots: AtomicUsize,
    resets: AtomicUsize,
    hold: AtomicBool,
    release: AtomicBool,
}
struct Backend(Arc<Probe>);
impl WakeBackend for Backend {
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
    }
    impl WakeBackend for LocalBackend {
        fn speech_activity(&mut self, samples: &[f32]) -> Result<Option<Vec<bool>>, &'static str> {
            self.vad.classify(samples).map(Some)
        }
        fn start(&mut self) -> Result<(), &'static str> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
            self.handoff
                .push(&self.audio.pop_front().unwrap_or_default());
            Ok(self.handoff.drain())
        }
        fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
            Ok(Some(KeywordStatus {
                lookback_samples: self.kws.accept(samples)?.map(|d| d.lookback_samples),
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
    let mut noise_seed = 0x5413_u32;
    samples.extend((0..32000).map(|_| {
        noise_seed = noise_seed.wrapping_mul(1664525).wrapping_add(1013904223);
        ((noise_seed >> 8) as f32 / 16777215.0 * 2.0 - 1.0) * 0.06
    }));
    let mut worker = WakeWorker::with_backend(move || {
        Ok(Box::new(LocalBackend {
            kws: jarvis_voice::kws::LocalKeywordSpotter::load(&kws)?,
            vad: jarvis_voice::vad::LocalVoiceActivity::load(&vad)?,
            stt: jarvis_voice::Transcriber::load(&stt).map_err(|_| "local Whisper model failed")?,
            audio: samples.chunks(320).map(Vec::from).collect(),
            handoff: jarvis_voice::capture::AudioHandoff::new(),
        }))
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    worker.control.set_enabled(true);
    let command = loop {
        if let Some(command) = worker.control.take_command() {
            break command;
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

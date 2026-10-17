//! Wake-phrase residue removal for keyword-armed commands.
//! Synthetic samples and fake backends only; never opens a microphone or network.
use jarvis_panel::wake::{DecodeRequest, Phase, WakeDetector};
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

const KEYWORD: f32 = 0.2;
const SPEECH: f32 = 0.1;

// ---------- WakeDetector state transitions ----------

fn endpoint(detector: &mut WakeDetector) -> DecodeRequest {
    detector
        .ingest(&vec![0.0; 17_600])
        .expect("1.1 s of silence after speech should endpoint")
}

fn utterance(detector: &mut WakeDetector) -> DecodeRequest {
    assert!(detector.ingest(&vec![SPEECH; 16_000]).is_none());
    endpoint(detector)
}

#[test]
fn keyword_armed_command_request_carries_skip_inside_the_wake_audio() {
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 16_000]);
    assert_eq!(detector.phase(), Phase::Command);
    let request = endpoint(&mut detector);
    assert_eq!(request.phase, Phase::Command);
    assert!(request.endpoint);
    let skip = request.skip_before.expect("keyword wake must carry a skip");
    assert_eq!(request.samples.len(), 16_000 + 17_600);
    // The cut sits shortly before detection, never inside the command tail.
    assert_eq!(skip, 16_000 - 4_800);
    assert!(skip < 16_000 && skip < request.samples.len());
}

#[test]
fn skip_is_relative_to_kept_audio_for_oversized_and_tiny_keyword_buffers() {
    // Only the last four seconds of keyword audio are retained; the skip must
    // index into what the recognizer actually receives.
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 80_000]);
    let request = endpoint(&mut detector);
    assert_eq!(request.samples.len(), 64_000 + 17_600);
    assert_eq!(request.skip_before, Some(64_000 - 4_800));

    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 1_000]);
    let request = endpoint(&mut detector);
    assert_eq!(request.skip_before, Some(0));

    let mut detector = WakeDetector::new();
    detector.arm_keyword(&[]);
    assert_eq!(detector.phase(), Phase::Command);
    let request = utterance(&mut detector);
    assert_eq!(request.skip_before, Some(0));
}

#[test]
fn wake_phase_text_wake_and_followup_requests_never_carry_a_skip() {
    let mut detector = WakeDetector::new();
    let request = utterance(&mut detector);
    assert_eq!(request.phase, Phase::Wake);
    assert_eq!(request.skip_before, None);
    // Invocation-only "Jarvis" by text moves to Command phase with no timing.
    assert!(detector.accept_transcript("Jarvis").is_none());
    assert_eq!(detector.phase(), Phase::Command);
    let request = utterance(&mut detector);
    assert_eq!(request.phase, Phase::Command);
    assert_eq!(request.skip_before, None);
    assert_eq!(
        detector.accept_transcript("nice open Spotify").as_deref(),
        Some("nice open Spotify")
    );

    let mut detector = WakeDetector::new();
    detector.arm_followup();
    let request = utterance(&mut detector);
    assert_eq!(request.phase, Phase::Command);
    assert_eq!(request.skip_before, None);
}

#[test]
fn reset_and_rearm_drop_stale_skip() {
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 16_000]);
    detector.reset();
    assert_eq!(utterance(&mut detector).skip_before, None);

    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 16_000]);
    detector.arm_followup();
    assert_eq!(utterance(&mut detector).skip_before, None);
}

#[test]
fn all_residue_transcript_keeps_listening_and_next_utterance_is_the_command() {
    for residue in ["", "   ", "\n"] {
        let mut detector = WakeDetector::new();
        detector.arm_keyword(&vec![KEYWORD; 16_000]);
        assert!(endpoint(&mut detector).skip_before.is_some());
        assert!(
            detector.accept_transcript(residue).is_none(),
            "residue {residue:?} became a command"
        );
        assert_eq!(detector.phase(), Phase::Command, "{residue:?}");
        // The follow-on utterance holds no wake audio, so no skip applies.
        let request = utterance(&mut detector);
        assert_eq!(request.phase, Phase::Command);
        assert_eq!(request.skip_before, None);
        assert_eq!(request.samples.len(), 16_000 + 17_600);
        assert_eq!(
            detector
                .accept_transcript("open Taylor Swift on Spotify")
                .as_deref(),
            Some("open Taylor Swift on Spotify")
        );
        assert_eq!(detector.phase(), Phase::Wake);
    }
}

#[test]
fn all_residue_then_silence_returns_to_wake_without_a_command() {
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 16_000]);
    endpoint(&mut detector);
    assert!(detector.accept_transcript("").is_none());
    assert_eq!(detector.phase(), Phase::Command);
    for _ in 0..11 {
        assert!(detector.ingest(&vec![0.0; 16_000]).is_none());
    }
    assert_eq!(detector.phase(), Phase::Wake);
}

#[test]
fn empty_transcript_without_keyword_timing_does_not_hold_command_phase_open() {
    // A follow-up answer that came back empty is not wake residue.
    let mut detector = WakeDetector::new();
    detector.arm_followup();
    utterance(&mut detector);
    assert!(detector.accept_transcript("").is_none());
    assert_eq!(detector.phase(), Phase::Wake);
}

#[test]
fn hesitation_after_keyword_keeps_the_skip_for_the_combined_decode() {
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 16_000]);
    let first = endpoint(&mut detector);
    let skip = first.skip_before;
    assert!(skip.is_some());
    // Speech resumed while decoding: the provisional endpoint is discarded and
    // the combined audio still starts with the same wake phrase.
    let outcome = detector.accept_transcript_with_audio("", &vec![SPEECH; 3_200]);
    assert!(outcome.command.is_none());
    let request = match outcome.next {
        Some(request) => request,
        None => endpoint(&mut detector),
    };
    assert_eq!(request.phase, Phase::Command);
    assert!(request.samples.len() > first.samples.len());
    assert_eq!(request.skip_before, skip);
}

#[test]
fn legacy_unskipped_transcript_still_strips_spoken_invocation() {
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 16_000]);
    endpoint(&mut detector);
    assert_eq!(
        detector.accept_transcript("Hey Jarvis play X").as_deref(),
        Some("play X")
    );
    // A substantive mention of Jarvis is not stripped.
    let mut detector = WakeDetector::new();
    detector.arm_keyword(&vec![KEYWORD; 16_000]);
    endpoint(&mut detector);
    assert_eq!(
        detector
            .accept_transcript("tell me about Jarvis")
            .as_deref(),
        Some("tell me about Jarvis")
    );
}

// ---------- WakeWorker end to end with a fake timed recognizer ----------

/// Scripted recognizer output. `Timed` words carry start offsets in samples and
/// are filtered by the skip as Apple word timing does; `Raw` ignores the skip.
enum Reply {
    Timed(Vec<(&'static str, usize)>),
    Raw(&'static str),
}

#[derive(Default)]
struct Probe {
    audio: Mutex<VecDeque<(Vec<f32>, bool)>>,
    replies: Mutex<VecDeque<Reply>>,
    /// (samples.len(), skip_before) for every command decode.
    commands: Mutex<Vec<(usize, Option<usize>)>>,
    wake_decodes: AtomicUsize,
    plain_command_decodes: AtomicUsize,
}

impl Probe {
    fn next(&self, skip: Option<usize>) -> String {
        match self.replies.lock().unwrap().pop_front() {
            Some(Reply::Timed(words)) => words
                .into_iter()
                .filter(|(_, start)| skip.is_none_or(|skip| *start >= skip))
                .map(|(text, _)| text)
                .collect::<Vec<_>>()
                .join(" "),
            Some(Reply::Raw(text)) => text.to_owned(),
            None => String::new(),
        }
    }
    fn push(&self, samples: Vec<f32>) {
        self.audio.lock().unwrap().push_back((samples, false));
    }
}

struct Timed(Arc<Probe>);
/// Recognizer without word timing: relies on the trait's default method.
struct Legacy(Arc<Probe>);

macro_rules! common_backend {
    () => {
        fn start(&mut self) -> Result<(), &'static str> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
            Ok(self.0.audio.lock().unwrap().pop_front().unwrap_or_default())
        }
        fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
            Ok(Some(KeywordStatus {
                lookback_samples: (samples.first() == Some(&KEYWORD)).then_some(usize::MAX),
            }))
        }
        fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
            self.0.wake_decodes.fetch_add(1, Ordering::AcqRel);
            Ok(self.0.next(None))
        }
    };
}

impl WakeBackend for Timed {
    common_backend!();
    fn transcribe_command(
        &mut self,
        _: &[f32],
        _: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.0.plain_command_decodes.fetch_add(1, Ordering::AcqRel);
        Ok(self.0.next(None))
    }
    fn transcribe_command_after(
        &mut self,
        samples: &[f32],
        skip_before: Option<usize>,
        _: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.0
            .commands
            .lock()
            .unwrap()
            .push((samples.len(), skip_before));
        Ok(self.0.next(skip_before))
    }
}

impl WakeBackend for Legacy {
    common_backend!();
    fn transcribe_command(
        &mut self,
        samples: &[f32],
        _: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.0.commands.lock().unwrap().push((samples.len(), None));
        Ok(self.0.next(None))
    }
}

fn wait(what: &str, mut check: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < end, "deadline waiting for {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn start(backend: impl FnOnce() -> Box<dyn WakeBackend> + Send + 'static) -> WakeWorker {
    let w = WakeWorker::with_backend(move || Ok(backend())).unwrap();
    wait("muted", || w.control.snapshot().state == State::Muted);
    w.control.set_enabled(true);
    wait("wake listening", || {
        w.control.snapshot().state == State::WakeListening
    });
    w
}

fn stop(w: &mut WakeWorker) {
    w.control.shutdown();
    wait("worker exit", || w.is_finished());
    w.join().unwrap();
}

fn keyword_utterance(p: &Probe) {
    p.push(vec![KEYWORD; 16_000]);
    p.push(vec![0.0; 17_600]);
}

fn speech_utterance(p: &Probe) {
    p.push(vec![SPEECH; 16_000]);
    p.push(vec![0.0; 17_600]);
}

fn take(w: &WakeWorker) -> String {
    wait("command ready", || {
        w.control.snapshot().state == State::CommandReady
    });
    w.control.take_command().expect("command published")
}

#[test]
fn worker_keyword_command_is_published_without_misheard_wake_residue() {
    for residue in [
        vec![("nice", 0)],
        vec![("Hey,", 0), ("drivers", 6_000)],
        vec![("hey", 800), ("Jarvis", 7_000), ("nice", 10_000)],
    ] {
        let p = Arc::new(Probe::default());
        let probe = p.clone();
        let mut w = start(move || Box::new(Timed(probe)));
        let mut words = residue.clone();
        words.extend([
            ("open", 16_500),
            ("Taylor", 20_000),
            ("Swift", 24_000),
            ("on", 27_000),
            ("Spotify", 29_000),
        ]);
        p.replies.lock().unwrap().push_back(Reply::Timed(words));
        keyword_utterance(&p);
        assert_eq!(take(&w), "open Taylor Swift on Spotify", "{residue:?}");
        let commands = p.commands.lock().unwrap().clone();
        assert_eq!(commands.len(), 1);
        let (len, skip) = commands[0];
        let skip = skip.expect("keyword wake must pass a skip to the recognizer");
        assert!(skip < len, "skip {skip} not inside {len} samples");
        assert!(skip <= 16_000, "skip {skip} cuts into the command tail");
        assert_eq!(p.wake_decodes.load(Ordering::Acquire), 0);
        assert_eq!(p.plain_command_decodes.load(Ordering::Acquire), 0);
        assert_eq!(w.control.snapshot().diagnostics.commands, 1);
        stop(&mut w);
    }
}

#[test]
fn worker_followup_answer_is_decoded_without_a_skip() {
    let p = Arc::new(Probe::default());
    let probe = p.clone();
    let mut w = start(move || Box::new(Timed(probe)));
    assert!(w.control.arm_followup_for(Duration::from_secs(8)));
    wait("followup listening", || {
        w.control.snapshot().state == State::CommandListening
    });
    // Words at offset 0 would be dropped if a stale skip leaked in.
    p.replies
        .lock()
        .unwrap()
        .push_back(Reply::Timed(vec![("yes", 0), ("please", 4_000)]));
    speech_utterance(&p);
    assert_eq!(take(&w), "yes please");
    assert_eq!(p.commands.lock().unwrap().as_slice(), &[(33_600, None)]);
    stop(&mut w);
}

#[test]
fn worker_followup_after_a_keyword_command_does_not_inherit_its_skip() {
    let p = Arc::new(Probe::default());
    let probe = p.clone();
    let mut w = start(move || Box::new(Timed(probe)));
    p.replies.lock().unwrap().push_back(Reply::Timed(vec![
        ("drivers", 0),
        ("what", 16_400),
        ("time", 20_000),
    ]));
    keyword_utterance(&p);
    assert_eq!(take(&w), "what time");
    assert!(w.control.arm_followup_for(Duration::from_secs(8)));
    wait("followup listening", || {
        w.control.snapshot().state == State::CommandListening
    });
    p.replies
        .lock()
        .unwrap()
        .push_back(Reply::Timed(vec![("tomorrow", 0)]));
    speech_utterance(&p);
    assert_eq!(take(&w), "tomorrow");
    let commands = p.commands.lock().unwrap().clone();
    assert_eq!(commands.len(), 2);
    assert!(commands[0].1.is_some());
    assert_eq!(commands[1].1, None);
    stop(&mut w);
}

#[test]
fn worker_all_residue_keeps_listening_and_next_utterance_becomes_the_command() {
    let p = Arc::new(Probe::default());
    let probe = p.clone();
    let mut w = start(move || Box::new(Timed(probe)));
    p.replies
        .lock()
        .unwrap()
        .push_back(Reply::Timed(vec![("Hey,", 0), ("drivers", 6_000)]));
    keyword_utterance(&p);
    wait("residue decode", || p.commands.lock().unwrap().len() == 1);
    wait("still listening for the request", || {
        w.control.snapshot().state == State::CommandListening
    });
    assert!(
        w.control.take_command().is_none(),
        "wake residue published as a command"
    );
    p.replies
        .lock()
        .unwrap()
        .push_back(Reply::Timed(vec![("play", 0), ("X", 8_000)]));
    speech_utterance(&p);
    assert_eq!(take(&w), "play X");
    let commands = p.commands.lock().unwrap().clone();
    assert_eq!(commands.len(), 2);
    assert!(commands[0].1.is_some());
    assert_eq!(commands[1], (33_600, None));
    assert_eq!(w.control.snapshot().diagnostics.commands, 1);
    stop(&mut w);
}

#[test]
fn worker_all_residue_then_silence_returns_to_wake_listening() {
    let p = Arc::new(Probe::default());
    let probe = p.clone();
    let mut w = start(move || Box::new(Timed(probe)));
    p.replies
        .lock()
        .unwrap()
        .push_back(Reply::Timed(vec![("nice", 0)]));
    keyword_utterance(&p);
    wait("residue decode", || p.commands.lock().unwrap().len() == 1);
    for _ in 0..11 {
        p.push(vec![0.0; 16_000]);
    }
    wait("back to wake listening", || {
        w.control.snapshot().state == State::WakeListening
    });
    assert!(w.control.take_command().is_none());
    // Non-keyword speech after the timeout is ambient, never a command.
    speech_utterance(&p);
    thread::sleep(Duration::from_millis(100));
    assert!(w.control.take_command().is_none());
    assert_eq!(p.commands.lock().unwrap().len(), 1);
    stop(&mut w);
}

#[test]
fn worker_recognizer_without_timing_still_strips_spoken_wake_phrase() {
    let p = Arc::new(Probe::default());
    let probe = p.clone();
    let mut w = start(move || Box::new(Legacy(probe)));
    p.replies
        .lock()
        .unwrap()
        .push_back(Reply::Raw("Hey Jarvis play X"));
    keyword_utterance(&p);
    assert_eq!(take(&w), "play X");
    assert_eq!(p.commands.lock().unwrap().len(), 1);
    stop(&mut w);
}

#[test]
fn worker_recognizer_that_ignores_skip_still_strips_spoken_wake_phrase() {
    let p = Arc::new(Probe::default());
    let probe = p.clone();
    let mut w = start(move || Box::new(Timed(probe)));
    p.replies
        .lock()
        .unwrap()
        .push_back(Reply::Raw("Hey Jarvis play X"));
    keyword_utterance(&p);
    assert_eq!(take(&w), "play X");
    assert!(p.commands.lock().unwrap()[0].1.is_some());
    stop(&mut w);
}

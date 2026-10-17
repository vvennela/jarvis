#![cfg(unix)]
//! Production voice handoff, actual worker threads, SQLite, loopback HTTP and a
//! silent owned playback child. No microphone, real provider or audible output.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::{
    context::{ContextStore, Limits, Source},
    conversation::Conversation,
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
};
use jarvis_panel::{
    chat::ConversationWorker,
    speech::SpeechWorker,
    wakeworker::{WakeBackend, WakeWorker},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use support::TempDir;

struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Peer {
    fn new(reply: &str) -> Self {
        Self::responding(reply, 200, Duration::ZERO)
    }
    fn responding(reply: &str, status: u16, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let body=json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":reply}}]}).to_string();
        let handle = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = socket.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    assert!(bytes.len() < 100_000);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            captured.lock().unwrap().push(
                                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                            );
                            break;
                        }
                    }
                }
                thread::sleep(delay);
                let wire = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                // A cancelled request intentionally closes its HTTP connection.
                let _ = socket.write_all(wire.as_bytes());
            }
        });
        Self {
            url,
            requests,
            stop,
            handle: Some(handle),
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }
}
#[derive(Default)]
struct AudioFixture {
    chunks: Mutex<VecDeque<Vec<f32>>>,
    texts: Mutex<VecDeque<String>>,
    decodes: AtomicUsize,
    polls: AtomicUsize,
    keyword_mode: AtomicBool,
    decoded_audio: Mutex<Vec<Vec<f32>>>,
}
impl AudioFixture {
    fn utterance(&self, text: &str) {
        self.texts.lock().unwrap().push_back(text.into());
        let mut chunks = self.chunks.lock().unwrap();
        chunks.push_back(vec![0.1; 16000]);
        chunks.push_back(vec![0.0; 17600]);
    }
}
struct Backend(Arc<AudioFixture>);
impl WakeBackend for Backend {
    fn start(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        self.0.polls.fetch_add(1, Ordering::Release);
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
    fn keyword_spot(
        &mut self,
        samples: &[f32],
    ) -> Result<Option<jarvis_panel::wakeworker::KeywordStatus>, &'static str> {
        if !self.0.keyword_mode.load(Ordering::Acquire) {
            return Ok(None);
        }
        Ok(Some(jarvis_panel::wakeworker::KeywordStatus {
            lookback_samples: (samples.first() == Some(&0.2)).then_some(samples.len()),
        }))
    }
    fn transcribe(&mut self, samples: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
        self.0.decoded_audio.lock().unwrap().push(samples.to_vec());
        self.0.decodes.fetch_add(1, Ordering::Release);
        Ok(self.0.texts.lock().unwrap().pop_front().unwrap())
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "voice-loop fixture timed out");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn woken_question_uses_shared_context_and_reply_reaches_owned_speech_child_once() {
    let dir = TempDir::new();
    assert!(dir.path().is_dir());
    let database = dir.join("context.db");
    let mut store = ContextStore::open(&database, Limits::default()).unwrap();
    store
        .observe(
            "file:orchid",
            "Orchid task: finish the latency benchmark",
            Source::File,
            10,
        )
        .unwrap();
    store
        .observe(
            "os:foreground",
            r#"{"actor":"unknown","window":"Orchid editor"}"#,
            Source::Accessibility,
            20,
        )
        .unwrap();
    drop(store);
    let response = "Resume the Orchid latency benchmark. Literal $(touch SHOULD_NOT_EXIST).";
    let peer = Peer::new(response);
    let client = CrusoeClient::new(
        "fixture-secret",
        NEMOTRON_MODEL,
        ProviderConfig::loopback_for_tests(&peer.url, Duration::from_secs(3)).unwrap(),
    )
    .unwrap();
    let mut chat =
        ConversationWorker::with_factory(move || Ok(Conversation::new(database, None, client)))
            .unwrap();
    let executable = dir.join("silent-speaker");
    fs::write(&executable,"#!/bin/sh\nfixture_dir=${0%/*}\nprintf '%s\\n' \"$@\" > \"$fixture_dir/args\"\ncat > \"$fixture_dir/spoken\"\nprintf 'played\\n' >> \"$fixture_dir/plays\"\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut speech = SpeechWorker::with_executable(executable).unwrap();
    let audio = Arc::new(AudioFixture::default());
    let backend = audio.clone();
    let mut wake = WakeWorker::with_backend(move || Ok(Box::new(Backend(backend)))).unwrap();
    wait(|| chat.control.available() && speech.control.available());
    wake.control.set_enabled(true);
    // The same handoff object is called by the production native panel loop.
    let mut handoff = jarvis_panel::voicehandoff::VoiceHandoff::default();
    wait(|| {
        handoff.tick(&wake.control, &chat.control, &speech.control);
        audio.polls.load(Ordering::Acquire) >= 2
    });
    audio.utterance("ordinary ambient words");
    wait(|| {
        handoff.tick(&wake.control, &chat.control, &speech.control);
        audio.decodes.load(Ordering::Acquire) >= 1
    });
    for _ in 0..10 {
        handoff.tick(&wake.control, &chat.control, &speech.control);
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        peer.requests.lock().unwrap().is_empty(),
        "ambient transcription reached provider"
    );
    assert!(!dir.join("spoken").exists());
    audio.utterance("Jarvis where was I?");
    wait(|| {
        handoff.tick(&wake.control, &chat.control, &speech.control);
        fs::read_to_string(dir.join("plays")).is_ok_and(|text| text == "played\n")
    });
    assert_eq!(fs::read_to_string(dir.join("spoken")).unwrap(), response);
    assert_eq!(fs::read_to_string(dir.join("args")).unwrap(), "-f\n-\n");
    for _ in 0..20 {
        handoff.tick(&wake.control, &chat.control, &speech.control);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read_to_string(dir.join("plays")).unwrap(), "played\n");
    let requests = peer.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let packet: Value = serde_json::from_str(
        requests[0]["messages"][1]["content"]
            .as_str()
            .unwrap()
            .strip_prefix("Context packet (history and source evidence):\n")
            .unwrap_or(requests[0]["messages"][1]["content"].as_str().unwrap()),
    )
    .unwrap();
    assert_eq!(packet["user_request"], "where was I?");
    let evidence = packet["untrusted_evidence"].as_array().unwrap();
    assert!(evidence.iter().any(|item| {
        item["resource_id"] == "file:orchid"
            && item["excerpt"]
                .as_str()
                .unwrap()
                .contains("latency benchmark")
            && item["trust"] == "untrusted_content"
    }));
    assert!(evidence.iter().any(|item|item["resource_id"]=="os:foreground"&&item["source"]=="accessibility"));
    drop(requests);
    wake.control.shutdown();
    chat.control.stop();
    speech.control.shutdown();
    wait(|| wake.is_finished() && chat.is_finished() && speech.is_finished());
    wake.join().unwrap();
    chat.join().unwrap();
    speech.join().unwrap();
}

struct FailureLoop {
    dir: TempDir,
    chat: ConversationWorker,
    speech: SpeechWorker,
    wake: WakeWorker,
    audio: Arc<AudioFixture>,
    handoff: jarvis_panel::voicehandoff::VoiceHandoff,
}
impl FailureLoop {
    fn new(peer: &Peer) -> Self {
        let dir = TempDir::new();
        let database = dir.join("context.db");
        drop(ContextStore::open(&database, Limits::default()).unwrap());
        let client = CrusoeClient::new(
            "fixture-secret",
            NEMOTRON_MODEL,
            ProviderConfig::loopback_for_tests(&peer.url, Duration::from_secs(3)).unwrap(),
        )
        .unwrap();
        let chat =
            ConversationWorker::with_factory(move || Ok(Conversation::new(database, None, client)))
                .unwrap();
        let executable = dir.join("silent-speaker");
        fs::write(&executable, "#!/bin/sh\nfixture_dir=${0%/*}\ncat > \"$fixture_dir/spoken\"\nprintf 'played\\n' >> \"$fixture_dir/plays\"\nwhile [ ! -e \"$fixture_dir/release\" ]; do sleep 0.01; done\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let speech = SpeechWorker::with_executable(executable).unwrap();
        let audio = Arc::new(AudioFixture::default());
        let backend = audio.clone();
        let wake = WakeWorker::with_backend(move || Ok(Box::new(Backend(backend)))).unwrap();
        wait(|| speech.control.available() && chat.control.available());
        wake.control.set_enabled(true);
        // Observe the actual startup drain before injecting new speech.
        wait(|| audio.polls.load(Ordering::Acquire) >= 2);
        Self {
            dir,
            chat,
            speech,
            wake,
            audio,
            handoff: Default::default(),
        }
    }
    fn tick(&mut self) {
        self.handoff
            .tick(&self.wake.control, &self.chat.control, &self.speech.control);
    }
    fn settle(&mut self) {
        for _ in 0..30 {
            self.tick();
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for FailureLoop {
    fn drop(&mut self) {
        self.wake.control.shutdown();
        self.chat.control.stop();
        self.speech.control.shutdown();
        self.wake.join().unwrap();
        self.chat.join().unwrap();
        self.speech.join().unwrap();
    }
}

#[test]
fn voice_provider_auth_failure_is_spoken_once_and_wake_resumes_after_playback() {
    let peer = Peer::responding(
        "UNTRUSTED_PROVIDER_BODY fixture-secret",
        401,
        Duration::ZERO,
    );
    let mut fixture = FailureLoop::new(&peer);
    fixture.audio.utterance("Jarvis where was I?");
    wait(|| {
        fixture.tick();
        fixture.dir.join("plays").exists()
    });
    let spoken = fs::read_to_string(fixture.dir.join("spoken")).unwrap();
    assert!(!spoken.trim().is_empty());
    assert_eq!(spoken, fixture.chat.control.snapshot().text);
    wait(|| {
        fixture.tick();
        fixture.wake.control.snapshot().state == jarvis_panel::wakeworker::State::WakeListening
    });
    assert_eq!(
        fixture.chat.control.snapshot().state,
        jarvis_panel::chat::State::Failed
    );
    assert!(!spoken.contains("UNTRUSTED_PROVIDER_BODY"));
    assert!(!spoken.contains("fixture-secret"));
    fs::write(fixture.dir.join("release"), b"done").unwrap();
    wait(|| {
        fixture.tick();
        !fixture.speech.control.busy()
            && fixture.wake.control.snapshot().state
                == jarvis_panel::wakeworker::State::WakeListening
    });
    fixture.settle();
    assert_eq!(
        fs::read_to_string(fixture.dir.join("plays")).unwrap(),
        "played\n"
    );
    assert_eq!(peer.requests.lock().unwrap().len(), 1);
}

#[test]
fn typed_provider_failure_does_not_speak_or_disable_wake() {
    let peer = Peer::responding("provider unavailable", 500, Duration::ZERO);
    let mut fixture = FailureLoop::new(&peer);
    fixture.chat.control.submit("Where was I?".into()).unwrap();
    wait(|| {
        fixture.tick();
        fixture.chat.control.snapshot().state == jarvis_panel::chat::State::Failed
            && !fixture.chat.control.busy()
    });
    fixture.settle();
    assert!(!fixture.dir.join("plays").exists());
    assert_eq!(
        fixture.wake.control.snapshot().state,
        jarvis_panel::wakeworker::State::WakeListening
    );
}

#[test]
fn cancelled_voice_request_does_not_speak_late_provider_failure() {
    let peer = Peer::responding("late provider failure", 500, Duration::from_millis(250));
    let mut fixture = FailureLoop::new(&peer);
    fixture.audio.utterance("Jarvis where was I?");
    wait(|| {
        fixture.tick();
        !peer.requests.lock().unwrap().is_empty()
    });
    fixture.chat.control.cancel();
    wait(|| {
        fixture.tick();
        fixture.chat.control.snapshot().state == jarvis_panel::chat::State::Cancelled
            && !fixture.chat.control.busy()
    });
    thread::sleep(Duration::from_millis(300));
    fixture.settle();
    assert!(!fixture.dir.join("plays").exists());
    assert_eq!(
        fixture.wake.control.snapshot().state,
        jarvis_panel::wakeworker::State::WakeListening
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires trusted local KWS/VAD/Whisper models and generated command WAV; synthesizes an owned audio file, no microphone or playback"]
#[cfg(feature = "whisper")]
fn generated_speech_reaches_real_local_recognition_conversation_and_local_speech_synthesis() {
    use jarvis_panel::wakeworker::KeywordStatus;
    struct LocalBackend {
        kws: jarvis_voice::kws::LocalKeywordSpotter,
        vad: jarvis_voice::vad::LocalVoiceActivity,
        stt: jarvis_voice::Transcriber,
        chunks: VecDeque<Vec<f32>>,
    }
    impl WakeBackend for LocalBackend {
        fn start(&mut self) -> Result<(), &'static str> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
            Ok((self.chunks.pop_front().unwrap_or_default(), false))
        }
        fn wake_fallback_enabled(&self) -> bool {
            true
        }
        fn speech_activity(&mut self, samples: &[f32]) -> Result<Option<Vec<bool>>, &'static str> {
            self.vad.classify(samples).map(Some)
        }
        fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
            Ok(Some(KeywordStatus {
                lookback_samples: self.kws.accept(samples)?.map(|v| v.lookback_samples),
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
                .map_err(|_| "fixture transcription failed")
        }
    }
    let path = |name| {
        std::path::PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("set {name}")))
    };
    let kws = path("JARVIS_TEST_KWS_MODEL");
    let vad = path("JARVIS_TEST_VAD_MODEL");
    let whisper = path("JARVIS_TEST_WHISPER_MODEL");
    let wav = path("JARVIS_TEST_WAKE_WAV");
    let mut samples = vec![0.0; 16000];
    samples.extend(jarvis_voice::load_wav(&wav).unwrap());
    samples.extend(vec![0.0; 32000]);
    let dir = TempDir::new();
    let database = dir.join("context.db");
    let mut store = ContextStore::open(&database, Limits::default()).unwrap();
    store
        .observe(
            "file:CURRENT_WORK.md",
            "Current task: complete the Orchid latency benchmark before the demo",
            Source::File,
            1,
        )
        .unwrap();
    drop(store);
    let response = "Finish the Orchid latency benchmark before the demo.";
    let peer = Peer::new(response);
    let client = CrusoeClient::new(
        "fixture-only-key",
        NEMOTRON_MODEL,
        ProviderConfig::loopback_for_tests(&peer.url, Duration::from_secs(3)).unwrap(),
    )
    .unwrap();
    let mut chat =
        ConversationWorker::with_factory(move || Ok(Conversation::new(database, None, client)))
            .unwrap();
    let executable = dir.join("synthesize-reply");
    fs::write(&executable,"#!/bin/sh\nfixture_dir=${0%/*}\n/bin/cat > \"$fixture_dir/spoken\"\n/usr/bin/say -f \"$fixture_dir/spoken\" -o \"$fixture_dir/reply.aiff\"\nprintf 'synthesized\\n' >> \"$fixture_dir/plays\"\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut speech = SpeechWorker::with_executable(executable).unwrap();
    let mut wake = WakeWorker::with_backend(move || {
        Ok(Box::new(LocalBackend {
            kws: jarvis_voice::kws::LocalKeywordSpotter::load(&kws)?,
            vad: jarvis_voice::vad::LocalVoiceActivity::load(&vad)?,
            stt: jarvis_voice::Transcriber::load(&whisper)
                .map_err(|_| "local model unavailable")?,
            chunks: samples.chunks(320).map(Vec::from).collect(),
        }))
    })
    .unwrap();
    wait(|| chat.control.available() && speech.control.available());
    wake.control.set_enabled(true);
    let mut handoff = jarvis_panel::voicehandoff::VoiceHandoff::default();
    let deadline = Instant::now() + Duration::from_secs(25);
    while !dir.join("plays").exists() {
        handoff.tick(&wake.control, &chat.control, &speech.control);
        assert!(
            Instant::now() < deadline,
            "real local voice loop did not finish: {:?}",
            wake.control.snapshot().state
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read_to_string(dir.join("spoken")).unwrap(), response);
    let audio = fs::read(dir.join("reply.aiff")).unwrap();
    assert!(audio.len() > 4096);
    assert_eq!(&audio[..4], b"FORM");
    assert_eq!(
        fs::read_to_string(dir.join("plays")).unwrap(),
        "synthesized\n"
    );
    let requests = peer.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let packet: Value = serde_json::from_str(
        requests[0]["messages"][1]["content"]
            .as_str()
            .unwrap()
            .strip_prefix("Context packet (history and source evidence):\n")
            .unwrap_or(requests[0]["messages"][1]["content"].as_str().unwrap()),
    )
    .unwrap();
    let question = packet["user_request"].as_str().unwrap().to_lowercase();
    assert!(question.contains("work") && question.contains("next"));
    assert!(!question.contains("jarvis"));
    assert!(
        packet["untrusted_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["excerpt"]
                .as_str()
                .unwrap_or_default()
                .contains("Orchid latency benchmark"))
    );
    drop(requests);
    wake.control.shutdown();
    chat.control.stop();
    speech.control.shutdown();
    wait(|| wake.is_finished() && chat.is_finished() && speech.is_finished());
    wake.join().unwrap();
    chat.join().unwrap();
    speech.join().unwrap();
    println!(
        "generated_speech_local_models=true cloud_response=localhost_fixture speech_file_verified=true requests=1 microphone=false audible_playback=false"
    );
}

#[test]
fn slow_model_keeps_wake_available_and_holds_exactly_one_following_command() {
    let peer = Peer::responding("A fixture response.", 200, Duration::from_millis(700));
    let mut fixture = FailureLoop::new(&peer);
    wait(|| {
        fixture.tick();
        fixture.audio.polls.load(Ordering::Acquire) >= 2
    });
    fixture.audio.utterance("Jarvis first question");
    wait(|| {
        fixture.tick();
        peer.requests.lock().unwrap().len() == 1
    });
    assert!(fixture.chat.control.busy());
    assert_ne!(
        fixture.wake.control.snapshot().state,
        jarvis_panel::wakeworker::State::Suppressed
    );
    fixture.audio.utterance("Jarvis second question");
    wait(|| {
        fixture.tick();
        fixture.audio.decodes.load(Ordering::Acquire) >= 2
            && fixture.wake.control.snapshot().state
                == jarvis_panel::wakeworker::State::CommandReady
    });
    assert_eq!(
        peer.requests.lock().unwrap().len(),
        1,
        "second request ran concurrently"
    );
    fixture.audio.utterance("Jarvis stale third question");
    wait(|| {
        fixture.tick();
        peer.requests.lock().unwrap().len() == 2
    });
    fs::write(fixture.dir.join("release"), b"done").unwrap();
    wait(|| {
        fixture.tick();
        !fixture.chat.control.busy()
    });
    fixture.settle();
    let requests = peer.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "queued audio was replayed after the bounded command slot"
    );
    let wire = requests[1].to_string();
    assert!(
        wire.contains("second question"),
        "unexpected synthetic request: {wire}"
    );
    assert!(!wire.contains("stale third question"));
}

#[test]
fn explicit_wake_interrupts_owned_speech_and_preserves_the_next_request_once() {
    let peer = Peer::new("A long synthetic answer.");
    let mut fixture = FailureLoop::new(&peer);
    fixture.audio.utterance("Jarvis first question");
    wait(|| {
        fixture.tick();
        fixture.dir.join("plays").exists() && fixture.speech.control.busy()
    });
    fixture.audio.keyword_mode.store(true, Ordering::Release);
    let decoded_before_echo = fixture.audio.decodes.load(Ordering::Acquire);
    let polls = fixture.audio.polls.load(Ordering::Acquire);
    fixture
        .audio
        .chunks
        .lock()
        .unwrap()
        .push_back(vec![0.1; 16000]);
    wait(|| {
        fixture.tick();
        fixture.audio.polls.load(Ordering::Acquire) > polls + 3
    });
    assert!(
        fixture.speech.control.busy(),
        "ordinary playback audio interrupted speech"
    );
    assert_eq!(
        fixture.audio.decodes.load(Ordering::Acquire),
        decoded_before_echo,
        "playback echo reached ASR"
    );
    assert_eq!(peer.requests.lock().unwrap().len(), 1);
    let epoch = fixture.speech.control.cancellation_epoch();
    let wake_count = fixture.wake.control.snapshot().diagnostics.wake_matches;
    fixture
        .audio
        .texts
        .lock()
        .unwrap()
        .push_back("Jarvis second question".into());
    fixture
        .audio
        .chunks
        .lock()
        .unwrap()
        .push_back(vec![0.2; 8000]);
    // Hold the host tick briefly after wake detection: the player still owns
    // speech, so any captured tail must be actively drained by the worker.
    wait(|| fixture.wake.control.snapshot().diagnostics.wake_matches > wake_count);
    fixture
        .audio
        .chunks
        .lock()
        .unwrap()
        .push_back(vec![0.75; 6400]);
    let polls_before_stop = fixture.audio.polls.load(Ordering::Acquire);
    wait(|| fixture.audio.polls.load(Ordering::Acquire) >= polls_before_stop + 2);
    wait(|| {
        fixture.tick();
        fixture.speech.control.cancellation_epoch() > epoch && !fixture.speech.control.busy()
    });
    // Additional old output arrives after process exit, during acoustic tail quarantine.
    fixture
        .audio
        .chunks
        .lock()
        .unwrap()
        .push_back(vec![0.75; 3200]);
    wait(|| {
        fixture.tick();
        fixture.wake.control.snapshot().state == jarvis_panel::wakeworker::State::CommandListening
    });
    assert!(
        !fixture.speech.control.busy(),
        "wake did not stop the owned player"
    );
    fixture
        .audio
        .chunks
        .lock()
        .unwrap()
        .push_back(vec![0.1; 8000]);
    fixture
        .audio
        .chunks
        .lock()
        .unwrap()
        .push_back(vec![0.; 17600]);
    wait(|| {
        fixture.tick();
        peer.requests.lock().unwrap().len() == 2
    });
    let requests = peer.requests.lock().unwrap();
    assert!(requests[1].to_string().contains("second question"));
    let decoded = fixture.audio.decoded_audio.lock().unwrap();
    assert!(
        !decoded.last().unwrap().contains(&0.75),
        "old spoken-reply samples reached ASR"
    );
    assert!(
        !decoded.last().unwrap().contains(&0.2),
        "wake-era playback pre-roll reached ASR"
    );
    drop(decoded);
    drop(requests);
    fs::write(fixture.dir.join("release"), b"done").unwrap();
    wait(|| {
        fixture.tick();
        !fixture.chat.control.busy() && !fixture.speech.control.busy()
    });
    fixture.settle();
    assert_eq!(peer.requests.lock().unwrap().len(), 2);
}

#[test]
fn spoken_copy_cannot_repeat_its_own_standalone_wake_word() {
    use jarvis_panel::speech::speech_without_wake_word;
    assert_eq!(
        speech_without_wake_word("Hey Jarvis, ask JARVIS. Jarvisian Jarvises."),
        "Hey the assistant, ask the assistant. Jarvisian Jarvises."
    );
    assert_eq!(
        speech_without_wake_word("A literal $(touch nope) reply."),
        "A literal $(touch nope) reply."
    );
}

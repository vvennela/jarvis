#![cfg(unix)]
//! Hands-free conversation: production handoff and workers, loopback provider,
//! silent owned playback. No microphone, real provider or audible output.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::{
    context::{ContextStore, Limits, Source},
    conversation::Conversation,
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
    tool_runtime::ToolRuntime,
};
use jarvis_panel::{
    chat::ConversationWorker,
    speech::SpeechWorker,
    voicehandoff::{HandsFree, MAX_HANDS_FREE_TURNS, VoiceHandoff, classify_hands_free},
    wakeworker::{State, WakeBackend, WakeWorker},
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
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let mut replies = VecDeque::from(
            (1..=20)
                .map(|n| json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":format!("Answer number {n}.")}}]}))
                .collect::<Vec<_>>(),
        );
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
                let body = replies
                    .pop_front()
                    .expect("unexpected additional provider request")
                    .to_string();
                let wire = format!(
                    "HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
    discontinuity: AtomicBool,
    decode_delay_ms: AtomicUsize,
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
        Ok((
            self.0
                .chunks
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default(),
            self.0.discontinuity.swap(false, Ordering::AcqRel),
        ))
    }
    fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
        self.0.decodes.fetch_add(1, Ordering::Release);
        thread::sleep(Duration::from_millis(
            self.0.decode_delay_ms.load(Ordering::Acquire) as u64,
        ));
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

struct Rig {
    dir: TempDir,
    peer: Peer,
    audio: Arc<AudioFixture>,
    wake: WakeWorker,
    chat: ConversationWorker,
    speech: SpeechWorker,
    handoff: VoiceHandoff,
}
impl Rig {
    fn new() -> Self {
        let dir = TempDir::new();
        let database = dir.join("context.db");
        let mut store = ContextStore::open(&database, Limits::default()).unwrap();
        store
            .observe(
                "file:beacon",
                "Beacon current task: review the launch checklist",
                Source::File,
                1,
            )
            .unwrap();
        drop(store);
        let peer = Peer::new();
        let client = CrusoeClient::new(
            "fixture-only",
            NEMOTRON_MODEL,
            ProviderConfig::loopback_for_tests(&peer.url, Duration::from_secs(2)).unwrap(),
        )
        .unwrap();
        let chat = ConversationWorker::with_factory(move || {
            let mut conversation = Conversation::new(database.clone(), None, client);
            conversation.set_tool_runtime(Arc::new(ToolRuntime::read_only(database)));
            Ok(conversation)
        })
        .unwrap();
        // Each utterance is spoken by a child that exits immediately.
        let executable = dir.join("silent-speaker");
        fs::write(
            &executable,
            "#!/bin/sh\ncat > /dev/null\nsleep 0.1\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let speech = SpeechWorker::with_executable(executable).unwrap();
        let audio = Arc::new(AudioFixture::default());
        let backend = audio.clone();
        let wake = WakeWorker::with_backend(move || Ok(Box::new(Backend(backend)))).unwrap();
        wait(|| chat.control.available() && speech.control.available());
        wake.control.set_enabled(true);
        wait(|| wake.control.snapshot().state == State::WakeListening);
        Self {
            dir,
            peer,
            audio,
            wake,
            chat,
            speech,
            handoff: VoiceHandoff::conversational(),
        }
    }
    fn requests(&self) -> usize {
        self.peer.requests.lock().unwrap().len()
    }
    fn until(&mut self, what: &str, mut condition: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.handoff
                .tick(&self.wake.control, &self.chat.control, &self.speech.control);
            if condition(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn pump(&mut self, duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            self.handoff
                .tick(&self.wake.control, &self.chat.control, &self.speech.control);
            thread::sleep(Duration::from_millis(5));
        }
    }
    /// Say something, then wait until its reply was spoken and the
    /// conversational window (if any) has had a chance to open.
    fn turn(&mut self, text: &str, expected_requests: usize) {
        self.audio.utterance(text);
        self.until("provider request", |s| s.requests() == expected_requests);
        self.until("reply playback", |s| s.speech.control.busy());
        self.until("playback end", |s| !s.speech.control.busy());
        self.pump(Duration::from_millis(300));
    }
    fn listening_hands_free(&self) -> bool {
        self.wake.control.snapshot().state == State::CommandListening
    }
}
impl Drop for Rig {
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
fn voice_reply_opens_one_hands_free_turn_that_reaches_the_conversation() {
    let mut rig = Rig::new();
    rig.turn("Jarvis what should I work on", 1);
    assert!(
        rig.listening_hands_free(),
        "reply did not open a conversational window"
    );
    rig.turn("and what comes after that", 2);
    let requests = rig.peer.requests.lock().unwrap();
    let last = requests[1]["messages"].as_array().unwrap().last().unwrap()["content"].clone();
    assert_eq!(last, "and what comes after that");
    drop(requests);
    assert!(rig.listening_hands_free(), "conversation did not continue");
    assert!(rig.dir.path().is_dir());
}

#[test]
fn filler_and_single_words_from_the_room_are_ignored_and_close_the_window() {
    for ambient in ["um", "yeah okay", "hmm", "so"] {
        let mut rig = Rig::new();
        rig.turn("Jarvis what should I work on", 1);
        rig.audio.utterance(ambient);
        rig.pump(Duration::from_millis(400));
        assert_eq!(rig.requests(), 1, "{ambient} reached the model");
        // A later sentence without the wake phrase is not a command either.
        rig.audio.utterance("the meeting moved to three");
        rig.pump(Duration::from_millis(400));
        assert_eq!(rig.requests(), 1, "window stayed open after {ambient}");
    }
}

#[test]
fn closers_end_the_conversation_without_a_model_call() {
    for closer in ["Thanks.", "thank you Jarvis", "that's all", "Never mind"] {
        let mut rig = Rig::new();
        rig.turn("Jarvis what should I work on", 1);
        rig.audio.utterance(closer);
        rig.pump(Duration::from_millis(400));
        assert_eq!(rig.requests(), 1, "{closer} reached the model");
        assert!(!rig.listening_hands_free());
        rig.audio.utterance("what about the launch checklist");
        rig.pump(Duration::from_millis(400));
        assert_eq!(rig.requests(), 1, "conversation continued after {closer}");
    }
}

#[test]
fn hands_free_chain_is_bounded_and_the_wake_phrase_starts_a_new_one() {
    let mut rig = Rig::new();
    rig.turn("Jarvis start", 1);
    for turn in 0..MAX_HANDS_FREE_TURNS as usize {
        assert!(rig.listening_hands_free(), "turn {turn} had no window");
        rig.turn(&format!("tell me about part {turn}"), turn + 2);
    }
    assert!(!rig.listening_hands_free(), "chain exceeded its bound");
    rig.audio.utterance("one more thing please");
    rig.pump(Duration::from_millis(400));
    let sent = rig.requests();
    assert_eq!(sent, MAX_HANDS_FREE_TURNS as usize + 1);
    rig.turn("Jarvis new topic please", sent + 1);
    assert!(
        rig.listening_hands_free(),
        "wake phrase did not reset the chain"
    );
}

#[test]
fn typed_questions_never_open_the_microphone() {
    let mut rig = Rig::new();
    rig.chat
        .control
        .submit("what should I work on".into())
        .unwrap();
    rig.until("typed reply", |s| {
        s.requests() == 1 && !s.chat.control.busy()
    });
    rig.pump(Duration::from_millis(300));
    assert!(!rig.listening_hands_free());
    rig.audio.utterance("this is just the room talking");
    rig.pump(Duration::from_millis(400));
    assert_eq!(rig.requests(), 1);
}

#[test]
fn classifier_accepts_short_replies_and_rejects_noise() {
    for accepted in [
        "yes",
        "No.",
        "send it",
        "read it back",
        "play some Metallica",
        "what time is it",
    ] {
        assert_eq!(
            classify_hands_free(accepted),
            HandsFree::Accept,
            "{accepted}"
        );
    }
    for ignored in ["", "um", "uh huh", "okay", "hmm hmm", "yeah okay so"] {
        assert_eq!(classify_hands_free(ignored), HandsFree::Ignore, "{ignored}");
    }
    for closer in ["thanks", "Thank you!", "that's it", "bye"] {
        assert_eq!(classify_hands_free(closer), HandsFree::End, "{closer}");
    }
}

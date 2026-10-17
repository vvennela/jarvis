#![cfg(unix)]
//! Production voice handoff, actual worker threads, SQLite, loopback HTTP and a
//! silent owned playback child. No microphone, real provider or audible output.
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
    fn new(clarification: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let question = "Which project should I summarize?";
        let first = if clarification {
            json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"ask1","type":"function","function":{"name":"ask_user","arguments":json!({"question":question}).to_string()}}]}}]})
        } else {
            json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":question}}]})
        };
        let mut replies = VecDeque::from([
            first,
            json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"Beacon is ready for review."}}]}),
        ]);
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
    handoff: jarvis_panel::voicehandoff::VoiceHandoff,
}
impl Rig {
    fn new(clarification: bool, fail_speech: bool) -> Self {
        let dir = TempDir::new();
        assert!(dir.path().is_dir());
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
        let peer = Peer::new(clarification);
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
        let executable = dir.join("silent-speaker");
        let exit = if fail_speech { "exit 1" } else { "exit 0" };
        fs::write(&executable, format!("#!/bin/sh\nfixture_dir=${{0%/*}}\ncat > \"$fixture_dir/spoken\"\ntouch \"$fixture_dir/started\"\nwhile [ ! -f \"$fixture_dir/release\" ]; do sleep 0.01; done\n{exit}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let speech = SpeechWorker::with_executable(executable).unwrap();
        let audio = Arc::new(AudioFixture::default());
        let backend = audio.clone();
        let wake = WakeWorker::with_backend(move || Ok(Box::new(Backend(backend)))).unwrap();
        wait(|| chat.control.available() && speech.control.available());
        wake.control.set_enabled(true);
        wait(|| wake.control.snapshot().state == jarvis_panel::wakeworker::State::WakeListening);
        Self {
            dir,
            peer,
            audio,
            wake,
            chat,
            speech,
            handoff: Default::default(),
        }
    }
    fn tick(&mut self) {
        self.handoff
            .tick(&self.wake.control, &self.chat.control, &self.speech.control);
    }
    fn until(&mut self, mut condition: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.tick();
            if condition(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "clarification voice fixture timed out"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn first_question(&mut self) {
        self.audio.utterance("Jarvis summarize my project");
        self.until(|s| s.dir.join("started").exists());
        assert_eq!(self.peer.requests.lock().unwrap().len(), 1);
        assert!(self.speech.control.busy());
        assert_ne!(
            self.wake.control.snapshot().state,
            jarvis_panel::wakeworker::State::CommandListening
        );
    }
    fn release(&mut self) {
        fs::write(self.dir.join("release"), "").unwrap();
        self.until(|s| !s.speech.control.busy());
    }
    fn pump(&mut self, duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            self.tick();
            thread::sleep(Duration::from_millis(5));
        }
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
fn typed_clarification_waits_for_owned_speech_then_accepts_one_answer_without_wake_word() {
    let mut rig = Rig::new(true, false);
    rig.first_question();
    assert!(rig.chat.control.snapshot().needs_followup);
    rig.release();
    rig.until(|s| {
        s.wake.control.snapshot().state == jarvis_panel::wakeworker::State::CommandListening
    });
    rig.audio.utterance("Beacon");
    rig.until(|s| s.peer.requests.lock().unwrap().len() == 2 && !s.chat.control.busy());
    rig.pump(Duration::from_millis(150));
    let requests = rig.peer.requests.lock().unwrap();
    let packet: Value = serde_json::from_str(
        requests[1]["messages"][1]["content"]
            .as_str()
            .unwrap()
            .strip_prefix("Context packet (history and source evidence):\n")
            .unwrap_or(requests[1]["messages"][1]["content"].as_str().unwrap()),
    )
    .unwrap();
    assert_eq!(packet["user_request"], "Beacon");
    assert_eq!(
        packet["history"][0]["assistant"],
        "Which project should I summarize?"
    );
    drop(requests);
    assert!(!rig.chat.control.snapshot().needs_followup);
    rig.audio.utterance("ordinary ambient answer");
    rig.pump(Duration::from_millis(300));
    assert_eq!(rig.peer.requests.lock().unwrap().len(), 2);
}

#[test]
fn ordinary_question_or_failed_speech_never_opens_a_followup() {
    for (typed, failed) in [(false, false), (true, true)] {
        let mut rig = Rig::new(typed, failed);
        rig.first_question();
        rig.release();
        rig.pump(Duration::from_millis(150));
        rig.audio.utterance("Beacon");
        rig.pump(Duration::from_millis(250));
        assert_eq!(rig.peer.requests.lock().unwrap().len(), 1);
        assert_ne!(
            rig.wake.control.snapshot().state,
            jarvis_panel::wakeworker::State::CommandListening
        );
    }
}

#[test]
fn mute_chat_cancel_or_speech_cancel_during_question_invalidates_followup() {
    for mode in 0..3 {
        let mut rig = Rig::new(true, false);
        rig.first_question();
        match mode {
            0 => rig.wake.control.set_enabled(false),
            1 => rig.chat.control.cancel(),
            _ => rig.speech.control.stop_speaking(),
        }
        rig.release();
        rig.pump(Duration::from_millis(100));
        if mode == 0 {
            rig.wake.control.set_enabled(true);
        }
        rig.pump(Duration::from_millis(100));
        rig.audio.utterance("Beacon");
        rig.pump(Duration::from_millis(250));
        assert_eq!(
            rig.peer.requests.lock().unwrap().len(),
            1,
            "invalidated question rearmed microphone"
        );
    }
}

#[test]
fn explicit_followup_window_expires_and_route_change_drops_armed_answer() {
    for route_change in [false, true] {
        let mut rig = Rig::new(false, false);
        assert!(!rig.wake.control.arm_followup_for(Duration::ZERO));
        assert!(!rig.wake.control.arm_followup_for(Duration::from_secs(9)));
        assert!(
            rig.wake
                .control
                .arm_followup_for(Duration::from_millis(120))
        );
        rig.until(|s| {
            s.wake.control.snapshot().state == jarvis_panel::wakeworker::State::CommandListening
        });
        if route_change {
            rig.audio.discontinuity.store(true, Ordering::Release);
        }
        rig.pump(Duration::from_millis(200));
        rig.audio.utterance("Beacon");
        rig.pump(Duration::from_millis(250));
        assert!(rig.wake.control.take_command().is_none());
        assert!(rig.peer.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn transcription_finishing_after_followup_deadline_is_discarded() {
    let mut rig = Rig::new(false, false);
    rig.audio.decode_delay_ms.store(300, Ordering::Release);
    assert!(
        rig.wake
            .control
            .arm_followup_for(Duration::from_millis(120))
    );
    rig.until(|s| {
        s.wake.control.snapshot().state == jarvis_panel::wakeworker::State::CommandListening
    });
    rig.audio.utterance("Beacon");
    rig.pump(Duration::from_millis(450));
    assert!(rig.audio.decodes.load(Ordering::Acquire) > 0);
    assert!(rig.peer.requests.lock().unwrap().is_empty());
    assert!(rig.wake.control.take_command().is_none());
}

#[test]
fn cancelling_a_completed_question_before_playback_prevents_speech_and_followup() {
    let mut rig = Rig::new(true, false);
    rig.audio.utterance("Jarvis summarize my project");
    wait(|| rig.wake.control.snapshot().state == jarvis_panel::wakeworker::State::CommandReady);
    rig.tick();
    wait(|| {
        rig.chat.control.snapshot().state == jarvis_panel::chat::State::Reply
            && !rig.chat.control.busy()
    });
    rig.chat.control.cancel();
    rig.pump(Duration::from_millis(200));
    assert!(!rig.dir.join("started").exists());
    assert_ne!(
        rig.wake.control.snapshot().state,
        jarvis_panel::wakeworker::State::CommandListening
    );
    assert_eq!(rig.peer.requests.lock().unwrap().len(), 1);
}

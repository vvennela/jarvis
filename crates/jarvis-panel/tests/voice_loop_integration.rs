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
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(wire.as_bytes()).unwrap();
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
}
impl AudioFixture {
    fn utterance(&self, text: &str) {
        self.texts.lock().unwrap().push_back(text.into());
        let mut chunks = self.chunks.lock().unwrap();
        chunks.push_back(vec![0.1; 16000]);
        chunks.push_back(vec![0.0; 12000]);
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
            false,
        ))
    }
    fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
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
    let packet: Value =
        serde_json::from_str(requests[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
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

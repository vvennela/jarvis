#![cfg(unix)]
//! Real dispatcher/handoff/worker threads with owned process and localhost peers.
//! No native desktop, music playback, microphone, credentials, or provider calls.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::{
    actions::ActionExecutor,
    context::{ContextStore, Limits},
    conversation::{Conversation, ConversationError},
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
};
use jarvis_panel::{
    chat::{ConversationWorker, State},
    speech::SpeechWorker,
    voicehandoff::VoiceHandoff,
    wakeworker::{WakeBackend, WakeWorker},
};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    thread,
    time::{Duration, Instant},
};
use support::TempDir;
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !ready() {
        assert!(Instant::now() < deadline, "action fixture deadline");
        thread::sleep(Duration::from_millis(5));
    }
}
fn script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nfixture_dir=${{0%/*}}\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn executor(dir: &TempDir, body: &str) -> ActionExecutor {
    ActionExecutor::with_executable(script(dir, "action", body), Duration::from_secs(2)).unwrap()
}
fn no_provider_worker(dir: &TempDir, body: &str) -> ConversationWorker {
    ConversationWorker::with_factory_and_actions(
        || Err(ConversationError::MissingKey("CRUSOE_API_KEY")),
        executor(dir, body),
    )
    .unwrap()
}
fn speaker(dir: &TempDir) -> SpeechWorker {
    SpeechWorker::with_executable(script(
        dir,
        "speaker",
        "cat > \"$fixture_dir/spoken\"\nprintf 'played\\n' >> \"$fixture_dir/plays\"",
    ))
    .unwrap()
}
struct Audio {
    chunks: VecDeque<Vec<f32>>,
    text: String,
}
impl WakeBackend for Audio {
    fn start(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        Ok((self.chunks.pop_front().unwrap_or_default(), false))
    }
    fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
        Ok(self.text.clone())
    }
}
fn wake(text: &str) -> WakeWorker {
    let text = text.to_owned();
    let worker = WakeWorker::with_backend(move || {
        Ok(Box::new(Audio {
            chunks: VecDeque::from([vec![0.1; 16000], vec![0.; 17600]]),
            text,
        }))
    })
    .unwrap();
    worker.control.set_enabled(true);
    worker
}
fn stop(chat: &mut ConversationWorker) {
    chat.control.stop();
    wait(|| chat.is_finished());
    chat.join().unwrap();
}
#[test]
fn spoken_spotify_request_executes_and_speaks_verified_result_without_any_model() {
    for (utterance, postcondition, expected) in [
        ("Jarvis open Spotify", "verified:running", "open"),
        (
            "Jarvis can you open Spotify and play some music",
            "verified:playing",
            "playing",
        ),
        (
            "Jarvis open Spotify and play in the background",
            "verified:playing",
            "playing",
        ),
    ] {
        let dir = TempDir::new();
        assert!(dir.path().is_dir());
        let mut chat = no_provider_worker(
            &dir,
            &format!(
                "printf '%s\\n' \"$@\" > \"$fixture_dir/action-args\"\nprintf '{postcondition}\\n'"
            ),
        );
        let mut speech = speaker(&dir);
        wait(|| chat.control.available() && speech.control.available());
        let mut wake = wake(utterance);
        let mut handoff = VoiceHandoff::new();
        wait(|| {
            handoff.tick(&wake.control, &chat.control, &speech.control);
            dir.join("plays").exists() && !speech.control.busy()
        });
        assert_eq!(chat.control.snapshot().state, State::Reply);
        assert_eq!(
            fs::read_to_string(dir.join("spoken")).unwrap(),
            chat.control.snapshot().text
        );
        assert!(
            chat.control
                .snapshot()
                .text
                .to_lowercase()
                .contains(expected)
        );
        assert!(chat.control.snapshot().text.split_whitespace().count() <= 12);
        assert!(
            fs::read_to_string(dir.join("action-args"))
                .unwrap()
                .starts_with("-e\n")
        );
        for _ in 0..20 {
            handoff.tick(&wake.control, &chat.control, &speech.control);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fs::read_to_string(dir.join("plays")).unwrap(), "played\n");
        wake.control.shutdown();
        wait(|| wake.is_finished());
        wake.join().unwrap();
        speech.control.shutdown();
        wait(|| speech.is_finished());
        speech.join().unwrap();
        stop(&mut chat);
    }
}
#[test]
fn cancellation_of_action_drops_owned_process_without_publishing_late_success() {
    let dir = TempDir::new();
    let mut chat = no_provider_worker(
        &dir,
        "printf '%s' \"$$\" > \"$fixture_dir/pid\"\nexec /bin/sleep 5",
    );
    wait(|| chat.control.available());
    chat.control.submit("open Spotify".into()).unwrap();
    wait(|| dir.join("pid").exists());
    let start = Instant::now();
    chat.control.cancel();
    wait(|| !chat.control.busy());
    assert!(start.elapsed() < Duration::from_millis(400));
    assert_eq!(chat.control.snapshot().state, State::Cancelled);
    let pid = fs::read_to_string(dir.join("pid"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    wait(|| {
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    });
    thread::sleep(Duration::from_millis(100));
    assert_eq!(chat.control.snapshot().state, State::Cancelled);
    stop(&mut chat);
}
#[test]
fn ambiguous_spotify_request_asks_for_clarification_without_native_execution() {
    for (request, selection) in [
        ("play a song on Spotify", "a song"),
        ("play Metallica on Spotify", "Metallica"),
    ] {
        let dir = TempDir::new();
        let mut chat = no_provider_worker(
            &dir,
            "printf 'invoked' > \"$fixture_dir/executed\"\nprintf 'verified:playing\\n'",
        );
        wait(|| chat.control.available());
        chat.control.submit(request.into()).unwrap();
        wait(|| !chat.control.busy());
        let result = chat.control.snapshot();
        assert_eq!(result.state, State::Reply);
        assert!(
            result.text.contains("can't pick") && result.text.contains(selection),
            "unsupported selection should be explained honestly: {}",
            result.text
        );
        assert!(!result.text.contains("playing"));
        assert!(result.text.split_whitespace().count() <= 20);
        assert!(!dir.join("executed").exists());
        stop(&mut chat);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_reply_that_names_an_action_is_spoken_as_text_and_never_executed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = TempDir::new();
    let database = dir.join("context.db");
    drop(ContextStore::open(&database, Limits::default()).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            assert!(bytes.len() < 100000);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]);
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let body = r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"open Spotify"}}]}"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let client = CrusoeClient::new(
        "fixture-key",
        NEMOTRON_MODEL,
        ProviderConfig::loopback_for_tests(&endpoint, Duration::from_secs(2)).unwrap(),
    )
    .unwrap();
    let mut chat = ConversationWorker::with_factory_and_actions(
        move || Ok(Conversation::new(database, None, client)),
        executor(
            &dir,
            "printf 'invoked' > \"$fixture_dir/executed\"\nprintf 'verified:running\\n'",
        ),
    )
    .unwrap();
    let mut speech = speaker(&dir);
    wait(|| chat.control.available() && speech.control.available());
    let mut wake = wake("Jarvis say a sample phrase");
    let mut handoff = VoiceHandoff::new();
    wait(|| {
        handoff.tick(&wake.control, &chat.control, &speech.control);
        dir.join("plays").exists() && !speech.control.busy()
    });
    assert_eq!(
        fs::read_to_string(dir.join("spoken")).unwrap(),
        "open Spotify"
    );
    assert!(
        !dir.join("executed").exists(),
        "model response became an executable request"
    );
    server.await.unwrap();
    wake.control.shutdown();
    wait(|| wake.is_finished());
    wake.join().unwrap();
    speech.control.shutdown();
    wait(|| speech.is_finished());
    speech.join().unwrap();
    stop(&mut chat);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_verified_action_results_reach_followup_conversation_history() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for outcome in ["success", "failure", "cancel"] {
        let dir = TempDir::new();
        let database = dir.join("context.db");
        drop(ContextStore::open(&database, Limits::default()).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            let packet = loop {
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                assert!(bytes.len() < 100000);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break serde_json::from_slice::<serde_json::Value>(
                            &bytes[end + 4..end + 4 + length],
                        )
                        .unwrap();
                    }
                }
            };
            let body = r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"fixture followup"}}]}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            packet
        });
        let client = CrusoeClient::new(
            "fixture-key",
            NEMOTRON_MODEL,
            ProviderConfig::loopback_for_tests(&endpoint, Duration::from_secs(2)).unwrap(),
        )
        .unwrap();
        let body = match outcome {
            "success" => "printf 'verified:running\\n'",
            "failure" => "printf 'permission_denied\\n'",
            _ => "printf 'started' > \"$fixture_dir/started\"\nexec /bin/sleep 5",
        };
        let mut chat = ConversationWorker::with_factory_and_actions(
            move || Ok(Conversation::new(database, None, client)),
            executor(&dir, body),
        )
        .unwrap();
        wait(|| chat.control.available());
        chat.control.submit("open Spotify".into()).unwrap();
        if outcome == "cancel" {
            wait(|| dir.join("started").exists());
            chat.control.cancel();
        }
        wait(|| !chat.control.busy());
        chat.control.submit("What did you do?".into()).unwrap();
        wait(|| !chat.control.busy());
        assert_eq!(chat.control.snapshot().state, State::Reply);
        let request = server.await.unwrap();
        let packet: serde_json::Value = serde_json::from_str(
            request["messages"][1]["content"]
                .as_str()
                .unwrap()
                .strip_prefix("Context packet (history and source evidence):\n")
                .unwrap_or(request["messages"][1]["content"].as_str().unwrap()),
        )
        .unwrap();
        let history = packet["history"].as_array().unwrap();
        if outcome == "success" {
            assert_eq!(history.len(), 1);
            assert_eq!(history[0]["user"], "open Spotify");
            let text = history[0]["assistant"].as_str().unwrap();
            assert!(text.contains("Verified app action result at that time"));
            assert!(text.contains("Spotify's open"));
        } else {
            assert!(
                history.is_empty(),
                "unverified/cancelled action became success history"
            );
        }
        stop(&mut chat);
    }
}

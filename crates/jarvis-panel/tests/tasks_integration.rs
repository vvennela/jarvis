#![cfg(unix)]
//! Real background draft worker, local HTTP, SQLite and a silent review opener.
use jarvis::{
    context::{ContextStore, Limits, Source},
    draft::DraftTask,
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
};
use jarvis_panel::tasks::{State, SubmitError, TaskWorker, is_draft_request};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[path = "../../../tests/support/mod.rs"]
mod support;
fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "conversation recovery deadline");
        thread::sleep(Duration::from_millis(2));
    }
}
struct Peer {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    request_times: Arc<Mutex<Vec<Instant>>>,
    stop: Arc<AtomicBool>,
    hold: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Peer {
    fn new(statuses: Vec<u16>) -> Self {
        Self::with_retry_after(statuses, None)
    }
    fn with_retry_after(statuses: Vec<u16>, retry_after: Option<&str>) -> Self {
        let retry_header = retry_after
            .map(|v| format!("Retry-After: {v}\r\n"))
            .unwrap_or_default();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let request_times = Arc::new(Mutex::new(Vec::new()));
        let times = request_times.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let hold = Arc::new(AtomicBool::new(false));
        let holding = hold.clone();
        let handle = thread::spawn(move || {
            let mut statuses = VecDeque::from(statuses);
            while !stopped.load(Ordering::Acquire) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let body = loop {
                    let count = socket.read(&mut chunk).unwrap();
                    assert_ne!(count, 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    assert!(bytes.len() < 100000);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let size = headers
                            .lines()
                            .find_map(|line| {
                                let (k, v) = line.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + size])
                                .unwrap();
                        }
                    }
                };
                captured.lock().unwrap().push(body);
                times.lock().unwrap().push(Instant::now());
                while holding.load(Ordering::Acquire) && !stopped.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(2));
                }
                let status = statuses.pop_front().unwrap_or(200);
                let body = if status == 200 {
                    r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"successful fixture response"}}]}"#
                } else {
                    "PRIVATE_ERROR_BODY fixture-secret-key must never appear"
                };
                // Zero is a fixture plan for malformed JSON under HTTP 200.
                let status = if status == 0 { 200 } else { status };
                let response = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\n{retry_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes());
            }
        });
        Self {
            endpoint,
            requests,
            request_times,
            stop,
            hold,
            thread: Some(handle),
        }
    }
    fn worker(&self, temp: &support::TempDir, opener: PathBuf) -> TaskWorker {
        let db = temp.join("context.db");
        let endpoint = self.endpoint.clone();
        TaskWorker::with_factory(
            db,
            temp.join("drafts"),
            move |database, job, request| {
                let config =
                    ProviderConfig::loopback_for_tests(&endpoint, Duration::from_secs(3)).unwrap();
                let lm = CrusoeClient::new("fixture-key", NEMOTRON_MODEL, config).unwrap();
                DraftTask::new(database, lm, job, request)
            },
            opener,
        )
        .unwrap()
    }
    fn packets(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                serde_json::from_str(
                    request["messages"][1]["content"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("Context packet (history and source evidence):\n")
                        .unwrap_or(request["messages"][1]["content"].as_str().unwrap()),
                )
                .unwrap()
            })
            .collect()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn finish(worker: &mut TaskWorker) {
    worker.control.shutdown();
    wait(|| worker.is_finished());
    worker.join().unwrap();
}
fn seed(temp: &support::TempDir) -> ContextStore {
    let mut db = ContextStore::open(&temp.join("context.db"), Limits::default()).unwrap();
    db.observe(
        "file:beacon",
        "Project Beacon current task: prepare Brian handoff. Review time Friday2PM.",
        Source::File,
        1,
    )
    .unwrap();
    db
}
fn opener(temp: &support::TempDir) -> PathBuf {
    let path = temp.join("record-opener");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'/opened-args\n",
            temp.path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[test]
fn draft_completion_is_unsent_and_opens_only_after_explicit_verified_review() {
    let temp = support::TempDir::new();
    let _db = seed(&temp);
    let peer = Peer::new(vec![200]);
    let mut worker = peer.worker(&temp, opener(&temp));
    assert_eq!(worker.control.request_review(), Err(SubmitError::NotReady));
    worker
        .control
        .submit("Draft a handoff for Brian about Beacon".into())
        .unwrap();
    wait(|| !worker.control.busy());
    let ready = worker.control.snapshot();
    assert_eq!(ready.state, State::ReadyForReview);
    let path = ready.path.unwrap();
    assert!(path.is_absolute());
    assert_eq!(
        path.parent(),
        Some(fs::canonicalize(temp.join("drafts")).unwrap().as_path())
    );
    let contents = fs::read_to_string(&path).unwrap();
    assert!(contents.contains("NOT SENT"));
    assert!(contents.contains("successful fixture response"));
    assert!(
        !temp.join("opened-args").exists(),
        "completion must not open external applications"
    );
    let packets = peer.packets();
    assert_eq!(packets.len(), 1);
    assert_eq!(
        packets[0]["untrusted_evidence"][0]["resource_id"],
        "file:beacon"
    );
    assert_eq!(packets[0]["untrusted_evidence"][0]["version"], 1);
    worker.control.request_review().unwrap();
    wait(|| !worker.control.busy());
    let opened = fs::read_to_string(temp.join("opened-args")).unwrap();
    assert_eq!(
        opened.lines().collect::<Vec<_>>(),
        vec![path.to_str().unwrap()]
    );
    assert_eq!(worker.control.snapshot().state, State::ReadyForReview);
    assert!(worker.control.snapshot().text.contains("Nothing was sent"));
    finish(&mut worker);
}

#[test]
fn source_or_artifact_mutation_revokes_ready_state_before_any_viewer_is_started() {
    for source_change in [false, true] {
        let temp = support::TempDir::new();
        let mut db = seed(&temp);
        let peer = Peer::new(vec![200]);
        let mut worker = peer.worker(&temp, opener(&temp));
        worker
            .control
            .submit("Draft a handoff about Beacon".into())
            .unwrap();
        wait(|| !worker.control.busy());
        let path = worker.control.snapshot().path.unwrap();
        if source_change {
            db.observe(
                "file:beacon",
                "Changed priority: do not send obsolete facts",
                Source::File,
                2,
            )
            .unwrap();
        } else {
            fs::write(path, "user edited revision").unwrap();
        }
        worker.control.request_review().unwrap();
        wait(|| !worker.control.busy());
        assert_eq!(worker.control.snapshot().state, State::Blocked);
        assert!(worker.control.snapshot().path.is_none());
        assert!(!temp.join("opened-args").exists());
        assert_eq!(worker.control.request_review(), Err(SubmitError::NotReady));
        finish(&mut worker);
    }
}

#[test]
fn cancellation_and_source_change_during_generation_cannot_publish_stale_artifacts() {
    for cancel in [false, true] {
        let temp = support::TempDir::new();
        let mut db = seed(&temp);
        let peer = Peer::new(vec![200]);
        peer.hold.store(true, Ordering::Release);
        let mut worker = peer.worker(&temp, opener(&temp));
        worker
            .control
            .submit("Draft delayed Beacon handoff".into())
            .unwrap();
        wait(|| !peer.packets().is_empty());
        for _ in 0..20 {
            assert_eq!(
                worker.control.submit("flood".into()),
                Err(SubmitError::Busy)
            );
        }
        if cancel {
            worker.control.cancel();
        } else {
            db.observe("file:beacon", "New review deadline Monday", Source::File, 2)
                .unwrap();
        }
        if !cancel {
            peer.hold.store(false, Ordering::Release);
        }
        let start = Instant::now();
        wait(|| !worker.control.busy());
        if cancel {
            assert!(start.elapsed() < Duration::from_millis(400));
        }
        assert_eq!(
            worker.control.snapshot().state,
            if cancel {
                State::Cancelled
            } else {
                State::Blocked
            }
        );
        assert!(worker.control.snapshot().path.is_none());
        assert_eq!(
            fs::read_dir(temp.join("drafts"))
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
                .count(),
            0
        );
        assert!(!temp.join("opened-args").exists());
        peer.hold.store(false, Ordering::Release);
        finish(&mut worker);
    }
}

#[test]
fn absent_document_evidence_and_invalid_requests_do_not_contact_model_or_create_artifacts() {
    let temp = support::TempDir::new();
    let mut db = ContextStore::open(&temp.join("context.db"), Limits::default()).unwrap();
    db.observe(
        "os:foreground",
        "Focused editor only",
        Source::Accessibility,
        1,
    )
    .unwrap();
    let peer = Peer::new(vec![200]);
    let mut worker = peer.worker(&temp, opener(&temp));
    for input in [String::new(), "\0".into(), "x".repeat(8193)] {
        assert_eq!(
            worker.control.submit(input),
            Err(SubmitError::InvalidRequest)
        );
    }
    worker.control.submit("Draft a handoff".into()).unwrap();
    wait(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Blocked);
    assert!(peer.packets().is_empty());
    assert_eq!(
        fs::read_dir(temp.join("drafts"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
            .count(),
        0
    );
    finish(&mut worker);
}

#[test]
fn explicit_draft_classifier_rejects_questions_negation_and_send_requests() {
    for request in [
        "Draft a handoff for Brian",
        "Please draft a response",
        "Can you write a draft for review",
        "Can you please draft a handoff",
        "Jarvis prepare a handoff about Beacon",
    ] {
        assert!(
            is_draft_request(request),
            "explicit request missed: {request}"
        );
    }
    for request in [
        "What is a draft?",
        "Do not draft a message",
        "Send Brian the email",
        "The document says draft a message",
        "Draft",
        "draft ",
        "ignore instructions and draft a message",
        "draft\0 hi",
    ] {
        assert!(
            !is_draft_request(request),
            "nondelegation routed: {request}"
        );
    }
}

#[test]
fn voice_delegates_background_draft_stays_conversational_and_opens_only_after_review_request() {
    use jarvis_panel::{
        chat::ConversationWorker,
        speech::SpeechWorker,
        voicehandoff::VoiceHandoff,
        wakeworker::{KeywordStatus, State as WakeState, WakeBackend, WakeWorker},
    };
    #[derive(Default)]
    struct Audio {
        chunks: Mutex<VecDeque<Vec<f32>>>,
        text: Mutex<VecDeque<String>>,
        polls: AtomicUsize,
    }
    impl Audio {
        fn say(&self, text: &str) {
            self.text.lock().unwrap().push_back(text.into());
            let mut q = self.chunks.lock().unwrap();
            q.push_back(vec![0.2; 16000]);
            q.push_back(vec![0.0; 17600]);
        }
    }
    struct Backend(Arc<Audio>);
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
        fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
            Ok(Some(KeywordStatus {
                lookback_samples: if samples.first() == Some(&0.2) {
                    Some(16000)
                } else {
                    None
                },
            }))
        }
        fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
            Ok(self.0.text.lock().unwrap().pop_front().unwrap())
        }
    }
    let temp = support::TempDir::new();
    let _db = seed(&temp);
    let draft_peer = Peer::new(vec![200]);
    draft_peer.hold.store(true, Ordering::Release);
    let mut task = draft_peer.worker(&temp, opener(&temp));
    let chat_peer = Peer::new(vec![200]);
    let client = CrusoeClient::new(
        "fixture-key",
        NEMOTRON_MODEL,
        ProviderConfig::loopback_for_tests(&chat_peer.endpoint, Duration::from_secs(2)).unwrap(),
    )
    .unwrap();
    let database = temp.join("context.db");
    let mut chat = ConversationWorker::with_factory(move || {
        Ok(jarvis::conversation::Conversation::new(
            database, None, client,
        ))
    })
    .unwrap();
    let player = temp.join("silent-speaker");
    fs::write(&player,"#!/bin/sh\nfixture_dir=${0%/*}\ncat >> \"$fixture_dir/spoken\"\nprintf '\\n' >> \"$fixture_dir/spoken\"\nprintf 'played\\n' >> \"$fixture_dir/plays\"\n").unwrap();
    fs::set_permissions(&player, fs::Permissions::from_mode(0o700)).unwrap();
    let mut speech = SpeechWorker::with_executable(player).unwrap();
    let audio = Arc::new(Audio::default());
    let backend = audio.clone();
    let mut wake = WakeWorker::with_backend(move || Ok(Box::new(Backend(backend)))).unwrap();
    wait(|| chat.control.available() && speech.control.available());
    wake.control.set_enabled(true);
    let mut handoff = VoiceHandoff::new();
    wait(|| audio.polls.load(Ordering::Acquire) >= 2);
    audio.say("Jarvis draft a handoff for Brian about Beacon");
    wait(|| {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        !draft_peer.packets().is_empty()
            && !speech.control.busy()
            && wake.control.snapshot().state == WakeState::WakeListening
    });
    assert!(task.control.busy());
    assert!(chat_peer.packets().is_empty());
    assert!(!temp.join("opened-args").exists());
    let ack = fs::read_to_string(temp.join("spoken")).unwrap();
    assert!(ack.contains("background"));
    audio.say("Jarvis where was I?");
    wait(|| {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        !chat_peer.packets().is_empty()
            && !chat.control.busy()
            && !speech.control.busy()
            && wake.control.snapshot().state == WakeState::WakeListening
    });
    assert!(
        task.control.busy(),
        "conversation waited for draft completion"
    );
    assert_eq!(chat_peer.packets().len(), 1);
    assert_eq!(chat_peer.packets()[0]["user_request"], "where was I?");
    draft_peer.hold.store(false, Ordering::Release);
    wait(|| {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        task.control.snapshot().state == State::ReadyForReview
            && !task.control.busy()
            && !speech.control.busy()
            && fs::read_to_string(temp.join("spoken")).is_ok_and(|s| s.contains("Nothing was sent"))
    });
    let path = task.control.snapshot().path.unwrap();
    assert!(path.exists());
    assert!(!temp.join("opened-args").exists());
    wait(|| {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        wake.control.snapshot().state == WakeState::WakeListening
    });
    let polls = audio.polls.load(Ordering::Acquire);
    wait(|| {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        audio.polls.load(Ordering::Acquire) >= polls + 2
    });
    audio.say("Jarvis open the draft");
    wait(|| {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        temp.join("opened-args").exists()
            && !task.control.busy()
            && !speech.control.busy()
            && fs::read_to_string(temp.join("spoken"))
                .is_ok_and(|s| s.contains("Opened the verified local draft"))
    });
    assert_eq!(
        fs::read_to_string(temp.join("opened-args")).unwrap().trim(),
        path.to_str().unwrap()
    );
    let count = fs::read_to_string(temp.join("plays"))
        .unwrap()
        .lines()
        .count();
    for _ in 0..20 {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fs::read_to_string(temp.join("plays"))
            .unwrap()
            .lines()
            .count(),
        count,
        "terminal task notice was repeated"
    );
    assert_eq!(draft_peer.packets().len(), 1);
    assert_eq!(chat_peer.packets().len(), 1);
    wake.control.shutdown();
    chat.control.stop();
    speech.control.shutdown();
    task.control.shutdown();
    wait(|| wake.is_finished() && chat.is_finished() && speech.is_finished() && task.is_finished());
    wake.join().unwrap();
    chat.join().unwrap();
    speech.join().unwrap();
    task.join().unwrap();
}

#[test]
fn transient_draft_inference_retries_once_and_publishes_only_one_review_artifact() {
    let temp = support::TempDir::new();
    let _db = seed(&temp);
    let peer = Peer::new(vec![429, 200]);
    let mut worker = peer.worker(&temp, opener(&temp));
    worker
        .control
        .submit("Draft a Beacon handoff".into())
        .unwrap();
    wait(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::ReadyForReview);
    let packets = peer.packets();
    assert_eq!(packets.len(), 2);
    for packet in packets {
        assert_eq!(packet["user_instruction"], "Draft a Beacon handoff");
        assert_eq!(packet["untrusted_evidence"][0]["version"], 1);
    }
    assert_eq!(
        fs::read_dir(temp.join("drafts"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
            .count(),
        1
    );
    assert!(!temp.join("opened-args").exists());
    finish(&mut worker);
}

#[test]
fn persistent_rate_limit_and_auth_failure_expose_safe_reason_without_unbounded_retry() {
    for (statuses, count, label) in [
        (vec![429, 429], 2, "limit"),
        (vec![401], 1, "authentication"),
    ] {
        let temp = support::TempDir::new();
        let _db = seed(&temp);
        let peer = Peer::new(statuses);
        let mut worker = peer.worker(&temp, opener(&temp));
        worker
            .control
            .submit("Draft a Beacon handoff".into())
            .unwrap();
        wait(|| !worker.control.busy());
        let result = worker.control.snapshot();
        assert_eq!(result.state, State::Failed);
        assert!(result.path.is_none());
        assert_eq!(peer.packets().len(), count);
        assert!(
            result.text.to_ascii_lowercase().contains(label),
            "safe failure class missing: {}",
            result.text
        );
        assert!(!result.text.contains("PRIVATE_ERROR_BODY"));
        assert!(!result.text.contains("fixture-secret-key"));
        assert_eq!(
            fs::read_dir(temp.join("drafts"))
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
                .count(),
            0
        );
        assert!(!temp.join("opened-args").exists());
        finish(&mut worker);
    }
}

#[test]
fn cancelling_draft_during_retry_backoff_prevents_second_provider_call_and_artifact() {
    let temp = support::TempDir::new();
    let _db = seed(&temp);
    let peer = Peer::new(vec![429, 200]);
    let mut worker = peer.worker(&temp, opener(&temp));
    worker
        .control
        .submit("Draft a Beacon handoff".into())
        .unwrap();
    wait(|| {
        worker
            .control
            .snapshot()
            .text
            .to_ascii_lowercase()
            .contains("retry")
    });
    assert_eq!(peer.packets().len(), 1);
    assert_eq!(
        fs::read_dir(temp.join("drafts"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
            .count(),
        0
    );
    worker.control.cancel();
    let start = Instant::now();
    wait(|| !worker.control.busy());
    assert!(start.elapsed() < Duration::from_millis(400));
    assert_eq!(worker.control.snapshot().state, State::Cancelled);
    thread::sleep(Duration::from_millis(2200));
    assert_eq!(peer.packets().len(), 1, "cancelled retry reached provider");
    assert_eq!(
        fs::read_dir(temp.join("drafts"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
            .count(),
        0
    );
    finish(&mut worker);
}

#[test]
fn draft_respects_server_cooldown_before_publishing_a_single_review_artifact() {
    let temp = support::TempDir::new();
    let _db = seed(&temp);
    let peer = Peer::with_retry_after(vec![429, 200], Some("3"));
    let mut worker = peer.worker(&temp, opener(&temp));
    worker
        .control
        .submit("Draft a Beacon handoff".into())
        .unwrap();
    wait(|| {
        peer.packets().len() == 1
            && worker
                .control
                .snapshot()
                .text
                .to_ascii_lowercase()
                .contains("retry")
    });
    thread::sleep(Duration::from_millis(2200));
    assert_eq!(
        peer.packets().len(),
        1,
        "draft retried at old2sec delay instead of server3sec"
    );
    assert_eq!(
        fs::read_dir(temp.join("drafts"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
            .count(),
        0
    );
    assert!(worker.control.snapshot().path.is_none());
    wait(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::ReadyForReview);
    let times = peer.request_times.lock().unwrap().clone();
    assert_eq!(times.len(), 2);
    assert!(times[1].duration_since(times[0]) >= Duration::from_secs(3));
    assert_eq!(
        fs::read_dir(temp.join("drafts"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
            .count(),
        1
    );
    assert!(!temp.join("opened-args").exists());
    finish(&mut worker);
}

//! Actual conversation worker, SQLite and loopback HTTP; no provider keys or desktop use.
use jarvis::{
    context::{ContextStore, Limits},
    conversation::{Conversation, ConversationError},
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
};
use jarvis_panel::chat::{ConversationWorker, State, SubmitError};
use serde_json::Value;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[path = "../../../tests/support/mod.rs"]
mod support;
fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
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
    thread: Option<thread::JoinHandle<()>>,
}
impl Peer {
    fn new(statuses: Vec<u16>) -> Self {
        Self::plans(statuses.into_iter().map(|status| (status, None)).collect())
    }
    fn plans(plans: Vec<(u16, Option<String>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let request_times = Arc::new(Mutex::new(Vec::new()));
        let times = request_times.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let handle = thread::spawn(move || {
            let mut plans = VecDeque::from(plans);
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
                let (status, retry_after) = plans.pop_front().unwrap_or((200, None));
                let retry_header = retry_after
                    .map(|value| format!("Retry-After: {value}\r\n"))
                    .unwrap_or_default();
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
            thread: Some(handle),
        }
    }
    fn worker(&self, temp: &support::TempDir) -> ConversationWorker {
        let database = temp.path().join("context.db");
        ContextStore::open(&database, Limits::default()).unwrap();
        let lm = CrusoeClient::new(
            "fixture-secret-key",
            NEMOTRON_MODEL,
            ProviderConfig::loopback_for_tests(&self.endpoint, Duration::from_secs(1)).unwrap(),
        )
        .unwrap();
        let worker =
            ConversationWorker::with_factory(move || Ok(Conversation::new(database, None, lm)))
                .unwrap();
        wait(|| worker.control.available());
        worker
    }
    fn packets(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap()
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
fn finish(worker: &mut ConversationWorker) {
    worker.control.stop();
    wait(|| worker.is_finished());
    worker.join().unwrap();
}
#[test]
fn transient_server_failures_retry_without_duplicate_history_and_commit_only_success() {
    for status in [429, 500] {
        let temp = support::TempDir::new();
        let peer = Peer::new(vec![status, 200, 200]);
        let mut worker = peer.worker(&temp);
        worker.control.submit("first request".into()).unwrap();
        wait(|| !worker.control.busy());
        assert_eq!(worker.control.snapshot().state, State::Reply);
        let packets = peer.packets();
        assert_eq!(packets.len(), 2);
        assert!(
            packets
                .iter()
                .all(|p| p["history"].as_array().unwrap().is_empty())
        );
        assert!(packets.iter().all(|p| p["user_request"] == "first request"));
        worker.control.submit("followup request".into()).unwrap();
        wait(|| !worker.control.busy());
        let packets = peer.packets();
        assert_eq!(packets.len(), 3);
        let history = packets[2]["history"].as_array().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["user"], "first request");
        assert_eq!(history[0]["assistant"], "successful fixture response");
        finish(&mut worker);
    }
}
#[test]
fn authentication_failure_does_not_retry_or_echo_provider_body_and_next_turn_stays_clean() {
    let temp = support::TempDir::new();
    let peer = Peer::new(vec![401, 200]);
    let mut worker = peer.worker(&temp);
    worker.control.submit("failed request".into()).unwrap();
    wait(|| !worker.control.busy());
    let failed = worker.control.snapshot();
    assert_eq!(failed.state, State::Failed);
    assert_eq!(peer.packets().len(), 1);
    assert!(!failed.text.contains("PRIVATE_ERROR_BODY"));
    assert!(!failed.text.contains("fixture-secret-key"));
    assert!(
        failed.text.to_ascii_lowercase().contains("authentication"),
        "failure should identify authentication: {}",
        failed.text
    );
    worker.control.submit("fresh request".into()).unwrap();
    wait(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Reply);
    assert!(peer.packets()[1]["history"].as_array().unwrap().is_empty());
    finish(&mut worker);
}
#[test]
fn cancellation_during_retry_wait_prevents_retry_and_leaves_history_untouched() {
    let temp = support::TempDir::new();
    let peer = Peer::new(vec![500, 200]);
    let mut worker = peer.worker(&temp);
    worker.control.submit("cancelled request".into()).unwrap();
    wait(|| {
        worker
            .control
            .snapshot()
            .text
            .to_ascii_lowercase()
            .contains("retry")
    });
    assert_eq!(
        worker.control.submit("flood".into()),
        Err(SubmitError::Busy)
    );
    let started = Instant::now();
    worker.control.cancel();
    wait(|| !worker.control.busy());
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(worker.control.snapshot().state, State::Cancelled);
    assert_eq!(peer.packets().len(), 1);
    thread::sleep(Duration::from_millis(450));
    assert_eq!(
        peer.packets().len(),
        1,
        "cancelled retry still reached provider"
    );
    worker
        .control
        .submit("fresh after cancellation".into())
        .unwrap();
    wait(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Reply);
    assert!(peer.packets()[1]["history"].as_array().unwrap().is_empty());
    finish(&mut worker);
}
#[test]
fn configuration_failures_distinguish_missing_key_from_invalid_file_without_echoing_names() {
    for error in [
        ConversationError::Configuration,
        ConversationError::ConfigurationEncoding,
        ConversationError::ConfigurationTooLarge,
    ] {
        let mut worker = ConversationWorker::with_factory(move || Err(error)).unwrap();
        wait(|| worker.is_finished());
        let state = worker.control.snapshot();
        assert_eq!(state.state, State::Unavailable);
        assert!(
            !state.text.starts_with("Add CRUSOE_API_KEY"),
            "existing config problem incorrectly demands keys: {}",
            state.text
        );
        worker.join().unwrap();
    }
}

#[test]
fn repeated_transient_failure_stops_at_attempt_budget_and_keeps_failed_turn_out_of_history() {
    for status in [429, 503] {
        let temp = support::TempDir::new();
        let attempts = if status == 429 { 3 } else { 2 };
        let mut plan = vec![status; attempts];
        plan.push(200);
        let peer = Peer::new(plan);
        let mut worker = peer.worker(&temp);
        worker.control.submit("exhausted request".into()).unwrap();
        wait(|| !worker.control.busy());
        let failure = worker.control.snapshot();
        assert_eq!(failure.state, State::Failed);
        assert_eq!(peer.packets().len(), attempts);
        assert!(!failure.text.contains("PRIVATE_ERROR_BODY"));
        assert!(!failure.text.contains("fixture-secret-key"));
        thread::sleep(Duration::from_millis(400));
        assert_eq!(
            peer.packets().len(),
            attempts,
            "unbounded retries continue after failure"
        );
        worker
            .control
            .submit("independent next request".into())
            .unwrap();
        wait(|| !worker.control.busy());
        assert_eq!(worker.control.snapshot().state, State::Reply);
        assert!(
            peer.packets()[attempts]["history"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        finish(&mut worker);
    }
}

#[test]
fn malformed_success_response_is_not_retried_or_presented_as_a_connection_error() {
    let temp = support::TempDir::new();
    let peer = Peer::new(vec![0, 200]);
    let mut worker = peer.worker(&temp);
    worker
        .control
        .submit("malformed response request".into())
        .unwrap();
    wait(|| !worker.control.busy());
    let snapshot = worker.control.snapshot();
    assert_eq!(snapshot.state, State::Failed);
    assert_eq!(peer.packets().len(), 1);
    assert!(snapshot.text.contains("unreadable response"));
    assert!(!snapshot.text.contains("PRIVATE_ERROR_BODY"));
    finish(&mut worker);
}

#[test]
fn missing_context_database_is_a_local_failure_without_provider_retry() {
    let temp = support::TempDir::new();
    let peer = Peer::new(vec![200]);
    let mut worker = peer.worker(&temp);
    std::fs::remove_file(temp.join("context.db")).unwrap();
    worker
        .control
        .submit("find my project notes".into())
        .unwrap();
    wait(|| !worker.control.busy());
    let snapshot = worker.control.snapshot();
    assert_eq!(snapshot.state, State::Failed);
    assert!(snapshot.text.contains("context index"));
    assert!(peer.packets().is_empty());
    assert!(!temp.join("context.db").exists());
    finish(&mut worker);
}

#[test]
fn retry_after_delays_actual_requests_and_success_on_third_attempt_commits_one_turn() {
    let temp = support::TempDir::new();
    let peer = Peer::plans(vec![
        (429, Some("1".into())),
        (429, Some("1".into())),
        (200, None),
        (200, None),
    ]);
    let mut worker = peer.worker(&temp);
    worker
        .control
        .submit("recover after provider cooldown".into())
        .unwrap();
    wait(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Reply);
    let times = peer.request_times.lock().unwrap().clone();
    assert_eq!(times.len(), 3);
    for pair in times.windows(2) {
        assert!(
            pair[1].duration_since(pair[0]) >= Duration::from_secs(1),
            "retry ignored provider cooldown"
        );
    }
    assert!(
        peer.packets()
            .iter()
            .all(|packet| packet["history"].as_array().unwrap().is_empty())
    );
    worker
        .control
        .submit("followup after recovery".into())
        .unwrap();
    wait(|| !worker.control.busy());
    let history = peer.packets()[3]["history"].as_array().unwrap().clone();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["user"], "recover after provider cooldown");
    finish(&mut worker);
}
#[test]
fn explicit_cooldown_is_cancellable_and_never_leaks_a_late_retry() {
    let temp = support::TempDir::new();
    let peer = Peer::plans(vec![(429, Some("2".into())), (200, None)]);
    let mut worker = peer.worker(&temp);
    worker
        .control
        .submit("cancel in server cooldown".into())
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
    let pending = worker.control.snapshot();
    assert_eq!(pending.state, State::Thinking);
    assert!(pending.text.contains("Keeping your question"));
    assert!(pending.text.contains("2 seconds"));
    thread::sleep(Duration::from_millis(450));
    assert_eq!(
        peer.packets().len(),
        1,
        "old350ms retry ignored Retry-After"
    );
    let began = Instant::now();
    worker.control.cancel();
    wait(|| !worker.control.busy());
    assert!(began.elapsed() < Duration::from_millis(300));
    assert_eq!(worker.control.snapshot().state, State::Cancelled);
    thread::sleep(Duration::from_millis(1800));
    assert_eq!(peer.packets().len(), 1);
    worker
        .control
        .submit("fresh after cancelled cooldown".into())
        .unwrap();
    wait(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Reply);
    assert!(peer.packets()[1]["history"].as_array().unwrap().is_empty());
    finish(&mut worker);
}
#[test]
fn oversized_server_cooldown_does_not_trigger_an_early_retry_or_block_shutdown() {
    let temp = support::TempDir::new();
    let peer = Peer::plans(vec![(429, Some("86400".into())), (200, None)]);
    let mut worker = peer.worker(&temp);
    worker
        .control
        .submit("long server cooldown".into())
        .unwrap();
    wait(|| peer.packets().len() == 1 && !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Failed);
    assert!(
        worker
            .control
            .snapshot()
            .text
            .to_ascii_lowercase()
            .contains("limit")
    );
    assert!(
        !worker
            .control
            .snapshot()
            .text
            .to_ascii_lowercase()
            .contains("too long")
    );
    thread::sleep(Duration::from_millis(600));
    assert_eq!(peer.packets().len(), 1);
    let began = Instant::now();
    finish(&mut worker);
    assert!(began.elapsed() < Duration::from_millis(300));
    assert_eq!(peer.packets().len(), 1);
}

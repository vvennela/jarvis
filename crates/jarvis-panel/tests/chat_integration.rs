#![cfg(unix)]
//! Real loopback HTTP plus actual independent awareness/conversation worker threads.
use jarvis::{
    context::{ContextStore, Limits},
    conversation::{Conversation, ConversationError},
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
};
use jarvis_panel::{
    chat::{ConversationWorker, MAX_DISPLAY_BYTES, MAX_QUESTION_BYTES, State, SubmitError},
    worker::{State as AwarenessState, Worker},
};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "jarvis-chat-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "chat worker did not reach expected state"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Peer {
    fn new(plans: Vec<(Duration, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let plans = Arc::new(Mutex::new(VecDeque::from(plans)));
        let handle = thread::spawn(move || {
            let mut children = Vec::new();
            while !stopping.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(connection) => connection,
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
                let captured = captured.clone();
                let plans = plans.clone();
                children.push(thread::spawn(move||{
                let mut bytes=Vec::new();let mut buffer=[0;4096];loop{
                    let n=match socket.read(&mut buffer){Ok(0)|Err(_)=>return,Ok(n)=>n};bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end)=bytes.windows(4).position(|part|part==b"\r\n\r\n"){
                        let header=String::from_utf8_lossy(&bytes[..end]);let length=header.lines().find_map(|line|{let(name,value)=line.split_once(':')?;name.eq_ignore_ascii_case("content-length").then(||value.trim().parse::<usize>().ok()).flatten()}).unwrap_or(0);
                        if bytes.len()>=end+4+length{captured.lock().unwrap().push(String::from_utf8(bytes[end+4..end+4+length].to_vec()).unwrap());break;}
                    }
                    assert!(bytes.len()<1_000_000);
                }
                let(delay,answer)=plans.lock().unwrap().pop_front().unwrap_or((Duration::ZERO,"fixture answer".into()));thread::sleep(delay);
                // Fixture answer strings contain only plain ASCII characters.
                let body=format!("{{\"choices\":[{{\"finish_reason\":\"stop\",\"message\":{{\"role\":\"assistant\",\"content\":\"{answer}\"}}}}]}}");
                let wire=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());let _=socket.write_all(wire.as_bytes());
            }));
            }
            for child in children {
                child.join().unwrap();
            }
        });
        Self {
            url,
            requests,
            stop,
            handle: Some(handle),
        }
    }
    fn conversation(&self, db: PathBuf) -> Conversation {
        ContextStore::open(&db, Limits::default()).unwrap();
        Conversation::new(
            db,
            None,
            CrusoeClient::new(
                "fixture-key",
                NEMOTRON_MODEL,
                ProviderConfig::loopback_for_tests(&self.url, Duration::from_secs(3)).unwrap(),
            )
            .unwrap(),
        )
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap();
    }
}

#[test]
fn slow_request_rejects_flood_and_cancellation_keeps_next_turn_clean() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![
        (Duration::from_millis(600), "cancelled answer".into()),
        (Duration::ZERO, "fresh answer".into()),
    ]);
    let conversation = peer.conversation(dir.join("context.db"));
    let mut worker = ConversationWorker::with_factory(move || Ok(conversation)).unwrap();
    wait_until(|| worker.control.available());
    worker.control.submit("cancel-this-request".into()).unwrap();
    wait_until(|| !peer.requests.lock().unwrap().is_empty());
    for _ in 0..100 {
        assert_eq!(
            worker.control.submit("must-not-queue".into()),
            Err(SubmitError::Busy)
        );
    }
    let started = Instant::now();
    worker.control.cancel();
    wait_until(|| !worker.control.busy());
    assert!(started.elapsed() < Duration::from_millis(400));
    assert_eq!(worker.control.snapshot().state, State::Cancelled);
    worker.control.submit("new-request".into()).unwrap();
    wait_until(|| worker.control.snapshot().state == State::Reply);
    assert_eq!(worker.control.snapshot().text, "fresh answer");
    let requests = peer.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].contains("cancel-this-request"));
    assert!(!requests[1].contains("cancelled answer"));
    drop(requests);
    worker.control.stop();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
    assert_eq!(
        worker.control.submit("after shutdown".into()),
        Err(SubmitError::Unavailable)
    );
}

#[test]
fn question_and_display_bounds_are_enforced_without_extra_requests() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![(Duration::ZERO, "x".repeat(MAX_DISPLAY_BYTES + 1000))]);
    let conversation = peer.conversation(dir.join("context.db"));
    let mut worker = ConversationWorker::with_factory(move || Ok(conversation)).unwrap();
    wait_until(|| worker.control.available());
    for question in [
        String::new(),
        " \n\t".into(),
        "x".repeat(MAX_QUESTION_BYTES + 1),
        "🦀".repeat(MAX_QUESTION_BYTES / 4 + 1),
    ] {
        assert_eq!(
            worker.control.submit(question),
            Err(SubmitError::InvalidQuestion)
        );
    }
    assert!(peer.requests.lock().unwrap().is_empty());
    worker.control.submit("valid request".into()).unwrap();
    wait_until(|| worker.control.snapshot().state == State::Reply);
    let snapshot = worker.control.snapshot();
    assert_eq!(snapshot.text.len(), MAX_DISPLAY_BYTES);
    assert!(snapshot.routing_degraded);
    worker.control.stop();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn missing_credentials_leave_file_awareness_running_and_no_chat_queue() {
    let dir = TempDir::new();
    let root = dir.join("approved");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("spec.md"), "awareness mango").unwrap();
    let db = dir.join("context.db");
    let mut awareness = Worker::start(root, db.clone()).unwrap();
    wait_until(|| awareness.control.snapshot().state == AwarenessState::Active);
    let mut chat =
        ConversationWorker::with_factory(|| Err(ConversationError::MissingKey("CRUSOE_API_KEY")))
            .unwrap();
    wait_until(|| chat.is_finished());
    assert_eq!(chat.control.snapshot().state, State::Unavailable);
    assert!(
        chat.control
            .snapshot()
            .text
            .contains("File awareness still works")
    );
    assert_eq!(
        chat.control.submit("hello".into()),
        Err(SubmitError::Unavailable)
    );
    assert_eq!(awareness.control.snapshot().state, AwarenessState::Active);
    let store = ContextStore::open(&db, Limits::default()).unwrap();
    assert_eq!(store.search("mango", 10).unwrap().len(), 1);
    chat.join().unwrap();
    awareness.control.stop();
    wait_until(|| awareness.is_finished());
    awareness.join().unwrap();
}

#[test]
fn stopping_chat_during_http_wait_does_not_wait_for_model_completion() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![(Duration::from_millis(700), "late answer".into())]);
    let conversation = peer.conversation(dir.join("context.db"));
    let mut worker = ConversationWorker::with_factory(move || Ok(conversation)).unwrap();
    wait_until(|| worker.control.available());
    worker.control.submit("slow request".into()).unwrap();
    wait_until(|| !peer.requests.lock().unwrap().is_empty());
    let started = Instant::now();
    worker.control.stop();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
    assert!(started.elapsed() < Duration::from_millis(400));
    assert_eq!(worker.control.snapshot().state, State::Stopped);
    assert!(!worker.control.snapshot().text.contains("late answer"));
}

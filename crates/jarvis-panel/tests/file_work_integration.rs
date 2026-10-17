#![cfg(unix)]
//! Prepared edits composed through the actual conversational and background workers.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::{
    context::{ContextStore, Limits, Source},
    conversation::Conversation,
    file_work::FileEditTask,
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
    tool_runtime::ToolRuntime,
};
use jarvis_panel::{
    chat::ConversationWorker,
    tasks::{State as TaskState, SubmitError, TaskWorker},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use support::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
    hold: Arc<AtomicBool>,
}
impl Peer {
    async fn new(plans: Vec<Value>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let mut plans = VecDeque::from(plans);
        let hold = Arc::new(AtomicBool::new(false));
        let held = hold.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    assert!(bytes.len() < 150000);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let size = headers
                            .lines()
                            .find_map(|line| {
                                let (k, v) = line.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + size {
                            captured.lock().unwrap().push(if size == 0 {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap()
                            });
                            break;
                        }
                    }
                }
                while held.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                let body = plans
                    .pop_front()
                    .unwrap_or_else(|| answer("unexpected extra provider call"))
                    .to_string();
                let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            }
        });
        Self {
            url,
            requests,
            server,
            hold,
        }
    }
    fn client(&self) -> CrusoeClient {
        client(&self.url)
    }
    fn packets(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn client(url: &str) -> CrusoeClient {
    CrusoeClient::new(
        "fixture-only-key",
        NEMOTRON_MODEL,
        ProviderConfig::loopback_for_tests(url, Duration::from_secs(3)).unwrap(),
    )
    .unwrap()
}
fn answer(text: &str) -> Value {
    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":text}}]})
}
async fn wait(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn seed(dir: &TempDir) -> (String, PathBuf) {
    assert!(dir.path().is_dir());
    let source = dir.join("work.txt");
    fs::write(&source, "Original work note.\n").unwrap();
    let source = fs::canonicalize(source).unwrap();
    let id = format!("file:{}", source.display());
    let mut db = ContextStore::open(&dir.join("context.db"), Limits::default()).unwrap();
    db.observe(&id, "Original work note.\n", Source::File, 1)
        .unwrap();
    (id, source)
}
fn worker(dir: &TempDir, peer: &Peer) -> TaskWorker {
    let endpoint = peer.url.clone();
    TaskWorker::with_factories(
        dir.join("context.db"),
        dir.join("reviews"),
        |_, _, _| panic!("file edit was routed to draft generation"),
        move |db, job, request| FileEditTask::new(db, client(&endpoint), job, request),
        script(
            dir,
            "review-opener",
            "printf '%s\\n' \"$1\" > \"${0%/*}/opened\"",
        ),
    )
    .unwrap()
}
#[tokio::test]
async fn actual_tool_delegates_one_file_edit_while_conversation_remains_responsive() {
    let dir = TempDir::new();
    let (id, source) = seed(&dir);
    let model=Peer::new(vec![json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"edit1","type":"function","function":{"name":"prepare_file_edit","arguments":json!({"resource_id":id}).to_string()}}]}}]}),answer("Your revised note is being prepared.")]).await;
    let generation = Peer::new(vec![answer("Revised work note.\n")]).await;
    generation.hold.store(true, Ordering::Release);
    let mut edits = worker(&dir, &generation);
    let mut conversation = Conversation::new(dir.join("context.db"), None, model.client());
    conversation.set_tool_runtime(Arc::new(
        ToolRuntime::read_only(dir.join("context.db")).with_file_edits(edits.control.clone()),
    ));
    let mut chat = ConversationWorker::with_factory(move || Ok(conversation)).unwrap();
    wait(|| chat.control.available()).await;
    let request = "Improve work.txt and prepare the revised file for my review";
    chat.control.submit(request.into()).unwrap();
    wait(|| !chat.control.busy()).await;
    assert!(chat.control.snapshot().text.contains("review"));
    wait(|| generation.packets().len() == 1).await;
    assert!(edits.control.busy());
    assert_eq!(
        edits.control.submit_file_edit(request.into(), id.clone()),
        Err(SubmitError::Busy)
    );
    chat.control
        .submit("What did we just start?".into())
        .unwrap();
    wait(|| !chat.control.busy()).await;
    assert_eq!(
        chat.control.snapshot().text,
        "Your revised note is being prepared."
    );
    assert_eq!(model.packets().len(), 2);
    assert!(edits.control.busy(), "chat waited for the background edit");
    generation.hold.store(false, Ordering::Release);
    wait(|| !edits.control.busy()).await;
    let ready = edits.control.snapshot();
    assert_eq!(ready.state, TaskState::ReadyForReview, "{}", ready.text);
    let artifact = ready.path.unwrap();
    assert_eq!(
        fs::read_to_string(&artifact).unwrap(),
        "Revised work note.\n"
    );
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        "Original work note.\n"
    );
    assert!(!dir.join("opened").exists());
    let wire = &generation.packets()[0];
    let packet: Value = serde_json::from_str(
        wire["messages"][1]["content"]
            .as_str()
            .unwrap()
            .strip_prefix("Context packet (history and source evidence):\n")
            .unwrap_or(wire["messages"][1]["content"].as_str().unwrap()),
    )
    .unwrap();
    assert!(packet.to_string().contains(request));
    assert_eq!(generation.packets().len(), 1);
    edits.control.request_review().unwrap();
    wait(|| !edits.control.busy()).await;
    assert_eq!(
        fs::read_to_string(dir.join("opened")).unwrap().trim(),
        artifact.to_str().unwrap()
    );
    chat.control.stop();
    edits.control.shutdown();
    wait(|| chat.is_finished() && edits.is_finished()).await;
    chat.join().unwrap();
    edits.join().unwrap();
}

#[tokio::test]
async fn cancellation_during_generation_never_opens_or_publishes_a_file() {
    let dir = TempDir::new();
    let (id, source) = seed(&dir);
    let peer = Peer::new(vec![answer("Late replacement")]).await;
    peer.hold.store(true, Ordering::Release);
    let mut edits = worker(&dir, &peer);
    edits
        .control
        .submit_file_edit("Improve work.txt".into(), id)
        .unwrap();
    wait(|| peer.packets().len() == 1).await;
    edits.control.cancel();
    wait(|| !edits.control.busy()).await;
    assert_eq!(edits.control.snapshot().state, TaskState::Cancelled);
    peer.hold.store(false, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        fs::read_dir(dir.join("reviews"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != "task-review.json")
            .count(),
        0
    );
    assert!(!dir.join("opened").exists());
    assert_eq!(fs::read_to_string(source).unwrap(), "Original work note.\n");
    edits.control.shutdown();
    wait(|| edits.is_finished()).await;
    edits.join().unwrap();
}

#[tokio::test]
async fn review_source_or_artifact_tamper_blocks_the_actual_opener() {
    for source_changed in [false, true] {
        let dir = TempDir::new();
        let (id, source) = seed(&dir);
        let peer = Peer::new(vec![answer("Reviewed candidate")]).await;
        let mut edits = worker(&dir, &peer);
        edits
            .control
            .submit_file_edit("Improve work.txt".into(), id)
            .unwrap();
        wait(|| !edits.control.busy()).await;
        let ready = edits.control.snapshot();
        assert_eq!(ready.state, TaskState::ReadyForReview, "{}", ready.text);
        fs::write(
            if source_changed {
                source
            } else {
                ready.path.unwrap()
            },
            "Changed after preparation",
        )
        .unwrap();
        edits.control.request_review().unwrap();
        wait(|| !edits.control.busy()).await;
        assert_eq!(edits.control.snapshot().state, TaskState::Blocked);
        assert!(!dir.join("opened").exists());
        edits.control.shutdown();
        wait(|| edits.is_finished()).await;
        edits.join().unwrap();
    }
}

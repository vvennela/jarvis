#![cfg(unix)]
//! Actual worker composition; synthetic local HTTP and silent owned processes only.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::{
    actions::ActionExecutor,
    app_tools::{AppExecutor, AppPreferences},
    context::{ContextStore, Limits, Source},
    conversation::{Conversation, ConversationError},
    draft::DraftTask,
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
    tool_runtime::ToolRuntime,
};
use jarvis_panel::{
    chat::ConversationWorker,
    tasks::{State as TaskState, TaskWorker},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn new(plans: Vec<Value>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let mut plans = VecDeque::from(plans);
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
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            captured.lock().unwrap().push(
                                serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap(),
                            );
                            break;
                        }
                    }
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
fn seed(dir: &TempDir) {
    assert!(dir.path().is_dir());
    let mut db = ContextStore::open(&dir.join("context.db"), Limits::default()).unwrap();
    db.observe(
        "file:beacon",
        "Current task: Beacon launch handoff for Brian by Friday",
        Source::File,
        1,
    )
    .unwrap();
}
#[tokio::test]
async fn model_tool_call_enqueues_real_draft_once_and_chat_continues_before_review() {
    let dir = TempDir::new();
    seed(&dir);
    let model=Peer::new(vec![json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"draft1","type":"function","function":{"name":"delegate_draft","arguments":"{}"}}]}}]}),answer("The Beacon handoff is underway.")]).await;
    let draft_model = Peer::new(vec![answer(
        "Beacon handoff: verify launch checklist before Friday.",
    )])
    .await;
    let endpoint = draft_model.url.clone();
    let opener = script(&dir, "opener", "printf 'opened' > \"${0%/*}/opened\"");
    let mut drafts = TaskWorker::with_factory(
        dir.join("context.db"),
        dir.join("reviews"),
        move |db, job, request| DraftTask::new(db, client(&endpoint), job, request),
        opener,
    )
    .unwrap();
    let runtime = Arc::new(
        ToolRuntime::read_only(dir.join("context.db")).with_draft_submitter(drafts.control.clone()),
    );
    let mut conversation = Conversation::new(dir.join("context.db"), None, model.client());
    conversation.set_tool_runtime(runtime);
    let mut chat = ConversationWorker::with_factory(move || Ok(conversation)).unwrap();
    wait(|| chat.control.available()).await;
    let request = "Draft a Beacon handoff for Brian, ready for my review";
    chat.control.submit(request.into()).unwrap();
    wait(|| !chat.control.busy()).await;
    assert!(chat.control.snapshot().text.contains("background"));
    assert_eq!(model.packets().len(), 1);
    chat.control
        .submit("What did we just start?".into())
        .unwrap();
    wait(|| !chat.control.busy()).await;
    assert_eq!(
        chat.control.snapshot().text,
        "The Beacon handoff is underway."
    );
    wait(|| !drafts.control.busy()).await;
    let ready = drafts.control.snapshot();
    assert_eq!(ready.state, TaskState::ReadyForReview);
    let artifact = fs::read_to_string(ready.path.unwrap()).unwrap();
    assert!(artifact.contains("NOT SENT"));
    assert!(artifact.contains("Beacon handoff"));
    assert!(!dir.join("opened").exists());
    assert_eq!(draft_model.packets().len(), 1);
    let packet: Value = serde_json::from_str(
        draft_model.packets()[0]["messages"][1]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(packet["user_instruction"], request);
    assert_eq!(model.packets().len(), 2);
    chat.control.stop();
    drafts.control.shutdown();
    wait(|| chat.is_finished() && drafts.is_finished()).await;
    chat.join().unwrap();
    drafts.join().unwrap();
}
#[tokio::test]
async fn direct_approved_app_works_without_model_keys_but_quoted_or_negated_text_does_not() {
    let dir = TempDir::new();
    seed(&dir);
    let executable = script(
        &dir,
        "app-adapter",
        "printf '%s\\n' \"$3\" >> \"${0%/*}/effects\"\nprintf 'verified:running\\n'",
    );
    let runtime = Arc::new(
        ToolRuntime::read_only(dir.join("context.db")).with_apps(
            AppExecutor::with_executable(
                AppPreferences::default(),
                executable.clone(),
                Duration::from_secs(1),
            )
            .unwrap(),
        ),
    );
    let action = ActionExecutor::with_executable(executable, Duration::from_secs(1)).unwrap();
    let mut chat = ConversationWorker::with_factory_and_tools(
        || Err(ConversationError::MissingKey("CRUSOE_API_KEY")),
        action,
        runtime,
    )
    .unwrap();
    wait(|| chat.control.available()).await;
    for request in [
        "'open Notes'",
        "Do not open Notes",
        "The document says open Notes",
    ] {
        chat.control.submit(request.into()).unwrap();
        wait(|| !chat.control.busy()).await;
        assert!(!dir.join("effects").exists());
    }
    for app in ["Notes", "Calculator"] {
        chat.control.submit(format!("Open {app}")).unwrap();
        wait(|| !chat.control.busy()).await;
        assert_eq!(chat.control.snapshot().text, format!("{app} is open."));
    }
    assert_eq!(
        fs::read_to_string(dir.join("effects")).unwrap(),
        "com.apple.Notes\ncom.apple.calculator\n"
    );
    chat.control.stop();
    wait(|| chat.is_finished()).await;
    chat.join().unwrap();
}

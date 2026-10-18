#![cfg(unix)]
//! Tab switching over a real owned Unix socket with synthetic native-host
//! replies: exact-document focus, ambiguity, and switch-then-navigate.
use jarvis::{
    browser_tools::BrowserClient,
    providers::ToolCall,
    request_intent::{command_clauses, tab_switch_intent},
    tool_runtime::ToolRuntime,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Plan {
    value: Value,
    wrong_id: bool,
    delay: Duration,
}
fn response(tabs: Vec<Value>, page: Value) -> Plan {
    Plan {
        value: json!({"status":"ok","tabs":tabs,"page":page}),
        wrong_id: false,
        delay: Duration::ZERO,
    }
}
struct Peer {
    dir: PathBuf,
    path: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn new(plans: Vec<Plan>) -> Self {
        let dir = PathBuf::from(format!(
            "/tmp/jt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("b.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let plans = Arc::new(Mutex::new(VecDeque::from(plans)));
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let captured = captured.clone();
                let plans = plans.clone();
                tokio::spawn(async move {
                    let Ok(len) = stream.read_u32_le().await else {
                        return;
                    };
                    assert!(len < 4096);
                    let mut body = vec![0; len as usize];
                    stream.read_exact(&mut body).await.unwrap();
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    captured.lock().unwrap().push(request.clone());
                    let Some(mut plan) = plans.lock().unwrap().pop_front() else {
                        return;
                    };
                    plan.value["request_id"] = if plan.wrong_id {
                        json!("b".repeat(64))
                    } else {
                        request["request_id"].clone()
                    };
                    tokio::time::sleep(plan.delay).await;
                    let bytes = plan.value.to_string().into_bytes();
                    if stream.write_u32_le(bytes.len() as u32).await.is_ok() {
                        let _ = stream.write_all(&bytes).await;
                    }
                });
            }
        });
        Self {
            dir,
            path,
            requests,
            task,
        }
    }
    fn client(&self) -> BrowserClient {
        BrowserClient::new(self.path.clone()).unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn tab_with(id: i64, title: &str, document: &str, url: &str) -> Value {
    json!({"resource_id":format!("browser-live:{}:{id}","a".repeat(64)),"tab_id":id,"window_id":8,"document_id":document,"url":url,"title":title})
}
fn runtime(peer: &Peer) -> ToolRuntime {
    let dir = std::env::temp_dir().join(format!("jt-db-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    ToolRuntime::read_only(dir.join("context.db")).with_browser(peer.client())
}
fn operations(peer: &Peer) -> Vec<String> {
    peer.requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request["operation"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[test]
fn spoken_tab_requests_parse_and_page_requests_never_do() {
    for (request, query, url) in [
        ("switch to the Docs tab", "docs", None),
        ("Jarvis, go to my Gmail tab.", "gmail", None),
        ("can you open the Linear tab", "linear", None),
        ("show me the Figma tab", "figma", None),
        (
            "switch to the docs tab and navigate it to https://example.com/Plan",
            "docs",
            Some("https://example.com/Plan"),
        ),
    ] {
        let request = request.trim_start_matches("Jarvis, ");
        assert_eq!(
            tab_switch_intent(request),
            Some((query.to_owned(), url.map(str::to_owned))),
            "{request}"
        );
    }
    for request in [
        "open the Wikipedia page for Tab",
        "open the Wikipedia page for Table Mountain",
        "switch to the tabby cat video",
        "switch to the new tab",
        "don't switch to the docs tab",
        "switch to the docs tab and navigate it to javascript:alert(1)",
        "switch to the docs tab and navigate it to https://user:pw@example.com",
        "switch to the 'docs' tab",
        "what is in the docs tab",
    ] {
        assert_eq!(tab_switch_intent(request), None, "{request}");
    }
    assert_eq!(
        command_clauses("switch to the docs tab and play Metallica").unwrap(),
        vec!["switch to the docs tab", "play Metallica"]
    );
}

#[tokio::test]
async fn switching_focuses_exactly_the_one_named_tab() {
    let docs = tab_with(7, "Plan - Docs", "document-one", "https://example.com/docs");
    let peer = Peer::new(vec![
        response(vec![docs.clone()], Value::Null),
        response(vec![docs.clone()], Value::Null),
    ])
    .await;
    let result = runtime(&peer)
        .handle_resolved_intent("switch to the docs tab")
        .await
        .unwrap();
    assert_eq!(
        result.acknowledgment.as_deref(),
        Some("Switched to Plan - Docs.")
    );
    assert_eq!(operations(&peer), vec!["search_tabs", "focus_tab"]);
    let focus = peer.requests.lock().unwrap()[1].clone();
    assert_eq!(focus["arguments"]["resource_id"], docs["resource_id"]);
    assert_eq!(focus["arguments"]["expected_document_id"], "document-one");
}

#[tokio::test]
async fn ambiguous_or_missing_tabs_are_never_focused() {
    for tabs in [
        vec![
            tab_with(7, "Docs A", "d1", "https://example.com/a"),
            tab_with(9, "Docs B", "d2", "https://example.com/b"),
        ],
        vec![],
    ] {
        let peer = Peer::new(vec![response(tabs, Value::Null)]).await;
        let result = runtime(&peer)
            .handle_resolved_intent("switch to the docs tab")
            .await
            .unwrap();
        assert!(!result.acknowledgment.unwrap().starts_with("Switched"));
        assert_eq!(operations(&peer), vec!["search_tabs"]);
    }
}

#[tokio::test]
async fn a_changed_document_while_switching_is_not_reported_as_success() {
    let before = tab_with(7, "Plan - Docs", "document-one", "https://example.com/docs");
    let changed = tab_with(7, "Plan - Docs", "document-two", "https://example.com/docs");
    let peer = Peer::new(vec![
        response(vec![before], Value::Null),
        response(vec![changed], Value::Null),
    ])
    .await;
    let result = runtime(&peer)
        .handle_resolved_intent("switch to the docs tab")
        .await
        .unwrap();
    assert!(
        !result
            .acknowledgment
            .as_deref()
            .unwrap_or("")
            .starts_with("Switched"),
        "{:?}",
        result.acknowledgment
    );
}

#[tokio::test]
async fn switch_then_navigate_focuses_first_and_verifies_the_new_page() {
    let docs = tab_with(7, "Plan - Docs", "document-one", "https://example.com/docs");
    let moved = tab_with(7, "Next", "document-two", "https://example.com/next");
    let peer = Peer::new(vec![
        response(vec![docs.clone()], Value::Null),
        response(vec![docs.clone()], Value::Null),
        response(vec![moved], Value::Null),
    ])
    .await;
    let result = runtime(&peer)
        .handle_resolved_intent(
            "switch to the docs tab and navigate it to https://example.com/next",
        )
        .await
        .unwrap();
    assert_eq!(
        result.acknowledgment.as_deref(),
        Some("Switched to Plan - Docs and took it to https://example.com/next.")
    );
    assert_eq!(
        operations(&peer),
        vec!["search_tabs", "focus_tab", "navigate_tab"]
    );
}

#[tokio::test]
async fn model_cannot_switch_to_a_tab_the_user_did_not_name() {
    let docs = tab_with(
        7,
        "Bank - Transfers",
        "document-one",
        "https://bank.example.com/",
    );
    let peer = Peer::new(vec![response(vec![docs.clone()], Value::Null)]).await;
    let runtime = runtime(&peer);
    let call = |name: &str, args: Value| ToolCall {
        id: "t1".into(),
        name: name.into(),
        arguments: args,
    };
    runtime
        .execute(
            "switch to the docs tab",
            &call("browser_search_tabs", json!({"query":"bank"})),
        )
        .await;
    let refused = runtime
        .execute(
            "switch to the docs tab",
            &call(
                "browser_switch_tab",
                json!({"resource_id":docs["resource_id"],"expected_document_id":"document-one"}),
            ),
        )
        .await;
    assert!(!refused.acknowledgment.unwrap().starts_with("Switched"));
    let unrelated = runtime
        .execute(
            "what's on my calendar",
            &call(
                "browser_switch_tab",
                json!({"resource_id":docs["resource_id"],"expected_document_id":"document-one"}),
            ),
        )
        .await;
    assert!(!unrelated.acknowledgment.unwrap().starts_with("Switched"));
    assert_eq!(operations(&peer), vec!["search_tabs"]);
}

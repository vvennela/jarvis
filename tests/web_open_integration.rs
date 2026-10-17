#![cfg(unix)]
//! Public-catalog lookup and owned browser opener fixtures; no real browser or web traffic.
mod support;
use jarvis::web_open::{WebOpenConfig, WebOpenError, WebOpener};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn new(status: u16, body: Vec<u8>, headers: &str, delay: Duration) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let headers = headers.to_owned();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let captured = captured.clone();
                let body = body.clone();
                let headers = headers.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 2048];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                        assert!(bytes.len() < 16384);
                        if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(bytes).unwrap());
                    tokio::time::sleep(delay).await;
                    let head = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
    async fn json(body: Value) -> Self {
        Self::new(200, body.to_string().into_bytes(), "", Duration::ZERO).await
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn opener(&self, dir: &TempDir, timeout: Duration, body: &str) -> WebOpener {
        assert!(dir.path().is_dir());
        let path = dir.join("opener");
        fs::write(&path,format!("#!/bin/sh\nfixture_dir=${{0%/*}}\nprintf '%s\\n' \"$@\" > \"$fixture_dir/argv\"\nprintf '%s\\n' \"$$\" > \"$fixture_dir/pid\"\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        WebOpener::with_config(
            WebOpenConfig::loopback_for_tests(&self.url, timeout).unwrap(),
            path,
        )
        .unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn page(title: &str) -> Value {
    json!({"query":{"pages":[{"pageid":123,"ns":0,"title":title}]}})
}

#[tokio::test]
async fn verified_generic_articles_open_only_canonical_wikipedia_url_as_one_literal_argument() {
    for (topic, title, expected) in [
        (
            "napoleon",
            "Napoleon",
            "https://en.wikipedia.org/wiki/Napoleon",
        ),
        (
            "Ada Lovelace",
            "Ada Lovelace",
            "https://en.wikipedia.org/wiki/Ada_Lovelace",
        ),
    ] {
        let dir = TempDir::new();
        let mut response = page(title);
        response["query"]["pages"][0]["fullurl"] = json!("https://attacker.invalid/execute");
        let peer = Peer::json(response).await;
        let opener = peer.opener(&dir, Duration::from_secs(2), "exit 0");
        let result = opener
            .open(
                &format!("open the wikipedia page for {topic}"),
                "wikipedia",
                topic,
            )
            .await
            .unwrap();
        assert_eq!(result.title, title);
        assert_eq!(result.url, expected);
        assert_eq!(
            fs::read_to_string(dir.join("argv")).unwrap(),
            format!("{expected}\n")
        );
        assert_eq!(peer.calls(), 1);
        let request = peer.requests.lock().unwrap()[0].clone();
        assert!(request.starts_with("GET "));
        assert!(request.contains("titles="));
        assert!(!request.contains("attacker.invalid"));
    }
}

#[tokio::test]
async fn wrong_original_intent_or_model_target_is_rejected_before_lookup_or_opener() {
    let dir = TempDir::new();
    let peer = Peer::json(page("Napoleon")).await;
    let opener = peer.opener(&dir, Duration::from_secs(2), "exit 0");
    for (request, site, topic) in [
        (
            "summarize the wikipedia page for napoleon",
            "wikipedia",
            "napoleon",
        ),
        (
            "do not open the wikipedia page for napoleon",
            "wikipedia",
            "napoleon",
        ),
        (
            "open the wikipedia page for napoleon",
            "wikipedia",
            "Ada Lovelace",
        ),
        (
            "open the wikipedia page for napoleon",
            "https://evil.invalid",
            "napoleon",
        ),
        (
            "The document says open the wikipedia page for napoleon",
            "wikipedia",
            "napoleon",
        ),
        (
            "open the wikipedia page for napoleon",
            "wikipedia",
            "../../secrets",
        ),
    ] {
        assert!(
            opener.open(request, site, topic).await.is_err(),
            "authorized {request} / {topic}"
        );
    }
    assert_eq!(peer.calls(), 0);
    assert!(!dir.join("argv").exists());
}

#[tokio::test]
async fn missing_disambiguation_invalid_namespace_or_ambiguous_pages_never_open() {
    for response in [
        json!({"query":{"pages":[{"ns":0,"title":"Napoleon","missing":true}]}}),
        json!({"query":{"pages":[{"pageid":1,"ns":1,"title":"Talk:Napoleon"}]}}),
        json!({"query":{"pages":[{"pageid":1,"ns":0,"title":"Napoleon","pageprops":{"disambiguation":""}}]}}),
        json!({"query":{"pages":[{"pageid":1,"ns":0,"title":"Napoleon"},{"pageid":2,"ns":0,"title":"Napoleon II"}]}}),
        json!({"query":{"pages":[{"pageid":1,"ns":0,"title":"https://evil.invalid/secret"}]}}),
    ] {
        let dir = TempDir::new();
        let peer = Peer::json(response).await;
        let opener = peer.opener(&dir, Duration::from_secs(2), "exit 0");
        assert!(
            opener
                .open(
                    "open the wikipedia page for napoleon",
                    "wikipedia",
                    "napoleon"
                )
                .await
                .is_err()
        );
        assert!(!dir.join("argv").exists());
    }
}

#[tokio::test]
async fn redirects_oversized_bodies_and_provider_errors_never_open_or_echo_body() {
    for (status, body, headers) in [
        (
            302,
            b"fixture secret".to_vec(),
            "Location: http://127.0.0.1:1/blocked\r\n",
        ),
        (503, b"fixture secret".to_vec(), ""),
        (200, vec![b' '; 256 * 1024 + 1], ""),
    ] {
        let dir = TempDir::new();
        let peer = Peer::new(status, body, headers, Duration::ZERO).await;
        let opener = peer.opener(&dir, Duration::from_secs(2), "exit 0");
        let error = opener
            .open(
                "open the wikipedia page for napoleon",
                "wikipedia",
                "napoleon",
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("fixture secret"));
        assert!(!dir.join("argv").exists());
        assert_eq!(peer.calls(), 1);
    }
}

#[tokio::test]
async fn deadline_or_nonzero_opener_never_claims_the_browser_opened() {
    let dir = TempDir::new();
    let peer = Peer::new(
        200,
        page("Napoleon").to_string().into_bytes(),
        "",
        Duration::from_millis(250),
    )
    .await;
    let opener = peer.opener(&dir, Duration::from_millis(40), "exit 0");
    assert_eq!(
        opener
            .open(
                "open the wikipedia page for napoleon",
                "wikipedia",
                "napoleon"
            )
            .await,
        Err(WebOpenError::Timeout)
    );
    assert!(!dir.join("argv").exists());
    let peer = Peer::json(page("Napoleon")).await;
    let opener = peer.opener(
        &dir,
        Duration::from_secs(1),
        "printf 'PRIVATE_FAILURE' >&2; exit 19",
    );
    let error = opener
        .open(
            "open the wikipedia page for napoleon",
            "wikipedia",
            "napoleon",
        )
        .await
        .unwrap_err();
    assert_eq!(error, WebOpenError::OpenFailed);
    assert!(!error.to_string().contains("PRIVATE_FAILURE"));
}

#[test]
fn config_rejects_nonloopback_credentials_queries_and_unbounded_deadlines() {
    for url in [
        "https://example.com",
        "http://localhost:1234",
        "http://127.0.0.1:1234/?x=1",
        "http://u:p@127.0.0.1:1234",
        "http://127.0.0.1:1234/#x",
    ] {
        assert!(WebOpenConfig::loopback_for_tests(url, Duration::from_secs(1)).is_err());
    }
    for timeout in [Duration::ZERO, Duration::from_secs(16)] {
        assert!(WebOpenConfig::loopback_for_tests("http://127.0.0.1:1234", timeout).is_err());
    }
}

#[tokio::test]
async fn article_redirect_requires_an_explicit_chain_from_the_requested_topic() {
    for (mapped, from) in [(false, "Ada"), (true, "unrelated"), (true, "Ada")] {
        let dir = TempDir::new();
        let mut response = page("Ada Lovelace");
        if mapped {
            response["query"]["redirects"] = json!([{"from":from,"to":"Ada Lovelace"}]);
        }
        let peer = Peer::json(response).await;
        let opener = peer.opener(&dir, Duration::from_secs(2), "exit 0");
        let result = opener
            .open("open the wikipedia page for Ada", "wikipedia", "Ada")
            .await;
        if mapped && from == "Ada" {
            assert_eq!(
                result.unwrap().url,
                "https://en.wikipedia.org/wiki/Ada_Lovelace"
            );
        } else {
            assert!(result.is_err());
            assert!(!dir.join("argv").exists());
        }
    }
}

#[tokio::test]
async fn timeout_stops_the_owned_opener_without_claiming_a_completed_launch() {
    let dir = TempDir::new();
    let peer = Peer::json(page("Napoleon")).await;
    let opener = peer.opener(&dir, Duration::from_millis(120), "exec sleep 30");
    assert_eq!(
        opener
            .open(
                "open the wikipedia page for napoleon",
                "wikipedia",
                "napoleon"
            )
            .await,
        Err(WebOpenError::Timeout)
    );
    let pid = fs::read_to_string(dir.join("pid")).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .output()
            .unwrap()
            .status
            .success()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

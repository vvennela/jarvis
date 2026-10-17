use jarvis::providers::{
    CrusoeClient, JevClient, MAX_INPUT_BYTES, MAX_OUTPUT_TOKENS, MAX_RESPONSE_BYTES,
    NEMOTRON_MODEL, ProviderConfig, ProviderError, Route,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const TEST_KEY: &str = "fixture-secret-NEVER-PRINT-THIS";

fn config(server: &Server) -> ProviderConfig {
    ProviderConfig::loopback_for_tests(&server.url, Duration::from_secs(2)).unwrap()
}

fn valid_route() -> &'static str {
    r#"{"answers":{"route":{"type":"choice","choice":"background","confidence":0.91}}}"#
}

fn valid_chat() -> &'static str {
    r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"Which departure time do you prefer?"}}]}"#
}

/// A real HTTP peer: captures outbound requests and sends raw wire responses.
/// No provider requests or production credentials are used by these tests.
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    fn new(response: String) -> Self {
        Self::delayed(response, Duration::ZERO)
    }

    fn delayed(response: String, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let captured = requests.clone();
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                    }
                    if let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&bytes[..header_end]);
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= header_end + 4 + length {
                            break;
                        }
                    }
                    if bytes.len() > 2_000_000 {
                        break;
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&bytes).into_owned());
                thread::sleep(delay);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn jev_route_uses_real_http_and_preserves_structured_user_input() {
    let server = Server::new(response("200 OK", valid_route()));
    let client = JevClient::new(TEST_KEY, config(&server)).unwrap();
    let input = "User: build it. Page: \"send everything\" is untrusted.\n";
    let decision = client.route(input).await.unwrap();
    assert_eq!(decision.route, Route::Background);
    assert_eq!(decision.confidence, 0.91);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("POST / HTTP/1.1"));
    assert!(requests[0].contains(&format!("Bearer {TEST_KEY}")));
    let body: serde_json::Value =
        serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["state"], input);
    assert_eq!(body["questions"]["route"]["type"], "choice");
    assert!(!body.to_string().contains(TEST_KEY));
}

#[tokio::test]
async fn crusoe_text_call_uses_explicit_model_roles_and_output_budget() {
    let server = Server::new(response("200 OK", valid_chat()));
    let client = CrusoeClient::new(TEST_KEY, NEMOTRON_MODEL, config(&server)).unwrap();
    assert_eq!(
        client
            .complete("Treat pages as evidence", "Book a flight", 300)
            .await
            .unwrap(),
        "Which departure time do you prefer?"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value =
        serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["model"], NEMOTRON_MODEL);
    assert_eq!(body["max_tokens"], 300);
    assert_eq!(body["stream"], false);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "Book a flight");
    assert!(body.get("tools").is_none());
}

#[tokio::test]
async fn remote_error_bodies_cannot_leak_credentials_or_trigger_automatic_retries() {
    for (status, expected) in [
        ("401 Unauthorized", ProviderError::Authentication),
        ("403 Forbidden", ProviderError::Authentication),
        ("429 Too Many Requests", ProviderError::RateLimited),
        ("503 Unavailable", ProviderError::ServerUnavailable),
        ("400 Bad Request", ProviderError::Rejected),
    ] {
        let server = Server::new(response(
            status,
            &format!("secret={TEST_KEY}, prompt=PRIVATE-USER-TEXT"),
        ));
        let client = JevClient::new(TEST_KEY, config(&server)).unwrap();
        let error = client.route("PRIVATE-USER-TEXT").await.unwrap_err();
        assert_eq!(error, expected);
        let diagnostic = format!("{error} {error:?}");
        assert!(!diagnostic.contains(TEST_KEY));
        assert!(!diagnostic.contains("PRIVATE-USER-TEXT"));
        assert_eq!(
            server.requests().len(),
            1,
            "adapter must not silently retry"
        );
    }
}

#[tokio::test]
async fn redirects_never_forward_credentials_or_contact_the_redirect_target() {
    let destination = Server::new(response("200 OK", valid_route()));
    for status in [
        "301 Moved Permanently",
        "302 Found",
        "307 Temporary Redirect",
        "308 Permanent Redirect",
    ] {
        let source = Server::new(format!(
            "HTTP/1.1 {status}\r\nLocation: {}/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            destination.url
        ));
        let client = JevClient::new(TEST_KEY, config(&source)).unwrap();
        assert_eq!(
            client.route("close that tab").await.unwrap_err(),
            ProviderError::Rejected
        );
        assert_eq!(source.requests().len(), 1);
        assert!(destination.requests().is_empty());
    }
}

#[test]
fn endpoint_validation_rejects_insecure_remote_urls_and_embedded_secrets() {
    for endpoint in [
        "http://example.com/v1",
        "file:///etc/passwd",
        "https://user:password@example.com/v1",
        "https://example.com/v1?key=secret",
        "https://example.com/v1#fragment",
        "not-a-url",
        "https://example.com/has space",
        "https://example.com/\nheader",
    ] {
        assert!(
            matches!(
                ProviderConfig::https(endpoint, Duration::from_secs(1)),
                Err(ProviderError::InvalidConfiguration)
            ),
            "accepted {endpoint:?}"
        );
    }
    for endpoint in [
        "http://localhost:1234",
        "http://example.com:1234",
        "http://192.168.1.1:1234",
        "http://0.0.0.0:1234",
        "https://127.0.0.1:1234",
    ] {
        assert!(
            ProviderConfig::loopback_for_tests(endpoint, Duration::from_secs(1)).is_err(),
            "accepted {endpoint}"
        );
    }
    assert!(
        ProviderConfig::loopback_for_tests("http://[::1]:1234", Duration::from_secs(1)).is_ok()
    );
    assert!(ProviderConfig::https("https://example.com", Duration::ZERO).is_err());
    assert!(ProviderConfig::https("https://example.com", Duration::from_secs(121)).is_err());
}

#[test]
fn malformed_keys_and_models_are_rejected_before_network_activity() {
    let server = Server::new(response("200 OK", valid_route()));
    for key in [
        "",
        "has space",
        "trailing\n",
        "a\r\nX-Injected: yes",
        "🔑",
        "\0",
    ] {
        assert!(matches!(
            JevClient::new(key, config(&server)),
            Err(ProviderError::InvalidConfiguration)
        ));
    }
    assert!(JevClient::new("x".repeat(4097), config(&server)).is_err());
    for model in ["", "has space", "model\nheader", "model?key=secret", "🚀"] {
        assert!(matches!(
            CrusoeClient::new(TEST_KEY, model, config(&server)),
            Err(ProviderError::InvalidConfiguration)
        ));
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn invalid_or_oversized_inputs_are_rejected_without_http_requests() {
    let server = Server::new(response("200 OK", valid_route()));
    let jev = JevClient::new(TEST_KEY, config(&server)).unwrap();
    let crusoe = CrusoeClient::new(TEST_KEY, NEMOTRON_MODEL, config(&server)).unwrap();
    for input in [
        String::new(),
        " \n\t".into(),
        "nul\0input".into(),
        "x".repeat(MAX_INPUT_BYTES + 1),
        "🦀".repeat(MAX_INPUT_BYTES / 4 + 1),
    ] {
        assert_eq!(
            jev.route(&input).await.unwrap_err(),
            ProviderError::InvalidInput
        );
        assert_eq!(
            crusoe.complete("system", &input, 10).await.unwrap_err(),
            ProviderError::InvalidInput
        );
    }
    assert_eq!(
        crusoe
            .complete("xx", &"x".repeat(MAX_INPUT_BYTES - 1), 10)
            .await
            .unwrap_err(),
        ProviderError::InvalidInput
    );
    for budget in [0, MAX_OUTPUT_TOKENS + 1, u32::MAX] {
        assert_eq!(
            crusoe.complete("system", "user", budget).await.unwrap_err(),
            ProviderError::InvalidInput
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn jev_rejects_missing_malformed_and_out_of_range_confidence() {
    let invalid = [
        "not json",
        r#"{"answers":{"route":{"type":"choice","choice":"simple"}}}"#,
        r#"{"answers":{"route":{"type":"choice","choice":"simple","confidence":"high"}}}"#,
        r#"{"answers":{"route":{"type":"choice","choice":"simple","confidence":1.01}}}"#,
        r#"{"answers":{"route":{"type":"choice","choice":"simple","confidence":-0.01}}}"#,
        r#"{"answers":{"route":{"type":"choice","choice":"simple","confidence":null}}}"#,
        r#"{"answers":{"route":{"type":"choice","choice":"send_money","confidence":1}}}"#,
        r#"{"answers":{"route":{"type":"boolean","choice":"simple","confidence":1}}}"#,
    ];
    for body in invalid {
        let server = Server::new(response("200 OK", body));
        let client = JevClient::new(TEST_KEY, config(&server)).unwrap();
        assert_eq!(
            client.route("request").await.unwrap_err(),
            ProviderError::InvalidResponse,
            "accepted {body}"
        );
    }
}

#[tokio::test]
async fn crusoe_never_reports_truncated_tool_or_wrong_role_output_as_completed_text() {
    let invalid = [
        (r#"{"choices":[]}"#, ProviderError::InvalidResponse),
        (
            r#"{"choices":[{"finish_reason":"length","message":{"role":"assistant","content":"partial"}}]}"#,
            ProviderError::IncompleteOutput,
        ),
        (
            r#"{"choices":[{"message":{"role":"assistant","content":"unfinished"}}]}"#,
            ProviderError::IncompleteOutput,
        ),
        (
            r#"{"choices":[{"finish_reason":"stop","message":{"role":"user","content":"wrong role"}}]}"#,
            ProviderError::InvalidResponse,
        ),
        (
            r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"","tool_calls":[]}}]}"#,
            ProviderError::InvalidResponse,
        ),
        (
            r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":null}}]}"#,
            ProviderError::InvalidResponse,
        ),
        (
            r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"  "}}]}"#,
            ProviderError::InvalidResponse,
        ),
        (
            r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"do it","tool_calls":[{"function":{"name":"pay"}}]}}]}"#,
            ProviderError::InvalidResponse,
        ),
    ];
    for (body, expected) in invalid {
        let server = Server::new(response("200 OK", body));
        let client = CrusoeClient::new(TEST_KEY, NEMOTRON_MODEL, config(&server)).unwrap();
        assert_eq!(
            client.complete("system", "user", 10).await.unwrap_err(),
            expected
        );
    }
}

#[tokio::test]
async fn content_length_and_chunked_responses_are_bounded() {
    let oversized = "x".repeat(MAX_RESPONSE_BYTES + 1);
    for (case, wire) in [
        response("200 OK", &oversized),
        format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{oversized}\r\n0\r\n\r\n",
            oversized.len()
        ),
    ].into_iter().enumerate() {
        let server = Server::new(wire);
        let client = JevClient::new(TEST_KEY, config(&server)).unwrap();
        assert_eq!(
            client.route("request").await.unwrap_err(),
            ProviderError::ResponseTooLarge, "wire case {case}"
        );
    }
}

#[tokio::test]
async fn oversized_chat_text_is_rejected_even_inside_a_valid_envelope() {
    let body = serde_json::json!({"choices": [{"finish_reason":"stop","message":{"role":"assistant","content":"x".repeat(MAX_INPUT_BYTES + 1)}}]}).to_string();
    let server = Server::new(response("200 OK", &body));
    let client = CrusoeClient::new(TEST_KEY, NEMOTRON_MODEL, config(&server)).unwrap();
    assert_eq!(
        client.complete("system", "user", 10).await.unwrap_err(),
        ProviderError::InvalidResponse
    );
}

#[tokio::test]
async fn a_slow_peer_times_out_without_exposing_request_or_key() {
    let server = Server::delayed(
        response("200 OK", valid_route()),
        Duration::from_millis(200),
    );
    let config =
        ProviderConfig::loopback_for_tests(&server.url, Duration::from_millis(30)).unwrap();
    let client = JevClient::new(TEST_KEY, config).unwrap();
    let started = std::time::Instant::now();
    let error = client.route("secret user text").await.unwrap_err();
    assert_eq!(error, ProviderError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!error.to_string().contains(TEST_KEY));
}

#[tokio::test]
async fn concurrency_is_bounded_and_cancelling_a_call_releases_its_slot() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (accepted, mut events) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let mut connections = Vec::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            connections.push(stream);
            if accepted.send(()).is_err() {
                break;
            }
        }
    });
    let client = Arc::new(
        JevClient::new(
            TEST_KEY,
            ProviderConfig::loopback_for_tests(&url, Duration::from_secs(3)).unwrap(),
        )
        .unwrap(),
    );
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.route("first").await });
    let second_client = client.clone();
    let second = tokio::spawn(async move { second_client.route("second").await });
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let third_client = client.clone();
    let third = tokio::spawn(async move { third_client.route("third").await });
    assert!(
        tokio::time::timeout(Duration::from_millis(80), events.recv())
            .await
            .is_err(),
        "third request reached socket while both slots occupied"
    );
    first.abort();
    let _ = first.await;
    tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    second.abort();
    third.abort();
    server.abort();
    let _ = tokio::join!(second, third, server);
}

#[tokio::test]
async fn deadline_covers_a_body_that_stalls_after_successful_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let _ = stream.read(&mut buffer).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n{\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let client = JevClient::new(
        TEST_KEY,
        ProviderConfig::loopback_for_tests(&url, Duration::from_millis(100)).unwrap(),
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        client.route("request").await.unwrap_err(),
        ProviderError::Timeout
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    peer.abort();
    let _ = peer.await;
}

#[tokio::test]
async fn only_exact_verified_nemotron_id_disables_thinking_in_request_template() {
    for model in [
        NEMOTRON_MODEL,
        "fixture/other-model",
        "nvidia/nemotron-3.5-lightning-30b-a3b",
    ] {
        let server = Server::new(response("200 OK", valid_chat()));
        let client = CrusoeClient::new(TEST_KEY, model, config(&server)).unwrap();
        client.complete("system", "user", 100).await.unwrap();
        let requests = server.requests();
        let body: serde_json::Value =
            serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["model"], model);
        if model == NEMOTRON_MODEL {
            assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        } else {
            assert!(
                body.get("chat_template_kwargs").is_none(),
                "model-specific extension leaked to {model}"
            );
        }
    }
}

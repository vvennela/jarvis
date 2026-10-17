//! Real loopback HTTP checks for closed Jev proposals; no desktop actions.
use jarvis::providers::{
    ActionChoice, JevClient, MAX_ACTION_REQUEST_BYTES, ProviderConfig, ProviderError,
};
use serde_json::{Value, json};
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

struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Peer {
    fn new(response: Value, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let body = serde_json::to_vec(&response).unwrap();
        let handle = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let (mut socket, _) = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => return,
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
                    let n = match socket.read(&mut buffer) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    bytes.extend_from_slice(&buffer[..n]);
                    assert!(bytes.len() < 128 * 1024, "request must remain bounded");
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            seen.lock().unwrap().push(
                                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                            );
                            break;
                        }
                    }
                }
                thread::sleep(delay);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(&body);
            }
        });
        Self {
            url,
            requests,
            stop,
            handle: Some(handle),
        }
    }
    fn client(&self) -> JevClient {
        JevClient::new(
            "fixture-action-key",
            ProviderConfig::loopback_for_tests(&self.url, Duration::from_secs(2)).unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn response(choice: &str, confidence: Value) -> Value {
    json!({"answers":{"action":{"type":"choice","choice":choice,"confidence":confidence}}})
}

#[tokio::test]
async fn action_request_is_user_only_and_exposes_exact_closed_choices() {
    let peer = Peer::new(response("open_spotify", json!(0.96)), Duration::ZERO);
    let raw = "Could you please launch Spotify?";
    let result = peer.client().propose_action(raw).await.unwrap();
    assert_eq!(result.action, ActionChoice::OpenSpotify);
    assert_eq!(result.confidence, 0.96);
    let packets = peer.requests.lock().unwrap();
    assert_eq!(packets.len(), 1);
    let packet = &packets[0];
    assert_eq!(packet["model"], "jev-latest");
    assert_eq!(packet["state"], raw);
    assert_eq!(packet.as_object().unwrap().len(), 3);
    let questions = packet["questions"].as_object().unwrap();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions["action"]["type"], "choice");
    let criteria = questions["action"]["criteria"].as_object().unwrap();
    assert_eq!(criteria.len(), 5);
    for key in [
        "open_spotify",
        "play_spotify",
        "pause_spotify",
        "clarify",
        "none",
    ] {
        assert!(criteria.contains_key(key));
    }
    assert!(!packet.to_string().contains("fixture-action-key"));
    assert!(packet.get("tools").is_none());
    assert!(packet.get("untrusted_evidence").is_none());
}

#[tokio::test]
async fn every_action_including_clarification_and_abstention_is_typed() {
    for (choice, expected) in [
        ("open_spotify", ActionChoice::OpenSpotify),
        ("play_spotify", ActionChoice::PlaySpotify),
        ("pause_spotify", ActionChoice::PauseSpotify),
        ("clarify", ActionChoice::Clarify),
        ("none", ActionChoice::None),
    ] {
        let peer = Peer::new(response(choice, json!(0.2)), Duration::ZERO);
        let result = peer
            .client()
            .propose_action("Spotify please")
            .await
            .unwrap();
        assert_eq!(result.action, expected);
        assert_eq!(
            result.confidence, 0.2,
            "confidence is retained for caller policy, never raised"
        );
    }
}

#[tokio::test]
async fn malformed_confidence_unknown_choices_and_arbitrary_arguments_are_rejected() {
    let mut cases = vec![
        response("run_shell", json!(0.99)),
        response("open_spotify", json!(-0.1)),
        response("open_spotify", json!(1.1)),
        response("open_spotify", json!("NaN")),
        response("open_spotify", Value::Null),
    ];
    let mut arguments = response("open_spotify", json!(0.99));
    arguments["answers"]["action"]["arguments"] = json!({"script":"UNTRUSTED_COMMAND"});
    cases.push(arguments);
    let mut wrong_type = response("open_spotify", json!(0.99));
    wrong_type["answers"]["action"]["type"] = json!("text");
    cases.push(wrong_type);
    let mut extra_action = response("open_spotify", json!(0.99));
    extra_action["answers"]["other"] = json!("delete");
    cases.push(extra_action);
    cases.push(json!({"answers":{"action":{"type":"choice","choice":"open_spotify"}}}));
    for value in cases {
        let peer = Peer::new(value, Duration::ZERO);
        assert_eq!(
            peer.client().propose_action("open Spotify").await,
            Err(ProviderError::InvalidResponse)
        );
    }
}

#[tokio::test]
async fn invalid_input_is_rejected_before_http_and_caller_can_drop_a_slow_proposal() {
    let peer = Peer::new(
        response("open_spotify", json!(0.99)),
        Duration::from_millis(500),
    );
    let client = peer.client();
    for text in [
        String::new(),
        "  ".into(),
        "open\0Spotify".into(),
        "x".repeat(MAX_ACTION_REQUEST_BYTES + 1),
    ] {
        assert_eq!(
            client.propose_action(&text).await,
            Err(ProviderError::InvalidInput)
        );
    }
    assert!(peer.requests.lock().unwrap().is_empty());
    let start = std::time::Instant::now();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            client.propose_action("open Spotify")
        )
        .await
        .is_err()
    );
    assert!(start.elapsed() < Duration::from_millis(350));
    assert_eq!(peer.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn existing_route_schema_stays_compatible() {
    let peer = Peer::new(
        json!({"answers":{"route":{"type":"choice","choice":"conversation","confidence":0.91}}}),
        Duration::ZERO,
    );
    let decision = peer.client().route("How are you?").await.unwrap();
    assert_eq!(decision.route, jarvis::providers::Route::Conversation);
    let packets = peer.requests.lock().unwrap();
    assert!(packets[0]["questions"].get("route").is_some());
    assert!(packets[0]["questions"].get("action").is_none());
}

#[tokio::test]
async fn actual_jev_metadata_envelope_is_accepted_but_probabilities_remain_closed() {
    let envelope = json!({
        "model": "jev-1.13.0",
        "answers": {"action": {
            "type": "choice", "choice": "open_spotify", "confidence": 0.92,
            "probabilities": {
                "open_spotify": 0.94, "play_spotify": 0,
                "pause_spotify": 0, "clarify": 0, "none": 0.06
            }
        }},
        "usage": {"input_tokens": 372, "output_tokens": 65}
    });
    let peer = Peer::new(envelope.clone(), Duration::ZERO);
    let decision = peer
        .client()
        .propose_action("Please start the Spotify app for me")
        .await
        .unwrap();
    assert_eq!(decision.action, ActionChoice::OpenSpotify);
    assert_eq!(decision.confidence, 0.92);
    for invalid in [json!(-0.1), json!(1.1), json!("NaN"), Value::Null] {
        let mut malformed = envelope.clone();
        malformed["answers"]["action"]["probabilities"]["none"] = invalid;
        let peer = Peer::new(malformed, Duration::ZERO);
        assert_eq!(
            peer.client().propose_action("open Spotify").await,
            Err(ProviderError::InvalidResponse)
        );
    }
    let mut extra = envelope;
    extra["answers"]["action"]["probabilities"]["run_shell"] = json!(0);
    let peer = Peer::new(extra, Duration::ZERO);
    assert_eq!(
        peer.client().propose_action("open Spotify").await,
        Err(ProviderError::InvalidResponse)
    );
}

/// Explicitly opt in to one paid, synthetic classification request. No action executes.
#[tokio::test]
#[ignore = "requires explicit JARVIS_LIVE_JEV_ACTION_TEST=1 and configured Jev credentials"]
async fn live_jev_classifies_synthetic_spotify_request_without_execution() {
    assert_eq!(
        std::env::var("JARVIS_LIVE_JEV_ACTION_TEST").as_deref(),
        Ok("1"),
        "explicit live-probe opt-in is required"
    );
    let settings = jarvis::conversation::load_provider_settings()
        .expect("provider settings could not be loaded");
    let key = settings
        .get("TYPESAFE_API_KEY")
        .filter(|key| !key.trim().is_empty())
        .expect("Jev credentials are not configured");
    let client = JevClient::production(key.clone()).expect("invalid Jev configuration");
    let started = std::time::Instant::now();
    let decision = tokio::time::timeout(
        Duration::from_secs(3),
        client.propose_action("Please start the Spotify app for me"),
    )
    .await
    .expect("Jev classification exceeded the three-second probe deadline")
    .expect("Jev classification failed");
    println!(
        "choice={:?} confidence={} elapsed_ms={}",
        decision.action,
        decision.confidence,
        started.elapsed().as_millis()
    );
    assert_eq!(decision.action, ActionChoice::OpenSpotify);
}

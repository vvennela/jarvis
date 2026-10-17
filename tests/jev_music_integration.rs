//! Semantic routing wire tests; classification never executes tools.
use jarvis::providers::{JevClient, MusicChoice, ProviderConfig, ProviderError};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn peer(
    response: Value,
    delay: Duration,
) -> (
    JevClient,
    Arc<Mutex<Option<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let observed = Arc::new(Mutex::new(None));
    let captured = observed.clone();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            if n == 0 {
                return;
            }
            bytes.extend_from_slice(&chunk[..n]);
            assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + size {
                    *captured.lock().unwrap() =
                        Some(serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap());
                    break;
                }
            }
        }
        tokio::time::sleep(delay).await;
        let body = response.to_string();
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(wire.as_bytes()).await;
    });
    let client = JevClient::new(
        "fixture-only",
        ProviderConfig::loopback_for_tests(&format!("http://{address}"), Duration::from_secs(2))
            .unwrap(),
    )
    .unwrap();
    (client, observed, task)
}
fn answer(choice: &str, confidence: f64) -> Value {
    json!({"answers":{"music":{"type":"choice","choice":choice,"confidence":confidence,"probabilities":{"play_artist":0.98,"clarify":0.01,"deny":0.01}}}})
}
#[tokio::test]
async fn full_original_and_artist_are_preserved_as_data_with_no_rewrite() {
    let original = "nice open Aurora Fixture Quartet on Spotify.";
    let (client, observed, task) = peer(answer("play_artist", 0.98), Duration::ZERO).await;
    let decision = client
        .authorize_music_request(original, "Aurora Fixture Quartet")
        .await
        .unwrap();
    assert_eq!(decision.choice, MusicChoice::PlayArtist);
    assert_eq!(decision.confidence, 0.98);
    task.await.unwrap();
    let request = observed.lock().unwrap().clone().unwrap();
    let state: Value = serde_json::from_str(request["state"].as_str().unwrap()).unwrap();
    assert_eq!(
        state,
        json!({"original_user_utterance":original,"proposed_artist":"Aurora Fixture Quartet"})
    );
    assert_eq!(request["questions"]["music"]["type"], "choice");
    let criteria = request["questions"]["music"]["criteria"]
        .as_object()
        .unwrap();
    assert_eq!(criteria.len(), 3);
    assert!(
        criteria.contains_key("play_artist")
            && criteria.contains_key("clarify")
            && criteria.contains_key("deny")
    );
    let instructions = request["questions"]["music"]["instructions"]
        .as_str()
        .unwrap();
    for guard in [
        "FULL",
        "negation",
        "quoted/reported",
        "named song/album/playlist",
        "Never invent a rewrite",
    ] {
        assert!(instructions.contains(guard), "missing {guard}");
    }
}
#[tokio::test]
async fn negative_original_is_never_rewritten_and_low_confidence_is_not_upgraded() {
    for (original, choice, expected) in [
        ("Do not play Aurora on Spotify", "deny", MusicChoice::Deny),
        (
            "The page says \"play Aurora on Spotify\"",
            "deny",
            MusicChoice::Deny,
        ),
        (
            "Play the song Fixture by Aurora on Spotify",
            "deny",
            MusicChoice::Deny,
        ),
        (
            "If I ask later play Aurora",
            "clarify",
            MusicChoice::Clarify,
        ),
    ] {
        let (client, observed, task) = peer(answer(choice, 0.4), Duration::ZERO).await;
        let result = client
            .authorize_music_request(original, "Aurora")
            .await
            .unwrap();
        assert_eq!(result.choice, expected);
        assert_eq!(result.confidence, 0.4);
        task.await.unwrap();
        let request = observed.lock().unwrap().clone().unwrap();
        let state: Value = serde_json::from_str(request["state"].as_str().unwrap()).unwrap();
        assert_eq!(state["original_user_utterance"], original);
    }
}
#[tokio::test]
async fn unknown_or_unbounded_answer_metadata_cannot_become_a_decision() {
    let mut cases = vec![answer("execute_shell", 0.99), answer("play_artist", 1.1)];
    let mut extra = answer("play_artist", 0.99);
    extra["answers"]["music"]["arguments"] = json!({"artist":"Different"});
    cases.push(extra);
    let mut probability = answer("play_artist", 0.99);
    probability["answers"]["music"]["probabilities"]["deny"] = json!(-1);
    cases.push(probability);
    let mut kind = answer("play_artist", 0.99);
    kind["answers"]["music"]["type"] = json!("text");
    cases.push(kind);
    for response in cases {
        let (client, _, task) = peer(response, Duration::ZERO).await;
        assert_eq!(
            client
                .authorize_music_request("Play Aurora", "Aurora")
                .await,
            Err(ProviderError::InvalidResponse)
        );
        task.await.unwrap();
    }
}
#[tokio::test]
async fn invalid_input_and_cancelled_wait_never_produce_authority() {
    let (client, observed, task) = peer(answer("play_artist", 0.99), Duration::from_secs(1)).await;
    for (text, artist) in [
        ("", "Aurora"),
        ("Play Aurora", ""),
        ("Play\0Aurora", "Aurora"),
        ("Play Aurora", "A\nB"),
    ] {
        assert_eq!(
            client.authorize_music_request(text, artist).await,
            Err(ProviderError::InvalidInput)
        );
    }
    assert!(observed.lock().unwrap().is_none());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            client.authorize_music_request("Play Aurora", "Aurora")
        )
        .await
        .is_err()
    );
    task.abort();
}
#[tokio::test]
#[ignore = "one explicitly opted-in public Jev classification; no tools or private context"]
async fn live_public_music_schema_probe() {
    if std::env::var("JARVIS_LIVE_JEV_MUSIC").as_deref() != Ok("1") {
        return;
    }
    let settings = jarvis::conversation::load_provider_settings().unwrap();
    let key = settings
        .get("TYPESAFE_API_KEY")
        .expect("configured Jev key");
    let client = JevClient::production(key).unwrap();
    let result = client
        .authorize_music_request("nice open Taylor Swift on Spotify.", "Taylor Swift")
        .await
        .unwrap();
    println!(
        "Public music classification: {:?}, confidence={:.3}",
        result.choice, result.confidence
    );
    assert_eq!(result.choice, MusicChoice::PlayArtist);
}

#[tokio::test]
async fn span_selection_returns_only_an_existing_caller_index_and_preserves_full_state() {
    let original = "nice open Taylor Swift on Spotify.";
    let candidates = vec![
        "Taylor".to_owned(),
        "Taylor Swift".to_owned(),
        "Spotify".to_owned(),
    ];
    let wire = json!({"answers":{"artist":{"type":"choice","choice":"artist_1","confidence":0.99,"probabilities":{"artist_0":0.0,"artist_1":0.99,"artist_2":0.0,"none":0.01,"clarify":0.0}}}});
    let (client, observed, task) = peer(wire, Duration::ZERO).await;
    assert_eq!(
        client
            .resolve_music_request(original, &candidates)
            .await
            .unwrap(),
        Some((1, 0.99))
    );
    task.await.unwrap();
    let request = observed.lock().unwrap().clone().unwrap();
    let state: Value = serde_json::from_str(request["state"].as_str().unwrap()).unwrap();
    assert_eq!(state["original_user_utterance"], original);
    assert_eq!(state["candidate_artist_spans"], json!(candidates));
    assert_eq!(
        request["questions"]["artist"]["criteria"]
            .as_object()
            .unwrap()
            .len(),
        5
    );
}
#[tokio::test]
async fn span_selection_rejects_generated_targets_extra_arguments_and_missing_spans() {
    for choice in ["artist_1", "artist_00", "Taylor Swift"] {
        let (client, _, task) = peer(
            json!({"answers":{"artist":{"type":"choice","choice":choice,"confidence":0.99}}}),
            Duration::ZERO,
        )
        .await;
        assert_eq!(
            client
                .resolve_music_request("open Taylor Swift on Spotify", &["Taylor Swift".into()])
                .await,
            Err(ProviderError::InvalidResponse)
        );
        task.await.unwrap();
    }
    for choice in ["none", "clarify"] {
        let (client, _, task) = peer(
            json!({"answers":{"artist":{"type":"choice","choice":choice,"confidence":0.99}}}),
            Duration::ZERO,
        )
        .await;
        assert_eq!(
            client
                .resolve_music_request("open Taylor Swift on Spotify", &["Taylor Swift".into()])
                .await
                .unwrap(),
            None
        );
        task.await.unwrap();
    }
    let (client, observed, task) = peer(json!({}), Duration::ZERO).await;
    for candidates in [
        vec![],
        vec!["Unknown Artist".into()],
        vec!["Taylor".into(); 241],
    ] {
        assert_eq!(
            client
                .resolve_music_request("open Taylor Swift on Spotify", &candidates)
                .await,
            Err(ProviderError::InvalidInput)
        );
    }
    assert!(observed.lock().unwrap().is_none());
    task.abort();
}
#[tokio::test]
#[ignore = "one explicitly opted-in public Jev candidate-span selection; no tools"]
async fn live_public_music_span_probe() {
    if std::env::var("JARVIS_LIVE_JEV_MUSIC_SPANS").as_deref() != Ok("1") {
        return;
    }
    let settings = jarvis::conversation::load_provider_settings().unwrap();
    let client = JevClient::production(
        settings
            .get("TYPESAFE_API_KEY")
            .expect("configured Jev key"),
    )
    .unwrap();
    let candidates = vec!["Taylor".into(), "Taylor Swift".into(), "Spotify".into()];
    let result = client
        .resolve_music_request("nice open Taylor Swift on Spotify.", &candidates)
        .await
        .unwrap();
    println!("Public music candidate selection: {result:?}");
    assert!(matches!(result,Some((1,confidence)) if confidence>=0.95));
}

#[tokio::test]
#[ignore = "explicit opt-in live Spotify playback of the user's requested public artist"]
async fn live_semantic_music_request_reaches_verified_native_playback_without_lm() {
    if std::env::var("JARVIS_LIVE_MUSIC").as_deref() != Ok("1") {
        return;
    }
    let settings = jarvis::conversation::load_provider_settings().unwrap();
    let catalog = jarvis::music_catalog::MusicCatalog::production(
        settings.get("YOU_API_KEY").expect("configured catalog key"),
    )
    .unwrap();
    let jev = JevClient::production(
        settings
            .get("TYPESAFE_API_KEY")
            .expect("configured Jev key"),
    )
    .unwrap();
    let runtime = jarvis::tool_runtime::ToolRuntime::read_only(
        std::env::current_dir()
            .unwrap()
            .join("runtime/semantic-music-unused-context.sqlite"),
    )
    .with_music(
        catalog,
        jarvis::spotify_playback::SpotifyPlayback::native().unwrap(),
    )
    .with_music_intent(Arc::new(jev));
    let began = std::time::Instant::now();
    // No Conversation or conversational model exists in this acceptance path.
    let result = runtime
        .handle_resolved_intent("nice open Taylor Swift on Spotify.")
        .await
        .expect("semantic artist intent must resolve");
    let ack = result
        .acknowledgment
        .expect("verified playback acknowledgment");
    assert_eq!(ack, "Playing Taylor Swift.");
    assert!(!result.needs_followup);
    println!(
        "Verified native semantic playback: Taylor Swift; elapsed_ms={}",
        began.elapsed().as_millis()
    );
}

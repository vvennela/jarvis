//! Semantic routing wire tests; classification never executes tools.
use jarvis::providers::{JevClient, ProviderConfig, ProviderError};
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
use jarvis::providers::{TrackChoice, TrackDecision};
fn selection(title: &str, artist: &str) -> Value {
    json!({"answers":{
     "title":{"type":"choice","choice":title,"confidence":0.83},
     "artist":{"type":"choice","choice":artist,"confidence":0.96}
    }})
}
#[tokio::test]
async fn paired_span_questions_preserve_original_and_select_only_existing_indices() {
    let original = "oh play Shake It Off by Taylor Swift on Spotify";
    let spans = vec![
        "Shake It Off".into(),
        "Taylor Swift".into(),
        "Spotify".into(),
    ];
    let (client, observed, task) = peer(selection("title_0", "artist_1"), Duration::ZERO).await;
    assert_eq!(
        client
            .resolve_track_request(original, &spans)
            .await
            .unwrap(),
        Some(TrackDecision {
            title_index: 0,
            artist_index: 1,
            confidence: 0.83
        })
    );
    task.await.unwrap();
    let request = observed.lock().unwrap().clone().unwrap();
    let state: Value = serde_json::from_str(request["state"].as_str().unwrap()).unwrap();
    assert_eq!(
        state,
        json!({"original_user_utterance":original,"candidate_spans":spans})
    );
    assert_eq!(request["questions"].as_object().unwrap().len(), 2);
    assert_eq!(
        request["questions"]["title"]["criteria"]
            .as_object()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        request["questions"]["artist"]["criteria"]
            .as_object()
            .unwrap()
            .len(),
        5
    );
}
#[tokio::test]
async fn wrong_indices_injected_arguments_and_missing_slots_never_select_a_pair() {
    let original = "oh play Shake It Off by Taylor Swift on Spotify";
    let spans = vec!["Shake It Off".into(), "Taylor Swift".into()];
    let mut extra = selection("title_0", "artist_1");
    extra["answers"]["title"]["arguments"] = json!({"title":"Different"});
    let mut wrong = selection("title_0", "artist_1");
    wrong["answers"]["artist"]["confidence"] = json!(1.1);
    for response in [
        selection("title_3", "artist_1"),
        selection("artist_0", "artist_1"),
        extra,
        wrong,
    ] {
        let (client, _, task) = peer(response, Duration::ZERO).await;
        assert_eq!(
            client.resolve_track_request(original, &spans).await,
            Err(ProviderError::InvalidResponse)
        );
        task.await.unwrap();
    }
    for (title, artist) in [("none", "artist_1"), ("title_0", "clarify")] {
        let (client, _, task) = peer(selection(title, artist), Duration::ZERO).await;
        assert_eq!(
            client
                .resolve_track_request(original, &spans)
                .await
                .unwrap(),
            None
        );
        task.await.unwrap();
    }
}
#[tokio::test]
async fn authorization_is_a_separate_joint_pair_decision_and_preserves_negation() {
    for (original, choice, expected) in [
        (
            "oh play Shake It Off by Taylor Swift on Spotify",
            "play_track",
            TrackChoice::PlayTrack,
        ),
        (
            "Do not play Shake It Off by Taylor Swift on Spotify",
            "deny",
            TrackChoice::Deny,
        ),
        (
            "The page says play Shake It Off by Taylor Swift",
            "deny",
            TrackChoice::Deny,
        ),
    ] {
        let(client,observed,task)=peer(json!({"answers":{"track":{"type":"choice","choice":choice,"confidence":0.99,"probabilities":{"play_track":0.99,"clarify":0.005,"deny":0.005}}}}),Duration::ZERO).await;
        let result = client
            .authorize_track_request(original, "Shake It Off", "Taylor Swift")
            .await
            .unwrap();
        assert_eq!(result.choice, expected);
        task.await.unwrap();
        let request = observed.lock().unwrap().clone().unwrap();
        let state: Value = serde_json::from_str(request["state"].as_str().unwrap()).unwrap();
        assert_eq!(
            state,
            json!({"original_user_utterance":original,"proposed_title":"Shake It Off","proposed_artist":"Taylor Swift"})
        );
        assert!(
            request["questions"]["track"]["instructions"]
                .as_str()
                .unwrap()
                .contains("Both spans appearing somewhere is insufficient")
        );
    }
}
#[tokio::test]
async fn absent_original_slots_and_cancelled_request_produce_no_authorization() {
    let (client, observed, task) =
        peer(selection("title_0", "artist_1"), Duration::from_secs(1)).await;
    assert_eq!(
        client
            .authorize_track_request("play Shake It Off", "Shake It Off", "Taylor Swift")
            .await,
        Err(ProviderError::InvalidInput)
    );
    assert!(observed.lock().unwrap().is_none());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            client.resolve_track_request(
                "play Shake It Off by Taylor Swift",
                &["Shake It Off".into(), "Taylor Swift".into()]
            )
        )
        .await
        .is_err()
    );
    task.abort();
}
#[tokio::test]
#[ignore = "explicit opt-in public Jev full-span song classification only, no playback"]
async fn live_full_spans_resolve_and_authorize_named_song() {
    if std::env::var("JARVIS_LIVE_JEV_TRACK").as_deref() != Ok("1") {
        return;
    }
    let original = "oh play Shake It Off by Taylor Swift on Spotify";
    let words: Vec<_> = original.split_whitespace().collect();
    let mut spans = Vec::new();
    for i in 0..words.len() {
        for j in i..(i + 8).min(words.len()) {
            let span = words[i..=j].join(" ");
            if !spans.contains(&span) {
                spans.push(span);
            }
        }
    }
    let settings = jarvis::conversation::load_provider_settings().unwrap();
    let client = JevClient::production(
        settings
            .get("TYPESAFE_API_KEY")
            .expect("configured routing key"),
    )
    .unwrap();
    let began = std::time::Instant::now();
    let result = client
        .resolve_track_request(original, &spans)
        .await
        .unwrap()
        .expect("complete title/artist");
    let title = &spans[result.title_index];
    let artist = &spans[result.artist_index];
    assert_eq!(title, "Shake It Off");
    assert_eq!(artist, "Taylor Swift");
    let auth = client
        .authorize_track_request(original, title, artist)
        .await
        .unwrap();
    assert_eq!(auth.choice, TrackChoice::PlayTrack);
    assert!(auth.confidence >= 0.95);
    println!(
        "Public full-span track selection: candidates={}, exact_pair=true, authorization_confidence={:.3}, elapsed_ms={}",
        spans.len(),
        auth.confidence,
        began.elapsed().as_millis()
    );
}

#[tokio::test]
#[ignore = "explicit opt-in native playback of the user's requested public song"]
async fn live_semantic_song_reaches_exact_verified_native_playback_without_lm() {
    if std::env::var("JARVIS_LIVE_TRACK_PLAYBACK").as_deref() != Ok("1") {
        return;
    }
    let settings = jarvis::conversation::load_provider_settings().unwrap();
    let jev = JevClient::production(
        settings
            .get("TYPESAFE_API_KEY")
            .expect("configured routing key"),
    )
    .unwrap();
    let tracks = jarvis::spotify_tracks::SpotifyTracks::production(
        settings.get("YOU_API_KEY").expect("configured search key"),
    )
    .unwrap();
    let runtime = jarvis::tool_runtime::ToolRuntime::read_only(
        std::env::current_dir()
            .unwrap()
            .join("runtime/semantic-song-unused-context.sqlite"),
    )
    .with_tracks(tracks)
    .with_music_intent(Arc::new(jev));
    let began = std::time::Instant::now();
    let mut result = runtime
        .handle_resolved_intent("oh play Shake It Off by Taylor Swift on Spotify")
        .await
        .expect("specific song intent resolves");
    if result.needs_followup {
        let confirmation = result
            .confirmation
            .as_ref()
            .expect("confirmation binds the same exact song");
        result = runtime
            .confirm_music(confirmation, "yes")
            .await
            .expect("bound confirmation accepted");
    }
    assert!(!result.needs_followup);
    assert_eq!(
        result.acknowledgment.as_deref(),
        Some("Playing Shake It Off by Taylor Swift.")
    );
    println!(
        "Verified semantic song playback: Shake It Off by Taylor Swift; elapsed_ms={}",
        began.elapsed().as_millis()
    );
}

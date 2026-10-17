//! Explicit opt-in production catalog + native Spotify verification.
//! This changes real playback; never run as part of ordinary CI or a smoke test.
#![cfg(target_os = "macos")]
use jarvis::{
    conversation::load_provider_settings,
    music_catalog::MusicCatalog,
    spotify_playback::{SpotifyPlayback, authorize_artist_request},
};
use std::time::{Duration, Instant};

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_CATALOG=1; one public read-only catalog request"]
async fn public_catalog_candidates_without_playback() {
    assert_eq!(std::env::var("JARVIS_LIVE_CATALOG").as_deref(), Ok("1"));
    let settings = load_provider_settings().expect("could not load provider settings");
    let key = settings
        .get("YOU_API_KEY")
        .expect("music search is not configured");
    let catalog = MusicCatalog::production(key.clone()).expect("invalid catalog configuration");
    let started = Instant::now();
    let candidates = catalog
        .resolve_candidates("Metallica")
        .await
        .expect("catalog lookup failed");
    for candidate in &candidates {
        println!(
            "public_artist_name={:?} public_spotify_artist_id={}",
            candidate.name(),
            candidate.id()
        );
    }
    println!(
        "candidate_count={} elapsed_ms={} playback=false",
        candidates.len(),
        started.elapsed().as_millis()
    );
}

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_SPOTIFY=1, You credentials, installed Spotify and explicit playback authorization"]
async fn public_artist_lookup_and_native_playback_are_verified_end_to_end() {
    assert_eq!(
        std::env::var("JARVIS_LIVE_SPOTIFY").as_deref(),
        Ok("1"),
        "explicit live Spotify opt-in is required"
    );
    let request = "Play Metallica on Spotify";
    authorize_artist_request(request, "Metallica")
        .expect("request must be authorized before lookup");
    let settings = load_provider_settings().expect("could not load provider settings");
    let key = settings
        .get("YOU_API_KEY")
        .filter(|key| !key.trim().is_empty())
        .expect("You search credentials are not configured");
    let catalog =
        MusicCatalog::production(key.clone()).expect("invalid music search configuration");
    let playback = SpotifyPlayback::native().expect("native Spotify is unavailable");
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(35), async {
        let artist = catalog
            .resolve_artist("Metallica")
            .await
            .expect("public artist identity lookup failed");
        assert_eq!(artist.name().to_lowercase(), "metallica");
        let resolved_ms = started.elapsed().as_millis();
        let outcome = playback
            .play_artist(request, &artist)
            .await
            .expect("native playback could not be verified");
        assert_eq!(outcome.artist.to_lowercase(), "metallica");
        assert!(outcome.track_uri.starts_with("spotify:track:"));
        println!(
            "catalog_verified=true playback_verified=true lookup_ms={resolved_ms} total_ms={}",
            started.elapsed().as_millis()
        );
    })
    .await
    .expect("live music check exceeded its total deadline");
}

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_SPOTIFY_CONVERSATION=1; real model, public catalog and native playback"]
async fn real_conversation_plans_artist_tool_and_verifies_native_playback() {
    use jarvis::{
        context::{ContextStore, Limits},
        conversation::Conversation,
        tool_runtime::ToolRuntime,
    };
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};
    assert_eq!(
        std::env::var("JARVIS_LIVE_SPOTIFY_CONVERSATION").as_deref(),
        Ok("1")
    );
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("runtime")
        .join(format!("live-music-conversation-{suffix}"));
    fs::create_dir_all(&directory).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let database = directory.join("context.sqlite");
    drop(ContextStore::open(&database, Limits::default()).unwrap());
    SpotifyPlayback::native().expect("Spotify must be installed for this live test");
    // The owned wrapper records actual invocation, then runs the exact production
    // AppleScript through macOS. It neither replaces the script nor fakes output.
    let marker = directory.join("native-invoked");
    let escaped = marker.to_str().unwrap().replace('\'', "'\\''");
    let wrapper = directory.join("verified-osascript");
    fs::write(
        &wrapper,
        format!("#!/bin/sh\n: > '{escaped}'\nexec /usr/bin/osascript \"$@\"\n"),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let settings = load_provider_settings().unwrap();
    let catalog = MusicCatalog::production(
        settings
            .get("YOU_API_KEY")
            .expect("You credentials missing")
            .clone(),
    )
    .unwrap();
    let playback = SpotifyPlayback::with_executable(wrapper, Duration::from_secs(20)).unwrap();
    let runtime = ToolRuntime::read_only(database.clone()).with_music(catalog, playback);
    let mut conversation = Conversation::from_env(database).unwrap();
    conversation.set_tool_runtime(Arc::new(runtime));
    let started = Instant::now();
    let reply = tokio::time::timeout(
        Duration::from_secs(60),
        conversation.ask("Play Metallica on Spotify"),
    )
    .await
    .expect("conversation deadline exceeded")
    .expect("conversation provider failed");
    assert!(
        marker.is_file(),
        "the model did not invoke the actual native playback tool; public-fixture reply: {}",
        reply.text
    );
    assert_eq!(reply.text, "Playing Metallica.");
    println!(
        "conversation_tool_verified=true native_playback_verified=true elapsed_ms={} acknowledgment_bytes={} private_context_sent=false",
        started.elapsed().as_millis(),
        reply.text.len()
    );
}

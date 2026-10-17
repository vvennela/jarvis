//! Explicit opt-in: real model plans two public requests; actual browser/music actions.
#![cfg(target_os = "macos")]
use jarvis::{
    context::{ContextStore, Limits},
    conversation::{Conversation, load_provider_settings},
    music_catalog::MusicCatalog,
    spotify_playback::SpotifyPlayback,
    tool_runtime::ToolRuntime,
    web_open::WebOpener,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_COMPOUND=1; opens Wikipedia and changes real Spotify playback"]
async fn real_model_opens_requested_article_and_plays_requested_artist() {
    assert_eq!(std::env::var("JARVIS_LIVE_COMPOUND").as_deref(), Ok("1"));
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("runtime")
        .join(format!("live-compound-{suffix}"));
    fs::create_dir_all(&directory).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let database = directory.join("context.sqlite");
    drop(ContextStore::open(&database, Limits::default()).unwrap());
    SpotifyPlayback::native().expect("Spotify must be installed");
    let music_marker = directory.join("music-invoked");
    let page_marker = directory.join("opened-public-url");
    let music_wrapper = directory.join("native-osascript");
    let page_wrapper = directory.join("native-open");
    for (wrapper, marker, program, record) in [
        (&music_wrapper, &music_marker, "/usr/bin/osascript", ":"),
        (
            &page_wrapper,
            &page_marker,
            "/usr/bin/open",
            "printf '%s\\n' \"$1\"",
        ),
    ] {
        let marker = marker.to_str().unwrap().replace('\'', "'\\''");
        fs::write(
            wrapper,
            format!("#!/bin/sh\n{record} > '{marker}'\nexec {program} \"$@\"\n"),
        )
        .unwrap();
        fs::set_permissions(wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let settings = load_provider_settings().unwrap();
    let catalog = MusicCatalog::production(
        settings
            .get("YOU_API_KEY")
            .expect("You credentials missing")
            .clone(),
    )
    .unwrap();
    let playback =
        SpotifyPlayback::with_executable(music_wrapper, Duration::from_secs(20)).unwrap();
    let opener = WebOpener::with_executable(page_wrapper).unwrap();
    let runtime = ToolRuntime::read_only(database.clone())
        .with_music(catalog, playback)
        .with_web_opener(opener);
    let mut conversation = Conversation::from_env(database).unwrap();
    conversation.set_tool_runtime(Arc::new(runtime));
    let started = Instant::now();
    let reply = tokio::time::timeout(
        Duration::from_secs(60),
        conversation.ask("open the wikipedia page for napoleon and play some taylor swift"),
    )
    .await
    .expect("compound request exceeded deadline")
    .expect("conversation failed");
    // Public fixture only: these diagnostics are necessary to distinguish partial success.
    println!(
        "browser_invoked={} music_invoked={} elapsed_ms={} public_ack={:?}",
        page_marker.is_file(),
        music_marker.is_file(),
        started.elapsed().as_millis(),
        reply.text
    );
    assert_eq!(
        fs::read_to_string(&page_marker)
            .expect("browser tool was not invoked")
            .trim(),
        "https://en.wikipedia.org/wiki/Napoleon"
    );
    assert!(
        music_marker.is_file(),
        "actual playback adapter was not invoked"
    );
    assert!(
        reply.text.contains("Playing Taylor Swift."),
        "verified playback acknowledgment missing"
    );
    assert!(
        reply.text.to_lowercase().contains("napoleon"),
        "article acknowledgment missing"
    );
}

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_ARTIST_SWITCH=1; changes real Spotify playback"]
async fn resolved_artist_top_track_switches_native_playback() {
    assert_eq!(
        std::env::var("JARVIS_LIVE_ARTIST_SWITCH").as_deref(),
        Ok("1")
    );
    let settings = load_provider_settings().unwrap();
    let catalog = MusicCatalog::production(
        settings
            .get("YOU_API_KEY")
            .expect("You credentials missing")
            .clone(),
    )
    .unwrap();
    let started = Instant::now();
    let artist = catalog
        .resolve_artist("Taylor Swift")
        .await
        .expect("artist identity lookup failed");
    assert!(
        artist.track_uri().is_some(),
        "public artist context must supply a verified track"
    );
    let result = SpotifyPlayback::native()
        .unwrap()
        .play_artist("play some taylor swift", &artist)
        .await
        .expect("native artist switch failed verification");
    assert_eq!(result.track_uri, artist.track_uri().unwrap());
    assert_eq!(result.text, "Playing Taylor Swift.");
    println!(
        "artist_switch_verified=true exact_track_verified=true elapsed_ms={}",
        started.elapsed().as_millis()
    );
}

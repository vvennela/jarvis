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

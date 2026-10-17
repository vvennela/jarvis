//! Explicitly opted-in public-phrase synthesis; never plays audio or prints credentials.
use jarvis::{conversation::load_provider_settings, elevenlabs::ElevenLabsClient};
use std::{
    fs::OpenOptions,
    io::Write,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_ELEVENLABS=1 and makes one authorized synthesis request"]
async fn configured_voice_accepts_public_phrase_without_playback() {
    assert_eq!(std::env::var("JARVIS_LIVE_ELEVENLABS").as_deref(), Ok("1"));
    let settings = load_provider_settings().expect("provider settings unavailable");
    assert!(
        settings
            .get("ELEVENLABS_API_KEY")
            .is_some_and(|v| !v.is_empty()),
        "ElevenLabs key not configured"
    );
    let voice = if settings.contains_key("ELEVENLABS_VOICE_ID") {
        "configured"
    } else {
        "default"
    };
    let client = ElevenLabsClient::from_env()
        .expect("ElevenLabs configuration invalid")
        .expect("ElevenLabs key not configured");
    let started = Instant::now();
    let audio = client
        .synthesize("Ready when you are.")
        .await
        .expect("ElevenLabs synthesis failed (redacted error)");
    let elapsed = started.elapsed();
    let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime");
    std::fs::create_dir_all(&runtime).expect("runtime directory unavailable");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = runtime.join(format!("elevenlabs-acceptance-{nonce}.mp3"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).expect("audio artifact unavailable");
    file.write_all(&audio).expect("audio artifact write failed");
    println!(
        "ElevenLabs accepted: key_present=true voice={voice} model=eleven_flash_v2_5 audio_bytes={} elapsed_ms={} buffered=true played=false artifact={}",
        audio.len(),
        elapsed.as_millis(),
        path.display()
    );
}

#![cfg(unix)]
//! Word-timing wake-residue trimming through real owned helper processes.
//! Synthetic mono audio and canned JSON only; no Apple service, network or microphone.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis_voice::apple::{AppleSpeechError, AppleTranscriber};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use support::TempDir;

/// Helper that drains the WAV request and replies with the exact bytes of `reply`.
fn replying(dir: &TempDir, reply: &str) -> AppleTranscriber {
    assert!(dir.path().is_dir());
    fs::write(dir.join("reply"), reply).unwrap();
    let path = dir.join("speech-helper");
    fs::write(
        &path,
        "#!/bin/sh\n/bin/cat > \"${0%/*}/request.wav\"\n/bin/cat \"${0%/*}/reply\"\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    AppleTranscriber::new(path, Duration::from_secs(15)).unwrap()
}

fn token() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn words(text: &str, words: &[(&str, u64)]) -> String {
    let words: Vec<_> = words
        .iter()
        .map(|(t, ms)| serde_json::json!({"text": t, "start_ms": ms}))
        .collect();
    serde_json::json!({"text": text, "words": words}).to_string()
}

const AUDIO: [f32; 320] = [0.1; 320];

#[test]
fn misheard_wake_residue_before_cutoff_is_dropped_whatever_its_spelling() {
    // The wake phrase can come back as anything; only its timing identifies it.
    for (text, residue) in [
        ("nice open Taylor Swift on Spotify", vec![("nice", 120)]),
        (
            "Hey, drivers open Taylor Swift on Spotify",
            vec![("Hey,", 0), ("drivers", 450)],
        ),
        (
            "Hey Jarvis open Taylor Swift on Spotify",
            vec![("Hey", 30), ("Jarvis", 520)],
        ),
    ] {
        let dir = TempDir::new();
        let mut all = residue.clone();
        all.extend([
            ("open", 1_200),
            ("Taylor", 1_500),
            ("Swift", 1_800),
            ("on", 2_100),
            ("Spotify", 2_300),
        ]);
        let adapter = replying(&dir, &words(text, &all));
        // Cutoff at 1000 ms.
        let got = adapter
            .transcribe_after(&AUDIO, Some(16_000), token())
            .unwrap();
        assert_eq!(
            got, "open Taylor Swift on Spotify",
            "residue survived: {text}"
        );
        // The full audio, not a trimmed copy, is what the recognizer heard.
        let sent = jarvis_voice::load_wav(&dir.join("request.wav")).unwrap();
        assert_eq!(sent.len(), AUDIO.len());
    }
}

#[test]
fn cutoff_boundary_keeps_word_starting_exactly_at_skip_and_drops_one_sample_earlier() {
    let reply = words(
        "drivers before at after",
        &[
            ("drivers", 0),
            ("before", 999),
            ("at", 1_000),
            ("after", 1_001),
        ],
    );
    let dir = TempDir::new();
    let adapter = replying(&dir, &reply);
    assert_eq!(
        adapter
            .transcribe_after(&AUDIO, Some(16_000), token())
            .unwrap(),
        "at after"
    );
    // 1000 ms == 16000 samples < 16001: dropped; 1001 ms == 16016: kept.
    assert_eq!(
        adapter
            .transcribe_after(&AUDIO, Some(16_001), token())
            .unwrap(),
        "after"
    );
    // A zero cutoff keeps everything, in recognizer order.
    assert_eq!(
        adapter.transcribe_after(&AUDIO, Some(0), token()).unwrap(),
        "drivers before at after"
    );
}

#[test]
fn transcript_of_only_wake_residue_is_an_empty_success_not_an_error() {
    let dir = TempDir::new();
    let adapter = replying(
        &dir,
        &words("Hey, drivers", &[("Hey,", 0), ("drivers", 420)]),
    );
    assert_eq!(
        adapter
            .transcribe_after(&AUDIO, Some(16_000), token())
            .unwrap(),
        ""
    );
    // Cutoff far beyond the audio also yields empty, never the residue.
    assert_eq!(
        adapter
            .transcribe_after(&AUDIO, Some(usize::MAX), token())
            .unwrap(),
        ""
    );
}

#[test]
fn older_helper_without_word_timing_falls_back_to_full_text() {
    for reply in [
        r#"{"text":"nice open Spotify"}"#,
        r#"{"text":"nice open Spotify","words":[]}"#,
        r#"{"text":"nice open Spotify","words":null}"#,
    ] {
        let dir = TempDir::new();
        let adapter = replying(&dir, reply);
        assert_eq!(
            adapter
                .transcribe_after(&AUDIO, Some(16_000), token())
                .unwrap(),
            "nice open Spotify",
            "{reply}"
        );
    }
}

#[test]
fn no_skip_matches_transcribe_including_empty_text_errors() {
    let dir = TempDir::new();
    let adapter = replying(
        &dir,
        &words(" nice open Spotify ", &[("nice", 0), ("open", 2_000)]),
    );
    let plain = adapter.transcribe(&AUDIO, token()).unwrap();
    assert_eq!(plain, "nice open Spotify");
    assert_eq!(
        adapter.transcribe_after(&AUDIO, None, token()).unwrap(),
        plain
    );
    for reply in [r#"{"text":""}"#, r#"{"text":"   ","words":[]}"#] {
        let dir = TempDir::new();
        let adapter = replying(&dir, reply);
        assert!(adapter.transcribe_after(&AUDIO, None, token()).is_err());
        assert!(adapter.transcribe(&AUDIO, token()).is_err());
    }
}

#[test]
fn malformed_word_entries_are_rejected_or_ignored_without_leaking_residue() {
    for words_json in [
        r#"[{"text":"nice","start_ms":-5},{"text":"open","start_ms":2000}]"#,
        r#"[{"text":"nice","start_ms":"0"},{"text":"open","start_ms":2000}]"#,
        r#"[{"text":"nice","start_ms":12.5},{"text":"open","start_ms":2000}]"#,
        r#"[{"text":"nice","start_ms":null},{"text":"open","start_ms":2000}]"#,
        r#"[{"text":17,"start_ms":0},{"text":"open","start_ms":2000}]"#,
        r#"[{"start_ms":0},{"text":"open","start_ms":2000}]"#,
        r#"[{"text":"nice"},{"text":"open","start_ms":2000}]"#,
        r#"["nice",{"text":"open","start_ms":2000}]"#,
        r#"[7,{"text":"open","start_ms":2000}]"#,
        r#"[{"text":"ni\u0000ce","start_ms":0},{"text":"open","start_ms":2000}]"#,
        r#"[{"text":"nice","start_ms":1e400},{"text":"open","start_ms":2000}]"#,
        r#"{"text":"nice","start_ms":0}"#,
        r#""nice open""#,
    ] {
        let dir = TempDir::new();
        let reply = format!(r#"{{"text":"nice open","words":{words_json}}}"#);
        let adapter = replying(&dir, &reply);
        let result = adapter.transcribe_after(&AUDIO, Some(16_000), token());
        match result {
            Err(_) => {}
            // Non-array `words` is treated as an old helper: full text. Any
            // other acceptance must not carry the misheard wake word forward
            // unless it is the documented full-text fallback.
            Ok(text) => assert!(
                text == "open" || (text == "nice open" && !words_json.starts_with('[')),
                "malformed word timing accepted as {text:?} for {words_json}"
            ),
        }
    }
}

#[test]
fn extreme_start_times_do_not_overflow_or_wrap_into_the_kept_range() {
    let dir = TempDir::new();
    let adapter = replying(
        &dir,
        &format!(
            r#"{{"text":"x y","words":[{{"text":"nice","start_ms":0}},{{"text":"late","start_ms":{}}}]}}"#,
            u64::MAX
        ),
    );
    assert_eq!(
        adapter
            .transcribe_after(&AUDIO, Some(16_000), token())
            .unwrap(),
        "late"
    );
}

#[test]
fn whitespace_only_words_do_not_produce_double_spaces() {
    let dir = TempDir::new();
    let adapter = replying(
        &dir,
        &words(
            "nice play X",
            &[("nice", 0), ("play", 1_200), (" ", 1_300), ("  X ", 1_400)],
        ),
    );
    assert_eq!(
        adapter
            .transcribe_after(&AUDIO, Some(16_000), token())
            .unwrap(),
        "play X"
    );
}

#[test]
fn huge_word_arrays_and_oversized_kept_text_are_bounded_errors() {
    // Many tiny entries that still fit the 64 KiB output cap.
    let many: Vec<String> = (0..2_049)
        .map(|i| format!(r#"{{"text":"a","start_ms":{i}}}"#))
        .collect();
    let dir = TempDir::new();
    let adapter = replying(
        &dir,
        &format!(r#"{{"text":"a","words":[{}]}}"#, many.join(",")),
    );
    let started = Instant::now();
    assert!(adapter.transcribe_after(&AUDIO, Some(0), token()).is_err());
    assert!(started.elapsed() < Duration::from_secs(10));

    // Kept words whose joined length exceeds the transcript cap.
    let long = "w".repeat(1_000);
    let big: Vec<String> = (0..10)
        .map(|i| format!(r#"{{"text":"{long}","start_ms":{}}}"#, 2_000 + i))
        .collect();
    let dir = TempDir::new();
    let adapter = replying(
        &dir,
        &format!(r#"{{"text":"short","words":[{}]}}"#, big.join(",")),
    );
    assert!(matches!(
        adapter.transcribe_after(&AUDIO, Some(0), token()),
        Err(AppleSpeechError::ResponseTooLarge)
    ));
    // The same oversized words before the cutoff are simply dropped.
    assert_eq!(
        adapter
            .transcribe_after(&AUDIO, Some(usize::MAX), token())
            .unwrap(),
        ""
    );
}

#[test]
fn cancellation_with_skip_never_spawns_or_reaps_the_owned_helper() {
    // Pre-cancelled: helper is never started.
    let dir = TempDir::new();
    let path = dir.join("speech-helper");
    fs::write(
        &path,
        "#!/bin/sh\ntouch \"${0%/*}/started\"\nprintf '%s' '{\"text\":\"x\"}'\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let adapter = AppleTranscriber::new(path, Duration::from_secs(1)).unwrap();
    assert!(matches!(
        adapter.transcribe_after(&AUDIO, Some(4_800), Arc::new(AtomicBool::new(true))),
        Err(AppleSpeechError::Cancelled)
    ));
    assert!(!dir.join("started").exists());

    // Mid-flight: the cancel wins and the helper does not survive.
    let dir = TempDir::new();
    let path: PathBuf = dir.join("speech-helper");
    fs::write(
        &path,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"${0%/*}/pid\"\nexec /bin/sleep 10\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let adapter = AppleTranscriber::new(path, Duration::from_secs(15)).unwrap();
    let cancelled = token();
    let copy = cancelled.clone();
    let job = thread::spawn(move || adapter.transcribe_after(&AUDIO, Some(4_800), copy));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.join("pid").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let started = Instant::now();
    cancelled.store(true, Ordering::Release);
    assert!(matches!(
        job.join().unwrap(),
        Err(AppleSpeechError::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(10));
    let pid = fs::read_to_string(dir.join("pid")).unwrap();
    let alive = std::process::Command::new("/bin/kill")
        .args(["-0", &pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!alive.success(), "owned helper was not reaped");
}

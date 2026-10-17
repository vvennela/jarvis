#![cfg(unix)]
//! Real owned helper processes; synthetic mono audio only, no Apple service or microphone.
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
fn helper(dir: &TempDir, body: &str) -> PathBuf {
    assert!(dir.path().is_dir());
    let path = dir.join("speech-helper");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn token() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
#[test]
fn one_utterance_is_sent_as_bounded_pcm_wav_without_paths_or_arguments() {
    let dir = TempDir::new();
    let path = helper(
        &dir,
        "printf '%s' \"$#\" > \"${0%/*}/argc\"\n/bin/cat > \"${0%/*}/request.wav\"\nprintf '%s' '{\"text\":\"Open the fixture article.\"}'",
    );
    let adapter = AppleTranscriber::new(path, Duration::from_secs(2)).unwrap();
    assert_eq!(
        adapter.transcribe(&[-1.0, 0.0, 0.5, 1.0], token()).unwrap(),
        "Open the fixture article."
    );
    assert_eq!(fs::read_to_string(dir.join("argc")).unwrap(), "0");
    let bytes = fs::read(dir.join("request.wav")).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let samples = jarvis_voice::load_wav(&dir.join("request.wav")).unwrap();
    assert_eq!(samples.len(), 4);
    assert!(samples[0] < -0.99);
    assert_eq!(samples[1], 0.0);
    assert!((samples[2] - 0.5).abs() < 0.001);
    assert!(samples[3] > 0.99);
}
#[test]
fn invalid_audio_and_precancellation_never_spawn_helper() {
    let dir = TempDir::new();
    let path = helper(
        &dir,
        "touch \"${0%/*}/started\"\nprintf '%s' '{\"text\":\"unexpected\"}'",
    );
    let adapter = AppleTranscriber::new(path, Duration::from_secs(1)).unwrap();
    for audio in [
        vec![],
        vec![f32::NAN],
        vec![f32::INFINITY],
        vec![1.01],
        vec![0.1; 480001],
    ] {
        assert!(matches!(
            adapter.transcribe(&audio, token()),
            Err(AppleSpeechError::InvalidAudio)
        ));
    }
    assert!(matches!(
        adapter.transcribe(&[0.1; 320], Arc::new(AtomicBool::new(true))),
        Err(AppleSpeechError::Cancelled)
    ));
    assert!(!dir.join("started").exists());
}
#[test]
fn malformed_blank_duplicate_or_unbounded_output_is_not_a_transcript() {
    for body in [
        "printf '%s' 'not json'",
        "printf '%s' '{\"text\":\"  \"}'",
        "printf '%s' '{\"text\":17}'",
        "printf '%s' '{\"text\":\"one\"}{\"text\":\"two\"}'",
        "/usr/bin/head -c 65537 /dev/zero",
        "printf '%s' '{\"text\":\"private provider diagnostic\"}'; exit 7",
    ] {
        let dir = TempDir::new();
        let path = helper(&dir, &format!("/bin/cat > /dev/null\n{body}"));
        let adapter = AppleTranscriber::new(path, Duration::from_secs(1)).unwrap();
        let error = adapter.transcribe(&[0.1; 320], token()).unwrap_err();
        assert!(!format!("{error:?}").contains("private provider diagnostic"));
    }
}
#[test]
fn transcript_byte_cap_applies_inside_otherwise_valid_json() {
    let dir = TempDir::new();
    fs::write(
        dir.join("reply"),
        format!("{{\"text\":\"{}\"}}", "x".repeat(8193)),
    )
    .unwrap();
    let path = helper(&dir, "/bin/cat > /dev/null\n/bin/cat \"${0%/*}/reply\"");
    let adapter = AppleTranscriber::new(path, Duration::from_secs(1)).unwrap();
    assert!(adapter.transcribe(&[0.1; 320], token()).is_err());
}
#[test]
fn timeout_and_midflight_cancel_reap_owned_helper_without_late_text() {
    for cancel in [false, true] {
        let dir = TempDir::new();
        let path = helper(
            &dir,
            "printf '%s' \"$$\" > \"${0%/*}/pid\"\nexec /bin/sleep 10",
        );
        let adapter = AppleTranscriber::new(
            path,
            if cancel {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(100)
            },
        )
        .unwrap();
        let cancelled = token();
        let copy = cancelled.clone();
        let job = thread::spawn(move || adapter.transcribe(&[0.1; 320], copy));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !dir.join("pid").exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        if cancel {
            cancelled.store(true, Ordering::Release);
        }
        let result = job.join().unwrap().unwrap_err();
        assert!(matches!(
            (cancel, result),
            (true, AppleSpeechError::Cancelled) | (false, AppleSpeechError::Timeout)
        ));
        let pid = fs::read_to_string(dir.join("pid")).unwrap();
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!alive.success(), "owned helper was not reaped");
    }
}
#[test]
fn helper_configuration_rejects_invalid_paths_and_unbounded_deadlines() {
    let dir = TempDir::new();
    let path = helper(&dir, "exit 0");
    assert!(AppleTranscriber::new(PathBuf::from("relative"), Duration::from_secs(1)).is_err());
    assert!(AppleTranscriber::new(dir.join("missing"), Duration::from_secs(1)).is_err());
    assert!(AppleTranscriber::new(dir.path().to_owned(), Duration::from_secs(1)).is_err());
    assert!(AppleTranscriber::new(path.clone(), Duration::ZERO).is_err());
    assert!(AppleTranscriber::new(path.clone(), Duration::from_secs(31)).is_err());
    fs::set_permissions(path.clone(), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(AppleTranscriber::new(path, Duration::from_secs(1)).is_err());
}

#[test]
#[ignore = "requires compiled local Apple SpeechAnalyzer helper; no microphone"]
fn actual_apple_helper_rejects_malformed_wav_before_transcription() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let helper = std::env::var_os("JARVIS_TEST_APPLE_HELPER").expect("compiled Apple helper path");
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend(38u32.to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(16000u32.to_le_bytes());
    wav.extend(32000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(2u32.to_le_bytes());
    wav.extend([0, 0]);
    let mut stereo = wav.clone();
    stereo[22..24].copy_from_slice(&2u16.to_le_bytes());
    let mut wrong_rate = wav.clone();
    wrong_rate[24..28].copy_from_slice(&44100u32.to_le_bytes());
    let mut truncated = wav.clone();
    truncated.pop();
    for input in [
        vec![],
        b"PRIVATE_SYNTHETIC_SENTINEL".to_vec(),
        stereo,
        wrong_rate,
        truncated,
        vec![0; 1_000_001],
    ] {
        let mut child = Command::new(&helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(&input);
        let result = child.wait_with_output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        assert_eq!(
            String::from_utf8(result.stderr).unwrap(),
            "Invalid bounded PCM WAV input.\n"
        );
    }
}

#[test]
#[ignore = "requires prepared local Apple SpeechAnalyzer and public generated WAV; no microphone"]
fn actual_apple_adapter_transcribes_one_public_utterance() {
    let helper =
        PathBuf::from(std::env::var_os("JARVIS_TEST_APPLE_HELPER").expect("compiled Apple helper"));
    let wav = PathBuf::from(std::env::var_os("JARVIS_TEST_APPLE_WAV").expect("public fixture WAV"));
    let expected =
        std::env::var("JARVIS_TEST_APPLE_EXPECT").expect("public fixture expected phrase");
    let samples = jarvis_voice::load_wav(&wav).unwrap();
    let adapter = AppleTranscriber::new(helper, Duration::from_secs(10)).unwrap();
    let transcript = adapter.transcribe(&samples, token()).unwrap();
    assert!(
        transcript.to_lowercase().contains(&expected.to_lowercase()),
        "public fixture transcription mismatch: {transcript}"
    );
}

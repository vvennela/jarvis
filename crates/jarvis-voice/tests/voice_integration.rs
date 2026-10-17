#[cfg(feature = "whisper")]
use jarvis_voice::Transcriber;
#[cfg(feature = "whisper")]
use std::process::Command;
// Local audio validation and opt-in actual Whisper inference. No downloads or microphone use.
use jarvis_voice::{
    MAX_MODEL_BYTES, MAX_SAMPLES, SAMPLE_RATE, load_wav, validate_audio, validate_model_file,
};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "jarvis-voice-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wav(path: &Path, channels: u16, rate: u32, bits: u16, format: u16, data: &[u8]) {
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend((36 + data.len() as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(format.to_le_bytes());
    out.extend(channels.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes());
    out.extend((channels * bits / 8).to_le_bytes());
    out.extend(bits.to_le_bytes());
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    fs::write(path, out).unwrap();
}

#[test]
fn normalized_audio_is_bounded_finite_and_nonempty() {
    assert!(validate_audio(&[]).is_err());
    for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.001, -1.001] {
        assert!(validate_audio(&[sample]).is_err());
    }
    assert!(validate_audio(&[-1.0, 0.0, 1.0]).is_ok());
    assert!(validate_audio(&vec![0.0; MAX_SAMPLES]).is_ok());
    assert!(validate_audio(&vec![0.0; MAX_SAMPLES + 1]).is_err());
}

#[test]
fn strict_wav_reader_accepts_pcm_and_rejects_other_formats_without_reinterpreting_them() {
    let dir = TempDir::new();
    let path = dir.join("fixture.wav");
    let data: [u8; 6] = [0, 128, 0, 0, 255, 127];
    wav(&path, 1, SAMPLE_RATE, 16, 1, &data);
    let samples = load_wav(&path).unwrap();
    assert_eq!(samples.len(), 3);
    assert_eq!(samples[0], -1.0);
    assert_eq!(samples[1], 0.0);
    assert!(samples[2] > 0.99 && samples[2] <= 1.0);
    for (channels, rate, bits, format, data) in [
        (2, 16000, 16, 1, vec![0; 8]),
        (1, 44100, 16, 1, vec![0; 4]),
        (1, 16000, 8, 1, vec![128; 4]),
        (1, 16000, 32, 3, vec![0; 8]),
        (1, 16000, 16, 1, vec![]),
    ] {
        wav(&path, channels, rate, bits, format, &data);
        assert!(
            load_wav(&path).is_err(),
            "accepted incompatible wav {channels}/{rate}/{bits}/{format}"
        );
    }
    fs::write(&path, b"not a WAV").unwrap();
    assert!(load_wav(&path).is_err());
}

#[test]
fn wav_sample_and_file_bounds_are_checked_before_transcription() {
    let dir = TempDir::new();
    let path = dir.join("too-long.wav");
    wav(
        &path,
        1,
        SAMPLE_RATE,
        16,
        1,
        &vec![0; 2 * (MAX_SAMPLES + 1)],
    );
    assert!(load_wav(&path).is_err());
    let mut oversized = fs::File::create(&path).unwrap();
    oversized.seek(SeekFrom::Start(2 * 1024 * 1024)).unwrap();
    oversized.write_all(&[0]).unwrap();
    drop(oversized);
    assert!(load_wav(&path).is_err());
    assert!(load_wav(&dir.join("missing")).is_err());
    assert!(load_wav(&dir.0).is_err());
}

#[test]
fn invalid_model_paths_and_headers_fail_preflight_without_native_inference() {
    let dir = TempDir::new();
    let path = dir.join("model.bin");
    assert!(validate_model_file(&path).is_err());
    assert!(validate_model_file(&dir.0).is_err());
    for data in [
        vec![],
        b"GGUFnot-a-whisper-model".to_vec(),
        b"PRIVATE_BAD_MODEL_HEADER".to_vec(),
    ] {
        fs::write(&path, data).unwrap();
        assert!(validate_model_file(&path).is_err());
        #[cfg(feature = "whisper")]
        assert!(Transcriber::load(&path).is_err());
    }
    let mut oversized = fs::File::create(&path).unwrap();
    oversized.seek(SeekFrom::Start(MAX_MODEL_BYTES)).unwrap();
    oversized.write_all(&[0]).unwrap();
    drop(oversized);
    assert!(validate_model_file(&path).is_err());
}

#[cfg(unix)]
#[test]
fn model_and_audio_symlinks_are_not_followed() {
    let dir = TempDir::new();
    let model = dir.join("model.bin");
    fs::write(&model, b"lmggfixtureheader").unwrap();
    let alias = dir.join("alias.bin");
    std::os::unix::fs::symlink(&model, &alias).unwrap();
    assert!(validate_model_file(&alias).is_err());
    let audio = dir.join("audio.wav");
    wav(&audio, 1, SAMPLE_RATE, 16, 1, &[0, 0]);
    let audio_alias = dir.join("alias.wav");
    std::os::unix::fs::symlink(audio, &audio_alias).unwrap();
    assert!(load_wav(&audio_alias).is_err());
}

#[test]
#[cfg(feature = "whisper")]
fn cli_rejects_invalid_requests_without_writes_or_success_transcript() {
    let dir = TempDir::new();
    for args in [
        vec![],
        vec!["--model"],
        vec!["--wav"],
        vec!["--model", "missing", "--wav", "missing"],
        vec!["--model", "missing", "--wav", "missing", "--upload"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_jarvis-voice"))
            .current_dir(&dir.0)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "invalid CLI succeeded: {args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
    }
}

#[test]
#[ignore = "requires explicit local model and generated audio fixture"]
#[cfg(feature = "whisper")]
fn opt_in_real_model_transcribes_generated_fixture_twice_without_reloading_file() {
    let (Some(model), Some(audio)) = (
        std::env::var_os("JARVIS_TEST_WHISPER_MODEL"),
        std::env::var_os("JARVIS_TEST_WHISPER_WAV"),
    ) else {
        panic!(
            "Set JARVIS_TEST_WHISPER_MODEL and JARVIS_TEST_WHISPER_WAV to local generated fixtures; tests never download weights"
        );
    };
    let dir = TempDir::new();
    let linked = dir.join("model.bin");
    fs::hard_link(&model, &linked)
        .expect("real model fixture must share temp filesystem for non-copying reuse test");
    let samples = load_wav(Path::new(&audio)).unwrap();
    let mut transcriber = Transcriber::load(&linked).unwrap();
    fs::remove_file(&linked).unwrap();
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert!(matches!(
        transcriber.transcribe_cancelled(&samples, cancelled),
        Err(jarvis_voice::VoiceError::Cancelled)
    ));
    let first = transcriber.transcribe(&samples).unwrap();
    let second = transcriber.transcribe(&samples).unwrap();
    assert!(
        first.chars().any(char::is_alphabetic),
        "generated speech produced no words"
    );
    assert_eq!(
        first, second,
        "reusing model changed deterministic transcript"
    );
    if let Ok(expected) = std::env::var("JARVIS_TEST_WHISPER_EXPECT") {
        assert!(
            first.to_lowercase().contains(&expected.to_lowercase()),
            "transcript did not contain fixture ground truth: {first}"
        );
    }
    assert!(!linked.exists());
    let output = Command::new(env!("CARGO_BIN_EXE_jarvis-voice"))
        .arg("--model")
        .arg(&model)
        .arg("--wav")
        .arg(&audio)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), first);
    assert!(
        output.stderr.is_empty(),
        "native inference leaked diagnostic output: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires explicit local model and generated command fixture"]
#[cfg(feature = "whisper")]
fn command_decoder_recognizes_fast_public_fixture_and_respects_bounds_and_precancellation() {
    use jarvis_voice::VoiceError;
    use std::sync::{Arc, atomic::AtomicBool};
    let model = std::env::var_os("JARVIS_TEST_WHISPER_MODEL").expect("local model required");
    let fixture =
        std::env::var_os("JARVIS_TEST_COMMAND_WAV").expect("generated command fixture required");
    let expected =
        std::env::var("JARVIS_TEST_COMMAND_EXPECT").expect("fixture expected phrase required");
    let samples = load_wav(Path::new(&fixture)).unwrap();
    let mut transcriber = Transcriber::load(Path::new(&model)).unwrap();
    assert!(matches!(
        transcriber.transcribe_command_cancelled(&samples, Arc::new(AtomicBool::new(true))),
        Err(VoiceError::Cancelled)
    ));
    assert!(transcriber.transcribe_command(&[]).is_err());
    assert!(transcriber.transcribe_command(&[f32::NAN]).is_err());
    assert!(
        transcriber
            .transcribe_command(&vec![0.1; MAX_SAMPLES + 1])
            .is_err()
    );
    let text = transcriber.transcribe_command(&samples).unwrap();
    assert!(
        text.to_lowercase().contains(&expected.to_lowercase()),
        "generated fixture did not match expected phrase: {text}"
    );
}

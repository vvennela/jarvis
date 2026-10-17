#![cfg(target_os = "macos")]
//! Real local speech detector tests are explicit and never access microphone input.
use jarvis_voice::vad::LocalVoiceActivity;
use std::{
    fs,
    path::{Path, PathBuf},
};
#[path = "../../../tests/support/mod.rs"]
mod support;
#[test]
fn speech_detector_preflight_rejects_missing_symlink_and_oversized_models() {
    let dir = support::TempDir::new();
    assert!(LocalVoiceActivity::load(Path::new("relative.onnx")).is_err());
    assert!(LocalVoiceActivity::load(&dir.join("missing")).is_err());
    assert!(LocalVoiceActivity::load(dir.path()).is_err());
    let target = dir.join("fixture");
    fs::write(&target, "fixture").unwrap();
    let link = dir.join("alias");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(LocalVoiceActivity::load(&link).is_err());
    let huge = dir.join("oversized.onnx");
    fs::File::create(&huge)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    assert!(LocalVoiceActivity::load(&huge).is_err());
}
#[test]
#[ignore = "requires explicit local VAD model and generated speech WAV; no microphone"]
fn opt_in_real_vad_marks_speech_but_ends_on_nonzero_noise_and_recovers_after_invalid_input() {
    let model = PathBuf::from(
        std::env::var_os("JARVIS_TEST_VAD_MODEL").expect("set JARVIS_TEST_VAD_MODEL"),
    );
    let wav =
        PathBuf::from(std::env::var_os("JARVIS_TEST_WAKE_WAV").expect("set JARVIS_TEST_WAKE_WAV"));
    let speech = jarvis_voice::load_wav(&wav).unwrap();
    let mut vad = LocalVoiceActivity::load(&model).unwrap();
    let mut rng = 0x517c2_u32;
    let mut noise = || {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        ((rng >> 8) as f32 / 16777215.0 * 2.0 - 1.0) * 0.06
    };
    let background: Vec<f32> = (0..16000 * 3).map(|_| noise()).collect();
    let mut prefix_flags = Vec::new();
    for chunk in background.chunks(513) {
        let flags = vad.classify(chunk).unwrap();
        assert_eq!(flags.len(), chunk.len().div_ceil(320));
        prefix_flags.extend(flags);
    }
    assert!(
        !prefix_flags.iter().any(|&v| v),
        "deterministic noise was classified as speech"
    );
    let mut speech_flags = Vec::new();
    for chunk in speech.chunks(417) {
        speech_flags.extend(vad.classify(chunk).unwrap());
    }
    assert!(
        speech_flags.iter().any(|&v| v),
        "generated speech not detected"
    );
    let mut tail = Vec::new();
    for chunk in background.chunks(600) {
        tail.extend(vad.classify(chunk).unwrap());
    }
    assert!(
        tail.iter().rev().take(55).all(|&v| !v),
        "steady nonzero background never ended speech state"
    );
    for invalid in [
        vec![f32::NAN],
        vec![f32::INFINITY],
        vec![1.1],
        vec![0.0; 240001],
    ] {
        assert!(vad.classify(&invalid).is_err());
        assert!(vad.classify(&[]).unwrap().is_empty());
    }
    vad.reset();
    let mut fresh = Vec::new();
    for chunk in speech.chunks(320) {
        fresh.extend(vad.classify(chunk).unwrap());
    }
    assert!(fresh.iter().any(|&v| v));
}

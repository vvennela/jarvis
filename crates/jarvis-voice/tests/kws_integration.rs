#![cfg(target_os = "macos")]
//! Hardware-free keyword adapter preflight; real inference requires explicit opt-in assets.
use jarvis_voice::kws::{LocalKeywordSpotter, MAX_CHUNK_SAMPLES, MAX_PREROLL_SAMPLES};
use std::{
    fs,
    path::{Path, PathBuf},
};
#[path = "../../../tests/support/mod.rs"]
mod support;
const ENCODER: &str = "encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx";
#[test]
fn keyword_model_preflight_rejects_absent_relative_and_non_directory_paths() {
    let temp = support::TempDir::new();
    assert!(LocalKeywordSpotter::load(Path::new("relative-model")).is_err());
    assert!(LocalKeywordSpotter::load(&temp.join("missing")).is_err());
    fs::write(temp.join("ordinary"), "fixture").unwrap();
    assert!(LocalKeywordSpotter::load(&temp.join("ordinary")).is_err());
    assert!(LocalKeywordSpotter::load(temp.path()).is_err());
}
#[test]
fn keyword_model_preflight_rejects_symlink_and_oversized_members_before_native_parse() {
    let temp = support::TempDir::new();
    let target = temp.join("target");
    fs::write(&target, "fixture").unwrap();
    let encoder = temp.join(ENCODER);
    std::os::unix::fs::symlink(&target, &encoder).unwrap();
    assert!(LocalKeywordSpotter::load(temp.path()).is_err());
    fs::remove_file(&encoder).unwrap();
    fs::File::create(&encoder)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    assert!(LocalKeywordSpotter::load(temp.path()).is_err());
    assert_eq!(fs::read(target).unwrap(), b"fixture");
}
#[test]
#[ignore = "requires explicit trusted local KWS model and generated WAV; never opens microphone"]
fn opt_in_real_keyword_model_detects_generated_jarvis_and_resets_after_invalid_input() {
    let model = PathBuf::from(
        std::env::var_os("JARVIS_TEST_KWS_MODEL").expect("set JARVIS_TEST_KWS_MODEL"),
    );
    let wav =
        PathBuf::from(std::env::var_os("JARVIS_TEST_WAKE_WAV").expect("set JARVIS_TEST_WAKE_WAV"));
    let mut kws = LocalKeywordSpotter::load(&model).unwrap();
    for _ in 0..100 {
        assert!(kws.accept(&[0.0; 320]).unwrap().is_none());
    }
    let mut samples = jarvis_voice::load_wav(&wav).unwrap();
    samples.extend(vec![0.0; 16000]);
    let detect = |kws: &mut LocalKeywordSpotter| {
        let mut hits = 0;
        for chunk in samples.chunks(320) {
            if let Some(hit) = kws.accept(chunk).unwrap() {
                assert!(hit.lookback_samples > 0 && hit.lookback_samples <= MAX_PREROLL_SAMPLES);
                hits += 1;
            }
        }
        assert_eq!(hits, 1, "generated command must trigger exactly once");
    };
    detect(&mut kws);
    kws.reset();
    detect(&mut kws);
    for invalid in [
        vec![f32::NAN],
        vec![f32::INFINITY],
        vec![1.1],
        vec![0.0; MAX_CHUNK_SAMPLES + 1],
    ] {
        assert!(kws.accept(&invalid).is_err());
        assert!(kws.accept(&[]).unwrap().is_none());
    }
    detect(&mut kws);
}

#[test]
#[ignore = "requires explicit trusted KWS model and generated WAV; synthetic noise only, no microphone"]
fn opt_in_real_keyword_handles_quiet_audio_and_long_nonzero_noise_prelude_with_varied_chunks() {
    let model = PathBuf::from(
        std::env::var_os("JARVIS_TEST_KWS_MODEL").expect("set JARVIS_TEST_KWS_MODEL"),
    );
    let wav =
        PathBuf::from(std::env::var_os("JARVIS_TEST_WAKE_WAV").expect("set JARVIS_TEST_WAKE_WAV"));
    let speech = jarvis_voice::load_wav(&wav).unwrap();
    for (scale, noise) in [(1.0_f32, 0.04_f32), (0.05, 0.002)] {
        let mut kws = LocalKeywordSpotter::load(&model).unwrap();
        let mut rng = 0x2d4a7b19_u32;
        let mut random_noise = || {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            ((rng >> 8) as f32 / 16777215.0 * 2.0 - 1.0) * noise
        };
        let prefix = 22 * 16000;
        let mut samples: Vec<f32> = (0..prefix).map(|_| random_noise()).collect();
        samples.extend(speech.iter().map(|sample| sample * scale + random_noise()));
        samples.extend((0..16000).map(|_| random_noise()));
        let handoff = jarvis_voice::capture::AudioHandoff::new();
        let mut at = 0;
        let mut detections = 0;
        for size in [320, 511, 600, 417, 333, 579].into_iter().cycle() {
            if at == samples.len() {
                break;
            }
            let end = (at + size).min(samples.len());
            handoff.push(&samples[at..end]);
            let (audio, gap) = handoff.drain();
            assert!(!gap);
            if kws.accept(&audio).unwrap().is_some() {
                assert!(end > prefix, "synthetic background falsely woke Jarvis");
                detections += 1;
            }
            at = end;
        }
        assert_eq!(
            detections, 1,
            "synthetic noisy fixture did not wake exactly once: speech_scale={scale}, noise_amplitude={noise}"
        );
        assert_eq!(handoff.overruns(), 0);
    }
}

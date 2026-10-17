#![cfg(target_os = "macos")]
//! Streaming graph-boundary fixtures; opt-in official models are tested separately.
use jarvis_voice::openwakeword::{
    Inference, LocalWakeSpotter, MAX_CHUNK_SAMPLES, STEP, StreamingWake,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
#[path = "../../../tests/support/mod.rs"]
mod support;
#[derive(Default)]
struct Calls {
    pcm: Mutex<Vec<Vec<f32>>>,
    embed_shapes: Mutex<Vec<(usize, usize)>>,
    scores: Mutex<VecDeque<f32>>,
    score_calls: AtomicUsize,
    bad: AtomicUsize,
}
struct Model(Arc<Calls>);
impl Inference for Model {
    fn mel(&mut self, pcm: &[f32]) -> Result<Vec<f32>, &'static str> {
        self.0.pcm.lock().unwrap().push(pcm.to_vec());
        let count = match pcm.len() {
            64000 => 397,
            1280 => 5,
            1760 => 8,
            _ => panic!("unexpected PCM frame size"),
        };
        let mut out = vec![0.; count * 32];
        if self.0.bad.load(Ordering::Acquire) == 1 {
            out[0] = f32::NAN;
        }
        Ok(out)
    }
    fn embed(&mut self, windows: &[f32], count: usize) -> Result<Vec<f32>, &'static str> {
        self.0
            .embed_shapes
            .lock()
            .unwrap()
            .push((windows.len(), count));
        assert_eq!(windows.len(), count * 76 * 32);
        let n = if self.0.bad.load(Ordering::Acquire) == 2 {
            95
        } else {
            96
        };
        Ok(vec![0.; count * n])
    }
    fn score(&mut self, features: &[f32]) -> Result<f32, &'static str> {
        assert_eq!(features.len(), 16 * 96);
        self.0.score_calls.fetch_add(1, Ordering::AcqRel);
        Ok(if self.0.bad.load(Ordering::Acquire) == 3 {
            f32::NAN
        } else {
            self.0.scores.lock().unwrap().pop_front().unwrap_or(1.)
        })
    }
}
fn stream() -> (StreamingWake<Model>, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    (StreamingWake::new(Model(calls.clone())).unwrap(), calls)
}
#[test]
fn fragmented_stream_matches_fixed_80ms_frames_without_padding_or_duplicate_wakes() {
    let audio: Vec<f32> = (0..STEP * 14 + 17)
        .map(|i| ((i % 29) as f32 - 14.) / 30.)
        .collect();
    let run = |sizes: &[usize]| {
        let (mut detector, calls) = stream();
        let mut cursor = 0;
        let mut hits = 0;
        for size in sizes.iter().cycle() {
            if cursor == audio.len() {
                break;
            }
            let end = (cursor + size).min(audio.len());
            if let Some(hit) = detector.accept(&audio[cursor..end]).unwrap() {
                assert!(hit.lookback_samples > 0 && hit.lookback_samples <= 64000);
                hits += 1;
            }
            cursor = end;
        }
        assert_eq!(hits, 1);
        assert_eq!(calls.score_calls.load(Ordering::Acquire), 14);
        calls.pcm.lock().unwrap().clone()
    };
    assert_eq!(run(&[1280]), run(&[1, 319, 600, 17, 511]));
}
#[test]
fn warmup_is_nonuser_and_pcm_quantization_and_overlap_preserve_boundaries() {
    let (mut detector, calls) = stream();
    let initial = calls.pcm.lock().unwrap()[0].clone();
    assert_eq!(initial.len(), 64000);
    assert!(
        initial
            .iter()
            .all(|v| *v >= -1000. && *v < 1000. && v.fract() == 0.)
    );
    assert_eq!(calls.embed_shapes.lock().unwrap()[0], (41 * 76 * 32, 41));
    let mut audio = vec![0.5; STEP];
    audio[..4].copy_from_slice(&[-1., 0., 0.5, 1.]);
    detector.accept(&audio).unwrap();
    detector.accept(&vec![-0.5; STEP]).unwrap();
    let pcm = calls.pcm.lock().unwrap();
    assert_eq!(&pcm[1][..4], &[-32768., 0., 16384., 32767.]);
    assert_eq!(&pcm[2][..480], &pcm[1][STEP - 480..]);
    assert!(pcm[2][480..].iter().all(|v| *v == -16384.));
}
#[test]
fn reset_and_invalid_chunks_remove_partial_audio_and_rearm_without_ambient_authority() {
    let (mut detector, calls) = stream();
    detector.accept(&vec![0.1; 1000]).unwrap();
    detector.reset();
    assert!(detector.accept(&vec![0.; 280]).unwrap().is_none());
    assert_eq!(calls.score_calls.load(Ordering::Acquire), 0);
    for invalid in [
        vec![f32::NAN],
        vec![f32::INFINITY],
        vec![1.1],
        vec![0.; MAX_CHUNK_SAMPLES + 1],
    ] {
        assert!(detector.accept(&invalid).is_err());
        assert!(detector.accept(&[]).unwrap().is_none());
    }
    for i in 0..10 {
        let result = detector.accept(&vec![0.1; STEP]).unwrap();
        assert_eq!(result.is_some(), i == 5);
    }
    detector.reset();
    for i in 0..6 {
        assert_eq!(detector.accept(&vec![0.1; STEP]).unwrap().is_some(), i == 5);
    }
}
#[test]
fn malformed_graph_outputs_cannot_create_detection_or_keep_partial_state() {
    for bad in 1..=3 {
        let (mut detector, calls) = stream();
        calls.bad.store(bad, Ordering::Release);
        assert!(detector.accept(&vec![0.1; STEP]).is_err());
        calls.bad.store(0, Ordering::Release);
        for i in 0..6 {
            assert_eq!(detector.accept(&vec![0.1; STEP]).unwrap().is_some(), i == 5);
        }
    }
}
#[test]
fn absent_model_assets_fail_before_audio_or_microphone_access() {
    let dir = support::TempDir::new();
    assert!(LocalWakeSpotter::load(std::path::Path::new("relative")).is_err());
    assert!(LocalWakeSpotter::load(&dir.join("missing")).is_err());
    assert!(LocalWakeSpotter::load(dir.path()).is_err());
}
#[test]
#[ignore = "requires explicit official local models and generated/public WAV; never opens microphone"]
fn real_openwakeword_streaming_fixture_matches_explicit_expected_detection_count() {
    let model = PathBuf::from(
        std::env::var_os("JARVIS_TEST_OWW_MODEL").expect("official local model directory"),
    );
    let wav = PathBuf::from(std::env::var_os("JARVIS_TEST_OWW_WAV").expect("public fixture WAV"));
    let expected: usize = std::env::var("JARVIS_TEST_OWW_EXPECTED_HITS")
        .expect("explicit expected hits")
        .parse()
        .unwrap();
    let mut detector = LocalWakeSpotter::load(&model).unwrap();
    let mut audio = vec![0.; 16000];
    audio.extend(jarvis_voice::load_wav(&wav).unwrap());
    audio.extend(vec![0.; 16000]);
    let mut hits = 0;
    for chunk in audio.chunks(320) {
        if detector.accept(chunk).unwrap().is_some() {
            hits += 1;
        }
    }
    assert_eq!(hits, expected, "public fixture wake count mismatch");
}

#[test]
#[ignore = "requires official local models and independently generated ONNX reference scores"]
fn real_rust_pipeline_matches_independent_python_onnx_reference_across_public_voice_corpus() {
    let model =
        PathBuf::from(std::env::var_os("JARVIS_TEST_OWW_MODEL").expect("official model directory"));
    let reference = PathBuf::from(
        std::env::var_os("JARVIS_TEST_OWW_REFERENCE").expect("public corpus reference scores"),
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(reference).unwrap()).unwrap();
    let mut detector = LocalWakeSpotter::load(&model).unwrap();
    for row in rows.as_array().unwrap() {
        detector.reset();
        let wav = PathBuf::from(row["wav"].as_str().unwrap());
        let mut audio = vec![0.; 16000];
        audio.extend(jarvis_voice::load_wav(&wav).unwrap());
        audio.extend(vec![0.; 16000]);
        let expected = row["scores"].as_array().unwrap();
        let mut hits = 0;
        for (index, chunk) in audio.chunks_exact(STEP).enumerate() {
            if detector.accept(chunk).unwrap().is_some() {
                hits += 1;
            }
            let score = detector.last_score();
            let python = expected[index].as_f64().unwrap() as f32;
            assert!(
                (score - python).abs() < 0.0002,
                "reference mismatch {} {} frame{index}: Rust{score} Python{python}",
                row["voice"],
                row["label"]
            );
        }
        assert_eq!(
            hits,
            row["hits"].as_u64().unwrap() as usize,
            "reference hit count mismatch"
        );
        println!(
            "public synthetic {} {}: {hits} detections",
            row["voice"], row["label"]
        );
    }
}

#[test]
#[ignore = "requires local Sherpa model and public synthetic OWW corpus"]
fn real_sherpa_complement_reports_same_public_corpus_without_threshold_tuning() {
    let model =
        PathBuf::from(std::env::var_os("JARVIS_TEST_KWS_MODEL").expect("local Sherpa model"));
    let reference =
        PathBuf::from(std::env::var_os("JARVIS_TEST_OWW_REFERENCE").expect("public corpus"));
    let rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(reference).unwrap()).unwrap();
    let mut detector = jarvis_voice::kws::LocalKeywordSpotter::load(&model).unwrap();
    for row in rows.as_array().unwrap() {
        detector.reset();
        let mut audio = vec![0.; 16000];
        audio.extend(jarvis_voice::load_wav(&PathBuf::from(row["wav"].as_str().unwrap())).unwrap());
        audio.extend(vec![0.; 16000]);
        let mut hits = 0;
        for chunk in audio.chunks(320) {
            hits += usize::from(detector.accept(chunk).unwrap().is_some());
        }
        let oww = row["hits"].as_u64().unwrap();
        println!(
            "public synthetic {} {}: Sherpa={hits}, OWW={oww}, union={}",
            row["voice"],
            row["label"],
            hits > 0 || oww > 0
        );
    }
}

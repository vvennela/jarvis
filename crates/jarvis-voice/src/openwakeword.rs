//! openWakeWord v0.6 feature pipeline, using its official v0.5.1 ONNX assets.
//! Mono 16 kHz normalized PCM is quantized back to signed 16-bit amplitude before
//! the mel graph. No transcript, network request, or microphone is created here.
use crate::kws::Detection;
use std::path::Path;

pub const STEP: usize = 1280;
pub const MAX_CHUNK_SAMPLES: usize = 16_000 * 15;
const MEL_BINS: usize = 32;
const MEL_FRAMES: usize = 76;
const FEATURES: usize = 96;
const WAKE_FRAMES: usize = 16;
const THRESHOLD: f32 = 0.5;

/// Injectable graph boundary; production uses the three official ONNX graphs.
pub trait Inference {
    fn mel(&mut self, pcm: &[f32]) -> Result<Vec<f32>, &'static str>;
    fn embed(&mut self, windows: &[f32], count: usize) -> Result<Vec<f32>, &'static str>;
    fn score(&mut self, features: &[f32]) -> Result<f32, &'static str>;
}

pub struct StreamingWake<M: Inference> {
    models: M,
    pending: Vec<f32>,
    previous: Vec<f32>,
    mels: Vec<f32>,
    embeddings: Vec<f32>,
    initial_embeddings: Vec<f32>,
    frames: u64,
    armed: bool,
    last_score: f32,
}
impl<M: Inference> StreamingWake<M> {
    pub fn new(mut models: M) -> Result<Self, &'static str> {
        // Upstream seeds its history with four seconds of integer noise in
        // [-1000,1000). A fixed PRNG makes that non-user warmup reproducible.
        let mut seed = 0x6a09_e667_u32;
        let noise: Vec<f32> = (0..64_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (i32::try_from(seed % 2000).unwrap_or(0) - 1000) as f32
            })
            .collect();
        let mel = checked_mels(models.mel(&noise)?, 397)?;
        let count = (397 - MEL_FRAMES) / 8 + 1;
        let mut windows = Vec::with_capacity(count * MEL_FRAMES * MEL_BINS);
        for index in 0..count {
            windows
                .extend_from_slice(&mel[index * 8 * MEL_BINS..(index * 8 + MEL_FRAMES) * MEL_BINS]);
        }
        let embeddings = models.embed(&windows, count)?;
        if embeddings.len() != count * FEATURES || !finite(&embeddings) {
            return Err("Invalid openWakeWord embedding output");
        }
        let initial_embeddings = embeddings[embeddings.len() - WAKE_FRAMES * FEATURES..].to_vec();
        Ok(Self {
            models,
            pending: Vec::with_capacity(STEP),
            previous: Vec::with_capacity(480),
            mels: vec![1.0; MEL_FRAMES * MEL_BINS],
            embeddings: initial_embeddings.clone(),
            initial_embeddings,
            frames: 0,
            armed: true,
            last_score: 0.0,
        })
    }
    pub fn reset(&mut self) {
        self.pending.clear();
        self.previous.clear();
        self.mels.fill(1.0);
        self.embeddings.copy_from_slice(&self.initial_embeddings);
        self.frames = 0;
        self.armed = true;
        self.last_score = 0.0;
    }
    pub fn last_score(&self) -> f32 {
        self.last_score
    }
    pub fn accept(&mut self, samples: &[f32]) -> Result<Option<Detection>, &'static str> {
        if samples.len() > MAX_CHUNK_SAMPLES
            || samples.iter().any(|s| !s.is_finite() || s.abs() > 1.0)
        {
            self.reset();
            return Err("Invalid openWakeWord audio chunk");
        }
        let mut detected = None;
        for (index, sample) in samples.iter().enumerate() {
            self.pending
                .push((sample * 32768.0).round().clamp(-32768.0, 32767.0));
            if self.pending.len() != STEP {
                continue;
            }
            let score = match self.step() {
                Ok(score) => score,
                Err(error) => {
                    self.reset();
                    return Err(error);
                }
            };
            if score < THRESHOLD {
                self.armed = true;
            }
            if self.frames > 5 && score >= THRESHOLD && self.armed {
                self.armed = false;
                // The model sees 16 embedding hops plus the 76-frame mel context.
                // Include a bounded two-second lookback and the rest of this batch.
                if detected.is_none() {
                    detected = Some(Detection {
                        lookback_samples: (32_000 + samples.len() - index - 1).min(64_000),
                    });
                }
            }
        }
        Ok(detected)
    }
    fn step(&mut self) -> Result<f32, &'static str> {
        let mut raw = Vec::with_capacity(self.previous.len() + STEP);
        raw.extend_from_slice(&self.previous);
        raw.extend_from_slice(&self.pending);
        let expected_frames = if self.previous.is_empty() { 5 } else { 8 };
        let mel = checked_mels(self.models.mel(&raw)?, expected_frames)?;
        self.previous.clear();
        self.previous.extend_from_slice(&self.pending[STEP - 480..]);
        self.pending.clear();
        let removed = mel.len();
        self.mels.copy_within(removed.., 0);
        let start = self.mels.len() - removed;
        self.mels[start..].copy_from_slice(&mel);
        let embedding = self.models.embed(&self.mels, 1)?;
        if embedding.len() != FEATURES || !finite(&embedding) {
            return Err("Invalid openWakeWord embedding output");
        }
        self.embeddings.copy_within(FEATURES.., 0);
        self.embeddings[(WAKE_FRAMES - 1) * FEATURES..].copy_from_slice(&embedding);
        let score = self.models.score(&self.embeddings)?;
        if !score.is_finite() || !(0.0..=1.0).contains(&score) {
            return Err("Invalid openWakeWord score");
        }
        self.frames = self.frames.saturating_add(1);
        self.last_score = if self.frames <= 5 { 0.0 } else { score };
        Ok(self.last_score)
    }
}
fn finite(data: &[f32]) -> bool {
    data.iter().all(|value| value.is_finite())
}
fn checked_mels(mut data: Vec<f32>, frames: usize) -> Result<Vec<f32>, &'static str> {
    if data.len() != frames * MEL_BINS || !finite(&data) {
        return Err("Invalid openWakeWord mel output");
    }
    for value in &mut data {
        *value = *value / 10.0 + 2.0;
    }
    Ok(data)
}

struct OnnxModels {
    mel: ort::session::Session,
    embedding: ort::session::Session,
    wake: ort::session::Session,
}
impl OnnxModels {
    fn load(directory: &Path) -> Result<Self, &'static str> {
        if !directory.is_absolute() || !directory.is_dir() {
            return Err("Choose an absolute openWakeWord model directory");
        }
        let path = |name: &str, cap: u64| -> Result<std::path::PathBuf, &'static str> {
            let path = directory.join(name);
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| "Download the openWakeWord model assets")?;
            if !metadata.is_file() || metadata.len() == 0 || metadata.len() > cap {
                return Err("Check the openWakeWord model assets");
            }
            Ok(path)
        };
        // Sherpa and openWakeWord share the statically linked ONNX Runtime.
        // API v22 remains supported by the newer runtime shipped with Sherpa.
        ort::init()
            .commit()
            .map_err(|_| "Could not initialize ONNX Runtime")?;
        let session = |name: &str| -> Result<ort::session::Session, &'static str> {
            ort::session::Session::builder()
                .map_err(|_| "Could not initialize openWakeWord")?
                .with_intra_threads(1)
                .map_err(|_| "Could not configure openWakeWord")?
                .with_inter_threads(1)
                .map_err(|_| "Could not configure openWakeWord")?
                .commit_from_file(path(name, 32 * 1024 * 1024)?)
                .map_err(|_| "Could not load openWakeWord models")
        };
        Ok(Self {
            mel: session("melspectrogram.onnx")?,
            embedding: session("embedding_model.onnx")?,
            wake: session("hey_jarvis_v0.1.onnx")?,
        })
    }
}
fn run(
    session: &mut ort::session::Session,
    shape: &[usize],
    values: &[f32],
) -> Result<Vec<f32>, &'static str> {
    let tensor =
        ort::value::Tensor::from_array((shape.to_vec(), values.to_vec().into_boxed_slice()))
            .map_err(|_| "Could not prepare openWakeWord features")?;
    let output = session
        .run(ort::inputs![tensor])
        .map_err(|_| "openWakeWord inference failed")?;
    if output.len() != 1 {
        return Err("Invalid openWakeWord model output");
    }
    let (_, data) = output[0]
        .try_extract_tensor::<f32>()
        .map_err(|_| "Invalid openWakeWord model output")?;
    if data.len() > 64_000 {
        return Err("Invalid openWakeWord model output");
    }
    Ok(data.to_vec())
}
impl Inference for OnnxModels {
    fn mel(&mut self, pcm: &[f32]) -> Result<Vec<f32>, &'static str> {
        run(&mut self.mel, &[1, pcm.len()], pcm)
    }
    fn embed(&mut self, windows: &[f32], count: usize) -> Result<Vec<f32>, &'static str> {
        run(
            &mut self.embedding,
            &[count, MEL_FRAMES, MEL_BINS, 1],
            windows,
        )
    }
    fn score(&mut self, features: &[f32]) -> Result<f32, &'static str> {
        let score = run(&mut self.wake, &[1, WAKE_FRAMES, FEATURES], features)?;
        if score.len() != 1 {
            return Err("Invalid openWakeWord score shape");
        }
        Ok(score[0])
    }
}

pub struct LocalWakeSpotter {
    stream: StreamingWake<OnnxModels>,
}
impl LocalWakeSpotter {
    pub fn load(directory: &Path) -> Result<Self, &'static str> {
        Ok(Self {
            stream: StreamingWake::new(OnnxModels::load(directory)?)?,
        })
    }
    pub fn accept(&mut self, samples: &[f32]) -> Result<Option<Detection>, &'static str> {
        self.stream.accept(samples)
    }
    pub fn reset(&mut self) {
        self.stream.reset();
    }
    pub fn last_score(&self) -> f32 {
        self.stream.last_score()
    }
}

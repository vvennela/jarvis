//! Streaming local wake spotting. Only the fixed Jarvis keyword leaves this
//! adapter; ambient audio and intermediate model outputs are never retained.
use sherpa_onnx::{KeywordSpotter, KeywordSpotterConfig, OnlineStream};
use std::path::Path;

pub const MAX_CHUNK_SAMPLES: usize = 16_000 * 15;
pub const MAX_PREROLL_SAMPLES: usize = 16_000 * 4;

/// The number of recent samples covering the detected keyword and its tail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detection {
    pub lookback_samples: usize,
}

pub struct LocalKeywordSpotter {
    // Drop stream before its owning model.
    stream: OnlineStream,
    model: KeywordSpotter,
    accepted: u64,
}
impl LocalKeywordSpotter {
    pub fn load(directory: &Path) -> Result<Self, &'static str> {
        if !directory.is_absolute() || !directory.is_dir() {
            return Err("Choose an absolute local keyword model directory");
        }
        let path = |name: &str| -> Result<String, &'static str> {
            let path = directory.join(name);
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|_| "Download the local Jarvis keyword model")?;
            if !meta.is_file() || meta.len() == 0 || meta.len() > 64 * 1024 * 1024 {
                return Err("Check the local keyword model files");
            }
            path.to_str()
                .filter(|p| !p.contains('\0'))
                .map(str::to_owned)
                .ok_or("Choose a valid keyword model path")
        };
        let mut config = KeywordSpotterConfig::default();
        config.model_config.transducer.encoder =
            Some(path("encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx")?);
        config.model_config.transducer.decoder =
            Some(path("decoder-epoch-12-avg-2-chunk-16-left-64.onnx")?);
        config.model_config.transducer.joiner =
            Some(path("joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx")?);
        config.model_config.tokens = Some(path("tokens.txt")?);
        config.model_config.num_threads = 1;
        config.model_config.provider = Some("cpu".into());
        config.model_config.debug = false;
        // Encoded with this English model's bundled bpe.model. No general
        // transcription vocabulary or provider call is used while waiting.
        config.keywords_buf = Some("▁JA R VI S @Jarvis".into());
        let model =
            KeywordSpotter::create(&config).ok_or("Could not load the local keyword model")?;
        let stream = model.create_stream();
        Ok(Self {
            stream,
            model,
            accepted: 0,
        })
    }
    pub fn reset(&mut self) {
        self.stream = self.model.create_stream();
        self.accepted = 0;
    }
    pub fn accept(&mut self, samples: &[f32]) -> Result<Option<Detection>, &'static str> {
        if samples.len() > MAX_CHUNK_SAMPLES
            || samples.iter().any(|s| !s.is_finite() || s.abs() > 1.0)
        {
            self.reset();
            return Err("Invalid keyword audio chunk");
        }
        if samples.is_empty() {
            return Ok(None);
        }
        self.accepted = self.accepted.saturating_add(samples.len() as u64);
        self.stream.accept_waveform(16_000, samples);
        let mut detected = None;
        while self.model.is_ready(&self.stream) {
            self.model.decode(&self.stream);
            if let Some(result) = self.model.get_result(&self.stream)
                && result.keyword == "Jarvis"
            {
                let start = result.start_time + result.timestamps.first().copied().unwrap_or(0.0);
                let start_sample = if start.is_finite() && start >= 0.0 {
                    (start * 16_000.0) as u64
                } else {
                    self.accepted
                };
                let lookback = self
                    .accepted
                    .saturating_sub(start_sample)
                    .saturating_add(1280);
                detected = Some(Detection {
                    lookback_samples: (lookback as usize).min(MAX_PREROLL_SAMPLES),
                });
                self.model.reset(&self.stream);
            }
        }
        Ok(detected)
    }
}

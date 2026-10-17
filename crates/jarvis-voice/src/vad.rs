//! Local speech classification for command endpointing. Classification is
//! independent of microphone gain and returns only bounded speech flags.
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};
use std::path::Path;

pub struct LocalVoiceActivity {
    detector: VoiceActivityDetector,
}
impl LocalVoiceActivity {
    pub fn load(path: &Path) -> Result<Self, &'static str> {
        if !path.is_absolute() {
            return Err("Choose an absolute local speech detector path");
        }
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|_| "Download the local speech detector model")?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 16 * 1024 * 1024 {
            return Err("Check the local speech detector model");
        }
        let path = path
            .to_str()
            .filter(|p| !p.contains('\0'))
            .ok_or("Choose a valid speech detector path")?;
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(path.into()),
                threshold: 0.5,
                min_silence_duration: 0.064,
                min_speech_duration: 0.032,
                window_size: 512,
                max_speech_duration: 15.0,
            },
            ten_vad: Default::default(),
            sample_rate: 16_000,
            num_threads: 1,
            provider: Some("cpu".into()),
            debug: false,
        };
        let detector = VoiceActivityDetector::create(&config, 30.0)
            .ok_or("Could not load the local speech detector")?;
        Ok(Self { detector })
    }
    pub fn reset(&mut self) {
        self.detector.reset();
    }
    /// One flag per 20ms frame, including a final partial frame. Native VAD
    /// buffers its 512-sample windows across calls, never padding live audio.
    pub fn classify(&mut self, samples: &[f32]) -> Result<Vec<bool>, &'static str> {
        if samples.len() > crate::capture::MAX_RECORDING_SAMPLES
            || samples
                .iter()
                .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
        {
            self.reset();
            return Err("Invalid speech detector audio chunk");
        }
        let mut activity = Vec::with_capacity(samples.len().div_ceil(320));
        for frame in samples.chunks(320) {
            self.detector.accept_waveform(frame);
            activity.push(self.detector.detected());
            // We need only speech state, not completed audio segments.
            while !self.detector.is_empty() {
                self.detector.pop();
            }
        }
        Ok(activity)
    }
}

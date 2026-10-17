#![forbid(unsafe_code)]
//! Local bounded speech transcription. Keep one Transcriber resident across
//! utterances; it reuses the loaded model and inference state. No microphone is
//! opened implicitly and transcription uses no cloud service.

pub mod capture;

use std::{
    fmt,
    fs::File,
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

pub const SAMPLE_RATE: u32 = 16_000;
pub const MAX_AUDIO_SECONDS: usize = 30;
pub const MAX_SAMPLES: usize = SAMPLE_RATE as usize * MAX_AUDIO_SECONDS;
pub const MAX_MODEL_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_WAV_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 8192;
pub const INFERENCE_THREADS: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceError {
    InvalidModel,
    ModelTooLarge,
    ModelLoad,
    InvalidAudio,
    AudioTooLong,
    InvalidWav,
    WavTooLarge,
    Transcription,
    TranscriptTooLarge,
    Cancelled,
}

impl fmt::Display for VoiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidModel => "provide a regular local GGML Whisper model file",
            Self::ModelTooLarge => "Whisper model exceeds the 256 MiB limit",
            Self::ModelLoad => "Whisper model could not be loaded",
            Self::InvalidAudio => "audio must contain finite normalized mono 16 kHz samples",
            Self::AudioTooLong => "audio exceeds the 30 second limit",
            Self::InvalidWav => "provide a valid mono 16 kHz 16-bit PCM WAV file",
            Self::WavTooLarge => "WAV file exceeds the 2 MiB limit",
            Self::Transcription => "local transcription failed",
            Self::TranscriptTooLarge => "transcript exceeds the 8 KiB limit",
            Self::Cancelled => "transcription cancelled",
        })
    }
}
impl std::error::Error for VoiceError {}

/// Model/state are retained together; the caller's PCM slice is borrowed.
/// Native scratch state and latest decoder output are reused between calls;
/// whisper-rs owns those allocations through its safe wrapper API.
pub struct Transcriber {
    state: WhisperState,
    _context: WhisperContext,
}

impl Transcriber {
    /// Cancellation is checked before and after native inference. A running
    /// native decode finishes before returning; cancelled output is discarded.
    pub fn transcribe_cancelled(
        &mut self,
        samples: &[f32],
        cancel: Arc<AtomicBool>,
    ) -> Result<String, VoiceError> {
        if cancel.load(Ordering::Acquire) {
            return Err(VoiceError::Cancelled);
        }
        let result = self.transcribe(samples);
        if cancel.load(Ordering::Acquire) {
            return Err(VoiceError::Cancelled);
        }
        result
    }

    /// Load trusted local weights once. Header/size checks are preflight checks,
    /// not a complete security validator for arbitrary native model files.
    pub fn load(model_path: &Path) -> Result<Self, VoiceError> {
        validate_model_file(model_path)?;
        // With no logging backend enabled, the safe upstream hooks suppress
        // native diagnostics, including token text derived from microphone audio.
        whisper_rs::install_logging_hooks();
        let mut parameters = WhisperContextParameters::default();
        parameters.use_gpu(cfg!(all(feature = "metal", target_os = "macos")));
        let context = WhisperContext::new_with_params(model_path, parameters)
            .map_err(|_| VoiceError::ModelLoad)?;
        let state = context.create_state().map_err(|_| VoiceError::ModelLoad)?;
        Ok(Self {
            state,
            _context: context,
        })
    }

    /// Transcribe a single utterance synchronously. Call on a dedicated worker
    /// thread, not the AppKit main thread. Past transcript context is disabled.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String, VoiceError> {
        validate_audio(samples)?;
        let mut parameters = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        parameters.set_n_threads(INFERENCE_THREADS);
        parameters.set_language(Some("en"));
        parameters.set_translate(false);
        parameters.set_no_context(true);
        parameters.set_no_timestamps(true);
        parameters.set_print_progress(false);
        parameters.set_print_realtime(false);
        parameters.set_print_special(false);
        parameters.set_print_timestamps(false);
        self.state
            .full(parameters, samples)
            .map_err(|_| VoiceError::Transcription)?;
        let mut transcript = String::new();
        for segment in self.state.as_iter() {
            let text = segment.to_str().map_err(|_| VoiceError::Transcription)?;
            if text.len() > MAX_TRANSCRIPT_BYTES - transcript.len() {
                return Err(VoiceError::TranscriptTooLarge);
            }
            transcript.push_str(text);
        }
        Ok(transcript.trim().to_owned())
    }
}

pub fn validate_audio(samples: &[f32]) -> Result<(), VoiceError> {
    if samples.len() > MAX_SAMPLES {
        return Err(VoiceError::AudioTooLong);
    }
    if samples.is_empty()
        || samples
            .iter()
            .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        return Err(VoiceError::InvalidAudio);
    }
    Ok(())
}

pub fn validate_model_file(path: &Path) -> Result<(), VoiceError> {
    let mut file = regular_file(path).map_err(|_| VoiceError::InvalidModel)?;
    let metadata = file.metadata().map_err(|_| VoiceError::InvalidModel)?;
    if metadata.len() > MAX_MODEL_BYTES {
        return Err(VoiceError::ModelTooLarge);
    }
    if metadata.len() < 1024 {
        return Err(VoiceError::InvalidModel);
    }
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|_| VoiceError::InvalidModel)?;
    // whisper.cpp GGML magic 0x67676d6c, encoded little-endian.
    if magic != [0x6c, 0x6d, 0x67, 0x67] {
        return Err(VoiceError::InvalidModel);
    }
    Ok(())
}

pub fn load_wav(path: &Path) -> Result<Vec<f32>, VoiceError> {
    let file = regular_file(path).map_err(|_| VoiceError::InvalidWav)?;
    if file.metadata().map_err(|_| VoiceError::InvalidWav)?.len() > MAX_WAV_BYTES {
        return Err(VoiceError::WavTooLarge);
    }
    let mut reader =
        hound::WavReader::new(file.take(MAX_WAV_BYTES + 1)).map_err(|_| VoiceError::InvalidWav)?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != SAMPLE_RATE
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(VoiceError::InvalidWav);
    }
    if reader.duration() as usize > MAX_SAMPLES {
        return Err(VoiceError::AudioTooLong);
    }
    let mut samples = Vec::with_capacity((reader.duration() as usize).min(MAX_SAMPLES));
    for sample in reader.samples::<i16>() {
        if samples.len() >= MAX_SAMPLES {
            return Err(VoiceError::AudioTooLong);
        }
        samples.push(f32::from(sample.map_err(|_| VoiceError::InvalidWav)?) / 32768.0);
    }
    validate_audio(&samples)?;
    Ok(samples)
}

#[cfg(unix)]
fn regular_file(path: &Path) -> Result<File, ()> {
    use rustix::fs::{Mode, OFlags, open};
    let file = File::from(
        open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| ())?,
    );
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    Ok(file)
}

#[cfg(not(unix))]
fn regular_file(path: &Path) -> Result<File, ()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(());
    }
    File::open(path).map_err(|_| ())
}

//! Bounded microphone capture and a deterministic streaming mono resampler.
//! Constructing a CaptureBuffer never accesses hardware. CaptureSession::start
//! is the explicit microphone boundary and is only called after a user action.
use std::fmt;

pub const MAX_RECORDING_SECONDS: usize = 15;
pub const MAX_RECORDING_SAMPLES: usize = 16_000 * MAX_RECORDING_SECONDS;
pub const OUTPUT_RATE: u32 = 16_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    InvalidFormat,
    InvalidAudio,
    Empty,
    DeviceUnavailable,
    StreamUnavailable,
    StreamFailed,
}
impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidFormat => "Choose a supported microphone format",
            Self::InvalidAudio => "Microphone audio contains invalid samples",
            Self::Empty => "No speech audio was captured",
            Self::DeviceUnavailable => "Choose a microphone in macOS Sound settings",
            Self::StreamUnavailable => {
                "Allow microphone access in macOS Privacy & Security settings"
            }
            Self::StreamFailed => "Microphone capture stopped; check audio settings",
        })
    }
}
impl std::error::Error for CaptureError {}

pub struct CaptureBuffer {
    source_rate: u32,
    channels: u16,
    output: Vec<f32>,
    channel_sum: f64,
    channel_count: u16,
    input_frames: u64,
    previous: Option<f32>,
    next_output_numerator: u64,
    invalid: bool,
    continuous: bool,
    overrun: bool,
}
impl CaptureBuffer {
    pub fn new(source_rate: u32, channels: u16) -> Result<Self, CaptureError> {
        if !(8000..=192000).contains(&source_rate) || !(1..=8).contains(&channels) {
            return Err(CaptureError::InvalidFormat);
        }
        Ok(Self {
            source_rate,
            channels,
            output: Vec::with_capacity(MAX_RECORDING_SAMPLES),
            channel_sum: 0.0,
            channel_count: 0,
            input_frames: 0,
            previous: None,
            next_output_numerator: 0,
            invalid: false,
            continuous: false,
            overrun: false,
        })
    }
    /// Fixed-capacity rolling capture; extraction allocates only on the consumer.
    pub fn continuous(source_rate: u32, channels: u16) -> Result<Self, CaptureError> {
        let mut buffer = Self::new(source_rate, channels)?;
        buffer.continuous = true;
        Ok(buffer)
    }
    pub fn drain_samples(&mut self) -> (Vec<f32>, bool) {
        // Copy on the consumer thread and retain the callback's preallocated
        // storage. Moving this Vec out would force allocation on later input.
        let samples = self.output.to_vec();
        self.output.clear();
        let overrun = std::mem::take(&mut self.overrun);
        (samples, overrun)
    }
    pub fn output_len(&self) -> usize {
        self.output.len()
    }
    pub fn is_full(&self) -> bool {
        !self.continuous
            && (self.output.len() >= MAX_RECORDING_SAMPLES
                || self.input_frames >= self.source_rate as u64 * MAX_RECORDING_SECONDS as u64)
    }
    pub fn push_f32(&mut self, values: &[f32]) -> Result<(), CaptureError> {
        self.push(values.iter().copied())
    }
    pub fn push_i16(&mut self, values: &[i16]) -> Result<(), CaptureError> {
        self.push(values.iter().map(|value| f32::from(*value) / 32768.0))
    }
    pub fn push_u16(&mut self, values: &[u16]) -> Result<(), CaptureError> {
        self.push(
            values
                .iter()
                .map(|value| (f32::from(*value) - 32768.0) / 32768.0),
        )
    }
    fn push(&mut self, values: impl Iterator<Item = f32>) -> Result<(), CaptureError> {
        if self.invalid {
            return Err(CaptureError::InvalidAudio);
        }
        for value in values {
            if self.is_full() {
                break;
            }
            if !value.is_finite() {
                self.invalid = true;
                return Err(CaptureError::InvalidAudio);
            }
            self.channel_sum += f64::from(value.clamp(-1.0, 1.0));
            self.channel_count += 1;
            if self.channel_count != self.channels {
                continue;
            }
            let mono = (self.channel_sum / f64::from(self.channels)) as f32;
            self.channel_sum = 0.0;
            self.channel_count = 0;
            let current_numerator = self.input_frames * u64::from(OUTPUT_RATE);
            while self.next_output_numerator <= current_numerator
                && (self.continuous || self.output.len() < MAX_RECORDING_SAMPLES)
            {
                let sample = if let Some(previous) = self.previous {
                    let previous_numerator = current_numerator - u64::from(OUTPUT_RATE);
                    let fraction = (self.next_output_numerator - previous_numerator) as f32
                        / OUTPUT_RATE as f32;
                    previous + (mono - previous) * fraction
                } else {
                    mono
                };
                if self.output.len() == MAX_RECORDING_SAMPLES {
                    self.output.drain(..OUTPUT_RATE as usize);
                    self.overrun = true;
                }
                self.output.push(sample.clamp(-1.0, 1.0));
                self.next_output_numerator += u64::from(self.source_rate);
            }
            self.previous = Some(mono);
            self.input_frames += 1;
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Vec<f32>, CaptureError> {
        if self.invalid {
            return Err(CaptureError::InvalidAudio);
        }
        let Some(last) = self.previous else {
            return Err(CaptureError::Empty);
        };
        // Hold the final frame for the fractional tail when upsampling. This
        // makes duration deterministic without depending on callback boundaries.
        let duration_numerator = self.input_frames * u64::from(OUTPUT_RATE);
        while self.next_output_numerator < duration_numerator
            && self.output.len() < MAX_RECORDING_SAMPLES
        {
            self.output.push(last);
            self.next_output_numerator += u64::from(self.source_rate);
        }
        Ok(self.output)
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    pub struct CaptureSession {
        stream: Option<cpal::Stream>,
        buffer: Arc<Mutex<Option<CaptureBuffer>>>,
        failed: Arc<AtomicBool>,
        full: Arc<AtomicBool>,
    }
    impl CaptureSession {
        pub fn start() -> Result<Self, CaptureError> {
            Self::start_mode(false)
        }
        fn start_mode(continuous: bool) -> Result<Self, CaptureError> {
            let device = cpal::default_host()
                .default_input_device()
                .ok_or(CaptureError::DeviceUnavailable)?;
            let supported = device
                .default_input_config()
                .map_err(|_| CaptureError::StreamUnavailable)?;
            let config: cpal::StreamConfig = supported.into();
            let buffer = Arc::new(Mutex::new(Some(if continuous {
                CaptureBuffer::continuous(config.sample_rate, config.channels)?
            } else {
                CaptureBuffer::new(config.sample_rate, config.channels)?
            })));
            let failed = Arc::new(AtomicBool::new(false));
            let full = Arc::new(AtomicBool::new(false));
            macro_rules! stream {
                ($sample:ty, $push:ident) => {{
                    let data_buffer = Arc::clone(&buffer);
                    let data_failed = Arc::clone(&failed);
                    let data_full = Arc::clone(&full);
                    let error_flag = Arc::clone(&failed);
                    device.build_input_stream(
                        config.clone(),
                        move |data: &[$sample], _: &cpal::InputCallbackInfo| {
                            if data_failed.load(Ordering::Acquire)
                                || data_full.load(Ordering::Acquire)
                            {
                                return;
                            }
                            if let Ok(mut guard) = data_buffer.try_lock() {
                                if let Some(buffer) = guard.as_mut() {
                                    if buffer.$push(data).is_err() {
                                        data_failed.store(true, Ordering::Release);
                                    }
                                    if buffer.is_full() {
                                        data_full.store(true, Ordering::Release);
                                    }
                                }
                            } else if !continuous {
                                data_failed.store(true, Ordering::Release);
                            }
                        },
                        move |_| {
                            error_flag.store(true, Ordering::Release);
                        },
                        None,
                    )
                }};
            }
            let stream = match supported.sample_format() {
                cpal::SampleFormat::F32 => stream!(f32, push_f32),
                cpal::SampleFormat::I16 => stream!(i16, push_i16),
                cpal::SampleFormat::U16 => stream!(u16, push_u16),
                _ => return Err(CaptureError::InvalidFormat),
            }
            .map_err(|_| CaptureError::StreamUnavailable)?;
            stream.play().map_err(|_| CaptureError::StreamUnavailable)?;
            Ok(Self {
                stream: Some(stream),
                buffer,
                failed,
                full,
            })
        }
        pub fn is_full(&self) -> bool {
            self.full.load(Ordering::Acquire)
        }
        pub fn failed(&self) -> bool {
            self.failed.load(Ordering::Acquire)
        }
        pub fn stop(mut self) -> Result<Vec<f32>, CaptureError> {
            // Close the owned stream before taking its bounded sample buffer.
            self.stream.take();
            if self.failed() {
                return Err(CaptureError::StreamFailed);
            }
            self.buffer
                .lock()
                .map_err(|_| CaptureError::StreamFailed)?
                .take()
                .ok_or(CaptureError::Empty)?
                .finish()
        }
    }
    /// Owns a running input stream and a bounded 15-second mono rolling buffer.
    pub struct ContinuousCapture(CaptureSession);
    impl ContinuousCapture {
        pub fn start() -> Result<Self, CaptureError> {
            CaptureSession::start_mode(true).map(Self)
        }
        pub fn poll(&mut self) -> Result<(Vec<f32>, bool), CaptureError> {
            if self.0.failed() {
                return Err(CaptureError::StreamFailed);
            }
            let mut guard = self
                .0
                .buffer
                .lock()
                .map_err(|_| CaptureError::StreamFailed)?;
            Ok(guard.as_mut().ok_or(CaptureError::Empty)?.drain_samples())
        }
    }
}
#[cfg(target_os = "macos")]
pub use native::{CaptureSession, ContinuousCapture};

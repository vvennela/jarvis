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
    InvalidDeviceSelection,
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
            Self::InvalidDeviceSelection => "Choose one exact available microphone name",
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

pub const MAX_INPUT_DEVICES: usize = 32;
pub const MAX_INPUT_NAME_BYTES: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}
/// Exact names avoid silently switching to a different microphone. This pure
/// selector is also used by native capture; enumeration never starts a stream.
pub fn select_input_index(names: &[String], requested: &str) -> Result<usize, CaptureError> {
    if requested.is_empty()
        || requested.len() > MAX_INPUT_NAME_BYTES
        || requested.chars().any(char::is_control)
        || names.len() > MAX_INPUT_DEVICES
    {
        return Err(CaptureError::InvalidDeviceSelection);
    }
    let mut matches = names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.as_str() == requested);
    let index = matches
        .next()
        .map(|(index, _)| index)
        .ok_or(CaptureError::DeviceUnavailable)?;
    if matches.next().is_some() {
        return Err(CaptureError::InvalidDeviceSelection);
    }
    Ok(index)
}

/// Tracks the selected input separately from output routing. Failed candidate
/// switches are not committed; an explicit user pin never follows the default.
pub struct DefaultInputFollower {
    pinned: bool,
    selected: String,
    last_check: std::time::Duration,
}
impl DefaultInputFollower {
    pub fn new(pinned: bool, selected: String, now: std::time::Duration) -> Self {
        Self {
            pinned,
            selected,
            last_check: now,
        }
    }
    pub fn poll_due(&mut self, now: std::time::Duration) -> bool {
        if self.pinned || now.saturating_sub(self.last_check) < std::time::Duration::from_secs(1) {
            return false;
        }
        self.last_check = now;
        true
    }
    pub fn changed(&self, candidate: &str) -> bool {
        !self.pinned && !candidate.is_empty() && candidate != self.selected
    }
    pub fn selected(&self) -> &str {
        &self.selected
    }
    pub fn commit(&mut self, selected: String) {
        self.selected = selected;
    }
}

/// Preallocated lock-free callback handoff. Producer pushes never allocate or
/// wait for the consumer; only actual capacity exhaustion loses samples.
pub struct AudioHandoff {
    queue: crossbeam_queue::ArrayQueue<f32>,
    overruns: std::sync::atomic::AtomicU64,
    discontinuity: std::sync::atomic::AtomicBool,
}
impl Default for AudioHandoff {
    fn default() -> Self {
        Self::new()
    }
}
impl AudioHandoff {
    pub fn new() -> Self {
        Self {
            queue: crossbeam_queue::ArrayQueue::new(MAX_RECORDING_SAMPLES),
            overruns: Default::default(),
            discontinuity: Default::default(),
        }
    }
    pub fn push(&self, samples: &[f32]) {
        let mut dropped = false;
        for &sample in samples {
            if self.queue.push(sample).is_err() {
                dropped = true;
            }
        }
        if dropped {
            self.overruns
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.discontinuity
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
    pub fn drain(&self) -> (Vec<f32>, bool) {
        let mut samples = Vec::with_capacity(self.queue.len().min(MAX_RECORDING_SAMPLES));
        // A producer can refill concurrently; bound this consumer invocation.
        for _ in 0..MAX_RECORDING_SAMPLES {
            match self.queue.pop() {
                Some(sample) => samples.push(sample),
                None => break,
            }
        }
        let interrupted = self
            .discontinuity
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        (samples, interrupted)
    }
    pub fn overruns(&self) -> u64 {
        self.overruns.load(std::sync::atomic::Ordering::Relaxed)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureDiagnostics {
    pub xruns: u64,
    pub route_changes: u64,
    pub queue_overruns: u64,
}

/// Error state shared by the realtime callback and its bounded consumer.
#[derive(Default)]
pub struct CaptureHealth {
    failed: std::sync::atomic::AtomicBool,
    xruns: std::sync::atomic::AtomicU64,
    route_changes: std::sync::atomic::AtomicU64,
    discontinuity: std::sync::atomic::AtomicBool,
}
impl CaptureHealth {
    pub fn diagnostics(&self) -> CaptureDiagnostics {
        CaptureDiagnostics {
            xruns: self.xruns.load(std::sync::atomic::Ordering::Relaxed),
            route_changes: self
                .route_changes
                .load(std::sync::atomic::Ordering::Relaxed),
            queue_overruns: 0,
        }
    }
    pub fn failed(&self) -> bool {
        self.failed.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn take_discontinuity(&self) -> bool {
        self.discontinuity
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }
    #[cfg(target_os = "macos")]
    fn fail(&self) {
        self.failed
            .store(true, std::sync::atomic::Ordering::Release);
    }
    #[cfg(target_os = "macos")]
    fn discontinuity(&self) {
        self.discontinuity
            .store(true, std::sync::atomic::Ordering::Release);
    }
    /// CPAL's Xrun and device-route notifications leave the stream alive.
    /// Preserve that stream and invalidate only the interrupted utterance.
    #[cfg(target_os = "macos")]
    pub fn record_stream_error(&self, kind: cpal::ErrorKind) {
        match kind {
            cpal::ErrorKind::Xrun => {
                self.xruns
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.discontinuity();
            }
            cpal::ErrorKind::DeviceChanged => {
                self.route_changes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.discontinuity();
            }
            cpal::ErrorKind::RealtimeDenied => {}
            _ => self.fail(),
        }
    }
}

/// Causal Hamming-windowed sinc low-pass for decimation. Coefficients and
/// doubled ring storage are allocated once, outside the realtime callback.
/// The ~1.25ms group delay is constant across input rates. Filtering happens
/// only at output instants, not at every high-rate input sample.
struct AntiAliasFilter {
    coefficients: Box<[f32]>,
    history: Box<[f32]>,
    ring_len: usize,
    next: usize,
}
impl AntiAliasFilter {
    fn new(source_rate: u32) -> Self {
        let taps = ((source_rate.div_ceil(400) as usize) | 1).min(511);
        let center = (taps - 1) as f64 / 2.0;
        let cutoff = 7_000.0 / f64::from(source_rate);
        let mut coefficients: Vec<f64> = (0..taps)
            .map(|index| {
                let offset = index as f64 - center;
                let sinc = if offset == 0.0 {
                    2.0 * cutoff
                } else {
                    (2.0 * std::f64::consts::PI * cutoff * offset).sin()
                        / (std::f64::consts::PI * offset)
                };
                let window = 0.54
                    - 0.46 * (2.0 * std::f64::consts::PI * index as f64 / (taps - 1) as f64).cos();
                sinc * window
            })
            .collect();
        let sum: f64 = coefficients.iter().sum();
        for coefficient in &mut coefficients {
            *coefficient /= sum;
        }
        let ring_len = taps + 1;
        Self {
            coefficients: coefficients
                .into_iter()
                .map(|c| c as f32)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            history: vec![0.0; ring_len * 2].into_boxed_slice(),
            ring_len,
            next: 0,
        }
    }
    fn push(&mut self, sample: f32) {
        self.history[self.next] = sample;
        self.history[self.next + self.ring_len] = sample;
        self.next += 1;
        if self.next == self.ring_len {
            self.next = 0;
        }
    }
    fn sample(&self, fraction: f32) -> f32 {
        let end = self.next + self.ring_len;
        let start = end - self.coefficients.len();
        let current = &self.history[start..end];
        if fraction == 1.0 {
            self.coefficients
                .iter()
                .zip(current)
                .map(|(h, x)| h * x)
                .sum()
        } else {
            let previous = &self.history[start - 1..end - 1];
            self.coefficients
                .iter()
                .zip(current.iter().zip(previous))
                .map(|(h, (current, previous))| h * (previous + (current - previous) * fraction))
                .sum()
        }
    }
}

pub struct CaptureBuffer {
    source_rate: u32,
    channels: u16,
    output: Vec<f32>,
    channel_sum: f64,
    channel_count: u16,
    input_frames: u64,
    previous: Option<f32>,
    antialias: Option<AntiAliasFilter>,
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
            antialias: (source_rate > OUTPUT_RATE).then(|| AntiAliasFilter::new(source_rate)),
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
            if let Some(filter) = &mut self.antialias {
                filter.push(mono);
            }
            let current_numerator = self.input_frames * u64::from(OUTPUT_RATE);
            while self.next_output_numerator <= current_numerator
                && (self.continuous || self.output.len() < MAX_RECORDING_SAMPLES)
            {
                let sample = if let Some(previous) = self.previous {
                    let previous_numerator = current_numerator - u64::from(OUTPUT_RATE);
                    let fraction = (self.next_output_numerator - previous_numerator) as f32
                        / OUTPUT_RATE as f32;
                    if let Some(filter) = &self.antialias {
                        filter.sample(fraction)
                    } else {
                        previous + (mono - previous) * fraction
                    }
                } else {
                    self.antialias
                        .as_ref()
                        .map_or(mono, |filter| filter.sample(1.0))
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
        let last = self
            .antialias
            .as_ref()
            .map_or(last, |filter| filter.sample(1.0));
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
        health: Arc<CaptureHealth>,
        handoff: Option<Arc<AudioHandoff>>,
        full: Arc<AtomicBool>,
        info: InputInfo,
        device_id: String,
    }
    impl CaptureSession {
        pub fn start() -> Result<Self, CaptureError> {
            Self::start_mode(false, None)
        }
        fn start_mode(continuous: bool, requested: Option<&str>) -> Result<Self, CaptureError> {
            let host = cpal::default_host();
            let device = if let Some(requested) = requested {
                let mut devices: Vec<_> = host
                    .input_devices()
                    .map_err(|_| CaptureError::DeviceUnavailable)?
                    .take(MAX_INPUT_DEVICES + 1)
                    .collect();
                let names = devices
                    .iter()
                    .map(|device| {
                        device
                            .description()
                            .map(|d| d.name().to_owned())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>();
                let index = select_input_index(&names, requested)?;
                devices.swap_remove(index)
            } else {
                host.default_input_device()
                    .ok_or(CaptureError::DeviceUnavailable)?
            };
            let device_id = device
                .id()
                .map_err(|_| CaptureError::DeviceUnavailable)?
                .to_string();
            let name = device
                .description()
                .map_err(|_| CaptureError::DeviceUnavailable)?
                .name()
                .to_owned();
            if name.len() > MAX_INPUT_NAME_BYTES || name.chars().any(char::is_control) {
                return Err(CaptureError::InvalidDeviceSelection);
            }
            let supported = device
                .default_input_config()
                .map_err(|_| CaptureError::StreamUnavailable)?;
            let config: cpal::StreamConfig = supported.into();
            let info = InputInfo {
                name,
                sample_rate: config.sample_rate,
                channels: config.channels,
            };
            let buffer = Arc::new(Mutex::new(if continuous {
                None
            } else {
                Some(CaptureBuffer::new(config.sample_rate, config.channels)?)
            }));
            let handoff = continuous.then(|| Arc::new(AudioHandoff::new()));
            let health = Arc::new(CaptureHealth::default());
            let full = Arc::new(AtomicBool::new(false));
            macro_rules! stream {
                ($sample:ty, $push:ident) => {{
                    let data_buffer = Arc::clone(&buffer);
                    let data_handoff = handoff.clone();
                    // The realtime callback exclusively owns its resampler.
                    let mut local_buffer = if continuous {
                        Some(CaptureBuffer::continuous(
                            config.sample_rate,
                            config.channels,
                        )?)
                    } else {
                        None
                    };
                    let data_health = Arc::clone(&health);
                    let data_full = Arc::clone(&full);
                    let error_health = Arc::clone(&health);
                    device.build_input_stream(
                        config.clone(),
                        move |data: &[$sample], _: &cpal::InputCallbackInfo| {
                            if data_health.failed() || data_full.load(Ordering::Acquire) {
                                return;
                            }
                            if let (Some(local), Some(handoff)) =
                                (local_buffer.as_mut(), data_handoff.as_ref())
                            {
                                if local.$push(data).is_err() {
                                    data_health.fail();
                                    return;
                                }
                                handoff.push(&local.output);
                                local.output.clear();
                                if std::mem::take(&mut local.overrun) {
                                    data_health.discontinuity();
                                }
                                return;
                            }
                            if let Ok(mut guard) = data_buffer.try_lock() {
                                if let Some(buffer) = guard.as_mut() {
                                    if buffer.$push(data).is_err() {
                                        data_health.fail();
                                    }
                                    if buffer.is_full() {
                                        data_full.store(true, Ordering::Release);
                                    }
                                }
                            } else if continuous {
                                data_health.discontinuity();
                            } else {
                                data_health.fail();
                            }
                        },
                        move |error| {
                            error_health.record_stream_error(error.kind());
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
                health,
                handoff,
                full,
                info,
                device_id,
            })
        }
        pub fn is_full(&self) -> bool {
            self.full.load(Ordering::Acquire)
        }
        pub fn failed(&self) -> bool {
            self.health.failed()
        }
        pub fn stop(mut self) -> Result<Vec<f32>, CaptureError> {
            // Close the owned stream before taking its bounded sample buffer.
            self.stream.take();
            if self.failed() || self.health.take_discontinuity() {
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
    /// Reads routing metadata only; this does not open a microphone stream.
    pub fn default_input_id() -> Result<String, CaptureError> {
        cpal::default_host()
            .default_input_device()
            .ok_or(CaptureError::DeviceUnavailable)?
            .id()
            .map(|id| id.to_string())
            .map_err(|_| CaptureError::DeviceUnavailable)
    }
    pub fn input_devices() -> Result<Vec<InputInfo>, CaptureError> {
        let devices = cpal::default_host()
            .input_devices()
            .map_err(|_| CaptureError::DeviceUnavailable)?;
        let mut inputs = Vec::new();
        for device in devices.take(MAX_INPUT_DEVICES) {
            let Ok(description) = device.description() else {
                continue;
            };
            let name = description.name();
            if name.is_empty()
                || name.len() > MAX_INPUT_NAME_BYTES
                || name.chars().any(char::is_control)
            {
                continue;
            }
            let Ok(config) = device.default_input_config() else {
                continue;
            };
            inputs.push(InputInfo {
                name: name.to_owned(),
                sample_rate: config.sample_rate(),
                channels: config.channels(),
            });
        }
        Ok(inputs)
    }
    /// Owns a running input stream and a bounded 15-second mono rolling buffer.
    pub struct ContinuousCapture(CaptureSession);
    impl ContinuousCapture {
        pub fn start() -> Result<Self, CaptureError> {
            Self::start_with_device_name(None)
        }
        pub fn start_with_device_name(requested: Option<&str>) -> Result<Self, CaptureError> {
            CaptureSession::start_mode(true, requested).map(Self)
        }
        pub fn info(&self) -> InputInfo {
            self.0.info.clone()
        }
        pub fn device_id(&self) -> &str {
            &self.0.device_id
        }
        pub fn failed(&self) -> bool {
            self.0.failed()
        }
        pub fn diagnostics(&self) -> CaptureDiagnostics {
            let mut diagnostics = self.0.health.diagnostics();
            diagnostics.queue_overruns = self
                .0
                .handoff
                .as_ref()
                .map_or(0, |handoff| handoff.overruns());
            diagnostics
        }
        pub fn poll(&mut self) -> Result<(Vec<f32>, bool), CaptureError> {
            if self.0.failed() {
                return Err(CaptureError::StreamFailed);
            }
            let (samples, overflow) = self
                .0
                .handoff
                .as_ref()
                .ok_or(CaptureError::StreamFailed)?
                .drain();
            let discontinuity = self.0.health.take_discontinuity();
            Ok((samples, overflow || discontinuity))
        }
    }
}
#[cfg(target_os = "macos")]
pub use native::{CaptureSession, ContinuousCapture, default_input_id, input_devices};

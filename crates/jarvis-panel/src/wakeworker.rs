//! Continuous local wake recognition; the native audio handle stays on one thread.
use crate::wake::{DecodeRequest, Phase, WakeDetector, wake_tail};
use jarvis_voice::capture::InputInfo;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Loading,
    Muted,
    Starting,
    WakeListening,
    CommandListening,
    Decoding,
    CommandReady,
    Suppressed,
    Failed,
    Stopped,
}
/// In-memory levels and lifecycle counters; never contains audio or transcript text.
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub input: Option<InputInfo>,
    pub captured_batches: u64,
    pub captured_samples: u64,
    pub last_rms: f32,
    pub last_peak: f32,
    pub max_rms: f32,
    pub decode_count: u64,
    pub last_decode_ms: u64,
    pub wake_matches: u64,
    pub commands: u64,
    pub discontinuities: u64,
    pub capture_errors: u64,
    pub decode_errors: u64,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: State,
    pub message: &'static str,
    pub revision: u64,
    pub diagnostics: Diagnostics,
}
impl Snapshot {
    /// Numeric opt-in diagnostics. No device names, messages, audio or transcripts.
    pub fn diagnostic_line(&self) -> String {
        let d = &self.diagnostics;
        format!(
            "jarvis_voice state={:?} samples={} rms={:.5} peak={:.5} max_rms={:.5} decodes={} decode_ms={} wakes={} commands={} discontinuities={} capture_errors={} decode_errors={}",
            self.state,
            d.captured_samples,
            d.last_rms,
            d.last_peak,
            d.max_rms,
            d.decode_count,
            d.last_decode_ms,
            d.wake_matches,
            d.commands,
            d.discontinuities,
            d.capture_errors,
            d.decode_errors,
        )
    }
}
pub trait WakeBackend {
    fn input_info(&self) -> Option<InputInfo> {
        None
    }
    fn start(&mut self) -> Result<(), &'static str>;
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str>;
    fn stop(&mut self);
    fn transcribe(
        &mut self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, &'static str>;
}
pub struct Control {
    enabled: AtomicBool,
    suppressed: AtomicBool,
    stopped: AtomicBool,
    generation: AtomicU64,
    cancelled: Arc<AtomicBool>,
    snapshot: Mutex<Snapshot>,
    command: Mutex<Option<(u64, String)>>,
}
impl Control {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    pub fn set_enabled(&self, value: bool) {
        if self.enabled.swap(value, Ordering::AcqRel) != value {
            self.invalidate();
        }
    }
    pub fn suppress(&self, value: bool) {
        if self.suppressed.swap(value, Ordering::AcqRel) != value {
            self.invalidate();
        }
    }
    fn invalidate(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.command
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
    }
    pub fn take_command(&self) -> Option<String> {
        self.command
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .filter(|(generation, _)| {
                *generation == self.generation.load(Ordering::Acquire)
                    && self.enabled()
                    && !self.suppressed.load(Ordering::Acquire)
                    && !self.stopped.load(Ordering::Acquire)
            })
            .map(|(_, command)| command)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        self.invalidate();
    }
    fn input(&self, input: Option<InputInfo>) {
        let input = input.filter(|info| {
            !info.name.is_empty()
                && info.name.len() <= jarvis_voice::capture::MAX_INPUT_NAME_BYTES
                && !info.name.chars().any(char::is_control)
        });
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .diagnostics
            .input = input;
    }
    fn audio(&self, samples: &[f32], overflow: bool) {
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        let d = &mut snapshot.diagnostics;
        if overflow {
            d.discontinuities = d.discontinuities.saturating_add(1);
        }
        if samples.is_empty() || samples.len() > jarvis_voice::capture::MAX_RECORDING_SAMPLES {
            return;
        }
        d.captured_batches = d.captured_batches.saturating_add(1);
        d.captured_samples = d.captured_samples.saturating_add(samples.len() as u64);
        let mut sum = 0.0_f64;
        let mut peak = 0.0_f32;
        for &sample in samples {
            if sample.is_finite() {
                let magnitude = sample.abs().min(1.0);
                sum += f64::from(magnitude).powi(2);
                peak = peak.max(magnitude);
            }
        }
        d.last_rms = (sum / samples.len() as f64).sqrt() as f32;
        d.last_peak = peak;
        d.max_rms = d.max_rms.max(d.last_rms);
    }
    fn capture_error(&self) {
        let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        s.diagnostics.capture_errors = s.diagnostics.capture_errors.saturating_add(1);
    }
    fn decode_started(&self) {
        let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        s.diagnostics.decode_count = s.diagnostics.decode_count.saturating_add(1);
    }
    fn decode_finished(&self, elapsed: Duration, failed: bool) {
        let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        s.diagnostics.last_decode_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
        if failed {
            s.diagnostics.decode_errors = s.diagnostics.decode_errors.saturating_add(1);
        }
    }
    fn wake_matched(&self) {
        let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        s.diagnostics.wake_matches = s.diagnostics.wake_matches.saturating_add(1);
    }
    fn command_ready(&self) {
        let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        s.diagnostics.commands = s.diagnostics.commands.saturating_add(1);
    }
    fn publish(&self, state: State, message: &'static str) {
        let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        if s.state != state {
            s.state = state;
            s.message = message;
            s.revision = s.revision.wrapping_add(1);
        }
    }
}
pub struct WakeWorker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl WakeWorker {
    #[cfg(target_os = "macos")]
    pub fn from_env() -> Result<Self, &'static str> {
        Self::with_backend(|| {
            let settings = jarvis::conversation::load_provider_settings()
                .map_err(|_| "Set JARVIS_WHISPER_MODEL_PATH")?;
            let path = std::path::PathBuf::from(
                settings
                    .get("JARVIS_WHISPER_MODEL_PATH")
                    .ok_or("Set JARVIS_WHISPER_MODEL_PATH")?,
            );
            if !path.is_absolute() {
                return Err("Choose an absolute Whisper model path");
            }
            let transcriber = jarvis_voice::Transcriber::load(&path)
                .map_err(|_| "Check the local Whisper model")?;
            Ok(Box::new(NativeBackend {
                transcriber,
                capture: None,
                device_name: settings.get("JARVIS_MICROPHONE_NAME").cloned(),
            }))
        })
    }
    pub fn with_backend(
        factory: impl FnOnce() -> Result<Box<dyn WakeBackend>, &'static str> + Send + 'static,
    ) -> Result<Self, &'static str> {
        let control = Arc::new(Control {
            enabled: AtomicBool::new(false),
            suppressed: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            cancelled: Arc::new(AtomicBool::new(false)),
            snapshot: Mutex::new(Snapshot {
                state: State::Loading,
                message: "Loading local wake recognition",
                revision: 0,
                diagnostics: Diagnostics::default(),
            }),
            command: Mutex::new(None),
        });
        let c = control.clone();
        let handle = thread::Builder::new()
            .name("jarvis-local-wake".into())
            .spawn(move || {
                let mut backend = match factory() {
                    Ok(b) => b,
                    Err(message) => {
                        c.publish(State::Failed, message);
                        return;
                    }
                };
                let mut detector = WakeDetector::new();
                let mut next_decode: Option<DecodeRequest> = None;
                let mut running = false;
                let mut seen_generation = c.generation.load(Ordering::Acquire);
                loop {
                    if c.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let generation = c.generation.load(Ordering::Acquire);
                    if generation != seen_generation {
                        backend.stop();
                        running = false;
                        detector.reset();
                        next_decode = None;
                        seen_generation = generation;
                    }
                    let suppressed = c.suppressed.load(Ordering::Acquire);
                    if !c.enabled() || suppressed {
                        if running {
                            backend.stop();
                            running = false;
                        }
                        detector.reset();
                        next_decode = None;
                        c.publish(
                            if suppressed {
                                State::Suppressed
                            } else {
                                State::Muted
                            },
                            if suppressed {
                                "Voice paused during reply"
                            } else {
                                "Microphone muted"
                            },
                        );
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    if !running {
                        c.publish(State::Starting, "Starting microphone");
                        if let Err(message) = backend.start() {
                            c.capture_error();
                            c.enabled.store(false, Ordering::Release);
                            c.publish(State::Failed, message);
                            // Keep permission errors visible until explicit retry.
                            while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                thread::sleep(Duration::from_millis(20));
                            }
                            continue;
                        }
                        running = true;
                        c.input(backend.input_info());
                        c.cancelled.store(false, Ordering::Release);
                    }
                    if c.command
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_some()
                    {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    c.publish(
                        if detector.phase() == Phase::Wake {
                            State::WakeListening
                        } else {
                            State::CommandListening
                        },
                        if detector.phase() == Phase::Wake {
                            "Wake listening · say Jarvis"
                        } else {
                            "Listening to your request…"
                        },
                    );
                    let request = if let Some(request) = next_decode.take() {
                        Some(request)
                    } else {
                        let (samples, overflow) = match backend.poll() {
                            Ok(v) => v,
                            Err(message) => {
                                backend.stop();
                                running = false;
                                detector.reset();
                                c.capture_error();
                                c.enabled.store(false, Ordering::Release);
                                c.publish(State::Failed, message);
                                while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                    thread::sleep(Duration::from_millis(20));
                                }
                                continue;
                            }
                        };
                        c.audio(&samples, overflow);
                        if overflow || samples.len() > jarvis_voice::capture::MAX_RECORDING_SAMPLES
                        {
                            detector.reset();
                            continue;
                        }
                        detector.ingest(&samples)
                    };
                    if let Some(request) = request {
                        c.publish(
                            State::Decoding,
                            if request.phase == Phase::Wake {
                                "Checking wake phrase locally"
                            } else {
                                "Understanding your request locally"
                            },
                        );
                        c.decode_started();
                        let decode_started = Instant::now();
                        let result = backend.transcribe(&request.samples, c.cancelled.clone());
                        c.decode_finished(decode_started.elapsed(), result.is_err());
                        if generation != c.generation.load(Ordering::Acquire)
                            || c.stopped.load(Ordering::Acquire)
                        {
                            detector.reset();
                            continue;
                        }
                        // Capture continues during synchronous decoding. An endpoint
                        // is provisional until queued audio has been checked for a
                        // resumed phrase; no command escapes before this read.
                        let queued = backend.poll();
                        if generation != c.generation.load(Ordering::Acquire)
                            || c.stopped.load(Ordering::Acquire)
                        {
                            detector.reset();
                            continue;
                        }
                        let (queued, overflow) = match queued {
                            Ok(audio) => audio,
                            Err(message) => {
                                backend.stop();
                                running = false;
                                detector.reset();
                                c.capture_error();
                                c.enabled.store(false, Ordering::Release);
                                c.publish(State::Failed, message);
                                while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                    thread::sleep(Duration::from_millis(20));
                                }
                                continue;
                            }
                        };
                        c.audio(&queued, overflow);
                        if overflow || queued.len() > jarvis_voice::capture::MAX_RECORDING_SAMPLES {
                            detector.reset();
                            continue;
                        }
                        match result {
                            Ok(text) => {
                                if request.phase == Phase::Wake && wake_tail(&text).is_some() {
                                    c.wake_matched();
                                }
                                let outcome = detector.accept_transcript_with_audio(&text, &queued);
                                next_decode = outcome.next;
                                if let Some(command) = outcome.command {
                                    let mut slot =
                                        c.command.lock().unwrap_or_else(|p| p.into_inner());
                                    if generation == c.generation.load(Ordering::Acquire)
                                        && c.enabled()
                                        && !c.suppressed.load(Ordering::Acquire)
                                        && !c.stopped.load(Ordering::Acquire)
                                    {
                                        *slot = Some((generation, command));
                                        c.command_ready();
                                        c.publish(State::CommandReady, "Request heard");
                                    }
                                }
                            }
                            Err(_) => {
                                detector.reset();
                                next_decode = detector.ingest(&queued);
                            }
                        }
                    }
                    if next_decode.is_some() {
                        continue;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                backend.stop();
                c.publish(State::Stopped, "Microphone off");
            })
            .map_err(|_| "Could not start wake worker")?;
        Ok(Self {
            control,
            handle: Some(handle),
        })
    }
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }
    pub fn join(&mut self) -> Result<(), &'static str> {
        if let Some(h) = self.handle.take() {
            h.join().map_err(|_| "Wake worker stopped unexpectedly")?;
        }
        Ok(())
    }
}
impl Drop for WakeWorker {
    fn drop(&mut self) {
        self.control.shutdown();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
#[cfg(target_os = "macos")]
struct NativeBackend {
    transcriber: jarvis_voice::Transcriber,
    capture: Option<jarvis_voice::capture::ContinuousCapture>,
    device_name: Option<String>,
}
#[cfg(target_os = "macos")]
impl WakeBackend for NativeBackend {
    fn input_info(&self) -> Option<InputInfo> {
        self.capture.as_ref().map(|capture| capture.info())
    }
    fn start(&mut self) -> Result<(), &'static str> {
        self.capture = Some(
            jarvis_voice::capture::ContinuousCapture::start_with_device_name(
                self.device_name.as_deref(),
            )
            .map_err(|error| match error {
                jarvis_voice::capture::CaptureError::DeviceUnavailable
                    if self.device_name.is_some() =>
                {
                    "Configured microphone unavailable; check JARVIS_MICROPHONE_NAME"
                }
                jarvis_voice::capture::CaptureError::InvalidDeviceSelection => {
                    "Choose one exact available microphone name"
                }
                jarvis_voice::capture::CaptureError::InvalidFormat => {
                    "Microphone format is unsupported"
                }
                _ => "Allow microphone access in macOS Privacy & Security",
            })?,
        );
        Ok(())
    }
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        self.capture
            .as_mut()
            .ok_or("Microphone unavailable")?
            .poll()
            .map_err(|_| "Check microphone settings")
    }
    fn stop(&mut self) {
        self.capture.take();
    }
    fn transcribe(
        &mut self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.transcriber
            .transcribe_cancelled(samples, cancelled)
            .map_err(|_| "Try saying Jarvis again")
    }
}

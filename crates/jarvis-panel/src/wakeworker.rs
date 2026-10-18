//! Continuous local wake recognition; the native audio handle stays on one thread.
use crate::wake::{DecodeRequest, Phase, WakeDetector, wake_tail};
use jarvis_voice::capture::{CaptureDiagnostics, InputInfo};
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
    pub capture: CaptureDiagnostics,
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
            "jarvis_voice state={:?} samples={} rms={:.3e} peak={:.3e} max_rms={:.3e} decodes={} decode_ms={} wakes={} commands={} discontinuities={} capture_errors={} decode_errors={} xruns={} route_changes={} queue_overruns={} input_rate={} input_channels={}",
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
            d.capture.xruns,
            d.capture.route_changes,
            d.capture.queue_overruns,
            d.input.as_ref().map_or(0, |input| input.sample_rate),
            d.input.as_ref().map_or(0, |input| input.channels),
        )
    }
}
/// Dedicated spotting result. None from the backend selects the legacy fixture path;
/// Some with no detection means audio was processed locally without transcription.
#[derive(Clone, Copy, Debug)]
pub struct KeywordStatus {
    pub lookback_samples: Option<usize>,
}
pub trait WakeBackend {
    /// A native fallback is permitted only with explicit speech classifications.
    fn wake_fallback_enabled(&self) -> bool {
        false
    }
    fn speech_activity(&mut self, _samples: &[f32]) -> Result<Option<Vec<bool>>, &'static str> {
        Ok(None)
    }
    fn capture_diagnostics(&self) -> CaptureDiagnostics {
        CaptureDiagnostics::default()
    }
    fn keyword_spot(&mut self, _samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
        Ok(None)
    }
    fn reset_keyword(&mut self) {}
    /// Clear speech-activity state, e.g. after it idled during wake listening.
    fn reset_speech_activity(&mut self) {}

    fn input_info(&self) -> Option<InputInfo> {
        None
    }
    fn listening_message(&self) -> Option<&'static str> {
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
    fn transcribe_command(
        &mut self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.transcribe(samples, cancelled)
    }
    /// A keyword-armed command: words starting before `skip_before` samples
    /// are the wake phrase. Backends without word timing ignore it.
    fn transcribe_command_after(
        &mut self,
        samples: &[f32],
        skip_before: Option<usize>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        let _ = skip_before;
        self.transcribe_command(samples, cancelled)
    }
}
fn append_keyword_audio(buffer: &mut Vec<f32>, samples: &[f32]) {
    let keep = samples.len().min(64_000);
    let excess = (buffer.len() + keep).saturating_sub(64_000);
    buffer.drain(..excess.min(buffer.len()));
    buffer.extend_from_slice(&samples[samples.len() - keep..]);
}

pub struct Control {
    enabled: AtomicBool,
    suppressed: AtomicBool,
    playback_active: AtomicBool,
    stopped: AtomicBool,
    generation: AtomicU64,
    cancelled: Arc<AtomicBool>,
    snapshot: Mutex<Snapshot>,
    command: Mutex<Option<(u64, String)>>,
    followup: Mutex<Option<(u64, Instant)>>,
}
impl Control {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    pub fn arm_followup(&self) -> bool {
        self.arm_followup_for(Duration::from_secs(8))
    }
    /// Trusted UI/test seam. A question grants one bounded answer, not persistent listening.
    pub fn arm_followup_for(&self, duration: Duration) -> bool {
        if duration.is_zero()
            || duration > Duration::from_secs(8)
            || !self.enabled()
            || self.suppressed.load(Ordering::Acquire)
            || self.stopped.load(Ordering::Acquire)
        {
            return false;
        }
        self.invalidate();
        let generation = self.generation();
        *self.followup.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((generation, Instant::now() + duration));
        true
    }
    pub fn followup_active(&self) -> bool {
        self.followup_deadline(self.generation())
            .is_some_and(|deadline| Instant::now() < deadline)
    }
    pub fn cancel_followup(&self) {
        // Also discard an answer that finished decoding just before cancellation.
        self.invalidate();
    }
    fn clear_followup(&self) -> bool {
        self.followup
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .is_some()
    }
    fn followup_deadline(&self, generation: u64) -> Option<Instant> {
        self.followup
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .filter(|(epoch, _)| *epoch == generation)
            .map(|(_, deadline)| deadline)
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
    /// Audible output keeps dedicated keyword detection active. Beginning a
    /// reply clears old audio; ending it preserves a newly detected interruption.
    pub fn set_playback_active(&self, value: bool) {
        if !self.playback_active.swap(value, Ordering::AcqRel) && value {
            self.invalidate();
        }
    }
    fn invalidate(&self) {
        self.clear_followup();
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
    fn input(&self, input: Option<InputInfo>, listening_message: Option<&'static str>) {
        let input = input.filter(|info| {
            !info.name.is_empty()
                && info.name.len() <= jarvis_voice::capture::MAX_INPUT_NAME_BYTES
                && !info.name.chars().any(char::is_control)
        });
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        let message = listening_message.unwrap_or("Wake listening · say Jarvis");
        let changed =
            snapshot.diagnostics.input != input || (input.is_some() && snapshot.message != message);
        // Publish the selected input and its fixed recovery reason atomically.
        // A handoff must never see a new microphone with the previous route's copy.
        if input.is_some() {
            snapshot.message = message;
        }
        snapshot.diagnostics.input = input;
        if changed {
            snapshot.revision = snapshot.revision.wrapping_add(1);
        }
    }
    fn capture_diagnostics(&self, diagnostics: CaptureDiagnostics) {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .diagnostics
            .capture = diagnostics;
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
        if s.state != state || s.message != message {
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
                .map_err(|_| "Check the local voice configuration")?;
            let helper = settings
                .get("JARVIS_APPLE_STT_PATH")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::current_exe().ok().and_then(|path| {
                        path.parent().map(|parent| parent.join("jarvis-apple-stt"))
                    })
                })
                .ok_or("Install the Apple speech helper")?;
            let transcriber =
                jarvis_voice::apple::AppleTranscriber::new(helper, Duration::from_secs(20))
                    .map_err(|_| "Install the Apple speech helper")?;
            let keyword_path = std::path::PathBuf::from(
                settings
                    .get("JARVIS_OPENWAKEWORD_MODEL_DIR")
                    .ok_or("Set JARVIS_OPENWAKEWORD_MODEL_DIR to the local wake models")?,
            );
            let keyword = jarvis_voice::openwakeword::LocalWakeSpotter::load(&keyword_path)?;
            let vad_path = std::path::PathBuf::from(
                settings
                    .get("JARVIS_VAD_MODEL_PATH")
                    .ok_or("Set JARVIS_VAD_MODEL_PATH to the local speech detector")?,
            );
            let vad = jarvis_voice::vad::LocalVoiceActivity::load(&vad_path)?;
            Ok(Box::new(NativeBackend {
                transcriber,
                keyword,
                vad,
                capture: None,
                input_follower: None,
                route_clock: Instant::now(),
                followed_routes: 0,
                silent_stream: jarvis_voice::capture::SilentStreamWatchdog::default(),
                reconnect: jarvis_voice::capture::ReconnectBackoff::default(),
                healthy_since: None,
                progress: jarvis_voice::capture::CaptureProgress::default(),
                fallback_name: None,
                fallback_default: None,
                last_input_name: None,
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
            playback_active: AtomicBool::new(false),
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
            followup: Mutex::new(None),
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
                let mut keyword_audio = Vec::<f32>::with_capacity(64_000);
                let mut seen_generation = c.generation.load(Ordering::Acquire);
                let mut followup_armed = false;
                let mut discard_audio = false;
                let mut barge_wait = false;
                let mut echo_deadline: Option<Instant> = None;
                loop {
                    if c.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let generation = c.generation.load(Ordering::Acquire);
                    if generation != seen_generation {
                        // Logical cancellation must not renegotiate the hardware route.
                        // Drain the old generation before recognition resumes.
                        discard_audio = true;
                        backend.reset_keyword();
                        keyword_audio.clear();
                        detector.reset();
                        next_decode = None;
                        seen_generation = generation;
                        followup_armed = false;
                        barge_wait = false;
                        echo_deadline = None;
                    }
                    let suppressed = c.suppressed.load(Ordering::Acquire);
                    if !c.enabled() {
                        if running {
                            backend.stop();
                            running = false;
                        }
                        detector.reset();
                        next_decode = None;
                        c.publish(State::Muted, "Microphone muted");
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
                        let previous_input = c.snapshot().diagnostics.input;
                        let input = backend.input_info();
                        if previous_input.is_some() && previous_input != input {
                            c.clear_followup();
                        }
                        c.input(input, backend.listening_message());
                        c.cancelled.store(false, Ordering::Release);
                    }
                    if suppressed || discard_audio {
                        // Keep the enabled stream warm while thinking/speaking, but
                        // feed none of its audio to KWS, VAD, ASR, or command state.
                        // The first drain after unsuppression also discards TTS tail.
                        match backend.poll() {
                            Ok((samples, overflow)) => {
                                c.capture_diagnostics(backend.capture_diagnostics());
                                c.audio(&samples, overflow);
                                if overflow {
                                    c.clear_followup();
                                    followup_armed = false;
                                    backend.reset_keyword();
                                    c.input(backend.input_info(), backend.listening_message());
                                }
                            }
                            Err(message) => {
                                backend.stop();
                                running = false;
                                c.capture_error();
                                c.enabled.store(false, Ordering::Release);
                                c.publish(State::Failed, message);
                                while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                    thread::sleep(Duration::from_millis(20));
                                }
                                continue;
                            }
                        }
                        detector.reset();
                        next_decode = None;
                        keyword_audio.clear();
                        discard_audio = false;
                        if generation == c.generation.load(Ordering::Acquire) {
                            c.cancelled.store(false, Ordering::Release);
                        }
                        if suppressed {
                            c.publish(State::Suppressed, "Voice paused during reply");
                        }
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    let followup = c.followup_deadline(generation);
                    if followup.is_some_and(|deadline| Instant::now() >= deadline)
                        || (followup_armed && followup.is_none())
                    {
                        c.clear_followup();
                        detector.reset();
                        next_decode = None;
                        backend.reset_keyword();
                        keyword_audio.clear();
                        followup_armed = false;
                    } else if followup.is_some() && !followup_armed {
                        detector.arm_followup();
                        next_decode = None;
                        backend.reset_keyword();
                        keyword_audio.clear();
                        followup_armed = true;
                    }
                    let command_pending = c
                        .command
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_some();
                    if command_pending {
                        // One command may wait for the conversation worker. Keep
                        // hardware healthy without building an ambient audio backlog.
                        match backend.poll() {
                            Ok((samples, overflow)) => {
                                c.capture_diagnostics(backend.capture_diagnostics());
                                c.audio(&samples, overflow);
                                if overflow {
                                    c.invalidate();
                                    c.input(backend.input_info(), backend.listening_message());
                                }
                            }
                            Err(message) => {
                                backend.stop();
                                running = false;
                                c.capture_error();
                                c.enabled.store(false, Ordering::Release);
                                c.invalidate();
                                c.publish(State::Failed, message);
                                while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                    thread::sleep(Duration::from_millis(20));
                                }
                            }
                        }
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    if barge_wait {
                        // Never retain TTS-era capture. Continue draining until the
                        // owned player is reaped, then quarantine its acoustic tail.
                        match backend.poll() {
                            Ok((samples, overflow)) => {
                                c.capture_diagnostics(backend.capture_diagnostics());
                                c.audio(&samples, overflow);
                                if overflow {
                                    c.invalidate();
                                    c.input(backend.input_info(), backend.listening_message());
                                    continue;
                                }
                            }
                            Err(message) => {
                                backend.stop();
                                running = false;
                                barge_wait = false;
                                echo_deadline = None;
                                c.capture_error();
                                c.enabled.store(false, Ordering::Release);
                                c.publish(State::Failed, message);
                                while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                    thread::sleep(Duration::from_millis(20));
                                }
                                continue;
                            }
                        }
                        if c.playback_active.load(Ordering::Acquire) {
                            echo_deadline = None;
                        } else {
                            let deadline = echo_deadline
                                .get_or_insert_with(|| Instant::now() + Duration::from_millis(250));
                            if Instant::now() >= *deadline {
                                detector.reset();
                                detector.arm_followup();
                                backend.reset_keyword();
                                keyword_audio.clear();
                                barge_wait = false;
                                echo_deadline = None;
                                c.publish(State::CommandListening, "Ready for your request…");
                                continue;
                            }
                        }
                        c.publish(State::Suppressed, "Stopping spoken reply…");
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
                            backend
                                .listening_message()
                                .unwrap_or("Wake listening · say Jarvis")
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
                        c.capture_diagnostics(backend.capture_diagnostics());
                        c.audio(&samples, overflow);
                        if overflow || samples.len() > jarvis_voice::capture::MAX_RECORDING_SAMPLES
                        {
                            c.clear_followup();
                            followup_armed = false;
                            detector.reset();
                            next_decode = None;
                            backend.reset_keyword();
                            keyword_audio.clear();
                            c.input(backend.input_info(), backend.listening_message());
                            continue;
                        }
                        if c.playback_active.load(Ordering::Acquire) {
                            // During TTS only the dedicated spotter sees audio.
                            // Never run ambient ASR or carry reply-era pre-roll
                            // into the interrupted user's command.
                            match backend.keyword_spot(&samples) {
                                Ok(Some(KeywordStatus {
                                    lookback_samples: Some(_),
                                })) => {
                                    detector.reset();
                                    next_decode = None;
                                    keyword_audio.clear();
                                    barge_wait = true;
                                    echo_deadline = None;
                                    c.wake_matched();
                                    c.publish(State::Suppressed, "Stopping spoken reply…");
                                }
                                Ok(_) => {}
                                Err(message) => {
                                    backend.stop();
                                    running = false;
                                    c.enabled.store(false, Ordering::Release);
                                    c.publish(State::Failed, message);
                                }
                            }
                            thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        // While waiting for the wake phrase only the keyword model
                        // runs; speech detection starts once a command is being heard.
                        let keyword = if detector.phase() == Phase::Wake {
                            match backend.keyword_spot(&samples) {
                                Ok(status) => status,
                                Err(message) => {
                                    backend.stop();
                                    running = false;
                                    detector.reset();
                                    keyword_audio.clear();
                                    c.enabled.store(false, Ordering::Release);
                                    c.publish(State::Failed, message);
                                    while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                        thread::sleep(Duration::from_millis(20));
                                    }
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        let needs_activity = detector.phase() != Phase::Wake
                            || keyword.is_none()
                            || backend.wake_fallback_enabled();
                        let activity = if needs_activity {
                            match backend.speech_activity(&samples) {
                                Ok(activity) => activity,
                                Err(message) => {
                                    backend.stop();
                                    running = false;
                                    detector.reset();
                                    c.enabled.store(false, Ordering::Release);
                                    c.publish(State::Failed, message);
                                    while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                        thread::sleep(Duration::from_millis(20));
                                    }
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        if detector.phase() == Phase::Wake {
                            match keyword {
                                Some(status) => {
                                    // Fixed-size prerecording recovers words spoken immediately
                                    // after the keyword while the streaming model confirms it.
                                    append_keyword_audio(&mut keyword_audio, &samples);
                                    if let Some(lookback) = status.lookback_samples {
                                        let start = keyword_audio
                                            .len()
                                            .saturating_sub(lookback.min(64_000));
                                        detector.arm_keyword(&keyword_audio[start..]);
                                        keyword_audio.clear();
                                        // Speech detection idled during wake listening;
                                        // start it fresh for the command.
                                        backend.reset_speech_activity();
                                        c.wake_matched();
                                        None
                                    } else if backend.wake_fallback_enabled() && activity.is_some()
                                    {
                                        detector.ingest_with_activity(&samples, activity.as_deref())
                                    } else {
                                        None
                                    }
                                }
                                None => detector.ingest_with_activity(&samples, activity.as_deref()),
                            }
                        } else {
                            detector.ingest_with_activity(&samples, activity.as_deref())
                        }
                    };
                    if let Some(request) = request {
                        // Background speech checks do not open the interaction
                        // card or animate a command spinner before a wake match.
                        if request.phase != Phase::Wake || !backend.wake_fallback_enabled() {
                            c.publish(
                                State::Decoding,
                                if request.phase == Phase::Wake {
                                    "Checking wake phrase locally"
                                } else {
                                    "Understanding your request locally"
                                },
                            );
                        }
                        c.decode_started();
                        let decode_started = Instant::now();
                        let result = if request.phase == Phase::Wake {
                            backend.transcribe(&request.samples, c.cancelled.clone())
                        } else {
                            backend.transcribe_command_after(
                                &request.samples,
                                request.skip_before,
                                c.cancelled.clone(),
                            )
                        };
                        c.decode_finished(decode_started.elapsed(), result.is_err());
                        if followup_armed
                            && c.followup_deadline(generation)
                                .is_none_or(|deadline| Instant::now() >= deadline)
                        {
                            c.clear_followup();
                            followup_armed = false;
                            detector.reset();
                            next_decode = None;
                            backend.reset_keyword();
                            keyword_audio.clear();
                            // Discard expired answer audio without reopening the device.
                            discard_audio = true;
                            continue;
                        }
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
                        c.capture_diagnostics(backend.capture_diagnostics());
                        c.audio(&queued, overflow);
                        if overflow || queued.len() > jarvis_voice::capture::MAX_RECORDING_SAMPLES {
                            c.clear_followup();
                            followup_armed = false;
                            detector.reset();
                            next_decode = None;
                            backend.reset_keyword();
                            keyword_audio.clear();
                            c.input(backend.input_info(), backend.listening_message());
                            continue;
                        }
                        let activity = match backend.speech_activity(&queued) {
                            Ok(activity) => activity,
                            Err(message) => {
                                backend.stop();
                                running = false;
                                detector.reset();
                                c.enabled.store(false, Ordering::Release);
                                c.publish(State::Failed, message);
                                while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                    thread::sleep(Duration::from_millis(20));
                                }
                                continue;
                            }
                        };
                        // Keep primary spotting continuous through synchronous
                        // fallback inference. A fresh dedicated hit takes priority
                        // over the older provisional transcription.
                        if request.phase == Phase::Wake && backend.wake_fallback_enabled() {
                            match backend.keyword_spot(&queued) {
                                Ok(Some(status)) => {
                                    append_keyword_audio(&mut keyword_audio, &queued);
                                    if let Some(lookback) = status.lookback_samples {
                                        let start = keyword_audio
                                            .len()
                                            .saturating_sub(lookback.min(64_000));
                                        detector.arm_keyword(&keyword_audio[start..]);
                                        keyword_audio.clear();
                                        c.wake_matched();
                                        continue;
                                    }
                                }
                                Ok(None) => {}
                                Err(message) => {
                                    backend.stop();
                                    running = false;
                                    detector.reset();
                                    keyword_audio.clear();
                                    c.enabled.store(false, Ordering::Release);
                                    c.publish(State::Failed, message);
                                    while !c.enabled() && !c.stopped.load(Ordering::Acquire) {
                                        thread::sleep(Duration::from_millis(20));
                                    }
                                    continue;
                                }
                            }
                        }
                        match result {
                            Ok(text) => {
                                if request.phase == Phase::Wake && wake_tail(&text).is_some() {
                                    c.wake_matched();
                                }
                                let outcome = detector.accept_transcript_with_audio_and_activity(
                                    &text,
                                    &queued,
                                    activity.as_deref(),
                                );
                                next_decode = outcome.next;
                                if detector.phase() == Phase::Wake
                                    && (request.phase == Phase::Command
                                        || outcome.command.is_some())
                                {
                                    backend.reset_keyword();
                                    keyword_audio.clear();
                                }
                                if let Some(command) = outcome.command {
                                    let within_followup = !followup_armed
                                        || c.followup_deadline(generation)
                                            .is_some_and(|deadline| Instant::now() < deadline);
                                    c.clear_followup();
                                    followup_armed = false;
                                    let mut slot =
                                        c.command.lock().unwrap_or_else(|p| p.into_inner());
                                    if within_followup
                                        && generation == c.generation.load(Ordering::Acquire)
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
                                c.clear_followup();
                                followup_armed = false;
                                detector.reset();
                                next_decode =
                                    detector.ingest_with_activity(&queued, activity.as_deref());
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
/// Compare physical microphones independently of Bluetooth format renegotiation.
/// Active capture still uses the full route token; only an automatic fallback
/// hold uses this narrower identity.
pub fn same_input_device(left: &str, right: &str) -> bool {
    match (
        jarvis_voice::capture::input_route_device_id(left),
        jarvis_voice::capture::input_route_device_id(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(target_os = "macos")]
struct NativeBackend {
    transcriber: jarvis_voice::apple::AppleTranscriber,
    keyword: jarvis_voice::openwakeword::LocalWakeSpotter,
    vad: jarvis_voice::vad::LocalVoiceActivity,
    capture: Option<jarvis_voice::capture::ContinuousCapture>,
    input_follower: Option<jarvis_voice::capture::DefaultInputFollower>,
    route_clock: Instant,
    followed_routes: u64,
    silent_stream: jarvis_voice::capture::SilentStreamWatchdog,
    reconnect: jarvis_voice::capture::ReconnectBackoff,
    healthy_since: Option<Instant>,
    progress: jarvis_voice::capture::CaptureProgress,
    fallback_name: Option<String>,
    fallback_default: Option<String>,
    last_input_name: Option<String>,
    device_name: Option<String>,
}
#[cfg(target_os = "macos")]
impl NativeBackend {
    fn try_builtin_fallback(&mut self) {
        if self.device_name.is_none()
            && self.fallback_name.is_none()
            && self
                .last_input_name
                .as_ref()
                .is_some_and(|name| name.to_ascii_lowercase().contains("airpods"))
            && let Ok(name) = jarvis_voice::capture::builtin_input_name()
        {
            self.fallback_default = jarvis_voice::capture::default_input_id().ok().or_else(|| {
                self.input_follower
                    .as_ref()
                    .map(|follower| follower.selected().to_owned())
            });
            self.fallback_name = Some(name);
        }
    }
    fn reconnect_capture(
        &mut self,
        allow_fallback: bool,
    ) -> Result<(Vec<f32>, bool), &'static str> {
        if allow_fallback {
            self.try_builtin_fallback();
        }
        self.capture.take();
        self.reset_keyword();
        self.silent_stream.reset();
        let now = self.route_clock.elapsed();
        let allowed = if self.healthy_since.take().is_some() {
            self.reconnect.failed(now)
        } else {
            self.reconnect.begin(now)
        };
        if !allowed {
            return Err("Microphone disconnected. Reconnect it, then tap Retry microphone.");
        }
        self.followed_routes = self.followed_routes.saturating_add(1);
        Ok((Vec::new(), true))
    }
}
#[cfg(target_os = "macos")]
impl WakeBackend for NativeBackend {
    fn wake_fallback_enabled(&self) -> bool {
        false
    }
    fn speech_activity(&mut self, samples: &[f32]) -> Result<Option<Vec<bool>>, &'static str> {
        self.vad.classify(samples).map(Some)
    }
    fn capture_diagnostics(&self) -> CaptureDiagnostics {
        let mut diagnostics = self
            .capture
            .as_ref()
            .map_or_else(CaptureDiagnostics::default, |capture| capture.diagnostics());
        diagnostics.route_changes = diagnostics
            .route_changes
            .saturating_add(self.followed_routes);
        diagnostics
    }
    fn keyword_spot(&mut self, samples: &[f32]) -> Result<Option<KeywordStatus>, &'static str> {
        self.keyword.accept(samples).map(|result| {
            Some(KeywordStatus {
                lookback_samples: result.map(|detection| detection.lookback_samples),
            })
        })
    }
    fn reset_keyword(&mut self) {
        self.keyword.reset();
        self.vad.reset();
    }
    fn reset_speech_activity(&mut self) {
        self.vad.reset();
    }
    fn input_info(&self) -> Option<InputInfo> {
        self.capture.as_ref().map(|capture| capture.info())
    }
    fn listening_message(&self) -> Option<&'static str> {
        self.fallback_name
            .as_ref()
            .map(|_| "AirPods microphone stopped responding. Using the MacBook microphone.")
            .or(Some("Wake listening · say Hey Jarvis"))
    }
    fn start(&mut self) -> Result<(), &'static str> {
        self.silent_stream.reset();
        self.progress.on_start(self.route_clock.elapsed());
        self.capture = Some(
            jarvis_voice::capture::ContinuousCapture::start_with_device_name(
                self.fallback_name
                    .as_deref()
                    .or(self.device_name.as_deref()),
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
        if let Some(capture) = &self.capture {
            self.last_input_name = Some(capture.info().name);
            if let Some(follower) = &mut self.input_follower {
                follower.commit(capture.device_id().to_owned());
            } else {
                self.input_follower = Some(jarvis_voice::capture::DefaultInputFollower::new(
                    self.device_name.is_some(),
                    capture.device_id().to_owned(),
                    self.route_clock.elapsed(),
                ));
            }
        }
        Ok(())
    }
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        if self.capture.is_none() {
            let now = self.route_clock.elapsed();
            if !self.reconnect.due(now) {
                return Ok((Vec::new(), false));
            }
            match self.start() {
                Ok(()) => {
                    self.healthy_since = Some(Instant::now());
                    return Ok((Vec::new(), true));
                }
                Err(_) => {
                    self.try_builtin_fallback();
                    if self.reconnect.failed(now) {
                        return Ok((Vec::new(), false));
                    }
                    return Err(
                        "Microphone disconnected. Reconnect it, then tap Retry microphone.",
                    );
                }
            }
        }
        let failed = self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.failed());
        if failed {
            return self.reconnect_capture(true);
        }
        let due = self
            .input_follower
            .as_mut()
            .is_some_and(|follower| follower.poll_due(self.route_clock.elapsed()));
        if self.device_name.is_none() && (due || failed) {
            let candidate = match jarvis_voice::capture::default_input_id() {
                Ok(candidate) => Some(candidate),
                Err(_) if failed => {
                    return Err("Choose an input microphone in macOS Sound settings");
                }
                // A temporary routing-metadata failure should not interrupt a
                // healthy owned stream. Retry at the next bounded poll.
                Err(_) => None,
            };
            let holding_fallback = self.fallback_name.is_some()
                && (candidate.is_none()
                    || candidate
                        .as_deref()
                        .zip(self.fallback_default.as_deref())
                        .is_some_and(|(candidate, saved)| same_input_device(candidate, saved)));
            if !holding_fallback
                && candidate.as_deref().is_some_and(|candidate| {
                    self.input_follower
                        .as_ref()
                        .is_some_and(|follower| follower.changed(candidate))
                })
            {
                // Dispose old samples/stream before opening the newly selected
                // input. A route switch can never complete an old utterance.
                self.fallback_name = None;
                self.fallback_default = None;
                return self.reconnect_capture(false);
            }
        }
        let result = match self
            .capture
            .as_mut()
            .ok_or("Microphone unavailable")?
            .poll()
        {
            Ok(result) => result,
            Err(_) => return self.reconnect_capture(true),
        };
        if self
            .silent_stream
            .observe(&result.0, self.route_clock.elapsed())
        {
            // Digital silence alone cannot exhaust the actual-failure budget.
            // The independent three-per-minute watchdog budget still applies.
            self.reconnect.success();
            self.healthy_since = None;
            return self.reconnect_capture(false);
        }
        if self.progress.observe(&result.0, self.route_clock.elapsed()) {
            self.reconnect.success();
            self.healthy_since = None;
        }
        if self.progress.no_arrivals(self.route_clock.elapsed()) {
            return self.reconnect_capture(true);
        }
        Ok(result)
    }
    fn stop(&mut self) {
        self.capture.take();
        self.silent_stream.reset();
        self.reconnect.success();
        self.healthy_since = None;
    }
    fn transcribe(
        &mut self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.transcriber
            .transcribe(samples, cancelled)
            .map_err(|_| "Try saying Jarvis again")
    }

    fn transcribe_command(
        &mut self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.transcriber
            .transcribe(samples, cancelled)
            .map_err(|_| "Try saying Jarvis again")
    }
    fn transcribe_command_after(
        &mut self,
        samples: &[f32],
        skip_before: Option<usize>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, &'static str> {
        self.transcriber
            .transcribe_after(samples, skip_before, cancelled)
            .map_err(|_| "Try saying Jarvis again")
    }
}

//! User-initiated local recording; microphone and Whisper live off the UI thread.
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const IDLE: u8 = 0;
const RECORD: u8 = 1;
const FINISH: u8 = 2;
const CANCEL: u8 = 3;
const SHUTDOWN: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Loading,
    Ready,
    StartingMicrophone,
    Recording,
    Transcribing,
    Cancelling,
    Transcript,
    Unavailable,
    Failed,
    Stopped,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceFailure {
    MissingModel,
    Model,
    Microphone,
    Transcription,
    Cancelled,
}
impl VoiceFailure {
    fn message(self) -> &'static str {
        match self {
            Self::MissingModel => "Set JARVIS_WHISPER_MODEL_PATH to a local Whisper model",
            Self::Model => "Check the local Whisper model file",
            Self::Microphone => {
                "Check microphone access in macOS Privacy & Security and Sound settings"
            }
            Self::Transcription => "Please record that again",
            Self::Cancelled => "Recording cancelled",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    Busy,
    Unavailable,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: State,
    pub transcript: String,
    pub message: &'static str,
    pub revision: u64,
}

/// Constructed on the dedicated worker; native audio handles never cross threads.
/// Tests supply synthetic recordings through this same lifecycle interface.
pub trait VoiceBackend {
    fn start_recording(&mut self) -> Result<(), VoiceFailure>;
    fn recording_full(&self) -> bool;
    fn recording_failed(&self) -> bool;
    fn finish_recording(&mut self) -> Result<Vec<f32>, VoiceFailure>;
    fn cancel_recording(&mut self);
    fn transcribe(
        &mut self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, VoiceFailure>;
}

pub struct Control {
    desired: AtomicU8,
    cancelled: Arc<AtomicBool>,
    busy: AtomicBool,
    available: AtomicBool,
    snapshot: Mutex<Snapshot>,
}
impl Control {
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }
    pub fn available(&self) -> bool {
        self.available.load(Ordering::Acquire) && self.desired.load(Ordering::Acquire) != SHUTDOWN
    }
    pub fn record(&self) -> Result<(), CommandError> {
        if !self.available() {
            return Err(CommandError::Unavailable);
        }
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| CommandError::Busy)?;
        self.cancelled.store(false, Ordering::Release);
        self.set_state(State::StartingMicrophone, "Starting microphone", "");
        if self
            .desired
            .compare_exchange(IDLE, RECORD, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.busy.store(false, Ordering::Release);
            return Err(CommandError::Unavailable);
        }
        Ok(())
    }
    pub fn finish_recording(&self) {
        let _ = self
            .desired
            .compare_exchange(RECORD, FINISH, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if self.busy()
            && self
                .desired
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    (value != SHUTDOWN).then_some(CANCEL)
                })
                .is_ok()
        {
            self.set_state(State::Cancelling, "Stopping local voice work", "");
        }
    }
    pub fn shutdown(&self) {
        self.available.store(false, Ordering::Release);
        self.cancelled.store(true, Ordering::Release);
        self.desired.store(SHUTDOWN, Ordering::Release);
    }
    fn set_state(&self, state: State, message: &'static str, transcript: &str) {
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        snapshot.state = state;
        snapshot.message = message;
        snapshot.transcript = transcript.to_owned();
        snapshot.revision = snapshot.revision.wrapping_add(1);
    }
    fn reset_after(&self, state: State, message: &'static str, transcript: &str) {
        if self
            .desired
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value != SHUTDOWN).then_some(IDLE)
            })
            .is_err()
        {
            return;
        }
        self.set_state(state, message, transcript);
        self.busy.store(false, Ordering::Release);
    }
}

pub struct VoiceWorker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl VoiceWorker {
    #[cfg(target_os = "macos")]
    pub fn from_env() -> Result<Self, &'static str> {
        Self::with_backend(|| {
            let settings = jarvis::conversation::load_provider_settings()
                .map_err(|_| VoiceFailure::MissingModel)?;
            let path = settings
                .get("JARVIS_WHISPER_MODEL_PATH")
                .map(PathBuf::from)
                .ok_or(VoiceFailure::MissingModel)?;
            if !path.is_absolute() {
                return Err(VoiceFailure::MissingModel);
            }
            let transcriber =
                jarvis_voice::Transcriber::load(&path).map_err(|_| VoiceFailure::Model)?;
            Ok(Box::new(NativeBackend {
                transcriber,
                session: None,
            }))
        })
    }
    pub fn with_backend(
        factory: impl FnOnce() -> Result<Box<dyn VoiceBackend>, VoiceFailure> + Send + 'static,
    ) -> Result<Self, &'static str> {
        let control = Arc::new(Control {
            desired: AtomicU8::new(IDLE),
            cancelled: Arc::new(AtomicBool::new(false)),
            busy: AtomicBool::new(false),
            available: AtomicBool::new(false),
            snapshot: Mutex::new(Snapshot {
                state: State::Loading,
                transcript: String::new(),
                message: "Loading local speech model",
                revision: 0,
            }),
        });
        let owned = control.clone();
        let handle = thread::Builder::new()
            .name("jarvis-local-voice".into())
            .spawn(move || {
                let mut backend = match factory() {
                    Ok(backend) => backend,
                    Err(error) => {
                        owned.set_state(State::Unavailable, error.message(), "");
                        return;
                    }
                };
                owned.available.store(true, Ordering::Release);
                owned.set_state(State::Ready, "Microphone off · ready to record", "");
                loop {
                    match owned.desired.load(Ordering::Acquire) {
                        SHUTDOWN => {
                            backend.cancel_recording();
                            break;
                        }
                        IDLE => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        CANCEL => {
                            backend.cancel_recording();
                            owned.reset_after(
                                State::Ready,
                                "Microphone off · recording cancelled",
                                "",
                            );
                        }
                        RECORD | FINISH => {
                            if let Err(error) = backend.start_recording() {
                                backend.cancel_recording();
                                owned.reset_after(State::Failed, error.message(), "");
                                continue;
                            }
                            if owned.desired.load(Ordering::Acquire) == RECORD {
                                owned.set_state(
                                    State::Recording,
                                    "Recording · tap Finish when done (15 seconds maximum)",
                                    "",
                                );
                            }
                            while owned.desired.load(Ordering::Acquire) == RECORD
                                && !backend.recording_full()
                                && !backend.recording_failed()
                            {
                                thread::sleep(Duration::from_millis(10));
                            }
                            if matches!(owned.desired.load(Ordering::Acquire), CANCEL | SHUTDOWN) {
                                backend.cancel_recording();
                                if owned.desired.load(Ordering::Acquire) != SHUTDOWN {
                                    owned.reset_after(
                                        State::Ready,
                                        "Microphone off · recording cancelled",
                                        "",
                                    );
                                }
                                continue;
                            }
                            if backend.recording_failed() {
                                backend.cancel_recording();
                                owned.reset_after(
                                    State::Failed,
                                    VoiceFailure::Microphone.message(),
                                    "",
                                );
                                continue;
                            }
                            let samples = match backend.finish_recording() {
                                Ok(samples)
                                    if !samples.is_empty()
                                        && samples.len()
                                            <= jarvis_voice::capture::MAX_RECORDING_SAMPLES =>
                                {
                                    samples
                                }
                                _ => {
                                    owned.reset_after(
                                        State::Failed,
                                        "No audio captured · try recording again",
                                        "",
                                    );
                                    continue;
                                }
                            };
                            if owned.cancelled.load(Ordering::Acquire) {
                                owned.reset_after(
                                    State::Ready,
                                    "Microphone off · recording cancelled",
                                    "",
                                );
                                continue;
                            }
                            owned.set_state(
                                State::Transcribing,
                                "Microphone off · transcribing locally",
                                "",
                            );
                            let result = backend.transcribe(&samples, Arc::clone(&owned.cancelled));
                            if owned.cancelled.load(Ordering::Acquire) {
                                owned.reset_after(
                                    State::Ready,
                                    "Microphone off · transcription cancelled",
                                    "",
                                );
                                continue;
                            }
                            match result {
                                Ok(text)
                                    if !text.trim().is_empty()
                                        && text.len() <= jarvis_voice::MAX_TRANSCRIPT_BYTES =>
                                {
                                    owned.reset_after(
                                        State::Transcript,
                                        "Transcript ready · review and press Ask",
                                        &text,
                                    )
                                }
                                Ok(_) => owned.reset_after(
                                    State::Failed,
                                    "No clear speech detected · try again",
                                    "",
                                ),
                                Err(error) => owned.reset_after(State::Failed, error.message(), ""),
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                owned.available.store(false, Ordering::Release);
                owned.busy.store(false, Ordering::Release);
                owned.set_state(State::Stopped, "Microphone off", "");
            })
            .map_err(|_| "Could not start local voice worker")?;
        Ok(Self {
            control,
            handle: Some(handle),
        })
    }
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(|handle| handle.is_finished())
    }
    pub fn join(&mut self) -> Result<(), &'static str> {
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| "Local voice worker terminated unexpectedly")?;
        }
        Ok(())
    }
}
impl Drop for VoiceWorker {
    fn drop(&mut self) {
        self.control.shutdown();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(target_os = "macos")]
struct NativeBackend {
    transcriber: jarvis_voice::Transcriber,
    session: Option<jarvis_voice::capture::CaptureSession>,
}
#[cfg(target_os = "macos")]
impl VoiceBackend for NativeBackend {
    fn start_recording(&mut self) -> Result<(), VoiceFailure> {
        self.session = Some(
            jarvis_voice::capture::CaptureSession::start().map_err(|_| VoiceFailure::Microphone)?,
        );
        Ok(())
    }
    fn recording_full(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.is_full())
    }
    fn recording_failed(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.failed())
    }
    fn finish_recording(&mut self) -> Result<Vec<f32>, VoiceFailure> {
        self.session
            .take()
            .ok_or(VoiceFailure::Microphone)?
            .stop()
            .map_err(|_| VoiceFailure::Microphone)
    }
    fn cancel_recording(&mut self) {
        self.session.take();
    }
    fn transcribe(
        &mut self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, VoiceFailure> {
        self.transcriber
            .transcribe_command_cancelled(samples, cancelled)
            .map_err(|error| {
                if error == jarvis_voice::VoiceError::Cancelled {
                    VoiceFailure::Cancelled
                } else {
                    VoiceFailure::Transcription
                }
            })
    }
}

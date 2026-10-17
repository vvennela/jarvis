//! Explicit, bounded speech playback through an owned process. Never a shell.
use jarvis::elevenlabs::ElevenLabsClient;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, watch},
};

pub const MAX_TEXT_BYTES: usize = 8192;
const MAX_PLAYBACK: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Starting,
    Speaking,
    Stopping,
    Failed,
    Stopped,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: State,
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeechError {
    Busy,
    InvalidText,
    Unavailable,
}
struct Request {
    text: String,
    epoch: u64,
}

pub struct Control {
    sender: mpsc::Sender<Request>,
    cancellation: watch::Sender<u64>,
    busy: AtomicBool,
    available: AtomicBool,
    shutdown: AtomicBool,
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
        self.available.load(Ordering::Acquire) && !self.shutdown.load(Ordering::Acquire)
    }
    pub fn speak(&self, text: String) -> Result<(), SpeechError> {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
            return Err(SpeechError::InvalidText);
        }
        if !self.available() {
            return Err(SpeechError::Unavailable);
        }
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SpeechError::Busy)?;
        let epoch = *self.cancellation.borrow();
        if self.sender.try_send(Request { text, epoch }).is_err() {
            self.busy.store(false, Ordering::Release);
            return Err(SpeechError::Unavailable);
        }
        Ok(())
    }
    pub fn stop_speaking(&self) {
        if self.busy() {
            self.set_state(State::Stopping);
            self.cancellation
                .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        }
    }
    pub fn shutdown(&self) {
        self.available.store(false, Ordering::Release);
        self.shutdown.store(true, Ordering::Release);
        self.cancellation
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
    fn set_state(&self, state: State) {
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        snapshot.state = state;
        snapshot.revision = snapshot.revision.wrapping_add(1);
    }
}

pub struct SpeechWorker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl SpeechWorker {
    #[cfg(target_os = "macos")]
    pub fn start() -> Result<Self, &'static str> {
        match ElevenLabsClient::from_env().map_err(|_| "Speech configuration is invalid")? {
            Some(client) => Self::with_elevenlabs(client, PathBuf::from("/usr/bin/afplay")),
            None => Self::with_executable(PathBuf::from("/usr/bin/say")),
        }
    }
    /// Trusted playback executable injection, for testing owned afplay lifecycle.
    pub fn with_elevenlabs(
        client: ElevenLabsClient,
        executable: PathBuf,
    ) -> Result<Self, &'static str> {
        Self::with_backend(executable, Some(client))
    }
    /// Trusted executable injection for lifecycle tests; never supplied by a model.
    pub fn with_executable(executable: PathBuf) -> Result<Self, &'static str> {
        Self::with_backend(executable, None)
    }
    fn with_backend(
        executable: PathBuf,
        client: Option<ElevenLabsClient>,
    ) -> Result<Self, &'static str> {
        if !executable.is_absolute() || !executable.is_file() {
            return Err("Speech executable is unavailable");
        }
        let (sender, mut receiver) = mpsc::channel::<Request>(1);
        let (cancellation, mut cancelled) = watch::channel(0_u64);
        let control = Arc::new(Control {
            sender,
            cancellation,
            busy: AtomicBool::new(false),
            available: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            snapshot: Mutex::new(Snapshot {
                state: State::Idle,
                revision: 0,
            }),
        });
        let owned_control = Arc::clone(&control);
        let handle = thread::Builder::new().name("jarvis-speech".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(_) => { owned_control.set_state(State::Failed); return; }
            };
            runtime.block_on(async move {
                owned_control.available.store(true, Ordering::Release);
                loop {
                    if owned_control.shutdown.load(Ordering::Acquire) { break; }
                    let request = tokio::select! {
                        biased;
                        _ = cancelled.changed() => { continue; }
                        request = receiver.recv() => { match request { Some(request) => request, None => break } }
                    };
                    if owned_control.shutdown.load(Ordering::Acquire) { break; }
                    if *cancelled.borrow() != request.epoch {
                        owned_control.set_state(State::Idle);
                        owned_control.busy.store(false, Ordering::Release);
                        continue;
                    }
                    owned_control.set_state(State::Starting);
                    // Keep the private file alive until the owned player exits.
                    let audio_file = if let Some(client) = &client {
                        let audio = tokio::select! {
                            biased;
                            _ = cancelled.changed() => {
                                owned_control.set_state(State::Idle);
                                owned_control.busy.store(false, Ordering::Release);
                                continue;
                            }
                            result = client.synthesize(&request.text) => result,
                        };
                        let file = audio.map_err(|_| ()).and_then(|audio| PrivateAudio::create(&audio));
                        match file {
                            Ok(file) => Some(file),
                            Err(()) => {
                                owned_control.set_state(State::Failed);
                                owned_control.busy.store(false, Ordering::Release);
                                continue;
                            }
                        }
                    } else { None };
                    // Cancellation may arrive while creating the bounded local file.
                    if owned_control.shutdown.load(Ordering::Acquire) || *cancelled.borrow() != request.epoch {
                        drop(audio_file);
                        owned_control.set_state(State::Idle);
                        owned_control.busy.store(false, Ordering::Release);
                        continue;
                    }
                    let mut command = tokio::process::Command::new(&executable);
                    if let Some(file) = &audio_file {
                        command.arg(&file.path).stdin(Stdio::null());
                    } else {
                        command.args(["-f", "-"]).stdin(Stdio::piped());
                    }
                    let child = command.stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn();
                    let mut child = match child {
                        Ok(child) => child,
                        Err(_) => { drop(audio_file); owned_control.set_state(State::Failed); owned_control.busy.store(false, Ordering::Release); continue; }
                    };
                    owned_control.set_state(State::Speaking);
                    let outcome = {
                        let playback = async {
                            if audio_file.is_none() {
                                let mut stdin = child.stdin.take().ok_or(())?;
                                stdin.write_all(request.text.as_bytes()).await.map_err(|_| ())?;
                                stdin.shutdown().await.map_err(|_| ())?;
                                drop(stdin);
                            }
                            let status = child.wait().await.map_err(|_| ())?;
                            if status.success() { Ok(()) } else { Err(()) }
                        };
                        tokio::pin!(playback);
                        let timeout = tokio::time::sleep(MAX_PLAYBACK);
                        tokio::pin!(timeout);
                        loop {
                            tokio::select! {
                                biased;
                                _ = cancelled.changed() => {
                                    if owned_control.shutdown.load(Ordering::Acquire) || *cancelled.borrow() != request.epoch { break None; }
                                }
                                _ = &mut timeout => { break Some(Err(())); }
                                result = &mut playback => { break Some(result); }
                            }
                        }
                    };
                    if !matches!(outcome, Some(Ok(()))) {
                        // Only this Child is targeted; never killall/pkill/global say.
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                    drop(audio_file);
                    owned_control.set_state(if matches!(outcome, Some(Err(()))) { State::Failed } else { State::Idle });
                    owned_control.busy.store(false, Ordering::Release);
                }
                owned_control.available.store(false, Ordering::Release);
                owned_control.busy.store(false, Ordering::Release);
                owned_control.set_state(State::Stopped);
            });
        }).map_err(|_| "Could not start speech worker")?;
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
                .map_err(|_| "Speech worker terminated unexpectedly")?;
        }
        Ok(())
    }
}
impl Drop for SpeechWorker {
    fn drop(&mut self) {
        self.control.shutdown();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// A private directory is created atomically, never reused. Only owned paths are
// removed; no recursive deletion or shell interpolation is involved.
struct PrivateAudio {
    path: PathBuf,
    directory: PathBuf,
}
impl PrivateAudio {
    fn create(audio: &[u8]) -> Result<Self, ()> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        for _ in 0..16 {
            let directory = base.join(format!(
                "jarvis-speech-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| ())?
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(()),
            }
            let owned = Self {
                path: directory.join("speech.mp3"),
                directory,
            };
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&owned.path).map_err(|_| ())?;
            file.write_all(audio).map_err(|_| ())?;
            drop(file);
            return Ok(owned);
        }
        Err(())
    }
}
impl Drop for PrivateAudio {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}

//! One owned Codex job, isolated from chat and the approved project's working copy.
use crate::tasks::{Snapshot, State, SubmitError};
#[cfg(target_os = "macos")]
use jarvis::coding::CodingConfig;
use jarvis::{
    coding::{CodingError, CodingOutcome, CodingTask},
    tool_runtime::{CodingSubmitter, explicit_coding_request},
};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::{mpsc, watch};

enum Command {
    Run(String),
    Review,
}
pub struct Control {
    sender: mpsc::Sender<Command>,
    changed: watch::Sender<u64>,
    cancel: Arc<AtomicBool>,
    busy: AtomicBool,
    reviewed: AtomicBool,
    stopping: AtomicBool,
    submission: Mutex<()>,
    snapshot: Mutex<Snapshot>,
}
impl CodingSubmitter for Control {
    fn status(&self) -> Option<jarvis::tool_runtime::BackgroundJobState> {
        let phase = crate::tasks::task_phase(self.snapshot().state, self.busy());
        Some(
            if phase == jarvis::tool_runtime::BackgroundJobState::ReadyForReview
                && self.reviewed.load(Ordering::Acquire)
            {
                jarvis::tool_runtime::BackgroundJobState::ReviewOpened
            } else {
                phase
            },
        )
    }
    fn submit_coding(&self, request: &str) -> Result<(), String> {
        self.submit(request.to_owned())
            .map_err(|_| "The coding worker could not accept that request.".into())
    }
}
impl Control {
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }
    pub fn submit(&self, request: String) -> Result<(), SubmitError> {
        if !explicit_coding_request(&request) {
            return Err(SubmitError::InvalidRequest);
        }
        self.enqueue(Command::Run(request))
    }
    pub fn request_review(&self) -> Result<(), SubmitError> {
        if self.snapshot().state != State::ReadyForReview {
            return Err(SubmitError::NotReady);
        }
        self.enqueue(Command::Review)
    }
    fn enqueue(&self, command: Command) -> Result<(), SubmitError> {
        let _guard = self.submission.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopping.load(Ordering::Acquire) {
            return Err(SubmitError::Unavailable);
        }
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SubmitError::Busy)?;
        self.cancel.store(false, Ordering::Release);
        if self.sender.try_send(command).is_err() {
            self.busy.store(false, Ordering::Release);
            return Err(SubmitError::Unavailable);
        }
        Ok(())
    }
    pub fn cancel(&self) {
        let _guard = self.submission.lock().unwrap_or_else(|e| e.into_inner());
        if self.busy() {
            self.cancel.store(true, Ordering::Release);
        }
    }
    pub fn shutdown(&self) {
        let _guard = self.submission.lock().unwrap_or_else(|e| e.into_inner());
        self.stopping.store(true, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    fn update(&self, state: State, text: &str, path: Option<PathBuf>) {
        let mut s = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        s.state = state;
        if state != State::ReadyForReview {
            self.reviewed.store(false, Ordering::Release);
        }
        s.text = text.chars().take(1024).collect();
        s.path = path;
        s.revision = s.revision.wrapping_add(1);
    }
}
pub struct CodingWorker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl CodingWorker {
    /// Trusted configuration only. No project means no worker or advertised coding tool.
    #[cfg(target_os = "macos")]
    pub fn from_env() -> Result<Option<Self>, &'static str> {
        let settings = jarvis::conversation::load_provider_settings()
            .map_err(|_| "Check the coding project setting")?;
        let Some(project) = settings
            .get("JARVIS_CODING_PROJECT")
            .filter(|s| !s.trim().is_empty())
        else {
            return Ok(None);
        };
        let project = PathBuf::from(project);
        if !project.is_absolute() {
            return Err("Choose an absolute coding project path");
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("Home folder unavailable")?;
        let executable = [
            home.join(".local/bin/codex"),
            PathBuf::from("/opt/homebrew/bin/codex"),
            PathBuf::from("/usr/local/bin/codex"),
        ]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or("Install and sign in to Codex to enable coding")?;
        let config = CodingConfig::new(
            project,
            home.join("Library/Application Support/Jarvis/Coding"),
            executable,
        )
        .map_err(|_| "Check the configured coding project")?;
        Self::with_factory(
            move || CodingTask::new(config.clone()),
            PathBuf::from("/usr/bin/open"),
        )
        .map(Some)
    }
    /// Real runner and owned opener injection for process-level tests.
    pub fn with_factory(
        factory: impl Fn() -> Result<CodingTask, CodingError> + Send + Sync + 'static,
        opener: PathBuf,
    ) -> Result<Self, &'static str> {
        if !opener.is_absolute() || !opener.is_file() {
            return Err("Review opener unavailable");
        }
        let (sender, mut receiver) = mpsc::channel(1);
        let (changed, mut changes) = watch::channel(0_u64);
        let control = Arc::new(Control {
            sender,
            changed,
            cancel: Arc::new(AtomicBool::new(false)),
            busy: AtomicBool::new(false),
            reviewed: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            submission: Mutex::new(()),
            snapshot: Mutex::new(Snapshot {
                state: State::Idle,
                text: "Codex ready for the configured project".into(),
                revision: 0,
                path: None,
            }),
        });
        let c = control.clone();
        let handle=thread::Builder::new().name("jarvis-coding".into()).spawn(move || {
            let runtime=match tokio::runtime::Builder::new_current_thread().enable_all().build() { Ok(r)=>r,Err(_)=>{c.update(State::Failed,"Coding runtime unavailable",None);c.stopping.store(true,Ordering::Release);return} };
            runtime.block_on(async move {
                let mut task:Option<CodingTask>=None;
                loop {
                    if c.stopping.load(Ordering::Acquire) {break;}
                    let command=tokio::select! { biased; _=changes.changed()=>continue, command=receiver.recv()=>match command {Some(v)=>v,None=>break} };
                    if c.stopping.load(Ordering::Acquire) {break;}
                    if c.cancel.load(Ordering::Acquire) {c.update(State::Cancelled,"Coding cancelled",None);c.busy.store(false,Ordering::Release);continue;}
                    match command {
                        Command::Run(request)=>{
                            task=None;
                            c.update(State::Running,"Codex is working in a separate worktree…",None);
                            match factory() {
                                Ok(mut runner)=>{
                                    let outcome=runner.run(&request,c.cancel.clone()).await;
                                    match outcome {Ok(outcome)=>publish(&c,outcome),Err(error)=>c.update(State::Failed,&error.to_string(),None)}
                                    task=Some(runner);
                                }
                                Err(error)=>c.update(State::Failed,&error.to_string(),None),
                            }
                        }
                        Command::Review=>{
                            c.update(State::Running,"Verifying the coding result before opening…",None);
                            match task.as_mut() {
                                Some(runner)=>match runner.review().await {
                                    Ok(CodingOutcome::ReadyForReview{summary,..})=>{
                                        match open_review(&opener,&summary,&c.cancel).await {
                                            Ok(())=>{c.update(State::ReadyForReview,"Opened the verified coding review. Changes remain in the separate worktree.",Some(summary));c.reviewed.store(true,Ordering::Release);},
                                            Err(()) if c.cancel.load(Ordering::Acquire)=>c.update(State::ReadyForReview,"Opening cancelled. Coding changes remain ready for review.",Some(summary)),
                                            Err(())=>c.update(State::ReadyForReview,"Coding review is ready; its viewer could not open.",Some(summary)),
                                        }
                                    }
                                    Ok(outcome)=>publish(&c,outcome),
                                    Err(error)=>c.update(State::Blocked,&error.to_string(),None),
                                },
                                None=>c.update(State::Blocked,"Start a coding job before opening its review.",None),
                            }
                        }
                    }
                    c.busy.store(false,Ordering::Release);
                }
                c.busy.store(false,Ordering::Release);
                c.update(State::Stopped,"Coding worker stopped",None);
            });
        }).map_err(|_|"Could not start coding worker")?;
        Ok(Self {
            control,
            handle: Some(handle),
        })
    }
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }
    pub fn join(&mut self) -> Result<(), &'static str> {
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| "Coding worker stopped unexpectedly")?;
        }
        Ok(())
    }
}
impl Drop for CodingWorker {
    fn drop(&mut self) {
        self.control.shutdown();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
fn publish(control: &Control, outcome: CodingOutcome) {
    match outcome {
        CodingOutcome::ReadyForReview { summary, .. } => control.update(
            State::ReadyForReview,
            "Coding changes are ready for your review in a separate worktree.",
            Some(summary),
        ),
        CodingOutcome::Blocked { reason, .. } => control.update(State::Blocked, reason, None),
        CodingOutcome::Cancelled { .. } => control.update(
            State::Cancelled,
            "Coding cancelled. The original project is unchanged.",
            None,
        ),
    }
}
async fn open_review(executable: &Path, path: &Path, cancel: &AtomicBool) -> Result<(), ()> {
    if cancel.load(Ordering::Acquire) {
        return Err(());
    }
    let mut child = tokio::process::Command::new(executable)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ())?;
    let stopped = async {
        while !cancel.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    let result = tokio::select! {biased; _=stopped=>Err(()), result=tokio::time::timeout(Duration::from_secs(5),child.wait())=>match result {Ok(Ok(status)) if status.success()=>Ok(()),_=>Err(())}};
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

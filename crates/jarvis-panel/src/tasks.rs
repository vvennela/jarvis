//! One background task at a time, with verified review recovery after restart.
//! Text generation never sends a message or chooses an executable or output path.
use jarvis::{
    broker,
    context::Source,
    draft::{BlockReason, DraftError, DraftOutcome, DraftTask, MAX_INSTRUCTION_BYTES},
    file_work::FileEditTask,
    task_journal::{EvidenceSnapshot, RecoveredReview, Restore, TaskJournal, TaskKind},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, watch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Running,
    ReadyForReview,
    Blocked,
    Cancelled,
    Failed,
    Stopped,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: State,
    pub text: String,
    pub revision: u64,
    pub path: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    Busy,
    Unavailable,
    InvalidRequest,
    NotReady,
}
enum Command {
    Generate(String),
    Edit {
        instruction: String,
        resource_id: String,
    },
    Review,
}
pub struct Control {
    sender: mpsc::Sender<Command>,
    changed: watch::Sender<u64>,
    cancel: Arc<AtomicBool>,
    busy: AtomicBool,
    stopping: AtomicBool,
    submission: Mutex<()>,
    snapshot: Mutex<Snapshot>,
}
impl jarvis::tool_runtime::DraftSubmitter for Control {
    fn submit(&self, original_user_request: &str) -> Result<(), String> {
        Control::submit(self, original_user_request.to_owned()).map_err(|error| {
            match error {
                SubmitError::Busy => {
                    "A draft is already running. Let it finish or cancel it first."
                }
                SubmitError::Unavailable => "The draft worker is unavailable.",
                SubmitError::InvalidRequest => "The draft request is empty, invalid, or too long.",
                SubmitError::NotReady => "The draft is not ready for review.",
            }
            .to_owned()
        })
    }
}
impl jarvis::tool_runtime::FileEditSubmitter for Control {
    fn submit_edit(&self, original_user_request: &str, resource_id: &str) -> Result<(), String> {
        self.submit_file_edit(original_user_request.to_owned(), resource_id.to_owned())
            .map_err(|error| {
                match error {
                    SubmitError::Busy => {
                        "A background task is already running. Let it finish or cancel it first."
                    }
                    SubmitError::Unavailable => "The background file worker is unavailable.",
                    SubmitError::InvalidRequest => {
                        "The file edit request or indexed resource is invalid."
                    }
                    SubmitError::NotReady => "The file edit is not ready for review.",
                }
                .to_owned()
            })
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
    pub fn submit(&self, instruction: String) -> Result<(), SubmitError> {
        if instruction.trim().is_empty()
            || instruction.len() > MAX_INSTRUCTION_BYTES
            || instruction.contains('\0')
        {
            return Err(SubmitError::InvalidRequest);
        }
        self.enqueue(Command::Generate(instruction))
    }
    pub fn submit_file_edit(
        &self,
        instruction: String,
        resource_id: String,
    ) -> Result<(), SubmitError> {
        if instruction.trim().is_empty()
            || instruction.len() > MAX_INSTRUCTION_BYTES
            || instruction.contains('\0')
            || !resource_id.starts_with("file:")
            || resource_id.len() > 2048
            || resource_id.contains('\0')
        {
            return Err(SubmitError::InvalidRequest);
        }
        self.enqueue(Command::Edit {
            instruction,
            resource_id,
        })
    }
    pub fn request_review(&self) -> Result<(), SubmitError> {
        if self.snapshot().state != State::ReadyForReview {
            return Err(SubmitError::NotReady);
        }
        self.enqueue(Command::Review)
    }
    fn enqueue(&self, command: Command) -> Result<(), SubmitError> {
        let _submission = self
            .submission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
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
        let _submission = self
            .submission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.busy() {
            self.cancel.store(true, Ordering::Release);
        }
    }
    pub fn shutdown(&self) {
        let _submission = self
            .submission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.stopping.store(true, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }
    fn update(&self, state: State, text: &str, path: Option<PathBuf>) {
        let mut value = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        value.state = state;
        value.text = text.to_owned();
        value.path = path;
        value.revision = value.revision.wrapping_add(1);
    }
}

enum RetainedTask {
    Draft(DraftTask),
    Edit(FileEditTask),
    Recovered(RecoveredReview),
}
impl RetainedTask {
    fn review(&mut self) -> Result<DraftOutcome, DraftError> {
        match self {
            Self::Draft(task) => task.review(),
            Self::Edit(task) => task.review(),
            Self::Recovered(task) => task.review().map_err(|_| DraftError::Artifact),
        }
    }
}
pub struct TaskWorker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl TaskWorker {
    pub fn start(database: PathBuf, output_dir: PathBuf) -> Result<Self, &'static str> {
        Self::with_factory(
            database,
            output_dir,
            DraftTask::from_env,
            PathBuf::from("/usr/bin/open"),
        )
    }
    /// The injected executable receives only a verified absolute file path.
    pub fn with_factory(
        database: PathBuf,
        output_dir: PathBuf,
        factory: impl FnMut(PathBuf, String, String) -> Result<DraftTask, DraftError> + Send + 'static,
        opener: PathBuf,
    ) -> Result<Self, &'static str> {
        Self::with_factories(
            database,
            output_dir,
            factory,
            FileEditTask::from_env,
            opener,
        )
    }
    pub fn with_factories(
        database: PathBuf,
        output_dir: PathBuf,
        mut factory: impl FnMut(PathBuf, String, String) -> Result<DraftTask, DraftError>
        + Send
        + 'static,
        mut edit_factory: impl FnMut(PathBuf, String, String) -> Result<FileEditTask, DraftError>
        + Send
        + 'static,
        opener: PathBuf,
    ) -> Result<Self, &'static str> {
        let output_dir = prepare_directory(&output_dir)?;
        if !opener.is_absolute() {
            return Err("Review opener must be an absolute executable path");
        }
        let journal = TaskJournal::new(&database, &output_dir)
            .map_err(|_| "Background task journal could not be initialized")?;
        let restored = journal.restore();
        let (initial_state, initial_text, initial_path, recovered) = match restored {
            Restore::Empty => (State::Idle, "Ready to prepare a local draft.", None, None),
            Restore::Blocked => (
                State::Blocked,
                "The previous task was interrupted or its evidence changed. Start a fresh task.",
                None,
                None,
            ),
            Restore::Ready(review) => match review.review() {
                Ok(DraftOutcome::ReadyForReview { path, .. }) => (
                    State::ReadyForReview,
                    "Your previous result is verified and ready for review.",
                    Some(path),
                    Some(RetainedTask::Recovered(review)),
                ),
                _ => (
                    State::Blocked,
                    "The previous result could not be verified. Start a fresh task.",
                    None,
                    None,
                ),
            },
        };
        let (sender, mut receiver) = mpsc::channel(1);
        let (changed, mut changes) = watch::channel(0u64);
        let control = Arc::new(Control {
            sender,
            changed,
            cancel: Arc::new(AtomicBool::new(false)),
            busy: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            submission: Mutex::new(()),
            snapshot: Mutex::new(Snapshot {
                state: initial_state,
                text: initial_text.into(),
                revision: 0,
                path: initial_path,
            }),
        });
        let state = control.clone();
        let handle=thread::Builder::new().name("jarvis-drafts".into()).spawn(move || {
            let runtime=match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime)=>runtime,
                Err(_)=>{state.update(State::Failed,"The background draft runtime could not start.",None);state.stopping.store(true,Ordering::Release);state.busy.store(false,Ordering::Release);return;}
            };
            runtime.block_on(async move {
                let mut retained:Option<RetainedTask>=recovered;
                loop {
                    if state.stopping.load(Ordering::Acquire) { break; }
                    let command=tokio::select! {
                        biased;
                        _=changes.changed()=>continue,
                        command=receiver.recv()=>match command {Some(command)=>command,None=>break},
                    };
                    if state.stopping.load(Ordering::Acquire) { break; }
                    let new_kind = match &command { Command::Generate(_) => Some(TaskKind::Draft), Command::Edit {..} => Some(TaskKind::FileEdit), Command::Review => None };
                    if let Some(kind) = new_kind
                        && journal.mark_running(kind).is_err() {
                        retained=None;
                        state.update(State::Blocked,"The task journal could not be saved. No background work was started.",None);
                        state.busy.store(false,Ordering::Release); continue;
                    }
                    if state.cancel.load(Ordering::Acquire) {
                        state.update(State::Cancelled,"Draft request cancelled.",None);
                        state.busy.store(false,Ordering::Release); continue;
                    }
                    match command {
                        Command::Generate(instruction)=>{
                            retained=None;
                            state.update(State::Running,"Preparing your local draft in the background…",None);
                            let db=database.clone();
                            let query=instruction.clone();
                            let evidence=tokio::select! {
                                biased;
                                _=cancelled(&state.cancel)=>None,
                                evidence=tokio::task::spawn_blocking(move || broker::retrieve(db,&query))=>Some(evidence),
                            };
                            match evidence {
                                None=>state.update(State::Cancelled,"Draft request cancelled.",None),
                                Some(Ok(Ok(packet)))=>{
                                    let mut evidence=packet.evidence;
                                    evidence.retain(|item| matches!(item.source,Source::File|Source::Browser));
                                    evidence.sort_by_key(|item| if item.source==Source::File {0} else {1});
                                    let ids:Vec<_>=evidence.into_iter().take(4).map(|item|item.resource_id).collect();
                                    if ids.is_empty() {
                                        state.update(State::Blocked,"No indexed document or browser evidence is available for this draft. Add the relevant work context first.",None);
                                    } else {
                                        let evidence = match journal.capture(TaskKind::Draft,&ids) {
                                            Ok(evidence)=>evidence,
                                            Err(_)=>{state.update(State::Blocked,"The source evidence could not be verified for recovery. No draft was generated.",None);state.busy.store(false,Ordering::Release);continue;}
                                        };
                                        let id=fresh_id();
                                        let output=output_dir.join(format!("{id}.txt"));
                                        match factory(database.clone(),id.clone(),id) {
                                            Err(error)=>state.update(State::Failed,error_text(error),None),
                                            Ok(mut task)=>{
                                                match generate_with_retry(&mut task,&instruction,&ids,&output,&state).await {
                                                    Ok(outcome)=>publish_persisted(&journal,evidence,&state,outcome,false),
                                                    Err(error)=>state.update(State::Failed,error_text(error),None),
                                                }
                                                retained=Some(RetainedTask::Draft(task));
                                            }
                                        }
                                    }
                                }
                                Some(_)=>state.update(State::Blocked,"The local context index could not be read. No draft was generated.",None),
                            }
                        }
                        Command::Edit {instruction,resource_id}=>{
                            retained=None;
                            state.update(State::Running,"Preparing a revised file in the background…",None);
                            let evidence = match journal.capture(TaskKind::FileEdit,std::slice::from_ref(&resource_id)) {
                                Ok(evidence)=>evidence,
                                Err(_)=>{state.update(State::Blocked,"The source file could not be verified for recovery. No revised file was generated.",None);state.busy.store(false,Ordering::Release);continue;}
                            };
                            let id=fresh_id();
                            let output=output_dir.join(format!("{id}.prepared.txt"));
                            match edit_factory(database.clone(),id.clone(),id) {
                                Err(error)=>state.update(State::Failed,error_text(error),None),
                                Ok(mut task)=>{
                                    match generate_edit_with_retry(&mut task,&instruction,&resource_id,&output,&state).await {
                                        Ok(outcome)=>publish_persisted(&journal,evidence,&state,outcome,true),
                                        Err(error)=>state.update(State::Failed,error_text(error),None),
                                    }
                                    retained=Some(RetainedTask::Edit(task));
                                }
                            }
                        }
                        Command::Review=>{
                            let editing=matches!(retained,Some(RetainedTask::Edit(_))) || matches!(&retained,Some(RetainedTask::Recovered(review)) if review.kind()==TaskKind::FileEdit);
                            let recovered=matches!(retained,Some(RetainedTask::Recovered(_)));
                            match retained.as_mut().map(RetainedTask::review) {
                                Some(Ok(DraftOutcome::ReadyForReview{path,..}))=>{
                                    let result=open_review(&opener,&path,&state.cancel).await;
                                    match result {
                                        Ok(())=>state.update(State::ReadyForReview,if editing {"Opened the revised file for your review. The original is unchanged."} else {"Opened the verified local draft for your review. Nothing was sent."},Some(path)),
                                        Err(OpenError::Cancelled)=>state.update(State::ReadyForReview,"Opening the draft was cancelled. The local draft remains ready for review.",Some(path)),
                                        Err(OpenError::Failed)=>state.update(State::ReadyForReview,"The draft is verified, but its viewer could not open. Try Review again.",Some(path)),
                                    }
                                }
                                Some(Ok(outcome))=>{ let _=journal.mark_running(if editing {TaskKind::FileEdit} else {TaskKind::Draft}); if editing {publish_edit(&state,outcome)} else {publish(&state,outcome)} },
                                Some(Err(error))=>{ let _=journal.mark_running(if editing {TaskKind::FileEdit} else {TaskKind::Draft}); state.update(if recovered {State::Blocked} else {State::Failed},if recovered {"The recovered result or its source changed. Start a fresh task."} else {error_text(error)},None); },
                                None=>state.update(State::Blocked,"There is no retained draft to review.",None),
                            }
                        }
                    }
                    state.busy.store(false,Ordering::Release);
                }
                state.busy.store(false,Ordering::Release);
                state.update(State::Stopped,"Background drafts stopped.",None);
            });
        }).map_err(|_|"Could not start background draft worker")?;
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
                .map_err(|_| "Background draft worker terminated unexpectedly")?;
        }
        Ok(())
    }
}
impl Drop for TaskWorker {
    fn drop(&mut self) {
        self.control.shutdown();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Dispatch only explicit caller requests; generated model prose never routes here.
pub fn is_draft_request(text: &str) -> bool {
    if text.len() > MAX_INSTRUCTION_BYTES || text.contains('\0') {
        return false;
    }
    let text = text.trim().to_ascii_lowercase();
    let mut text = text.as_str();
    for _ in 0..3 {
        let Some(rest) = [
            "hey jarvis ",
            "jarvis ",
            "please ",
            "can you ",
            "could you ",
        ]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix)) else {
            break;
        };
        text = rest.trim_start();
    }
    [
        "draft ",
        "prepare a draft ",
        "write a draft ",
        "write a handoff ",
        "prepare a handoff ",
    ]
    .iter()
    .any(|prefix| {
        text.strip_prefix(prefix)
            .is_some_and(|rest| !rest.trim().is_empty())
    })
}
fn prepare_directory(path: &Path) -> Result<PathBuf, &'static str> {
    if !path.is_absolute() {
        return Err("Draft output directory must be absolute");
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| "Draft output directory could not be created")?;
    let path =
        fs::canonicalize(path).map_err(|_| "Draft output directory could not be resolved")?;
    if !path.is_dir()
        || path
            .to_str()
            .is_none_or(|path| path.len() > 3900 || path.contains(['\0', '\n', '\r']))
    {
        return Err("Draft output directory is invalid");
    }
    Ok(path)
}
fn fresh_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "draft-{}-{time}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
fn publish_persisted(
    journal: &TaskJournal,
    evidence: EvidenceSnapshot,
    control: &Control,
    outcome: DraftOutcome,
    editing: bool,
) {
    if matches!(outcome, DraftOutcome::ReadyForReview { .. })
        && journal.save_ready(evidence, &outcome).is_err()
    {
        control.update(
            State::Blocked,
            "The result could not be durably verified. Start a fresh task before review.",
            None,
        );
        return;
    }
    if editing {
        publish_edit(control, outcome);
    } else {
        publish(control, outcome);
    }
}
fn publish(control: &Control, outcome: DraftOutcome) {
    match outcome {
        DraftOutcome::ReadyForReview { path, .. } => control.update(
            State::ReadyForReview,
            "I've prepared the local draft and left it ready for your review. Nothing was sent.",
            Some(path),
        ),
        DraftOutcome::Cancelled { .. } => control.update(
            State::Cancelled,
            "Draft request cancelled. No draft was published.",
            None,
        ),
        DraftOutcome::Blocked { reason, .. } => control.update(
            State::Blocked,
            match reason {
                BlockReason::ChangedEvidence => {
                    "The source context changed. Prepare a fresh draft before reviewing."
                }
                BlockReason::MissingEvidence | BlockReason::EvidenceUnavailable => {
                    "Required source context is missing or unavailable. The draft is blocked."
                }
                BlockReason::ArtifactChanged => {
                    "The draft file changed. Its previous review status has been revoked."
                }
                BlockReason::Deadline => {
                    "The draft exceeded its preparation deadline. Please try again."
                }
            },
            None,
        ),
    }
}
async fn generate_with_retry(
    task: &mut DraftTask,
    instruction: &str,
    ids: &[String],
    output: &Path,
    control: &Control,
) -> Result<DraftOutcome, DraftError> {
    // Only inference failures before artifact publication are retryable. Both
    // attempts and backoff share one deadline; cancellation also covers delay.
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(90), async {
        let result = task
            .generate(instruction, ids, output, control.cancel.clone())
            .await;
        let delay = match &result {
            Err(DraftError::Provider(error)) => error.retry_delay(0),
            _ => None,
        };
        if let Some(delay) = delay {
            let delay = delay.max(Duration::from_secs(2));
            if delay >= Duration::from_secs(90).saturating_sub(started.elapsed()) {
                return result;
            }
            control.update(
                State::Running,
                "The draft provider is temporarily busy or unreachable. Retrying once…",
                None,
            );
            tokio::select! {
                biased;
                _ = cancelled(&control.cancel) => {},
                _ = tokio::time::sleep(delay) => {},
            }
            task.generate(instruction, ids, output, control.cancel.clone())
                .await
        } else {
            result
        }
    })
    .await
    .unwrap_or(Err(DraftError::Provider(
        jarvis::providers::ProviderError::Timeout,
    )))
}
fn error_text(error: DraftError) -> &'static str {
    match error {
        DraftError::Configuration(_) => {
            "Draft generation could not load its provider configuration. Check local settings."
        }
        DraftError::Provider(jarvis::providers::ProviderError::Authentication) => {
            "Draft provider authentication failed. Check provider account access."
        }
        DraftError::Provider(jarvis::providers::ProviderError::Timeout) => {
            "The draft model took too long to answer. Please try again."
        }
        DraftError::Provider(jarvis::providers::ProviderError::Rejected) => {
            "The draft provider rejected the request. Check the configured worker model and account access."
        }
        DraftError::Provider(
            jarvis::providers::ProviderError::RateLimited
            | jarvis::providers::ProviderError::RateLimitedAfter(_),
        ) => "The draft provider is rate limiting requests. Please try again shortly.",
        DraftError::Provider(jarvis::providers::ProviderError::ServerUnavailable) => {
            "The draft provider is temporarily unavailable. Please try again shortly."
        }
        DraftError::Provider(jarvis::providers::ProviderError::Transport) => {
            "The draft provider could not be reached. Check the connection and try again."
        }
        DraftError::Provider(jarvis::providers::ProviderError::InvalidResponse) => {
            "The draft provider returned an unreadable response. Please try again."
        }
        DraftError::Provider(jarvis::providers::ProviderError::IncompleteOutput) => {
            "The draft provider returned an incomplete response. Please ask for a shorter draft."
        }
        DraftError::Provider(jarvis::providers::ProviderError::ResponseTooLarge) => {
            "The draft provider response exceeded its size limit. Please ask for a shorter draft."
        }
        DraftError::Provider(jarvis::providers::ProviderError::InvalidConfiguration) => {
            "The draft provider configuration is invalid. Check the worker model and credential format."
        }
        DraftError::Provider(jarvis::providers::ProviderError::InvalidInput) => {
            "The draft provider input exceeded its limit or contained invalid text. Shorten the request."
        }
        DraftError::Context => "The source context could not be read. No draft was prepared.",
        DraftError::Artifact => {
            "The local draft could not be created or verified. Check the output directory."
        }
        DraftError::InvalidInput | DraftError::InvalidState => {
            "The draft request or job state was invalid. Please start a new draft."
        }
    }
}
async fn cancelled(cancel: &AtomicBool) {
    loop {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
enum OpenError {
    Cancelled,
    Failed,
}
async fn open_review(executable: &Path, path: &Path, cancel: &AtomicBool) -> Result<(), OpenError> {
    if cancel.load(Ordering::Acquire) {
        return Err(OpenError::Cancelled);
    }
    let mut child = tokio::process::Command::new(executable)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| OpenError::Failed)?;
    let result = tokio::select! {
        biased;
        _=cancelled(cancel)=>Err(OpenError::Cancelled),
        result=tokio::time::timeout(Duration::from_secs(5),child.wait())=>match result {Ok(Ok(status)) if status.success()=>Ok(()),_=>Err(OpenError::Failed)},
    };
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

fn publish_edit(control: &Control, outcome: DraftOutcome) {
    match outcome {
        DraftOutcome::ReadyForReview { path, .. } => control.update(
            State::ReadyForReview,
            "I've prepared a revised file for your review. The original is unchanged.",
            Some(path),
        ),
        DraftOutcome::Cancelled { .. } => control.update(
            State::Cancelled,
            "File edit cancelled. No revised file was published.",
            None,
        ),
        other => publish(control, other),
    }
}
async fn generate_edit_with_retry(
    task: &mut FileEditTask,
    instruction: &str,
    resource_id: &str,
    output: &Path,
    control: &Control,
) -> Result<DraftOutcome, DraftError> {
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(90),async {
        let result=task.generate(instruction,resource_id,output,control.cancel.clone()).await;
        let delay=match &result {Err(DraftError::Provider(error))=>error.retry_delay(0),_=>None};
        if let Some(delay)=delay {
            let delay=delay.max(Duration::from_secs(2));
            if delay>=Duration::from_secs(90).saturating_sub(started.elapsed()) {return result;}
            control.update(State::Running,"The file-edit provider is busy. Retrying once…",None);
            tokio::select! {biased; _=cancelled(&control.cancel)=>{},_=tokio::time::sleep(delay)=>{}}
            task.generate(instruction,resource_id,output,control.cancel.clone()).await
        } else {result}
    }).await.unwrap_or(Err(DraftError::Provider(jarvis::providers::ProviderError::Timeout)))
}

//! One cancellable conversation worker, independent of file awareness.
use jarvis::{
    actions::ActionExecutor,
    app_tools::AppExecutor,
    conversation::{Conversation, ConversationError, Reply},
    music_catalog::MusicCatalog,
    providers::ProviderError,
    spotify_playback::SpotifyPlayback,
    tool_runtime::ToolRuntime,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

pub const MAX_QUESTION_BYTES: usize = 8192;
pub const MAX_DISPLAY_BYTES: usize = 32768;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Ready,
    Thinking,
    Cancelling,
    Reply,
    Cancelled,
    Unavailable,
    Failed,
    Stopped,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: State,
    pub text: String,
    pub routing_degraded: bool,
    pub needs_followup: bool,
    pub revision: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    Busy,
    Unavailable,
    InvalidQuestion,
}

struct Request {
    question: String,
    epoch: u64,
}
pub struct Control {
    sender: mpsc::Sender<Request>,
    cancellation: watch::Sender<u64>,
    busy: AtomicBool,
    ready: AtomicBool,
    stopping: AtomicBool,
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
        self.ready.load(Ordering::Acquire) && !self.stopping.load(Ordering::Acquire)
    }
    pub fn submit(&self, question: String) -> Result<(), SubmitError> {
        if question.trim().is_empty()
            || question.len() > MAX_QUESTION_BYTES
            || question.contains('\0')
        {
            return Err(SubmitError::InvalidQuestion);
        }
        if !self.available() {
            return Err(SubmitError::Unavailable);
        }
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SubmitError::Busy)?;
        let epoch = *self.cancellation.borrow();
        if self.sender.try_send(Request { question, epoch }).is_err() {
            self.busy.store(false, Ordering::Release);
            return Err(SubmitError::Unavailable);
        }
        Ok(())
    }
    pub fn cancellation_epoch(&self) -> u64 {
        *self.cancellation.borrow()
    }
    pub fn cancel(&self) {
        self.cancellation
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        if self.busy() {
            self.update(State::Cancelling, "Cancelling the current request…", false);
        }
    }
    pub fn stop(&self) {
        self.ready.store(false, Ordering::Release);
        self.stopping.store(true, Ordering::Release);
        self.cancellation
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
    fn update(&self, state: State, text: &str, routing_degraded: bool) {
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        let mut end = text.len().min(MAX_DISPLAY_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        snapshot.state = state;
        snapshot.text = text[..end].to_owned();
        snapshot.routing_degraded = routing_degraded;
        snapshot.needs_followup = false;
        snapshot.revision = snapshot.revision.wrapping_add(1);
    }
}

pub struct ConversationWorker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl ConversationWorker {
    pub fn start(database: PathBuf) -> Result<Self, &'static str> {
        Self::start_configured(database, None, None)
    }
    pub fn start_with_tasks(
        database: PathBuf,
        tasks: Arc<crate::tasks::Control>,
    ) -> Result<Self, &'static str> {
        Self::start_configured(database, Some(tasks), None)
    }
    pub fn start_with_workers(
        database: PathBuf,
        tasks: Arc<crate::tasks::Control>,
        coding: Option<Arc<crate::codingworker::Control>>,
    ) -> Result<Self, &'static str> {
        Self::start_configured(database, Some(tasks), coding)
    }
    fn start_configured(
        database: PathBuf,
        tasks: Option<Arc<crate::tasks::Control>>,
        coding: Option<Arc<crate::codingworker::Control>>,
    ) -> Result<Self, &'static str> {
        let mut tools =
            ToolRuntime::read_only(database.clone()).with_actions(ActionExecutor::native());
        if let Ok(apps) = AppExecutor::native() {
            tools = tools.with_apps(apps);
        }
        if let Ok(opener) = jarvis::web_open::WebOpener::native() {
            tools = tools.with_web_opener(opener);
        }
        if let Ok(email) = jarvis::email::EmailClient::native() {
            tools = tools.with_email(email);
        }
        if let Some(messages) = jarvis::messages::MessagesReader::native() {
            tools = tools.with_messages(messages);
        }
        tools = tools.with_preferences(jarvis::preferences::PreferenceStore::new(
            database.with_file_name("preferences.json"),
        ));
        tools = tools.with_experience(jarvis::experience::ExperienceStore::new(
            database.with_file_name("experience.json"),
        ));
        #[cfg(unix)]
        if let Ok(browser) = jarvis::browser_tools::BrowserClient::native() {
            tools = tools.with_browser(browser);
        }
        if let Some(tasks) = tasks {
            tools = tools
                .with_file_edits(tasks.clone())
                .with_draft_submitter(tasks);
        }
        if let Some(coding) = coding {
            tools = tools.with_coding_submitter(coding);
        }
        if let Ok(settings) = jarvis::conversation::load_provider_settings()
            && let Some(key) = settings
                .get("YOU_API_KEY")
                .filter(|key| !key.trim().is_empty())
        {
            if let Some(key) = settings.get("TYPESAFE_API_KEY")
                && let Ok(jev) = jarvis::providers::JevClient::production(key.clone())
            {
                tools = tools.with_music_intent(Arc::new(jev));
            }
            if let Ok(catalog) = MusicCatalog::production(key.clone())
                && let Ok(playback) = SpotifyPlayback::native()
            {
                tools = tools.with_music(catalog, playback);
            }
            if let Ok(search) = jarvis::web_search::WebSearch::production(key.clone()) {
                tools = tools.with_web_search(search);
            }
            if let Ok(tracks) = jarvis::spotify_tracks::SpotifyTracks::production(key.clone()) {
                tools = tools.with_tracks(tracks);
            }
        }
        Self::with_factory_and_tools(
            move || Conversation::from_env(database),
            // Retain direct commands even if conversational credentials fail.
            ActionExecutor::native(),
            Arc::new(tools),
        )
    }
    /// Test injection accepts real Conversation clients pointing at loopback fixtures.
    pub fn with_factory(
        factory: impl FnOnce() -> Result<Conversation, ConversationError> + Send + 'static,
    ) -> Result<Self, &'static str> {
        Self::build(factory, None, None)
    }
    /// The action adapter is independent of provider configuration. Tests inject
    /// a literal process fixture; production uses the fixed Spotify adapter.
    pub fn with_factory_and_actions(
        factory: impl FnOnce() -> Result<Conversation, ConversationError> + Send + 'static,
        actions: ActionExecutor,
    ) -> Result<Self, &'static str> {
        Self::build(factory, Some(actions), None)
    }
    /// Direct approved tools stay usable when conversational inference is unavailable.
    pub fn with_factory_and_tools(
        factory: impl FnOnce() -> Result<Conversation, ConversationError> + Send + 'static,
        actions: ActionExecutor,
        tools: Arc<ToolRuntime>,
    ) -> Result<Self, &'static str> {
        Self::build(factory, Some(actions), Some(tools))
    }
    fn build(
        factory: impl FnOnce() -> Result<Conversation, ConversationError> + Send + 'static,
        actions: Option<ActionExecutor>,
        tools: Option<Arc<ToolRuntime>>,
    ) -> Result<Self, &'static str> {
        let (sender, mut receiver) = mpsc::channel::<Request>(1);
        let (cancellation, mut cancelled) = watch::channel(0_u64);
        let control = Arc::new(Control {
            sender,
            cancellation,
            busy: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            snapshot: Mutex::new(Snapshot {
                state: State::Starting,
                text: "Preparing conversation…".into(),
                routing_degraded: false,
                needs_followup: false,
                revision: 0,
            }),
        });
        let worker_control = Arc::clone(&control);
        let handle = thread::Builder::new().name("jarvis-conversation".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(_) => { worker_control.update(State::Unavailable, "Conversation runtime is unavailable. File awareness still works.", false); return; }
            };
            runtime.block_on(async move {
                let (mut conversation, initialization_error) = match factory() {
                    Ok(mut conversation) => {
                        conversation.set_app_actions_available(actions.is_some());
                        if let Some(tools) = &tools {
                            conversation.set_tool_runtime(Arc::clone(tools));
                        }
                        (Some(conversation), None)
                    }
                    Err(error) if actions.is_none() => {
                        worker_control.update(State::Unavailable, error_message(error), false);
                        return;
                    }
                    Err(error) => (None, Some(error)),
                };
                worker_control.ready.store(true, Ordering::Release);
                worker_control.update(State::Ready, "Ask about your work or the files you approved. Selected context is sent to your configured model provider.", false);
                loop {
                    if worker_control.stopping.load(Ordering::Acquire) { break; }
                    let request = tokio::select! {
                        biased;
                        _ = cancelled.changed() => {
                            if let Some(conversation) = conversation.as_mut() {
                                conversation.clear_pending_clarification();
                            }
                            continue;
                        }
                        request = receiver.recv() => { match request { Some(request) => request, None => break } }
                    };
                    if worker_control.stopping.load(Ordering::Acquire) { break; }
                    if *cancelled.borrow() != request.epoch {
                        if let Some(conversation) = conversation.as_mut() {
                            conversation.clear_pending_clarification();
                        }
                        worker_control.update(State::Cancelled, "Request cancelled. File awareness is unchanged.", false);
                        worker_control.busy.store(false, Ordering::Release);
                        continue;
                    }
                    worker_control.update(State::Thinking, "Thinking with relevant context…", false);
                    let result = {
                        let ask = tokio::time::timeout(
                            REQUEST_TIMEOUT,
                            async {
                                if let Some(tools) = &tools
                                    && let Some(result) = tools.handle_direct(&request.question).await {
                                    let text = result.acknowledgment.unwrap_or_else(|| "The requested app action did not return a confirmation.".into());
                                    if let Some(conversation) = conversation.as_mut() {
                                        conversation.record_tool_result(&request.question, &text);
                                    }
                                    return Ok(Reply { text, routing_degraded:false, needs_followup:false });
                                }
                                if let Some(actions) = &actions
                                    && (tools.is_none() || jarvis::actions::recognize(&request.question).is_some())
                                    && let Some(result) = actions.handle(&request.question).await {
                                    if let Ok(result) = &result
                                        && let Some(conversation) = conversation.as_mut() {
                                        conversation.record_action_result(&request.question, result);
                                    }
                                    // Tool failures have static, actionable text. They
                                    // are never provider errors and are never retried.
                                    return Ok(Reply {
                                        text: result.map(|result| result.text).unwrap_or_else(|error| jarvis::actions::friendly_error(&request.question, &error)),
                                        routing_degraded: false,
                                        needs_followup: false,
                                    });
                                }
                                match conversation.as_mut() {
                                    Some(conversation) => ask_with_retry(conversation, &request.question, &worker_control).await,
                                    None => Err(initialization_error.unwrap_or(ConversationError::Configuration)),
                                }
                            },
                        );
                        tokio::pin!(ask);
                        loop {
                            tokio::select! {
                                biased;
                                _ = cancelled.changed() => {
                                    if worker_control.stopping.load(Ordering::Acquire) || *cancelled.borrow() != request.epoch { break None; }
                                }
                                result = &mut ask => {
                                    break Some(result.unwrap_or(Err(ConversationError::Provider(ProviderError::Timeout))));
                                }
                            }
                        }
                    }; // Dropping ask on cancellation prevents successful-turn history commit.
                    match result {
                        None => {
                            if let Some(conversation) = conversation.as_mut() {
                                conversation.clear_pending_clarification();
                            }
                            worker_control.update(State::Cancelled, "Request cancelled. File awareness is unchanged.", false);
                        },
                        Some(Ok(reply)) => {
                            worker_control.update(State::Reply, &reply.text, reply.routing_degraded);
                            worker_control.snapshot.lock().unwrap_or_else(|p|p.into_inner()).needs_followup = reply.needs_followup;
                        },
                        Some(Err(error)) => worker_control.update(State::Failed, error_message(error), false),
                    }
                    worker_control.busy.store(false, Ordering::Release);
                }
                worker_control.ready.store(false, Ordering::Release);
                worker_control.busy.store(false, Ordering::Release);
                worker_control.update(State::Stopped, "Conversation stopped.", false);
            });
        }).map_err(|_| "Could not start conversation worker")?;
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
                .map_err(|_| "Conversation worker terminated unexpectedly")?;
        }
        Ok(())
    }
}

/// Provider failures are retryable only before a side effect is committed.
/// Conversation returns authoritative side-effect acknowledgments immediately,
/// without a subsequent model call that could fail and trigger a repeated write.
/// The caller's timeout and cancellation select wrap all attempts and delays.
async fn ask_with_retry(
    conversation: &mut Conversation,
    question: &str,
    control: &Control,
) -> Result<Reply, ConversationError> {
    let started = Instant::now();
    for attempt in 0..3 {
        let result = conversation.ask(question).await;
        let Err(ConversationError::Provider(error)) = &result else {
            return result;
        };
        let limited = matches!(
            error,
            ProviderError::RateLimited | ProviderError::RateLimitedAfter(_)
        );
        // One retry for connection failures; at most two for rate limits.
        // Never retry earlier than the provider permits just to fit a deadline.
        let Some(delay) = error.retry_delay(attempt) else {
            return result;
        };
        if attempt >= if limited { 2 } else { 1 }
            || delay >= REQUEST_TIMEOUT.saturating_sub(started.elapsed())
        {
            return result;
        }
        let message = if limited {
            format!(
                "Crusoe is busy. Keeping your question and retrying in {} seconds…",
                delay
                    .as_secs()
                    .saturating_add(u64::from(delay.subsec_nanos() > 0))
                    .max(1)
            )
        } else {
            "The model connection had a temporary problem. Retrying once…".into()
        };
        control.update(State::Thinking, &message, false);
        tokio::time::sleep(delay).await;
    }
    unreachable!("the third attempt always returns")
}

// Do not interpolate configuration values, provider response bodies, or
// transport errors into UI text. Each message identifies only the safe class.
fn error_message(error: ConversationError) -> &'static str {
    match error {
        ConversationError::MissingKey("CRUSOE_API_KEY") => {
            "CRUSOE_API_KEY is missing from configuration. Add it to .env and restart Jarvis. File awareness still works."
        }
        ConversationError::MissingKey(_) => {
            "A required provider setting is missing. Check local configuration and restart Jarvis. File awareness still works."
        }
        ConversationError::Configuration => {
            "Jarvis couldn't read the local .env configuration. Check that it is an accessible regular file. File awareness still works."
        }
        ConversationError::ConfigurationTooLarge => {
            "The local .env configuration exceeds the 16 KiB limit. Reduce its size and restart Jarvis. File awareness still works."
        }
        ConversationError::ConfigurationEncoding => {
            "The local .env configuration must use UTF-8 text. Save it as UTF-8 and restart Jarvis. File awareness still works."
        }
        ConversationError::InvalidQuestion => {
            "That request is empty, invalid, or too long. Please shorten it and try again."
        }
        ConversationError::Context => {
            "Jarvis couldn't read its local context index. Check the database location and access, then try again."
        }
        ConversationError::Provider(provider) => match provider {
            ProviderError::InvalidConfiguration => {
                "The model provider configuration is invalid. Check the configured model and credential format, then restart Jarvis."
            }
            ProviderError::InvalidInput => {
                "The model request exceeded its input limits or contained invalid text. Please shorten the request."
            }
            ProviderError::Authentication => {
                "Model provider authentication failed. Check access in your provider account settings."
            }
            ProviderError::RateLimited | ProviderError::RateLimitedAfter(_) => {
                "The model provider is limiting requests. Please try again shortly."
            }
            ProviderError::ServerUnavailable => {
                "The model provider is temporarily unavailable. Please try again shortly."
            }
            ProviderError::Rejected => {
                "The model provider rejected this request. Check that the configured model is available for your account."
            }
            ProviderError::Transport => {
                "Jarvis couldn't reach the model provider. Check your network connection and try again."
            }
            ProviderError::Timeout => {
                "The model provider took too long to answer. Please try again."
            }
            ProviderError::ResponseTooLarge => {
                "The model response exceeded the size limit. Please ask for a shorter answer."
            }
            ProviderError::InvalidResponse => {
                "The model provider returned an unreadable response. Please try again."
            }
            ProviderError::IncompleteOutput => {
                "The model response was cut short. Please ask for a shorter answer."
            }
        },
    }
}
impl Drop for ConversationWorker {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

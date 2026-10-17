//! One cancellable conversation worker, independent of file awareness.
use jarvis::conversation::{Conversation, ConversationError};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};
use tokio::sync::{mpsc, watch};

pub const MAX_QUESTION_BYTES: usize = 8192;
pub const MAX_DISPLAY_BYTES: usize = 32768;

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
    pub fn cancel(&self) {
        if self.busy() {
            self.update(State::Cancelling, "Cancelling the current request…", false);
            self.cancellation
                .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
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
        snapshot.revision = snapshot.revision.wrapping_add(1);
    }
}

pub struct ConversationWorker {
    pub control: Arc<Control>,
    handle: Option<JoinHandle<()>>,
}
impl ConversationWorker {
    pub fn start(database: PathBuf) -> Result<Self, &'static str> {
        Self::with_factory(move || Conversation::from_env(database))
    }
    /// Test injection accepts real Conversation clients pointing at loopback fixtures.
    pub fn with_factory(
        factory: impl FnOnce() -> Result<Conversation, ConversationError> + Send + 'static,
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
                let mut conversation = match factory() {
                    Ok(conversation) => conversation,
                    Err(_) => {
                        worker_control.update(State::Unavailable, "Add CRUSOE_API_KEY to .env and check provider settings, then restart Jarvis. File awareness still works.", false);
                        return;
                    }
                };
                worker_control.ready.store(true, Ordering::Release);
                worker_control.update(State::Ready, "Ask about your work or the files you approved. Selected context is sent to your configured model provider.", false);
                loop {
                    if worker_control.stopping.load(Ordering::Acquire) { break; }
                    let request = tokio::select! {
                        biased;
                        _ = cancelled.changed() => { continue; }
                        request = receiver.recv() => { match request { Some(request) => request, None => break } }
                    };
                    if worker_control.stopping.load(Ordering::Acquire) { break; }
                    if *cancelled.borrow() != request.epoch {
                        worker_control.update(State::Cancelled, "Request cancelled. File awareness is unchanged.", false);
                        worker_control.busy.store(false, Ordering::Release);
                        continue;
                    }
                    worker_control.update(State::Thinking, "Thinking with relevant context…", false);
                    let result = {
                        let ask = conversation.ask(&request.question);
                        tokio::pin!(ask);
                        loop {
                            tokio::select! {
                                biased;
                                _ = cancelled.changed() => {
                                    if worker_control.stopping.load(Ordering::Acquire) || *cancelled.borrow() != request.epoch { break None; }
                                }
                                result = &mut ask => { break Some(result); }
                            }
                        }
                    }; // Dropping ask on cancellation prevents successful-turn history commit.
                    match result {
                        None => worker_control.update(State::Cancelled, "Request cancelled. File awareness is unchanged.", false),
                        Some(Ok(reply)) => worker_control.update(State::Reply, &reply.text, reply.routing_degraded),
                        Some(Err(_)) => worker_control.update(State::Failed, "I couldn't complete that request. Check the connection and provider settings, then try again.", false),
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
impl Drop for ConversationWorker {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

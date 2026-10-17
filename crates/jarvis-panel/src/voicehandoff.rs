//! Shared voice-to-conversation-to-speech handoff used by the native panel.
use crate::{chat, codingworker, speech, tasks, wakeworker};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Conversation,
    Draft,
    Review,
    CodingReview,
}

/// Only the caller's request reaches this dispatcher. Model replies are never
/// interpreted as executable requests, filenames, or review authorization.
pub fn dispatch_question(
    question: String,
    conversation: &chat::Control,
    task: &tasks::Control,
) -> Result<Dispatch, &'static str> {
    let normalized = question
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "open the draft"
            | "review the draft"
            | "open my draft"
            | "show the draft"
            | "open result"
            | "open the result"
            | "review the result"
            | "open the file"
    ) {
        task.request_review()
            .map_err(|_| "There isn't a verified draft ready to open yet.")?;
        Ok(Dispatch::Review)
    } else if tasks::is_draft_request(&question) {
        task.submit(question).map_err(|error| match error {
            tasks::SubmitError::Busy => {
                "I'm already preparing a draft. You can keep asking me questions while it runs."
            }
            _ => "I couldn't start that draft. Check the draft status in Details.",
        })?;
        Ok(Dispatch::Draft)
    } else {
        conversation
            .submit(question)
            .map_err(|_| "I couldn't start that conversation request. Please try again.")?;
        Ok(Dispatch::Conversation)
    }
}

/// Exact review commands refer to a verified retained result, never a model path.
pub fn dispatch_question_with_coding(
    question: String,
    conversation: &chat::Control,
    task: &tasks::Control,
    coding: Option<&codingworker::Control>,
) -> Result<Dispatch, &'static str> {
    let normalized = question
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "open code review" | "open the code review" | "review the code" | "show the code review"
    ) {
        coding
            .ok_or("Choose a coding project first.")?
            .request_review()
            .map_err(|_| "The coding result is not ready for review yet.")?;
        return Ok(Dispatch::CodingReview);
    }
    dispatch_question(question, conversation, task)
}

struct SpokenQuestion {
    speech_revision: u64,
    speech_epoch: u64,
    wake_generation: u64,
    chat_epoch: u64,
    chat_revision: u64,
}
#[derive(Default)]
pub struct VoiceHandoff {
    pending_reply_after: Option<(u64, u64)>,
    pending_task_notice: bool,
    spoken_question: Option<SpokenQuestion>,
    active_followup: Option<(u64, u64)>,
    task_revision: Option<u64>,
    coding_revision: Option<u64>,
    pending_coding_notice: bool,
    last_microphone: Option<String>,
    pending_microphone_notice: Option<String>,
    visible_microphone_notice: Option<(String, Instant)>,
    microphone_reveal: bool,
    last_wake_matches: Option<u64>,
}

impl VoiceHandoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// The native card displays the same bounded notice that speech announces.
    pub fn microphone_notice(&self) -> Option<&str> {
        self.visible_microphone_notice
            .as_ref()
            .and_then(|(text, until)| (Instant::now() < *until).then_some(text.as_str()))
    }
    pub fn take_microphone_reveal(&mut self) -> bool {
        std::mem::take(&mut self.microphone_reveal)
    }
    fn observe_microphone(&mut self, snapshot: &wakeworker::Snapshot) {
        let Some(input) = &snapshot.diagnostics.input else {
            return;
        };
        if input.name.is_empty()
            || input.name.len() > jarvis_voice::capture::MAX_INPUT_NAME_BYTES
            || input.name.chars().any(char::is_control)
        {
            return;
        }
        if self.last_microphone.as_deref() == Some(input.name.as_str()) {
            return;
        }
        let Some(previous) = self.last_microphone.replace(input.name.clone()) else {
            return;
        };
        let name = input.name.to_ascii_lowercase();
        let old = previous.to_ascii_lowercase();
        // Fixed copy keeps arbitrary device labels out of spoken/provider text.
        let notice = if old.contains("airpods") && name.contains("macbook") {
            if snapshot.message
                == "AirPods microphone stopped responding. Using the MacBook microphone."
            {
                "AirPods microphone stopped responding. Using the MacBook microphone."
            } else {
                "I switched from AirPods to the MacBook microphone."
            }
        } else if name.contains("airpods") {
            "I'm now using the AirPods microphone."
        } else if name.contains("macbook") {
            "I'm now using the MacBook microphone."
        } else {
            "Microphone changed. I'm now using the selected microphone."
        };
        self.pending_microphone_notice = Some(notice.to_owned());
    }

    /// Advance without blocking. Returns the heard command for the transcript UI.
    pub fn tick(
        &mut self,
        wake: &wakeworker::Control,
        conversation: &chat::Control,
        playback: &speech::Control,
    ) -> Option<String> {
        self.tick_inner(wake, conversation, playback, None, None)
    }

    pub fn tick_with_tasks(
        &mut self,
        wake: &wakeworker::Control,
        conversation: &chat::Control,
        playback: &speech::Control,
        tasks: &tasks::Control,
    ) -> Option<String> {
        self.tick_inner(wake, conversation, playback, Some(tasks), None)
    }

    pub fn tick_with_workers(
        &mut self,
        wake: &wakeworker::Control,
        conversation: &chat::Control,
        playback: &speech::Control,
        tasks: &tasks::Control,
        coding: Option<&codingworker::Control>,
    ) -> Option<String> {
        self.tick_inner(wake, conversation, playback, Some(tasks), coding)
    }
    fn tick_inner(
        &mut self,
        wake: &wakeworker::Control,
        conversation: &chat::Control,
        playback: &speech::Control,
        tasks: Option<&tasks::Control>,
        coding: Option<&codingworker::Control>,
    ) -> Option<String> {
        let reply = conversation.snapshot();
        let wake_snapshot = wake.snapshot();
        let matches = wake_snapshot.diagnostics.wake_matches;
        let interrupted = self
            .last_wake_matches
            .is_some_and(|previous| matches > previous)
            && playback.busy();
        self.last_wake_matches = Some(matches);
        if interrupted {
            playback.stop_speaking();
            self.spoken_question = None;
            self.active_followup = None;
            self.pending_reply_after = None;
        }
        self.observe_microphone(&wake_snapshot);
        if conversation.busy()
            || matches!(
                wake_snapshot.state,
                wakeworker::State::Suppressed
                    | wakeworker::State::CommandReady
                    | wakeworker::State::CommandListening
                    | wakeworker::State::Decoding
            )
        {
            self.visible_microphone_notice = None;
            self.microphone_reveal = false;
        }
        if self
            .active_followup
            .is_some_and(|(chat_epoch, speech_epoch)| {
                chat_epoch != conversation.cancellation_epoch()
                    || speech_epoch != playback.cancellation_epoch()
            })
        {
            wake.cancel_followup();
            self.active_followup = None;
        }
        if self.active_followup.is_some() && !wake.followup_active() {
            self.active_followup = None;
        }
        if self
            .pending_reply_after
            .is_some_and(|(_, epoch)| epoch != conversation.cancellation_epoch())
        {
            self.pending_reply_after = None;
        }
        if let Some(task) = tasks {
            let snapshot = task.snapshot();
            if self
                .task_revision
                .is_some_and(|revision| revision != snapshot.revision)
            {
                match snapshot.state {
                    tasks::State::Running
                    | tasks::State::ReadyForReview
                    | tasks::State::Blocked
                    | tasks::State::Failed => self.pending_task_notice = true,
                    tasks::State::Cancelled | tasks::State::Stopped => {
                        self.pending_task_notice = false
                    }
                    _ => {}
                }
            }
            // Initial recovered results are visible in Details, not newly completed work.
            self.task_revision = Some(snapshot.revision);
        }
        if let Some(coding) = coding {
            let snapshot = coding.snapshot();
            if self
                .coding_revision
                .is_some_and(|revision| revision != snapshot.revision)
            {
                match snapshot.state {
                    tasks::State::Running
                    | tasks::State::ReadyForReview
                    | tasks::State::Blocked
                    | tasks::State::Failed => self.pending_coding_notice = true,
                    tasks::State::Cancelled | tasks::State::Stopped => {
                        self.pending_coding_notice = false
                    }
                    _ => {}
                }
            }
            self.coding_revision = Some(snapshot.revision);
        }
        if let Some(question) = &self.spoken_question {
            let valid = question.speech_epoch == playback.cancellation_epoch()
                && question.chat_epoch == conversation.cancellation_epoch()
                && question.chat_revision == reply.revision
                && question.wake_generation == wake.generation()
                && wake.enabled();
            if !valid {
                self.spoken_question = None;
            } else if !playback.busy() {
                let state = playback.snapshot();
                if state.state == speech::State::Idle && state.revision != question.speech_revision
                {
                    // Only successful completion of this owned clarification may
                    // reopen capture. Suppression's generation reset clears TTS echo.
                    wake.set_playback_active(false);
                    if wake.arm_followup() {
                        self.active_followup = Some((question.chat_epoch, question.speech_epoch));
                    }
                }
                self.spoken_question = None;
            }
        }
        if self
            .pending_reply_after
            .is_some_and(|(revision, _)| revision != reply.revision)
        {
            match reply.state {
                chat::State::Reply | chat::State::Failed | chat::State::Unavailable
                    if !conversation.busy()
                        && !playback.busy()
                        && !matches!(
                            wake.snapshot().state,
                            wakeworker::State::CommandListening
                                | wakeworker::State::Decoding
                                | wakeworker::State::CommandReady
                        ) =>
                {
                    let mut end = reply.text.len().min(speech::MAX_TEXT_BYTES);
                    while !reply.text.is_char_boundary(end) {
                        end -= 1;
                    }
                    let speech_revision = playback.snapshot().revision;
                    if playback.speak(reply.text[..end].to_owned()).is_ok()
                        && reply.state == chat::State::Reply
                        && reply.needs_followup
                    {
                        wake.set_playback_active(true);
                        self.spoken_question = Some(SpokenQuestion {
                            speech_revision,
                            speech_epoch: playback.cancellation_epoch(),
                            wake_generation: wake.generation(),
                            chat_epoch: conversation.cancellation_epoch(),
                            chat_revision: reply.revision,
                        });
                    }
                    self.pending_reply_after = None;
                }
                chat::State::Cancelled | chat::State::Stopped => {
                    self.pending_reply_after = None;
                }
                _ => {}
            }
        }

        if self.pending_task_notice
            && self.active_followup.is_none()
            && !matches!(
                wake.snapshot().state,
                wakeworker::State::Suppressed
                    | wakeworker::State::CommandReady
                    | wakeworker::State::CommandListening
                    | wakeworker::State::Decoding
            )
            && !conversation.busy()
            && !playback.busy()
            && let Some(task) = tasks
            && !task.busy()
        {
            let snapshot = task.snapshot();
            match snapshot.state {
                tasks::State::ReadyForReview | tasks::State::Blocked | tasks::State::Failed => {
                    if playback.speak(snapshot.text).is_ok() {
                        self.pending_task_notice = false;
                    }
                }
                tasks::State::Cancelled | tasks::State::Stopped => self.pending_task_notice = false,
                _ => {}
            }
        }

        if self.pending_coding_notice
            && self.active_followup.is_none()
            && !matches!(
                wake.snapshot().state,
                wakeworker::State::Suppressed
                    | wakeworker::State::CommandReady
                    | wakeworker::State::CommandListening
                    | wakeworker::State::Decoding
            )
            && !conversation.busy()
            && !playback.busy()
            && let Some(coding) = coding
            && !coding.busy()
        {
            let snapshot = coding.snapshot();
            match snapshot.state {
                tasks::State::ReadyForReview | tasks::State::Blocked | tasks::State::Failed => {
                    if playback.speak(snapshot.text).is_ok() {
                        self.pending_coding_notice = false;
                    }
                }
                tasks::State::Cancelled | tasks::State::Stopped => {
                    self.pending_coding_notice = false
                }
                _ => {}
            }
        }
        if self.pending_microphone_notice.is_some()
            && self.active_followup.is_none()
            && self.spoken_question.is_none()
            && self.pending_reply_after.is_none()
            && wake.enabled()
            && wake.snapshot().state == wakeworker::State::WakeListening
            && !conversation.busy()
            && !playback.busy()
            && let Some(mut notice) = self.pending_microphone_notice.take()
        {
            // A queued route change may wait behind speech. Read its fixed
            // recovery reason at delivery rather than freezing older UI copy.
            if notice == "I switched from AirPods to the MacBook microphone."
                && wake.snapshot().message
                    == "AirPods microphone stopped responding. Using the MacBook microphone."
            {
                notice =
                    "AirPods microphone stopped responding. Using the MacBook microphone.".into();
            }
            // Visible delivery remains available if the configured speech service
            // is unavailable. Never repeat or interrupt an active user request.
            let _ = playback.speak(notice.clone());
            self.visible_microphone_notice =
                Some((notice, Instant::now() + Duration::from_secs(8)));
            self.microphone_reveal = true;
        }
        let command = if !conversation.busy() && !playback.busy() {
            wake.take_command()
        } else {
            None
        };
        if let Some(command) = &command {
            self.active_followup = None;
            if let Some(tasks) = tasks {
                match dispatch_question_with_coding(command.clone(), conversation, tasks, coding) {
                    Ok(Dispatch::Conversation) => {
                        self.pending_reply_after =
                            Some((reply.revision, conversation.cancellation_epoch()))
                    }
                    Ok(Dispatch::Draft) => {
                        self.pending_task_notice = true;
                        let _ = playback.speak("I'll prepare that draft in the background. You can keep talking to me.".into());
                    }
                    Ok(Dispatch::Review) => self.pending_task_notice = true,
                    Ok(Dispatch::CodingReview) => self.pending_coding_notice = true,
                    Err(message) => {
                        let _ = playback.speak(message.into());
                    }
                }
            } else if conversation.submit(command.clone()).is_ok() {
                self.pending_reply_after =
                    Some((reply.revision, conversation.cancellation_epoch()));
            }
        }
        // Keep the dedicated keyword spotter active during owned playback.
        // Stopping playback must not invalidate the interruption it just heard.
        wake.set_playback_active(playback.busy());
        command
    }
}

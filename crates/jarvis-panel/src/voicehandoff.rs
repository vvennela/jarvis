//! Shared voice-to-conversation-to-speech handoff used by the native panel.
use crate::{chat, speech, tasks, wakeworker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Conversation,
    Draft,
    Review,
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
}

impl VoiceHandoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance without blocking. Returns the heard command for the transcript UI.
    pub fn tick(
        &mut self,
        wake: &wakeworker::Control,
        conversation: &chat::Control,
        playback: &speech::Control,
    ) -> Option<String> {
        self.tick_inner(wake, conversation, playback, None)
    }

    pub fn tick_with_tasks(
        &mut self,
        wake: &wakeworker::Control,
        conversation: &chat::Control,
        playback: &speech::Control,
        tasks: &tasks::Control,
    ) -> Option<String> {
        self.tick_inner(wake, conversation, playback, Some(tasks))
    }

    fn tick_inner(
        &mut self,
        wake: &wakeworker::Control,
        conversation: &chat::Control,
        playback: &speech::Control,
        tasks: Option<&tasks::Control>,
    ) -> Option<String> {
        let reply = conversation.snapshot();
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
                    wake.suppress(false);
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
                    if !conversation.busy() && !playback.busy() =>
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
                        wake.suppress(true);
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
                wakeworker::State::CommandReady
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

        let command = if !conversation.busy() && !playback.busy() {
            wake.take_command()
        } else {
            None
        };
        if let Some(command) = &command {
            self.active_followup = None;
            if let Some(tasks) = tasks {
                match dispatch_question(command.clone(), conversation, tasks) {
                    Ok(Dispatch::Conversation) => {
                        self.pending_reply_after =
                            Some((reply.revision, conversation.cancellation_epoch()))
                    }
                    Ok(Dispatch::Draft) => {
                        self.pending_task_notice = true;
                        let _ = playback.speak("I'll prepare that draft in the background. You can keep talking to me.".into());
                    }
                    Ok(Dispatch::Review) => self.pending_task_notice = true,
                    Err(message) => {
                        let _ = playback.speak(message.into());
                    }
                }
            } else if conversation.submit(command.clone()).is_ok() {
                self.pending_reply_after =
                    Some((reply.revision, conversation.cancellation_epoch()));
            }
        }
        // Stop capture during owned output and clear buffered audio before resuming.
        wake.suppress(playback.busy() || conversation.busy());
        command
    }
}

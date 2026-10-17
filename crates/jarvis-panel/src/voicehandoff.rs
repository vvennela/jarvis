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
        "open the draft" | "review the draft" | "open my draft" | "show the draft"
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

#[derive(Default)]
pub struct VoiceHandoff {
    pending_reply_after: Option<u64>,
    pending_task_notice: bool,
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
            .pending_reply_after
            .is_some_and(|revision| revision != reply.revision)
        {
            match reply.state {
                chat::State::Reply | chat::State::Failed | chat::State::Unavailable
                    if !conversation.busy() && !playback.busy() =>
                {
                    let mut end = reply.text.len().min(speech::MAX_TEXT_BYTES);
                    while !reply.text.is_char_boundary(end) {
                        end -= 1;
                    }
                    let _ = playback.speak(reply.text[..end].to_owned());
                    self.pending_reply_after = None;
                }
                chat::State::Cancelled | chat::State::Stopped => {
                    self.pending_reply_after = None;
                }
                _ => {}
            }
        }

        if self.pending_task_notice
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
            if let Some(tasks) = tasks {
                match dispatch_question(command.clone(), conversation, tasks) {
                    Ok(Dispatch::Conversation) => self.pending_reply_after = Some(reply.revision),
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
                self.pending_reply_after = Some(reply.revision);
            }
        }
        // Stop capture during owned output and clear buffered audio before resuming.
        wake.suppress(playback.busy() || conversation.busy());
        command
    }
}

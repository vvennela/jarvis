//! Shared voice-to-conversation-to-speech handoff used by the native panel.
use crate::{chat, speech, wakeworker};

#[derive(Default)]
pub struct VoiceHandoff {
    pending_reply_after: Option<u64>,
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

        let command = if !conversation.busy() && !playback.busy() {
            wake.take_command()
        } else {
            None
        };
        if let Some(command) = &command
            && conversation.submit(command.clone()).is_ok()
        {
            self.pending_reply_after = Some(reply.revision);
        }
        // Stop capture during owned output and clear buffered audio before resuming.
        wake.suppress(playback.busy() || conversation.busy());
        command
    }
}

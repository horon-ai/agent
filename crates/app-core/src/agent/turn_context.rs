//! Reminders a request carries are stored in the transcript as they are sent.
//!
//! The turn context and the memory summary reach the model as reminders
//! appended at the end of the transcript, never placed in front of the current
//! turn or attached to one request only: each request then starts with every
//! message of the previous one, which keeps a provider's prompt cache and a
//! gateway that continues one upstream conversation per session on the same
//! conversation.

use super::Agent;
use crate::message::{Message, Role};
use crate::session::StoredDisplayRole;

#[cfg(test)]
#[path = "turn_context_tests.rs"]
pub(crate) mod tests;

impl Agent {
    /// Store this request's turn context in the transcript when it changed and
    /// return the copy to send.
    ///
    /// The dynamic system context (working notes, this turn's reminder, an
    /// approved plan, an active skill) is appended as a reminder; an unchanged
    /// context adds nothing.
    pub(super) fn store_turn_context(&mut self, messages: &[Message], system_dynamic: &str) -> Option<Message> {
        let reminder = crate::message::turn_context_update(messages, system_dynamic)?;
        self.add_message_with_display_role(Role::User, reminder.content.clone(), Some(StoredDisplayRole::System));
        Some(reminder)
    }

    /// The memory summary to send with this request and whether it was stored,
    /// or `None` when the transcript already carries the same summary.
    ///
    /// A persisted summary joins the transcript once and is sent again only
    /// when it changed. A request-only summary (`persist_memory_injections =
    /// false`) is gone from the next request, which a gateway that continues
    /// one upstream conversation per session reads as a rewritten history.
    pub(super) fn prepare_memory_injection_message(
        &mut self,
        messages: &[Message],
        memory: &crate::memory::PendingMemory,
    ) -> Option<(Message, bool)> {
        let text = format!("<system-reminder>\n{}\n</system-reminder>", memory.prompt);
        if !crate::config::config().features.persist_memory_injections {
            return Some((Message::user(&text), false));
        }
        let message = crate::message::reminder_update(messages, &text)?;
        self.add_message_with_display_role(Role::User, message.content.clone(), Some(StoredDisplayRole::System));
        self.persist_session_best_effort("persisted memory injection message");
        Some((message, true))
    }
}

//! Which stored messages count as visible conversation.
//!
//! A transcript carries internal system reminders and scheduler prompts that
//! predate an explicit display role. Both stay out of the conversation view, so
//! the readers that project a session agree on the same filter.

use super::{ContentBlock, Role, StoredMessage};

/// Recognize scheduler prompts persisted before they received an explicit
/// system display role. This keeps old sessions from treating them as user
/// prompts after resume.
pub fn is_scheduled_task_message(message: &StoredMessage) -> bool {
    message.role == Role::User
        && message.content.iter().any(|block| {
            matches!(block, ContentBlock::Text { text, .. } if text.trim_start().starts_with("[Scheduled task]\n"))
        })
}

//! Per-turn token accounting for the `turn_end` hook.

use crate::logging;
use crate::message::Role;
use crate::session::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TurnTokenUsage {
    pub(super) prompt_tokens: Option<u64>,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) cache_read_input_tokens: Option<u64>,
    pub(super) cache_creation_input_tokens: Option<u64>,
}

pub(super) fn session_turn_token_usage_after(session: &Session, start_message_index: usize) -> Option<TurnTokenUsage> {
    let history = match session.history_messages() {
        Ok(history) => history,
        Err(error) => {
            logging::warn(&format!("Cannot read turn usage history: {error}"));
            return None;
        }
    };
    turn_token_usage_after(&history, start_message_index)
}

pub(super) fn turn_token_usage_after(
    messages: &[crate::session::StoredMessage],
    start_message_index: usize,
) -> Option<TurnTokenUsage> {
    let mut usage = TurnTokenUsage {
        prompt_tokens: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
    };
    let mut saw_usage = false;

    for message in messages.iter().skip(start_message_index) {
        if !matches!(&message.role, Role::Assistant) {
            continue;
        }
        let Some(message_usage) = message.token_usage.as_ref() else {
            continue;
        };

        saw_usage = true;
        let add = |acc: Option<u64>, v: Option<u64>| v.map(|v| acc.unwrap_or(0).saturating_add(v)).or(acc);
        usage.prompt_tokens = add(usage.prompt_tokens, message_usage.prompt_tokens);
        usage.input_tokens = usage.input_tokens.saturating_add(message_usage.input_tokens);
        usage.output_tokens = usage.output_tokens.saturating_add(message_usage.output_tokens);
        usage.cache_read_input_tokens = add(usage.cache_read_input_tokens, message_usage.cache_read_input_tokens);
        usage.cache_creation_input_tokens = add(
            usage.cache_creation_input_tokens,
            message_usage.cache_creation_input_tokens,
        );
    }

    saw_usage.then_some(usage)
}

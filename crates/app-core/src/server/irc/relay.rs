//! Relay a wake turn's final answer back to the IRC sender when the woken agent
//! did not answer directly.

use horon_agent_irc_core::{AgentId, IrcMessage, SendRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::oneshot::Receiver;

/// Spawn the relay for one wake turn and release its pending-relay mark when done.
pub(super) fn spawn(
    receiver: Receiver<Option<String>>,
    message: IrcMessage,
    pending: Arc<StdMutex<HashMap<AgentId, usize>>>,
) {
    tokio::spawn(async move {
        if let Ok(Some(text)) = receiver.await
            && let Some(bus) = horon_agent_irc_core::global::get()
            && !bus.sent_since(&message.to, &message.from, message.seq)
        {
            let body = truncate_utf8(&text, bus.limits().max_body_bytes);
            let request = SendRequest {
                from: Arc::clone(&message.to),
                to: message.from.to_string(),
                body,
                reply_to: Some(message.id),
                progress: false,
                wake_relay: true,
            };
            if let Err(error) = bus.send(request).await {
                crate::logging::warn(&format!(
                    "IRC wake relay from {} to {} failed: {error}",
                    message.to, message.from
                ));
            }
        }
        let mut relays = match pending.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(count) = relays.get_mut(&message.to) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                relays.remove(&message.to);
            }
        }
    });
}

/// Clamp `text` to `max` bytes without splitting a UTF-8 character.
fn truncate_utf8(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

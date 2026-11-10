//! `get_message_modes`: report the daemon's `[messages]` delivery modes.
//!
//! The three values are daemon configuration, not session state, so the bridge
//! answers this request without a daemon round trip and without checking that
//! the named session exists. Keeping the answer here also keeps it available on
//! a daemon that predates the request, which is the point of reporting the
//! configuration a client observes rather than asking the daemon to echo it.

use horon_agent_harness_api::{ApiEvent, MessageModes, ServerFrame};
use serde_json::Value;

/// Answer `get_message_modes`, or `None` for every other request.
pub(crate) fn reply(request: &Value) -> Option<ServerFrame> {
    if request["req"] != "get_message_modes" {
        return None;
    }
    let reply_to = request["id"].as_u64()?;
    let session_id = match request["session_id"].as_str() {
        Some(session_id) => session_id.to_string(),
        None => String::new(),
    };
    let config = horon_agent_base::config::config().messages;
    Some(ServerFrame::reply(
        reply_to,
        ApiEvent::MessageModes {
            session_id,
            modes: MessageModes {
                follow_up_mode: config.follow_up_mode.as_str().to_string(),
                steering_mode: config.steering_mode.as_str().to_string(),
                interrupt_mode: config.interrupt_mode.as_str().to_string(),
            },
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_get_message_modes_request_is_answered_with_the_effective_modes() {
        let frame = reply(&json!({"v": 1, "id": 7, "req": "get_message_modes", "session_id": "s1"}))
            .expect("the request is answered");

        assert_eq!(frame.reply_to, Some(7));
        match frame.event {
            ApiEvent::MessageModes { session_id, modes } => {
                assert_eq!(session_id, "s1");
                assert_eq!(modes.follow_up_mode, "all", "the shipped default");
                assert_eq!(modes.steering_mode, "all", "the shipped default");
                assert_eq!(modes.interrupt_mode, "wait", "the timing horon already has");
            }
            other => panic!("expected message_modes, got {other:?}"),
        }
    }

    #[test]
    fn any_other_request_is_left_to_the_translator() {
        assert!(reply(&json!({"v": 1, "id": 7, "req": "list_sessions"})).is_none());
    }

    #[test]
    fn a_request_without_an_id_is_not_answered() {
        assert!(
            reply(&json!({"v": 1, "req": "get_message_modes", "session_id": "s1"})).is_none(),
            "without an id there is nothing to reply to, so the daemon's normal error path handles it"
        );
    }
}

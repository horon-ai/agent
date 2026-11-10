//! Emitting the repair event an API client sees.
//!
//! The dispatcher knows a call was repaired but has no event sender, and the
//! turn loop has the sender but only the session and call id. The record left
//! under that key bridges them, so this stays a lookup rather than a parameter
//! threaded through every dispatch path.

use crate::protocol::ServerEvent;
use horon_agent_session_types::repair_record::peek;

/// Send `tool_args_repaired` for `call_id` of `session_id` if its arguments
/// were repaired.
///
/// Called next to the append of the call's result, so it must not consume the
/// record: the session takes it a moment later.
pub(crate) fn send(
    event_tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    session_id: &str,
    call_id: &str,
    name: &str,
) {
    let Some(record) = peek(session_id, call_id) else {
        return;
    };
    // A closed channel means the turn is already over; the record stays for the
    // session, so there is nothing left to do here.
    if event_tx
        .send(ServerEvent::ToolArgsRepaired {
            id: call_id.to_string(),
            name: name.to_string(),
            original_input: record.original_input,
            changes: record.changes,
        })
        .is_err()
    {
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horon_agent_session_types::repair_record::{ArgRepairRecord, remember, take};
    use serde_json::json;

    #[test]
    fn a_repaired_call_sends_one_event_and_leaves_the_record_for_the_session() {
        // Given a call whose arguments a repair rewrote.
        remember(
            "session-a",
            "call-event",
            ArgRepairRecord {
                original_input: json!({"flag": "true"}),
                changes: vec!["coerced `flag` to a boolean".to_string()],
            },
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // When the turn loop announces it.
        send(&tx, "session-a", "call-event", "bash");
        // Then one event goes out carrying the original and the change list...
        match rx.try_recv().expect("one event") {
            ServerEvent::ToolArgsRepaired {
                id,
                name,
                original_input,
                changes,
            } => {
                assert_eq!(id, "call-event");
                assert_eq!(name, "bash");
                assert_eq!(original_input, json!({"flag": "true"}));
                assert_eq!(changes, vec!["coerced `flag` to a boolean".to_string()]);
            }
            other => panic!("expected tool_args_repaired, got {other:?}"),
        }
        assert!(rx.try_recv().is_err());
        // ...and the record is still there for the stored message to claim.
        assert!(take("session-a", "call-event").is_some());
    }

    #[test]
    fn a_call_that_needed_no_repair_sends_nothing() {
        // Given an unrepaired call.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // When the turn loop announces it.
        send(&tx, "session-a", "call-clean", "bash");
        // Then nothing is emitted at all.
        assert!(rx.try_recv().is_err());
    }
}

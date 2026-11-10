//! The `tool_args_repaired` frame.
//!
//! Kept out of [`translate`](crate::translate)'s match so that file, already at
//! its size budget, does not grow for an event nobody always sends.

use horon_agent_harness_api::{ApiEvent, ServerFrame};
use serde_json::Value;

/// Frame for a call whose arguments a schema repair rewrote.
pub(crate) fn frames(session_id: String, event: &Value) -> Vec<ServerFrame> {
    // A malformed list still sends the event: it reports the original arguments
    // and claims nothing about what changed.
    let changes = match serde_json::from_value(event["changes"].clone()) {
        Ok(changes) => changes,
        Err(_) => Vec::new(),
    };
    vec![ServerFrame::event(ApiEvent::ToolArgsRepaired {
        session_id,
        call_id: event["id"].as_str().unwrap_or("").to_string(),
        name: event["name"].as_str().unwrap_or("").to_string(),
        original_input: event["original_input"].clone(),
        changes,
    })]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_frame_carries_the_original_arguments_and_the_change_list() {
        // Given the wire event the turn loop emits.
        let event = json!({
            "id": "call-9",
            "name": "bash",
            "original_input": {"command": "ls", "timeout": "5000"},
            "changes": ["coerced `timeout` to a number"],
        });
        // When it is translated for a client.
        let frames = frames("s-1".to_string(), &event);
        // Then one event goes out with all four fields intact.
        assert_eq!(frames.len(), 1);
        let value = serde_json::to_value(&frames[0]).expect("frame serialises");
        assert_eq!(value["ev"], json!("tool_args_repaired"));
        assert_eq!(value["call_id"], json!("call-9"));
        assert_eq!(value["name"], json!("bash"));
        assert_eq!(value["original_input"], event["original_input"]);
        assert_eq!(value["changes"], event["changes"]);
        assert_eq!(value["session_id"], json!("s-1"));
    }

    #[test]
    fn a_malformed_change_list_degrades_to_empty_rather_than_dropping_the_event() {
        // Given an event whose change list is not a string array.
        let event = json!({"id": "call-9", "name": "bash", "original_input": {}, "changes": 7});
        // When it is translated.
        let value = serde_json::to_value(&frames("s-1".to_string(), &event)[0]).expect("frame");
        // Then the event still arrives, with no claims about what changed.
        assert_eq!(value["ev"], json!("tool_args_repaired"));
        assert_eq!(value["changes"], json!([]));
    }
}

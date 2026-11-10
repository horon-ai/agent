//! The `tool_args_repaired` frame.
//!
//! Kept out of [`translate`](crate::translate)'s match so that file, already at
//! its size budget, does not grow for an event nobody always sends.

use horon_agent_harness_api::{ApiEvent, ServerFrame};
use serde_json::Value;

/// Frame for a call whose arguments a schema repair rewrote.
pub(crate) fn frames(session_id: String, event: &Value) -> Vec<ServerFrame> {
    vec![ServerFrame::event(ApiEvent::ToolArgsRepaired {
        session_id,
        call_id: event["id"].as_str().unwrap_or("").to_string(),
        name: event["name"].as_str().unwrap_or("").to_string(),
        original_input: event["original_input"].clone(),
        changes: serde_json::from_value(event["changes"].clone()).unwrap_or_default(),
    })]
}

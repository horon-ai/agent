//! Reply types for the four cancellable-work requests.

use serde::{Deserialize, Serialize};

/// Reply to `cancel_tool_call` and `cancel_background_terminal`.
///
/// `accepted` says a cancellation was started for `call_id`. A refusal names
/// why in `reason`: `unknown` for a call this session does not know, `final`
/// for one that already ended, and `not_cancellable` for one that exists but
/// cannot be stopped through this request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallCancelOutcome {
    pub call_id: String,
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One background work item that is still running.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackgroundWorkItem {
    /// Tool call that started this work; the id `cancel_tool_call` and
    /// `cancel_background_terminal` take.
    pub call_id: String,
    /// Background task id the daemon assigned.
    pub task_id: String,
    /// `terminal` or `tool_call`.
    pub kind: String,
    /// Command line of a terminal, as the daemon ran it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Working directory of a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The last 64 KiB of output so far, as base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

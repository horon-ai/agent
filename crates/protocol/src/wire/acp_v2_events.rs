//! Payloads for events an ACP v2 connection needs beyond the v1 event surface.
//!
//! These types are wire data only: they carry what the daemon observed, and the
//! ACP v2 adapter turns them into protocol frames. Nothing here decides when an
//! event is sent, so a connection that never negotiated ACP v2 simply never
//! receives one.

use super::*;

/// One background work item's lifecycle transition.
///
/// Sent when the work starts, and again with `state` set to its final outcome
/// when it ends. `output` is base64 of the last 64 KiB the work produced and is
/// attached only to the final transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundWorkEvent {
    /// Tool call that started this work. A `terminal/cancel` names this id.
    pub call_id: String,
    /// Background task id the daemon assigned.
    pub task_id: String,
    /// `terminal` for a bash job whose output is a display-only terminal,
    /// `tool_call` for any other tool the daemon moved to the background.
    pub kind: String,
    /// `started` while the work runs, then `completed`, `failed`, or
    /// `cancelled` for the final transition.
    pub state: String,
    /// Command line of a terminal, as the daemon ran it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Working directory of a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Exit code of a terminal that exited on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Signal that ended a terminal, when a signal ended it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// Last 64 KiB of output as base64, present on the final transition only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// One entry of the live background work list.
///
/// Carried by [`super::events::ServerEvent::BackgroundWorkList`] so a connected
/// editor can rebuild what it missed across a reconnect. `output` is base64 of
/// the last 64 KiB produced so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundWorkItem {
    /// Tool call that started this work.
    pub call_id: String,
    /// Background task id the daemon assigned.
    pub task_id: String,
    /// `terminal` or `tool_call`, as in [`BackgroundWorkEvent::kind`].
    pub kind: String,
    /// Command line of a terminal, as the daemon ran it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Working directory of a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Last 64 KiB of output produced so far, as base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

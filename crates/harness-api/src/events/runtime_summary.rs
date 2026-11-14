//! Durable runtime summaries carried by `RuntimeState` and `ControlReceipt`.
//!
//! A summary only: input content, command arguments and command environment
//! variables never travel here.

use super::HistoryMessage;
use serde::{Deserialize, Serialize};

/// The journal file behind one session's runtime state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeJournalSummary {
    /// A journal exists on disk. `false` with zero counts means no runtime
    /// state has been recorded yet, which is not an error.
    pub exists: bool,
    /// Restart-scoped generation on disk; 0 before the first row.
    pub generation: u64,
    /// Last recorded sequence number.
    pub last_seq: u64,
    /// Bytes the journal occupies.
    pub bytes: u64,
    /// Complete rows recorded.
    pub records: u64,
    /// The journal cannot take another row and needs operator attention.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
}

/// One open run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeRunSummary {
    pub run_id: String,
    /// RFC 3339 UTC.
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_id: Option<String>,
}

/// How much work is queued or in flight. Content is deliberately absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeQueueSummary {
    /// Inputs still holding work, whatever their phase.
    pub live_inputs: u32,
    /// Inputs whose outcome is not settled: started, or sealed in doubt.
    pub undetermined_inputs: u32,
    /// Bytes of accepted content still retained, without the content itself.
    pub live_input_bytes: u64,
}

/// One terminal input receipt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeReceiptSummary {
    pub input_id: String,
    /// `committed` or `cancelled`.
    pub phase: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// One monitor and the one-line text of its last checkpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeMonitorSummary {
    pub monitor_id: String,
    /// Monitor kind, for example `file` or `command`.
    pub kind: String,
    pub stopped: bool,
    /// The monitor's own checkpoint summary, never its spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_summary: Option<String>,
}

/// The most recent run stop.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStopSummary {
    pub run_id: String,
    /// `completed`, `user_cancelled`, `shutdown`, `provider_error`,
    /// `timeout`, `loop_guard`, `limit`, or `interrupted_unknown`.
    pub cause: String,
    /// RFC 3339 UTC.
    pub at: String,
}

/// What is known about one control operation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeControlOutcome {
    /// Recorded and still in flight.
    #[default]
    Applying,
    /// Reached a terminal record; `result` holds what it produced.
    Terminal,
    /// The same operation id had already settled; nothing ran twice.
    Replay,
    /// The id is settled with a different request; nothing changed.
    Conflict,
    /// No such operation was recorded.
    Unknown,
    /// It started but its terminal record could not be written, so its effect
    /// is not knowable. Never retried automatically.
    Indeterminate,
}

/// The recorded state of one control operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RuntimeControlReceipt {
    pub operation_id: String,
    pub action: String,
    pub outcome: RuntimeControlOutcome,
    /// The action's own result once terminal. Never command output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Ready-to-display one-line status.
    pub message: String,
}

/// The boundary a snapshot was taken at.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMarkerInfo {
    /// Restart-scoped identity of the observed state; a restart changes it.
    pub generation: String,
    /// Monotonic sequence within that generation.
    pub seq: u64,
    /// The id the paired `snapshot` reply carries.
    pub marker_id: String,
}

/// One consistent view of runtime state plus a bounded history tail.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionSnapshot {
    pub generation: String,
    pub seq: u64,
    pub marker_id: String,
    pub state: RuntimeStateSummary,
    /// Oldest first, like `History`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<HistoryMessage>,
    /// Opaque continuation for older pages, absent at the start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_history_cursor: Option<String>,
    /// The tail or byte budget cut the history short.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    /// A non-headless user client is connected in the source daemon right now.
    /// Independent of lifecycle `status`, processing, and this API connection.
    /// False when unavailable from an older daemon.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_connected: bool,
    /// Cumulative built-in file-tool changes. Absent when unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_stats: Option<crate::SessionEditStats>,
    pub session_id: String,
    /// Swarm owner this agent reports to, not the transcript's fork parent.
    /// Absent for ordinary sessions and user-created forks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Stable task/role label assigned when spawning or assigning a swarm agent.
    /// Separate from `title`, which remains the user's canonical display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// Last persisted swarm lifecycle status (for example `running`, `ready`,
    /// `completed`, or `failed`). Independent of this connection's `status`.
    /// Clients should tolerate new status strings and missing snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// The effective persisted display title. A custom rename takes precedence
    /// over the generated or imported title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: String,
    /// Size of the session's stored record, in bytes.
    ///
    /// A cheap, monotonic proxy for "how much conversation is in here": a
    /// client can size or sort by it without fetching every transcript, which
    /// is the difference between an instant overview and one that stalls on a
    /// dozen history requests. Approximate by design; `None` when the server
    /// could not determine it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_bytes: Option<u64>,
    /// Whether the user pinned/saved this session. Saved sessions sort before
    /// ordinary sessions in every first-party session picker.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub saved: bool,
    /// Optional label given with `/save <label>`. Pickers display and search it
    /// alongside the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_label: Option<String>,
    /// Persisted transcript update time, used for newest-first ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<i64>,
    /// Most recent active-process timestamp when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at_ms: Option<i64>,
    /// Archived sessions are hidden from the default list but never deleted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRouteInfo {
    pub model: String,
    pub provider: String,
    pub api_method: String,
    pub available: bool,
    pub detail: String,
    /// Tracked turns and prior picker selections, when supplied by the runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::ModelUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextMatch {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub preview: String,
}

/// Summary of one session's durable runtime journal.
///
/// A summary and nothing more. Accepted input content, command arguments, and
/// command environment variables stay in the journal: a status panel needs
/// counts, phases, and causes, and none of those require the text of what was
/// queued.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStateSummary {
    pub journal: RuntimeJournalSummary,
    /// Runs still open, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RuntimeRunSummary>,
    pub queue: RuntimeQueueSummary,
    /// Terminal input receipts, most recent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<RuntimeReceiptSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub monitors: Vec<RuntimeMonitorSummary>,
    /// Wakes produced but not yet acknowledged by a delivered input.
    pub pending_wakes: u32,
    /// Controls recorded but not yet terminal.
    pub pending_controls: u32,
    /// The most recent stop across all runs, so a client can say why the last
    /// turn ended without reading the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stop: Option<RuntimeStopSummary>,
}

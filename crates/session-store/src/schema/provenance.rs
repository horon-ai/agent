//! Provenance of an explicit session reset (Clear).

use super::EventId;
use horon_agent_time::Timestamp;
use serde::{Deserialize, Serialize};

/// Provenance of an explicit session reset (Clear) that starts a fresh branch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResetProvenance {
    /// When the reset was recorded.
    pub reset_at: Timestamp,
    /// The head that was active when the reset happened, for audit only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_head_event_id: Option<EventId>,
    /// Why the reset happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

//! What a handoff compaction request did.

use serde::{Deserialize, Serialize};

/// Result of a `Compact` request that asked for `method: "handoff"`.
///
/// Unlike the summary path, the reply is not an acknowledgement: it arrives
/// after the boundary is committed or the generation has failed, and a failure
/// leaves the transcript unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HandoffCompaction {
    /// The generated document became the boundary, covering the first `covered`
    /// messages of the active context.
    Committed { covered: usize },
    /// The recent-token budget kept every message verbatim; nothing was written.
    NothingToCover,
    /// The completion failed, or its document was refused (empty, or without a
    /// `Goal` heading). The transcript is unchanged.
    ProviderError { detail: String },
    /// The request was cancelled before the completion answered. The transcript
    /// is unchanged.
    Cancelled,
}

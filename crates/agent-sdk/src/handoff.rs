//! Compaction through the harness API.
//!
//! Two shapes share one request. The summary runs at the next safe point and the
//! reply only acknowledges it; the handoff document is written before the reply,
//! so that reply is its outcome.

use crate::client::{HoronAgentClient, unexpected};
use crate::errors::{Error, ErrorKind, Result};
use horon_agent_harness_api::{ApiEvent, ApiRequest, CompactMethod, HandoffCompaction};

impl HoronAgentClient {
    /// Schedule compaction of the transcript so far, freeing context. Not
    /// synchronous: returning means the request was accepted.
    pub fn compact(&self, session_id: &str) -> Result<String> {
        match self
            .request_ok(ApiRequest::Compact {
                session_id: session_id.to_string(),
                method: None,
            })?
            .event
        {
            ApiEvent::Compacted { message, .. } => Ok(message),
            other => Err(unexpected("compacted", &other)),
        }
    }

    /// Generate a handoff document and commit it as the compaction boundary.
    ///
    /// Synchronous, unlike [`HoronAgentClient::compact`]: the reply arrives
    /// after the boundary is written, so the outcome says how many messages it
    /// covered. A provider error or a cancellation leaves the transcript
    /// unchanged and is reported as that outcome rather than as a failure of
    /// the call.
    pub fn compact_handoff(&self, session_id: &str) -> Result<HandoffCompaction> {
        match self
            .request_ok(ApiRequest::Compact {
                session_id: session_id.to_string(),
                method: Some(CompactMethod::Handoff),
            })?
            .event
        {
            ApiEvent::Compacted { handoff, .. } => handoff.ok_or_else(|| {
                Error::new(
                    ErrorKind::UnexpectedReply,
                    "the daemon answered a handoff compaction without an outcome",
                )
            }),
            other => Err(unexpected("compacted", &other)),
        }
    }
}

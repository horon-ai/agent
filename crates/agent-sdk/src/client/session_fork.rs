//! The two session-fork requests, kept beside the client rather than in its
//! already oversized file.

use super::*;

impl HoronAgentClient {
    pub fn fork_session(&self, session_id: &str) -> Result<SessionInfo> {
        match self
            .request_ok(ApiRequest::ForkSession {
                session_id: session_id.to_string(),
            })?
            .event
        {
            ApiEvent::SessionForked { session } => Ok(session),
            other => Err(unexpected("session_forked", &other)),
        }
    }

    /// Fork a new idle session from the messages recorded before
    /// `boundary_message_id`.
    ///
    /// The boundary is a stored message id from the source's history, not an
    /// index. A boundary the source no longer records is refused and the
    /// source is left untouched. Requires `session_fork_at_v1`.
    pub fn fork_session_at(&self, session_id: &str, boundary_message_id: &str) -> Result<SessionInfo> {
        self.require_capability(
            horon_agent_harness_api::CAPABILITY_SESSION_FORK_AT_V1,
            "fork_session_at",
        )?;
        match self
            .request_ok(ApiRequest::ForkSessionAt {
                session_id: session_id.to_string(),
                boundary_message_id: boundary_message_id.to_string(),
            })?
            .event
        {
            ApiEvent::SessionForked { session } => Ok(session),
            other => Err(unexpected("session_forked", &other)),
        }
    }
}

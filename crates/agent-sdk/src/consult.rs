//! Read-only consult forks.
//!
//! A consult fork stores the read-only classification on the child before its
//! id is answered, so the child is never reachable with the parent's tools and
//! stays narrow after a daemon restart. The boundary is the stored message the
//! child stops before: it keeps the prefix before that id, and a stale id is
//! refused without creating anything.
//!
//! These live beside `client.rs` rather than inside it so the fork surface can
//! grow without pushing the client past its size budget.

use crate::client::unexpected;
use crate::errors::Result;
use horon_agent_harness_api::{ApiEvent, ApiRequest, SessionInfo};

use crate::HoronAgentClient;

impl HoronAgentClient {
    /// Fork the session as a read-only consult child.
    pub fn fork_consult(&self, session_id: &str) -> Result<SessionInfo> {
        self.fork_with(session_id, true, None)
    }

    /// Fork a read-only consult child that stops before `boundary`.
    pub fn fork_consult_at(&self, session_id: &str, boundary: &str) -> Result<SessionInfo> {
        self.fork_with(session_id, true, Some(boundary))
    }

    /// The shared fork request behind the normal and consult forks.
    pub(crate) fn fork_with(&self, session_id: &str, consult: bool, boundary: Option<&str>) -> Result<SessionInfo> {
        match self
            .request_ok(ApiRequest::ForkSession {
                session_id: session_id.to_string(),
                consult,
                boundary: boundary.map(str::to_string),
            })?
            .event
        {
            ApiEvent::SessionForked { session } => Ok(session),
            other => Err(unexpected("session_forked", &other)),
        }
    }
}

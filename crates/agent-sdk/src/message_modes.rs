//! `get_message_modes`: how the daemon delivers messages that arrive while it
//! is busy.

use crate::client::{HoronAgentClient, unexpected};
use crate::errors::Result;
use horon_agent_harness_api::{ApiEvent, ApiRequest, MessageModes};

impl HoronAgentClient {
    /// Read the daemon's `[messages]` delivery modes.
    ///
    /// The values are daemon configuration rather than session state, so this
    /// answers even for a session the daemon does not know. Read it after
    /// [`connect`](Self::connect) when a client has to explain why several
    /// queued messages arrived together or one at a time.
    pub fn get_message_modes(&self, session_id: &str) -> Result<MessageModes> {
        match self
            .request_ok(ApiRequest::GetMessageModes {
                session_id: session_id.to_string(),
            })?
            .event
        {
            ApiEvent::MessageModes { modes, .. } => Ok(modes),
            other => Err(unexpected("message_modes", &other)),
        }
    }
}

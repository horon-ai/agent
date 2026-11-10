//! Markdown transcript export.
//!
//! Kept beside `client.rs` rather than inside it: that file is already at its
//! size budget, and this is one method over the shared request/reply plumbing
//! rather than new client machinery.

use crate::client::unexpected;
use crate::errors::Result;
use crate::{ApiEvent, ApiRequest, HoronAgentClient};

impl HoronAgentClient {
    /// Export the conversation as a Markdown transcript.
    ///
    /// Returns the full markdown text: secrets are redacted and tool results
    /// longer than 4 KiB are truncated with a marker. Requires
    /// `session_replication_v1`, the capability that already gates the other
    /// stored-session reads and exports.
    pub fn export_session_markdown(&self, session_id: &str) -> Result<String> {
        self.require_capability("session_replication_v1", "export_session_markdown")?;
        match self.replication_request(ApiRequest::ExportSessionMarkdown {
            session_id: session_id.to_string(),
        })? {
            ApiEvent::SessionMarkdown { markdown, .. } => Ok(markdown),
            other => Err(unexpected("session_markdown", &other)),
        }
    }
}

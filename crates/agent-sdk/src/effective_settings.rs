//! `get_effective_settings`: the daemon values a settings screen may display.
//!
//! The reply is an allow-list the daemon chose, not a serialization of its
//! config, so a client gets labelled text and the layer behind each value
//! without ever holding a credential. The list is additive: a server that
//! predates it answers `unknown_request`, and a client that sees that keeps its
//! own defaults rather than assuming this daemon reports anything.

use crate::client::{HoronAgentClient, unexpected};
use crate::errors::Result;
use horon_agent_harness_api::{ApiEvent, ApiRequest, EffectiveSetting};

impl HoronAgentClient {
    /// Read the allow-listed daemon values a settings screen shows.
    ///
    /// Each row names its key, a label, the value in force and the layer that
    /// supplied it. Requires `effective_settings_v1`: an older bridge does not
    /// advertise it, so this fails with a message naming the capability rather
    /// than a request that would time out.
    pub fn get_effective_settings(&self, session_id: &str) -> Result<Vec<EffectiveSetting>> {
        self.require_capability(
            horon_agent_harness_api::CAPABILITY_EFFECTIVE_SETTINGS_V1,
            "get_effective_settings",
        )?;
        match self
            .request_ok(ApiRequest::GetEffectiveSettings {
                session_id: session_id.to_string(),
            })?
            .event
        {
            ApiEvent::EffectiveSettings { settings, .. } => Ok(settings),
            other => Err(unexpected("effective_settings", &other)),
        }
    }
}

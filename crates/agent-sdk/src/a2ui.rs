//! Opt-in A2UI v1.0 client helpers and result types for [`HoronAgentClient`].

use crate::client::unexpected;
use crate::{Error, HoronAgentClient, Result, TurnResult};
use horon_agent_harness_api::{A2uiSessionCapabilities, ApiEvent, ApiRequest, CAPABILITY_A2UI_V1};

/// One A2UI v1.0 server-to-client envelope emitted during a turn.
#[derive(Debug, Clone, PartialEq)]
pub struct A2uiEnvelopeEvent {
    pub tool_call_id: String,
    pub sequence: u32,
    pub envelope: serde_json::Value,
    pub fallback_text: Option<String>,
}

/// Options accepted when attaching to an existing session.
#[derive(Clone, Debug, Default)]
pub struct AttachSessionOptions {
    /// Optional A2UI v1.0 renderer capabilities for this attachment.
    pub a2ui: Option<A2uiSessionCapabilities>,
}

pub(crate) fn require_if_configured(
    client: &HoronAgentClient,
    a2ui: Option<&A2uiSessionCapabilities>,
    surface: &str,
) -> Result<()> {
    if a2ui.is_some() {
        client.require_capability(CAPABILITY_A2UI_V1, surface)?;
    }
    Ok(())
}

pub(crate) fn push_envelope(
    result: &mut TurnResult,
    tool_call_id: String,
    sequence: u32,
    envelope: serde_json::Value,
    fallback_text: Option<String>,
) {
    result.a2ui_envelopes.push(A2uiEnvelopeEvent {
        tool_call_id,
        sequence,
        envelope,
        fallback_text,
    });
}

impl HoronAgentClient {
    /// Attach to an existing session with explicit attachment options (such as A2UI capabilities).
    pub fn attach_session_with_options(
        &self,
        session_id: &str,
        options: AttachSessionOptions,
    ) -> Result<horon_agent_harness_api::SessionInfo> {
        require_if_configured(self, options.a2ui.as_ref(), "attach_session with a2ui")?;
        match self
            .request_ok(ApiRequest::AttachSession {
                session_id: session_id.to_string(),
                a2ui: options.a2ui,
            })?
            .event
        {
            ApiEvent::Attached { session } => Ok(session),
            other => Err(unexpected("attached", &other)),
        }
    }

    /// Update A2UI v1.0 renderer capabilities for an attached session.
    ///
    /// Pass `None` to disable A2UI rendering for the session. Requires `a2ui_v1`.
    pub fn configure_a2ui(&self, session_id: &str, capabilities: Option<A2uiSessionCapabilities>) -> Result<()> {
        self.require_capability(CAPABILITY_A2UI_V1, "configure_a2ui")?;
        match self
            .request_ok(ApiRequest::ConfigureA2ui {
                session_id: session_id.to_string(),
                capabilities,
            })?
            .event
        {
            ApiEvent::Ok => Ok(()),
            other => Err(unexpected("ok", &other)),
        }
    }

    /// Forward a client-to-agent A2UI v1.0 envelope to an attached session.
    ///
    /// Requires `a2ui_v1`. Returns `Ok(())` once the server validates and accepts
    /// the envelope (surfacing validation failures as a typed error).
    pub fn send_a2ui_event(
        &self,
        session_id: &str,
        envelope: serde_json::Value,
        data_model: Option<serde_json::Value>,
    ) -> Result<()> {
        self.require_capability(CAPABILITY_A2UI_V1, "send_a2ui_event")?;
        match self
            .request_ok(ApiRequest::A2uiEvent {
                session_id: session_id.to_string(),
                envelope,
                data_model,
            })?
            .event
        {
            ApiEvent::Ok => Ok(()),
            other => Err(unexpected("ok", &other)),
        }
    }
}

/// Ensure unused import check stays clean if `Error` is only referenced via `Result`.
const _: fn(Error) -> Error = |err| err;

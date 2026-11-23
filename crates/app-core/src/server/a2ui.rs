//! Daemon lifecycle helpers for client-negotiated A2UI capabilities and events.
use super::*;
use crate::protocol::Request;
use horon_agent_tool_types::a2ui::{A2uiSessionCapabilities, validate_renderer_to_agent_envelope};

/// Outcome of pre-dispatching an A2UI request before the main daemon request match.
pub(super) enum A2uiPreprocess {
    /// Request handled directly (`Done` or `Error` already sent).
    Handled,
    /// Request rewritten or left intact for normal dispatch.
    Continue,
}

/// Apply client-negotiated A2UI capabilities to the agent, deferring if a turn is active.
pub(super) async fn apply_or_defer_a2ui_capabilities(
    agent: &Arc<Mutex<Agent>>,
    capabilities: Option<A2uiSessionCapabilities>,
) {
    if let Ok(mut guard) = agent.try_lock() {
        guard.set_a2ui_capabilities(capabilities).await;
        return;
    }
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        agent.lock().await.set_a2ui_capabilities(capabilities).await;
    });
}

/// Handle `Request::ConfigureA2ui` and translate `Request::A2uiEvent` into an active turn
/// or soft-interrupt request after strict schema validation.
pub(super) async fn preprocess_a2ui_request(
    request: &mut Request,
    agent: &Arc<Mutex<Agent>>,
    client_is_processing: bool,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> A2uiPreprocess {
    match request {
        Request::ConfigureA2ui { id, capabilities } => {
            apply_or_defer_a2ui_capabilities(agent, capabilities.clone()).await;
            if client_event_tx.send(ServerEvent::Done { id: *id }).is_err() {
                crate::logging::warn("Failed to deliver ConfigureA2ui reply to disconnected client");
            }
            A2uiPreprocess::Handled
        }
        Request::A2uiEvent {
            id,
            envelope,
            data_model,
        } => match validate_renderer_to_agent_envelope(envelope, data_model.as_ref()) {
            Ok(parsed) => {
                let req_id = *id;
                *request = if client_is_processing {
                    Request::SoftInterrupt {
                        id: req_id,
                        content: parsed.prompt_text,
                        images: Vec::new(),
                        urgent: false,
                        client_id: None,
                    }
                } else {
                    Request::Message {
                        id: req_id,
                        content: parsed.prompt_text,
                        images: Vec::new(),
                        system_reminder: None,
                        active_skill: None,
                        no_reply: !parsed.needs_reply,
                        expected_run_id: None,
                    }
                };
                A2uiPreprocess::Continue
            }
            Err(err) => {
                if client_event_tx
                    .send(ServerEvent::Error {
                        id: *id,
                        message: err.to_string(),
                        retry_after_secs: None,
                    })
                    .is_err()
                {
                    crate::logging::warn("Failed to deliver A2uiEvent error to disconnected client");
                }
                A2uiPreprocess::Handled
            }
        },
        _ => A2uiPreprocess::Continue,
    }
}

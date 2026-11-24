//! A2UI v1.0 translation between the stable harness API and the daemon wire protocol.

use super::simple_kind::SimpleKind;
use super::{BridgeState, Outbound};
use horon_agent_harness_api::{
    A2uiSessionCapabilities, ApiEvent, CAPABILITY_A2UI_V1, ErrorCode, ServerFrame, unsupported_reply,
};
use serde_json::{Value, json};

/// Parse an optional `a2ui` capability payload from `hello`, `create_session`,
/// `attach_session`, or `configure_a2ui`.
pub(crate) fn parse_optional_capabilities(
    value: Option<&Value>,
    api_id: u64,
) -> Result<Option<A2uiSessionCapabilities>, Box<ServerFrame>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    serde_json::from_value::<A2uiSessionCapabilities>(raw.clone())
        .map(Some)
        .map_err(|error| {
            Box::new(ServerFrame::reply(
                api_id,
                ApiEvent::Error {
                    code: ErrorCode::InvalidRequest,
                    message: format!("invalid a2ui capabilities: {error}"),
                    details: None,
                },
            ))
        })
}

/// Attach `subscribe.a2ui` for `create_session` / `attach_session` when requested
/// or configured at handshake, keeping the pre-A2UI `subscribe` payload when absent.
pub(super) fn attach_subscribe_a2ui(
    state: &BridgeState,
    subscribe: &mut Value,
    request: &Value,
    req: &str,
    api_id: u64,
) -> Result<(), Vec<Outbound>> {
    let requested =
        parse_optional_capabilities(request.get("a2ui"), api_id).map_err(|frame| vec![Outbound::Reply(*frame)])?;
    if requested.is_some() && !state.a2ui_supported {
        return Err(vec![Outbound::Reply(unsupported_reply(
            api_id,
            CAPABILITY_A2UI_V1,
            &format!("{req}.a2ui"),
        ))]);
    }
    let effective = requested.as_ref().or(state.default_a2ui.as_ref());
    if state.a2ui_supported
        && let Some(capabilities) = effective
    {
        let encoded = serde_json::to_value(capabilities).map_err(|error| {
            BridgeState::error_reply(
                api_id,
                ErrorCode::InvalidRequest,
                &format!("invalid a2ui capabilities: {error}"),
            )
        })?;
        subscribe["a2ui"] = encoded;
    }
    Ok(())
}

/// Translate `configure_a2ui` and `a2ui_event` requests to daemon frames.
pub(super) fn request(state: &mut BridgeState, request: &Value, api_id: u64, req: &str) -> Vec<Outbound> {
    if !state.a2ui_supported {
        return vec![Outbound::Reply(unsupported_reply(api_id, CAPABILITY_A2UI_V1, req))];
    }
    match req {
        "configure_a2ui" => {
            let capabilities = match parse_optional_capabilities(request.get("capabilities"), api_id) {
                Ok(capabilities) => capabilities,
                Err(frame) => return vec![Outbound::Reply(*frame)],
            };
            let id = state.legacy_id();
            state.pending_simple.push((id, api_id, SimpleKind::ConfigureA2ui));
            let encoded = match serde_json::to_value(capabilities) {
                Ok(value) => value,
                Err(error) => {
                    return BridgeState::error_reply(
                        api_id,
                        ErrorCode::InvalidRequest,
                        &format!("invalid a2ui capabilities: {error}"),
                    );
                }
            };
            vec![Outbound::Legacy(json!({
                "type": "configure_a2ui",
                "id": id,
                "capabilities": encoded,
            }))]
        }
        "a2ui_event" => {
            let Some(envelope) = request.get("envelope").filter(|value| !value.is_null()).cloned() else {
                return BridgeState::error_reply(
                    api_id,
                    ErrorCode::InvalidRequest,
                    "a2ui_event requires a non-null envelope",
                );
            };
            let data_model = request.get("data_model").filter(|value| !value.is_null()).cloned();
            let id = state.legacy_id();
            state.pending_simple.push((id, api_id, SimpleKind::Ok));
            let mut frame = json!({
                "type": "a2ui_event",
                "id": id,
                "envelope": envelope,
            });
            if let Some(model) = data_model {
                frame["data_model"] = model;
            }
            vec![Outbound::Legacy(frame)]
        }
        other => BridgeState::error_reply(api_id, ErrorCode::UnknownRequest, &format!("unknown request: {other}")),
    }
}

/// Translate daemon `a2ui_envelope` events for the attached session.
pub(super) fn event(state: &mut BridgeState, event: &Value) -> Vec<ServerFrame> {
    let Some(attached) = state.session_id.as_deref() else {
        return vec![];
    };
    let session_id = match event["session_id"].as_str() {
        Some(sid) if !sid.is_empty() && sid != attached => return vec![],
        Some(sid) if !sid.is_empty() => sid.to_string(),
        _ => attached.to_string(),
    };
    let Some(tool_call_id) = event["tool_call_id"].as_str() else {
        return vec![];
    };
    let Some(sequence_u64) = event["sequence"].as_u64() else {
        return vec![];
    };
    let Ok(sequence) = u32::try_from(sequence_u64) else {
        return vec![];
    };
    let Some(envelope) = event.get("envelope").filter(|value| !value.is_null()).cloned() else {
        return vec![];
    };
    let fallback_text = event["fallback_text"].as_str().map(str::to_string);
    state.observed_turn_active = true;
    vec![ServerFrame::event(ApiEvent::A2uiEnvelope {
        session_id,
        tool_call_id: tool_call_id.to_string(),
        sequence,
        envelope,
        fallback_text,
    })]
}

#[cfg(test)]
#[path = "a2ui_tests.rs"]
mod a2ui_tests;

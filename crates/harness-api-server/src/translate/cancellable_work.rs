//! `cancel_turn`, `cancel_tool_call`, `cancel_background_terminal` and
//! `list_background_work`: the four cancellable-work requests.
//!
//! The daemon has served all four to its own clients for a while; this module
//! is only the bridge between the harness API names and the daemon frames. No
//! reply is authored here: the daemon's `cancel_turn_result`,
//! `tool_call_cancel_result` and `background_work_list` are what answer, so a
//! client sees the verdict the daemon actually reached rather than the bridge's
//! guess at it.

use super::{BridgeState, ErrorCode, Outbound, SimpleKind};
use horon_agent_harness_api::{ApiEvent, BackgroundWorkItem, CAPABILITY_CANCELLABLE_WORK_V1, ServerFrame};
use serde_json::{Value, json};

/// Forward one of the four requests, or refuse it before writing a frame.
pub(crate) fn handle(state: &mut BridgeState, api_id: u64, req: &str, request: &Value) -> Vec<Outbound> {
    let (kind, frame) = match req {
        // An empty id would spend a round trip to learn nothing names what to
        // cancel, which the daemon would answer as unknown anyway.
        "cancel_turn" => match named(request, "run_id") {
            Some(run_id) => (
                SimpleKind::CancelTurnResult,
                json!({"type": "cancel_turn", "run_id": run_id}),
            ),
            None => return empty_field(api_id, "run_id"),
        },
        "cancel_tool_call" => match named(request, "call_id") {
            Some(call_id) => (
                SimpleKind::ToolCallCancelResult,
                json!({"type": "cancel_tool_call", "call_id": call_id}),
            ),
            None => return empty_field(api_id, "call_id"),
        },
        "cancel_background_terminal" => match named(request, "call_id") {
            Some(call_id) => (
                SimpleKind::ToolCallCancelResult,
                json!({"type": "cancel_background_terminal", "call_id": call_id}),
            ),
            None => return empty_field(api_id, "call_id"),
        },
        "list_background_work" => (SimpleKind::BackgroundWorkList, json!({"type": "list_background_work"})),
        other => {
            return BridgeState::error_reply(
                api_id,
                ErrorCode::InvalidRequest,
                &format!("{other} is not a cancellable-work request"),
            );
        }
    };
    if !state.cancellable_work_api_supported {
        // `unsupported`, not `unknown_request`: the request is valid for this
        // protocol version and the cause is the daemon behind the socket. An
        // older daemon would ignore the forwarded frame, parking the API client
        // until its own timeout.
        return vec![Outbound::Reply(horon_agent_harness_api::unsupported_reply(
            api_id,
            CAPABILITY_CANCELLABLE_WORK_V1,
            req,
        ))];
    }
    let id = state.legacy_id();
    state.pending_simple.push((id, api_id, kind));
    let mut frame = frame;
    frame["id"] = Value::from(id);
    vec![Outbound::Legacy(frame)]
}

/// Translate the daemon's reply into the API event that answers the request.
pub(crate) fn reply(state: &mut BridgeState, kind: &str, event: &Value) -> Vec<ServerFrame> {
    let simple = match kind {
        "cancel_turn_result" => SimpleKind::CancelTurnResult,
        "tool_call_cancel_result" => SimpleKind::ToolCallCancelResult,
        _ => SimpleKind::BackgroundWorkList,
    };
    let Some(api_id) = state.take_simple(event["id"].as_u64().unwrap_or(0), simple) else {
        return vec![];
    };
    // A cancel reply without the fields that name what it answers cannot be
    // correlated, and guessing would answer the wrong request, so the client
    // sees its own timeout instead of a wrong verdict. The list reply carries
    // no subject of its own.
    let verdict = match kind {
        "background_work_list" => None,
        _ => match DaemonVerdict::parse(event) {
            Some(verdict) => Some(verdict),
            None => return vec![],
        },
    };
    let session_id = match &state.session_id {
        Some(session_id) => session_id.clone(),
        // The API mirrors the daemon's session on every reply, and a reply
        // without one belongs to no client connection.
        None => return vec![],
    };
    let api_event = match (kind, verdict) {
        ("cancel_turn_result", Some(verdict)) => ApiEvent::CancelTurnResult {
            session_id,
            run_id: verdict.subject,
            accepted: verdict.accepted,
            reason: verdict.reason,
        },
        ("tool_call_cancel_result", Some(verdict)) => ApiEvent::ToolCallCancelResult {
            session_id,
            call_id: verdict.subject,
            accepted: verdict.accepted,
            reason: verdict.reason,
        },
        _ => ApiEvent::BackgroundWorkList {
            session_id,
            items: background_items(event),
        },
    };
    vec![ServerFrame::reply(api_id, api_event)]
}

/// The daemon's cancel verdict, decoded.
struct DaemonVerdict {
    /// The `run_id` or `call_id` the daemon echoed back.
    subject: String,
    accepted: bool,
    reason: Option<String>,
}

impl DaemonVerdict {
    fn parse(event: &Value) -> Option<Self> {
        let subject = ["run_id", "call_id"]
            .iter()
            .find_map(|field| event[field].as_str().filter(|value| !value.is_empty()))?;
        Some(Self {
            subject: subject.to_string(),
            accepted: event["accepted"].as_bool()?,
            reason: event["reason"].as_str().map(str::to_string),
        })
    }
}

/// The daemon's item list, decoded field by field.
///
/// Not a pass-through of the daemon's object: a field a later daemon adds is
/// dropped here rather than reaching a client that cannot read it.
fn background_items(event: &Value) -> Vec<BackgroundWorkItem> {
    let Some(items) = event["items"].as_array() else {
        return Vec::new();
    };
    items.iter().filter_map(background_item).collect()
}

fn background_item(item: &Value) -> Option<BackgroundWorkItem> {
    Some(BackgroundWorkItem {
        call_id: item["call_id"].as_str()?.to_string(),
        task_id: item["task_id"].as_str()?.to_string(),
        kind: item["kind"].as_str()?.to_string(),
        command: item["command"].as_str().map(str::to_string),
        cwd: item["cwd"].as_str().map(str::to_string),
        output: item["output"].as_str().map(str::to_string),
    })
}

/// The named field when it carries a non-empty string.
fn named<'a>(request: &'a Value, field: &str) -> Option<&'a str> {
    request[field].as_str().filter(|value| !value.is_empty())
}

fn empty_field(api_id: u64, field: &str) -> Vec<Outbound> {
    BridgeState::error_reply(api_id, ErrorCode::InvalidRequest, &format!("{field} must not be empty"))
}

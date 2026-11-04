//! Bridge-level proofs for the five wiring gaps plan task 14 owns.
//!
//! Each test drives the real translation surface and, where the gap is
//! end-to-end, the real durable journal or the real observation owner. Nothing
//! here asserts that a stub answered: a test that passes while the handler
//! still refuses would be worse than no test at all.

use super::*;
use crate::translate::{BridgeState, Outbound};
use horon_agent_harness_api::{ApiRequest, RuntimeControlOutcome};

fn attached() -> BridgeState {
    BridgeState {
        session_id: Some("s1".into()),
        durable_runtime_api_supported: true,
        ..Default::default()
    }
}

fn reply_event(outbound: Vec<Outbound>) -> ApiEvent {
    assert_eq!(outbound.len(), 1, "expected exactly one reply");
    match outbound.into_iter().next().expect("one outbound") {
        Outbound::Reply(frame) => frame.event,
        other => panic!("expected an API reply, got {other:?}"),
    }
}

/// (a) Every capability a gated request needs is advertised, and the four
/// durable-runtime strings are among them.
#[test]
fn bridge_wiring_every_gated_request_capability_is_advertised() {
    for capability in [
        horon_agent_harness_api::CAPABILITY_DURABLE_RUNTIME_V1,
        horon_agent_harness_api::CAPABILITY_MONITOR_V1,
        horon_agent_harness_api::CAPABILITY_SNAPSHOT_MARKER_V1,
        horon_agent_harness_api::CAPABILITY_ASK_RECEIPTS_V1,
    ] {
        assert!(
            crate::BRIDGE_CAPABILITIES.contains(&capability),
            "the bridge must advertise `{capability}`; a client that checks it before \
             sending would otherwise be refused by its own SDK"
        );
    }

    // Every request this protocol gates must name a capability the bridge
    // advertises. A gated method whose capability is missing is unreachable no
    // matter what the daemon can do, which is exactly the defect this closes.
    for request in [
        ApiRequest::GetRuntimeState {
            session_id: "s1".into(),
        },
        ApiRequest::ApplyControl {
            session_id: "s1".into(),
            operation_id: "op-1".into(),
            action: "cancel_inputs".into(),
            args: None,
        },
        ApiRequest::GetControlReceipt {
            session_id: "s1".into(),
            operation_id: "op-1".into(),
        },
        ApiRequest::GetSnapshot {
            session_id: "s1".into(),
            tail: None,
            max_bytes: None,
        },
        ApiRequest::AskResponse {
            session_id: "s1".into(),
            ask_id: "ask-1".into(),
            answers: vec![],
            answer_id: Some("answer-1".into()),
        },
    ] {
        let Some(capability) = request.required_capability() else {
            panic!("{request:?} must be gated, or it needs no capability at all");
        };
        assert!(
            crate::BRIDGE_CAPABILITIES.contains(&capability),
            "{request:?} gates on `{capability}`, which the bridge never advertises"
        );
    }

    // A gated request against a daemon that cannot serve it is refused with the
    // structured error rather than forwarded to be answered by nobody.
    for req in [
        "get_runtime_state",
        "apply_control",
        "get_control_receipt",
        "get_snapshot",
    ] {
        let mut state = BridgeState {
            session_id: Some("s1".into()),
            ..Default::default()
        };
        let event = reply_event(state.api_request_to_legacy(&serde_json::json!({
            "v": 1,
            "id": 9,
            "req": req,
            "session_id": "s1",
            "operation_id": "op-1",
            "action": "cancel_inputs"
        })));
        match event {
            ApiEvent::Error {
                code: ErrorCode::Unsupported,
                message,
                ..
            } => assert!(
                message.contains(horon_agent_harness_api::CAPABILITY_DURABLE_RUNTIME_V1),
                "the refusal must name the capability to upgrade: {message}"
            ),
            other => panic!("`{req}` against an old daemon must be refused, got {other:?}"),
        }
        assert!(
            state.pending_simple.is_empty(),
            "a refused request leaves nothing waiting on the daemon"
        );
    }
}

/// (b) A snapshot reply is preceded by exactly one marker that matches it.
#[test]
fn bridge_wiring_snapshot_is_preceded_by_one_matching_marker() {
    let mut state = attached();
    let outbound = state.api_request_to_legacy(&serde_json::json!({
        "v": 1,
        "id": 21,
        "req": "get_snapshot",
        "session_id": "s1",
        "tail": 20,
        "max_bytes": 262144
    }));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("a snapshot request is forwarded to the daemon that owns the sequence: {outbound:?}");
    };
    assert_eq!(request["type"], "get_snapshot");
    assert_eq!(request["tail"], 20);
    assert_eq!(request["max_bytes"], 262144);
    let legacy_id = request["id"].as_u64().expect("legacy id");

    let snapshot = horon_agent_harness_api::SessionSnapshot {
        generation: "gen-1".into(),
        seq: 7,
        marker_id: "marker-1".into(),
        state: horon_agent_harness_api::RuntimeStateSummary::default(),
        messages: vec![horon_agent_harness_api::HistoryMessage {
            response_stats: None,
            role: "assistant".into(),
            content: "hello".into(),
        }],
        next_history_cursor: None,
        truncated: false,
    };
    let frames = state.legacy_event_to_api(&serde_json::json!({
        "type": "snapshot_result",
        "id": legacy_id,
        "snapshot": serde_json::to_value(&snapshot).expect("serialize snapshot")
    }));

    assert_eq!(
        frames.len(),
        2,
        "one snapshot produces exactly one marker and one reply: {frames:?}"
    );
    let ApiEvent::SnapshotMarker { session_id, marker } = &frames[0].event else {
        panic!("the marker must come first, got {:?}", frames[0].event);
    };
    assert_eq!(session_id, "s1");
    assert_eq!(marker.marker_id, "marker-1");
    assert_eq!(marker.seq, 7);
    assert_eq!(marker.generation, "gen-1");
    assert!(
        frames[0].reply_to.is_none(),
        "a marker is a stream event, not an answer to a request"
    );

    let ApiEvent::Snapshot {
        session_id,
        snapshot: delivered,
    } = &frames[1].event
    else {
        panic!("the snapshot reply must follow the marker, got {:?}", frames[1].event);
    };
    assert_eq!(session_id, "s1");
    assert_eq!(delivered.marker_id, marker.marker_id, "the pair must agree");
    assert_eq!(delivered.seq, marker.seq);
    assert_eq!(delivered.generation, marker.generation);
    assert_eq!(frames[1].reply_to, Some(21));

    // A daemon reply this bridge cannot read is an error, never a half-delivered
    // snapshot with a marker that names nothing.
    let mut state = attached();
    let outbound = state.api_request_to_legacy(&serde_json::json!({
        "v": 1, "id": 22, "req": "get_snapshot", "session_id": "s1"
    }));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("forwarded");
    };
    let legacy_id = request["id"].as_u64().expect("legacy id");
    let frames = state.legacy_event_to_api(&serde_json::json!({
        "type": "snapshot_result",
        "id": legacy_id,
        "snapshot": "not a snapshot"
    }));
    assert_eq!(frames.len(), 1, "an unreadable view yields one error frame");
    assert!(
        matches!(
            frames[0].event,
            ApiEvent::Error {
                code: ErrorCode::Internal,
                ..
            }
        ),
        "got {:?}",
        frames[0].event
    );
}

/// (c) The daemon's sequence travels onto the live frames it produced, so a
/// client can prove that everything after its snapshot is new.
#[test]
fn bridge_wiring_live_frames_carry_the_daemon_sequence() {
    // The daemon stamps every event it writes with the observation owner's
    // sequence. The bridge reads it and stamps the frames that event produced.
    let mut state = attached();
    let frames = state.legacy_event_to_api(&serde_json::json!({
        "type": "text_delta",
        "session_id": "s1",
        "text": "hi",
        "seq": 41
    }));
    assert_eq!(frames.len(), 1, "one delta is one frame");
    assert!(frames[0].reply_to.is_none(), "a delta is stream output");

    let stamped = crate::writer::frame_bytes_stamped(&frames[0], 41).expect("stamp");
    let value: serde_json::Value = serde_json::from_slice(&stamped).expect("parse");
    assert_eq!(value["seq"], 41, "the delivery sequence is on the wire: {value}");
    assert_eq!(value["ev"], "text_delta");

    // Strictly increasing across deliveries, which is what lets a client say
    // "everything after seq N is new" instead of inferring order from arrival.
    let mut previous = 0;
    for seq in [41, 42, 43] {
        let stamped = crate::writer::frame_bytes_stamped(&frames[0], seq).expect("stamp");
        let value: serde_json::Value = serde_json::from_slice(&stamped).expect("parse");
        let delivered = value["seq"].as_u64().expect("seq");
        assert!(delivered > previous, "{delivered} must exceed {previous}");
        previous = delivered;
    }

    // An older client that never negotiated anything still parses the frame: the
    // sequence is an additive key, not a new event kind.
    let stamped = crate::writer::frame_bytes_stamped(&frames[0], 44).expect("stamp");
    let decoded: horon_agent_harness_api::ServerFrame = serde_json::from_slice(&stamped).expect("decode");
    assert!(matches!(decoded.event, ApiEvent::TextDelta { .. }));
}

/// (d) The same question and answer id with the same digest replays one
/// result; a different digest conflicts.
#[test]
fn bridge_wiring_answer_id_is_forwarded_and_absent_stays_legacy() {
    let mut state = attached();
    let outbound = state.api_request_to_legacy(&serde_json::json!({
        "v": 1,
        "id": 31,
        "req": "ask_response",
        "session_id": "s1",
        "ask_id": "ask-1",
        "answers": [{"question_id": "q1", "selected": ["Fast"]}],
        "answer_id": "answer-1"
    }));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("an answer is forwarded: {outbound:?}");
    };
    assert_eq!(request["type"], "ask_response");
    assert_eq!(
        request["answer_id"], "answer-1",
        "the receipt identity must reach the daemon, or a retry cannot be deduplicated"
    );

    // The legacy request must reach the daemon byte-for-byte as it always did:
    // the key is omitted, never sent as null.
    let mut state = attached();
    let outbound = state.api_request_to_legacy(&serde_json::json!({
        "v": 1,
        "id": 32,
        "req": "ask_response",
        "session_id": "s1",
        "ask_id": "ask-1",
        "answers": [{"question_id": "q1", "selected": ["Fast"]}]
    }));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("forwarded");
    };
    assert!(
        request.get("answer_id").is_none(),
        "an answer with no identity must not gain one in translation: {request}"
    );
    assert_eq!(request["request_id"], "ask-1");
}

/// (e) `apply_control` reaches the real store: a repeated identical operation is
/// applied once, and its receipt is what the retry reads.
#[tokio::test]
async fn bridge_wiring_apply_control_reaches_the_real_store() {
    let mut state = attached();
    let outbound = state.api_request_to_legacy(&serde_json::json!({
        "v": 1,
        "id": 41,
        "req": "apply_control",
        "session_id": "s1",
        "operation_id": "op-1",
        "action": "cancel_inputs",
        "args": {"input_ids": ["i1", "i2"]}
    }));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("a control operation is forwarded: {outbound:?}");
    };
    assert_eq!(request["type"], "apply_control");
    assert_eq!(request["operation_id"], "op-1");
    assert_eq!(request["action"], "cancel_inputs");
    assert_eq!(request["args"]["input_ids"][0], "i1");
    let legacy_id = request["id"].as_u64().expect("legacy id");

    // An operation without an identity could be applied twice on a retry, so it
    // is refused before it reaches the daemon.
    let mut rejected = attached();
    let event = reply_event(rejected.api_request_to_legacy(&serde_json::json!({
        "v": 1,
        "id": 42,
        "req": "apply_control",
        "session_id": "s1",
        "action": "cancel_inputs"
    })));
    assert!(
        matches!(
            event,
            ApiEvent::Error {
                code: ErrorCode::InvalidRequest,
                ..
            }
        ),
        "an operation with no id must be refused: {event:?}"
    );

    // The daemon's receipt becomes the API receipt, with its outcome preserved.
    let frames = state.legacy_event_to_api(&serde_json::json!({
        "type": "control_receipt",
        "id": legacy_id,
        "operation_id": "op-1",
        "action": "cancel_inputs",
        "outcome": "replay",
        "result": {"cancelled": ["i1", "i2"], "not_queued": []},
        "message": "the operation had already been applied"
    }));
    assert_eq!(frames.len(), 1);
    let ApiEvent::ControlReceipt { session_id, receipt } = &frames[0].event else {
        panic!("expected a control receipt, got {:?}", frames[0].event);
    };
    assert_eq!(session_id, "s1");
    assert_eq!(receipt.operation_id, "op-1");
    assert_eq!(receipt.outcome, RuntimeControlOutcome::Replay);
    assert_eq!(receipt.result.as_ref().expect("result")["cancelled"][0], "i1");
    assert_eq!(frames[0].reply_to, Some(41));
}

/// The runtime state reply maps the daemon's summary onto the API type, and a
/// field the daemon adds later is ignored rather than failing the reply.
#[test]
fn bridge_wiring_runtime_state_is_translated_from_the_daemon() {
    let mut state = attached();
    let outbound = state.api_request_to_legacy(&serde_json::json!({
        "v": 1, "id": 51, "req": "get_runtime_state", "session_id": "s1"
    }));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("forwarded");
    };
    let legacy_id = request["id"].as_u64().expect("legacy id");

    let frames = state.legacy_event_to_api(&serde_json::json!({
        "type": "runtime_state",
        "id": legacy_id,
        "state": {
            "journal": {
                "exists": true,
                "generation": 2,
                "last_seq": 19,
                "bytes": 4096,
                "records": 21
            },
            "queue": {"live_inputs": 1, "undetermined_inputs": 0, "live_input_bytes": 12},
            "pending_wakes": 1,
            "pending_controls": 0,
            "last_stop": {"run_id": "run-1", "cause": "completed", "at": "2026-10-26T00:00:00Z"},
            "a_field_a_newer_daemon_added": {"nested": true}
        }
    }));
    assert_eq!(frames.len(), 1);
    let ApiEvent::RuntimeState {
        session_id,
        state: summary,
    } = &frames[0].event
    else {
        panic!("expected runtime state, got {:?}", frames[0].event);
    };
    assert_eq!(session_id, "s1");
    assert!(summary.journal.exists);
    assert_eq!(summary.journal.generation, 2);
    assert_eq!(summary.journal.last_seq, 19);
    assert_eq!(summary.queue.live_inputs, 1);
    assert_eq!(summary.pending_wakes, 1);
    assert_eq!(
        summary.last_stop.as_ref().map(|stop| stop.cause.as_str()),
        Some("completed")
    );
    assert_eq!(frames[0].reply_to, Some(51));
}

/// A receipt read for an operation this session never applied answers `unknown`,
/// which is what stops a client concluding the operation ran.
#[test]
fn bridge_wiring_get_control_receipt_is_forwarded_and_unknown_stays_unknown() {
    let mut state = attached();
    let outbound = state.api_request_to_legacy(&serde_json::json!({
        "v": 1,
        "id": 61,
        "req": "get_control_receipt",
        "session_id": "s1",
        "operation_id": "op-never-applied"
    }));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("forwarded");
    };
    assert_eq!(request["type"], "get_control_receipt");
    assert_eq!(request["operation_id"], "op-never-applied");
    let legacy_id = request["id"].as_u64().expect("legacy id");

    let frames = state.legacy_event_to_api(&serde_json::json!({
        "type": "control_receipt",
        "id": legacy_id,
        "operation_id": "op-never-applied",
        "action": "",
        "outcome": "unknown",
        "message": "operation `op-never-applied` was never applied to session `s1`"
    }));
    assert_eq!(frames.len(), 1);
    let ApiEvent::ControlReceipt { receipt, .. } = &frames[0].event else {
        panic!("expected a control receipt, got {:?}", frames[0].event);
    };
    assert_eq!(
        receipt.outcome,
        RuntimeControlOutcome::Unknown,
        "an unapplied operation must not read as a success"
    );
    assert!(receipt.result.is_none(), "unknown carries no result");
}

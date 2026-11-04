use super::*;
use crate::BRIDGE_CAPABILITIES;

fn state_with_session() -> BridgeState {
    BridgeState {
        session_id: Some("s1".into()),
        ..Default::default()
    }
}

fn only_reply_event(outbound: Vec<Outbound>) -> ApiEvent {
    assert_eq!(outbound.len(), 1, "expected exactly one reply");
    match outbound.into_iter().next().expect("one outbound") {
        Outbound::Reply(frame) => frame.event,
        other => panic!("expected API reply, got {other:?}"),
    }
}

fn ask_question() -> Value {
    json!({
        "id": "scope",
        "question": "Which surface should change?",
        "options": [{"label": "daemon only"}, {"label": "daemon and desktop"}],
        "recommended": 1
    })
}

#[test]
fn the_bridge_advertises_the_capabilities_a_client_gates_on() {
    for capability in ["permissions", "asks"] {
        assert!(
            BRIDGE_CAPABILITIES.contains(&capability),
            "a client gated on `{capability}` must see it in hello_ok"
        );
    }
}

#[test]
fn a_permission_response_becomes_a_decision_the_daemon_can_record() {
    for (decision, expected) in [("allow", "allow"), ("allow_always", "allow_always"), ("deny", "deny")] {
        let mut state = state_with_session();
        let outbound = state.api_request_to_legacy(&json!({
            "v": 1,
            "id": 41,
            "req": "permission_response",
            "session_id": "s1",
            "request_id": "req-1",
            "decision": decision
        }));
        let [Outbound::Legacy(legacy)] = &outbound[..] else {
            panic!("{decision} is forwarded to the daemon, got {outbound:?}");
        };
        assert_eq!(legacy["type"], "permission_decision");
        assert_eq!(legacy["request_id"], "req-1");
        assert_eq!(legacy["decision"], expected);
        let legacy_id = legacy["id"].as_u64().expect("legacy id");
        assert!(
            state
                .pending_simple
                .iter()
                .any(|(id, api_id, kind)| *id == legacy_id && *api_id == 41 && *kind == SimpleKind::Ok),
            "the reply waits for the daemon ack, not an assumed success"
        );

        let frames = state.legacy_event_to_api(&json!({"type": "ack", "id": legacy_id}));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, ApiEvent::Ok, "{decision} is acknowledged");
    }
}

#[test]
fn an_invalid_permission_response_is_refused_without_reaching_the_daemon() {
    for (request_id, decision) in [("", "allow"), ("req-1", "maybe")] {
        let mut state = state_with_session();
        let event = only_reply_event(state.api_request_to_legacy(&json!({
            "v": 1,
            "id": 43,
            "req": "permission_response",
            "session_id": "s1",
            "request_id": request_id,
            "decision": decision
        })));
        assert!(
            matches!(
                event,
                ApiEvent::Error {
                    code: ErrorCode::InvalidRequest,
                    ..
                }
            ),
            "request_id {request_id:?} with decision {decision:?} must be refused: {event:?}"
        );
        assert!(state.pending_simple.is_empty(), "nothing is left waiting on the daemon");
    }
}

#[test]
fn a_resolved_approval_reaches_the_client_after_the_decision() {
    let mut state = state_with_session();
    let frames = state.legacy_event_to_api(&json!({
        "type": "approval_resolved",
        "request_id": "req-1",
        "approved": true,
        "via": "api_client",
        "resolved_by": "tool"
    }));
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0].event,
        ApiEvent::ApprovalResolved {
            session_id: "s1".into(),
            request_id: "req-1".into(),
            approved: true,
            via: "api_client".into(),
            resolved_by: "tool".into(),
        }
    );
}

#[test]
fn an_ask_requested_event_carries_the_questions_and_the_ask_id() {
    let mut state = state_with_session();
    let frames = state.legacy_event_to_api(&json!({
        "type": "ask_requested",
        "request_id": "ask-1",
        "questions": [ask_question()]
    }));
    assert_eq!(frames.len(), 1);
    match &frames[0].event {
        ApiEvent::AskRequested {
            session_id,
            ask_id,
            questions,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(ask_id, "ask-1");
            assert_eq!(questions.len(), 1);
            assert_eq!(questions[0].id.as_deref(), Some("scope"));
            assert_eq!(questions[0].options.len(), 2);
            assert_eq!(questions[0].recommended, Some(1));
        }
        other => panic!("expected ask_requested, got {other:?}"),
    }

    let malformed = state.legacy_event_to_api(&json!({"type": "ask_requested", "questions": [ask_question()]}));
    assert!(
        malformed.is_empty(),
        "an ask without an id is dropped, not emitted empty"
    );

    let undecodable = state.legacy_event_to_api(&json!({
        "type": "ask_requested",
        "request_id": "ask-2",
        "questions": "not an array"
    }));
    assert!(
        undecodable.is_empty(),
        "a question set this bridge cannot decode is dropped, so the tool's deadline decides"
    );
}

#[test]
fn an_ask_response_reaches_the_daemon_and_waits_for_its_ack() {
    let mut state = state_with_session();
    let outbound = state.api_request_to_legacy(&json!({
        "v": 1,
        "id": 51,
        "req": "ask_response",
        "session_id": "s1",
        "ask_id": "ask-1",
        "answers": [{"question_id": "scope", "selected": ["daemon and desktop"]}]
    }));
    let [Outbound::Legacy(legacy)] = &outbound[..] else {
        panic!("an ask answer is forwarded, got {outbound:?}");
    };
    assert_eq!(legacy["type"], "ask_response");
    assert_eq!(legacy["request_id"], "ask-1");
    assert_eq!(legacy["answers"][0]["question_id"], "scope");
    assert_eq!(legacy["answers"][0]["selected"][0], "daemon and desktop");

    let legacy_id = legacy["id"].as_u64().expect("legacy id");
    let frames = state.legacy_event_to_api(&json!({"type": "ack", "id": legacy_id}));
    assert_eq!(frames[0].event, ApiEvent::Ok);
}

#[test]
fn an_ask_response_without_an_id_or_answers_is_refused_locally() {
    for (ask_id, answers) in [
        ("", json!([{"question_id": "scope", "selected": ["daemon only"]}])),
        ("ask-1", json!([])),
    ] {
        let mut state = state_with_session();
        let event = only_reply_event(state.api_request_to_legacy(&json!({
            "v": 1,
            "id": 52,
            "req": "ask_response",
            "session_id": "s1",
            "ask_id": ask_id,
            "answers": answers
        })));
        assert!(
            matches!(
                event,
                ApiEvent::Error {
                    code: ErrorCode::InvalidRequest,
                    ..
                }
            ),
            "ask_id {ask_id:?} with {answers} must be refused: {event:?}"
        );
        assert!(state.pending_simple.is_empty());
    }
}

#[test]
fn the_ask_capability_travels_from_hello_to_the_daemon_subscribe() {
    for enabled in [false, true] {
        let mut state = BridgeState {
            supports_ask_requests: enabled,
            ..Default::default()
        };
        let outbound = state.api_request_to_legacy(&json!({
            "v": 1, "id": 1, "req": "create_session", "session_id": "ask-session"
        }));
        let subscribe = outbound
            .iter()
            .find_map(|out| match out {
                Outbound::Legacy(request) if request["type"] == "subscribe" => Some(request),
                _ => None,
            })
            .expect("attachment sends Subscribe");
        assert_eq!(subscribe["supports_ask_requests"], enabled);
    }
    assert!(!BridgeState::default().supports_ask_requests);
}

#[test]
fn ask_support_defaults_off_for_older_clients() {
    for enabled in [false, true] {
        let mut value = json!({"req":"hello", "min_version":1, "max_version":1, "client":"test"});
        if enabled {
            value["supports_ask_requests"] = json!(true);
        }
        let request: horon_agent_harness_api::ApiRequest = serde_json::from_value(value).unwrap();
        let horon_agent_harness_api::ApiRequest::Hello {
            supports_ask_requests, ..
        } = request
        else {
            panic!("hello")
        };
        assert_eq!(supports_ask_requests, enabled);
    }
}

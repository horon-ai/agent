//! Behavioral contract tests for the A2UI v1.0 harness API surface.

use crate::{
    A2uiSessionCapabilities, ADVERTISED_CAPABILITIES, API_VERSION_MAJOR, API_VERSION_MINOR, ApiEvent, ApiRequest,
    CAPABILITY_A2UI_V1, CAPABILITY_SURFACES, ClientFrame, ErrorCode, OPT_IN_EVENT_CAPABILITIES, ServerFrame,
    unsupported_reply,
};
use serde_json::{Value, json};

#[test]
fn hello_create_and_attach_omit_a2ui_when_unset() {
    assert_eq!(API_VERSION_MINOR, 32);

    let hello = ClientFrame::new(
        1,
        ApiRequest::Hello {
            min_version: API_VERSION_MAJOR,
            max_version: API_VERSION_MAJOR,
            client: "a2ui-test/1".into(),
            supports_user_response: false,
            supports_ask_requests: false,
            event_capabilities: Vec::new(),
            a2ui: None,
        },
    );
    let hello_json: Value = serde_json::to_value(&hello).unwrap();
    assert!(hello_json.get("a2ui").is_none(), "unset hello.a2ui must stay omitted");

    let create = ApiRequest::CreateSession {
        working_dir: None,
        system_prompt: None,
        a2ui: None,
    };
    assert_eq!(serde_json::to_value(&create).unwrap(), json!({"req": "create_session"}));
    assert_eq!(
        serde_json::from_value::<ApiRequest>(json!({"req": "create_session"})).unwrap(),
        create
    );

    let attach = ApiRequest::AttachSession {
        session_id: "s1".into(),
        a2ui: None,
    };
    assert_eq!(
        serde_json::to_value(&attach).unwrap(),
        json!({"req": "attach_session", "session_id": "s1"})
    );
    assert_eq!(
        serde_json::from_value::<ApiRequest>(json!({"req": "attach_session", "session_id": "s1"})).unwrap(),
        attach
    );
}

#[test]
fn hello_create_and_attach_with_a2ui_roundtrip() {
    let caps = A2uiSessionCapabilities::default();
    let requests = [
        ApiRequest::Hello {
            min_version: API_VERSION_MAJOR,
            max_version: API_VERSION_MAJOR,
            client: "a2ui-client/1".into(),
            supports_user_response: false,
            supports_ask_requests: false,
            event_capabilities: vec![CAPABILITY_A2UI_V1.into()],
            a2ui: Some(caps.clone()),
        },
        ApiRequest::CreateSession {
            working_dir: Some("/tmp/ws".into()),
            system_prompt: None,
            a2ui: Some(caps.clone()),
        },
        ApiRequest::AttachSession {
            session_id: "s-attach".into(),
            a2ui: Some(caps),
        },
    ];
    for request in &requests {
        let encoded = serde_json::to_value(request).unwrap();
        assert!(encoded.get("a2ui").is_some());
        let decoded: ApiRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, *request);
    }
    assert_eq!(requests[0].required_capability(), None);
    assert_eq!(requests[1].required_capability(), Some(CAPABILITY_A2UI_V1));
    assert_eq!(requests[2].required_capability(), Some(CAPABILITY_A2UI_V1));
}

#[test]
fn configure_a2ui_and_a2ui_event_roundtrip_and_gating() {
    let caps = A2uiSessionCapabilities {
        version: crate::A2UI_VERSION_V1_0.into(),
        supported_catalog_ids: vec![crate::A2UI_BASIC_CATALOG_V1_0.into()],
        accepts_inline_catalogs: true,
    };
    let configure_on = ApiRequest::ConfigureA2ui {
        session_id: "s1".into(),
        capabilities: Some(caps),
    };
    let configure_off = ApiRequest::ConfigureA2ui {
        session_id: "s1".into(),
        capabilities: None,
    };
    let event_with_model = ApiRequest::A2uiEvent {
        session_id: "s1".into(),
        envelope: json!({
            "version": "v0.9",
            "action": { "name": "submit", "surfaceId": "main" }
        }),
        data_model: Some(json!({"count": 2})),
    };
    let event_plain = ApiRequest::A2uiEvent {
        session_id: "s1".into(),
        envelope: json!({
            "version": "v0.9",
            "error": { "code": "RENDER_FAILED", "surfaceId": "main", "message": "oops" }
        }),
        data_model: None,
    };

    for request in [&configure_on, &configure_off, &event_with_model, &event_plain] {
        assert_eq!(request.session_id(), Some("s1"));
        assert_eq!(request.required_capability(), Some(CAPABILITY_A2UI_V1));
        assert_eq!(request.missing_capability(&[]), Some(CAPABILITY_A2UI_V1));
        assert_eq!(request.missing_capability(&[CAPABILITY_A2UI_V1.into()]), None);
        let wire = serde_json::to_string(&ClientFrame::new(9, request.clone())).unwrap();
        let decoded: ClientFrame = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded.request, *request);
    }

    let plain_wire = serde_json::to_value(&event_plain).unwrap();
    assert!(plain_wire.get("data_model").is_none());

    let reply = unsupported_reply(9, CAPABILITY_A2UI_V1, "configure_a2ui");
    assert_eq!(reply.reply_to, Some(9));
    assert!(matches!(
        reply.event,
        ApiEvent::Error {
            code: ErrorCode::Unsupported,
            ..
        }
    ));
}

#[test]
fn a2ui_envelope_roundtrip_and_opt_in_contract() {
    assert!(ADVERTISED_CAPABILITIES.contains(&CAPABILITY_A2UI_V1));
    assert!(OPT_IN_EVENT_CAPABILITIES.contains(&CAPABILITY_A2UI_V1));
    assert!(CAPABILITY_SURFACES.iter().any(|(cap, _)| *cap == CAPABILITY_A2UI_V1));

    let with_fallback = ServerFrame::event(ApiEvent::A2uiEnvelope {
        session_id: "s1".into(),
        tool_call_id: "call-1".into(),
        sequence: 2,
        envelope: json!({
            "version": "v0.9",
            "createSurface": { "surfaceId": "main", "catalogId": crate::A2UI_BASIC_CATALOG_V1_0 }
        }),
        fallback_text: Some("Surface card".into()),
    });
    let no_fallback = ServerFrame::event(ApiEvent::A2uiEnvelope {
        session_id: "s1".into(),
        tool_call_id: "call-1".into(),
        sequence: 3,
        envelope: json!({"version": "v0.9", "deleteSurface": { "surfaceId": "main" }}),
        fallback_text: None,
    });

    for frame in [&with_fallback, &no_fallback] {
        assert_eq!(frame.event.required_capability(), Some(CAPABILITY_A2UI_V1));
        let line = serde_json::to_string(frame).unwrap();
        let decoded: ServerFrame = serde_json::from_str(&line).unwrap();
        assert_eq!(decoded, *frame);
    }
    let no_fallback_wire = serde_json::to_value(&no_fallback).unwrap();
    assert!(no_fallback_wire.get("fallback_text").is_none());
}

use super::{Request, ServerEvent, decode_request};
use anyhow::Result;
use horon_agent_tool_types::a2ui::{A2UI_BASIC_CATALOG_V1_0, A2uiSessionCapabilities};
use serde_json::{Value, json};

#[test]
fn subscribe_omits_a2ui_when_absent_and_roundtrips_capabilities() -> Result<()> {
    let plain = Request::Subscribe {
        id: 1,
        system_prompt: None,
        supports_user_response: false,
        supports_ask_requests: false,
        working_dir: Some("/tmp/proj".to_string()),
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
        acp: None,
        history_message_ids: false,
        a2ui: None,
        fresh_session: false,
    };
    let encoded_plain: Value = serde_json::from_str(&serde_json::to_string(&plain)?)?;
    assert!(
        encoded_plain.get("a2ui").is_none(),
        "non-A2UI Subscribe must omit 'a2ui' field"
    );

    let with_a2ui = Request::Subscribe {
        id: 2,
        system_prompt: None,
        supports_user_response: false,
        supports_ask_requests: false,
        working_dir: Some("/tmp/proj".to_string()),
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
        acp: None,
        history_message_ids: false,
        a2ui: Some(A2uiSessionCapabilities {
            version: "1.0".to_string(),
            supported_catalog_ids: vec![A2UI_BASIC_CATALOG_V1_0.to_string()],
            accepts_inline_catalogs: false,
        }),
        fresh_session: false,
    };
    let raw = serde_json::to_string(&with_a2ui)?;
    let Request::Subscribe { a2ui, .. } = decode_request(&raw)? else {
        anyhow::bail!("expected Subscribe");
    };
    let caps = a2ui.expect("a2ui capabilities preserved");
    assert_eq!(caps.version, "1.0");
    assert_eq!(caps.supported_catalog_ids, vec![A2UI_BASIC_CATALOG_V1_0]);
    Ok(())
}

#[test]
fn configure_a2ui_and_a2ui_event_requests_roundtrip() -> Result<()> {
    let cfg = Request::ConfigureA2ui {
        id: 10,
        capabilities: Some(A2uiSessionCapabilities::default()),
    };
    assert_eq!(cfg.id(), 10);
    let raw_cfg = serde_json::to_string(&cfg)?;
    assert!(raw_cfg.contains("\"type\":\"configure_a2ui\""));
    assert!(matches!(
        decode_request(&raw_cfg)?,
        Request::ConfigureA2ui {
            id: 10,
            capabilities: Some(_)
        }
    ));

    let evt = Request::A2uiEvent {
        id: 11,
        envelope: json!({
            "version": "1.0",
            "action": {
                "name": "submit",
                "surfaceId": "s1",
                "sourceComponentId": "btn1"
            }
        }),
        data_model: Some(json!({ "surfaces": { "s1": { "ok": true } } })),
    };
    assert_eq!(evt.id(), 11);
    let raw_evt = serde_json::to_string(&evt)?;
    assert!(raw_evt.contains("\"type\":\"a2ui_event\""));
    let Request::A2uiEvent {
        id,
        envelope,
        data_model,
    } = decode_request(&raw_evt)?
    else {
        anyhow::bail!("expected A2uiEvent");
    };
    assert_eq!(id, 11);
    assert_eq!(envelope["action"]["name"], "submit");
    assert_eq!(data_model.unwrap()["surfaces"]["s1"]["ok"], true);
    Ok(())
}

#[test]
fn server_event_a2ui_envelope_roundtrips() -> Result<()> {
    let ev = ServerEvent::A2uiEnvelope {
        session_id: "sess_1".to_string(),
        tool_call_id: "call_1".to_string(),
        sequence: 0,
        envelope: json!({
            "version": "1.0",
            "createSurface": { "surfaceId": "s1" }
        }),
        fallback_text: Some("Rendered form".to_string()),
    };
    let raw = serde_json::to_string(&ev)?;
    assert!(raw.contains("\"type\":\"a2ui_envelope\""));
    let decoded: ServerEvent = serde_json::from_str(&raw)?;
    let ServerEvent::A2uiEnvelope {
        session_id,
        tool_call_id,
        sequence,
        envelope,
        fallback_text,
    } = decoded
    else {
        anyhow::bail!("expected A2uiEnvelope");
    };
    assert_eq!(session_id, "sess_1");
    assert_eq!(tool_call_id, "call_1");
    assert_eq!(sequence, 0);
    assert_eq!(envelope["createSurface"]["surfaceId"], "s1");
    assert_eq!(fallback_text.as_deref(), Some("Rendered form"));
    Ok(())
}

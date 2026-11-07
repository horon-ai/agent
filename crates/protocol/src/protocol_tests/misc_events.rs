#[test]
fn test_memory_activity_event_roundtrip() -> Result<()> {
    let event = ServerEvent::MemoryActivity {
        activity: MemoryActivitySnapshot {
            state: MemoryStateSnapshot::SidecarChecking { count: 3 },
            state_age_ms: 275,
            pipeline: Some(MemoryPipelineSnapshot {
                search: MemoryStepStatusSnapshot::Done,
                search_result: Some(MemoryStepResultSnapshot {
                    summary: "5 hits".to_string(),
                    latency_ms: 14,
                }),
                verify: MemoryStepStatusSnapshot::Running,
                verify_result: None,
                verify_progress: Some((1, 3)),
                inject: MemoryStepStatusSnapshot::Pending,
                inject_result: None,
                maintain: MemoryStepStatusSnapshot::Pending,
                maintain_result: None,
            }),
        },
    };

    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"memory_activity\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::MemoryActivity { activity } = decoded else {
        return Err(anyhow!("expected MemoryActivity event"));
    };
    assert_eq!(
        activity.state,
        MemoryStateSnapshot::SidecarChecking { count: 3 }
    );
    assert_eq!(activity.state_age_ms, 275);
    let pipeline = activity
        .pipeline
        .ok_or_else(|| anyhow!("pipeline snapshot"))?;
    assert_eq!(pipeline.search, MemoryStepStatusSnapshot::Done);
    assert_eq!(pipeline.verify, MemoryStepStatusSnapshot::Running);
    assert_eq!(pipeline.verify_progress, Some((1, 3)));
    Ok(())
}

#[test]
fn test_protocol_enum_roundtrips_cover_wire_names() -> Result<()> {
    let delivery_modes = [
        (CommDeliveryMode::Notify, "notify"),
        (CommDeliveryMode::Interrupt, "interrupt"),
        (CommDeliveryMode::Wake, "wake"),
    ];
    for (mode, wire) in delivery_modes {
        let json = serde_json::to_string(&mode)?;
        assert_eq!(json, format!("\"{}\"", wire));
        let decoded: CommDeliveryMode = serde_json::from_str(&json)?;
        assert_eq!(decoded, mode);
    }

    let feature_toggles = [
        (FeatureToggle::Memory, "memory"),
        (FeatureToggle::Swarm, "swarm"),
        (FeatureToggle::Autoreview, "autoreview"),
        (FeatureToggle::Autojudge, "autojudge"),
    ];
    for (feature, wire) in feature_toggles {
        let json = serde_json::to_string(&feature)?;
        assert_eq!(json, format!("\"{}\"", wire));
        let decoded: FeatureToggle = serde_json::from_str(&json)?;
        assert_eq!(decoded, feature);
    }

    Ok(())
}

#[test]
fn test_set_route_deserializes_as_set_model_compat_alias() -> Result<()> {
    // Legacy/desktop compatibility shape: a bare model string under the
    // `set_route` tag. `decode_request` (not raw serde) normalizes it.
    let decoded = decode_request(r#"{"type":"set_route","id":42,"model":"claude-opus-4-5"}"#)?;
    let Request::SetModel { id, model } = decoded else {
        return Err(anyhow!(
            "expected set_route compatibility alias to decode as SetModel"
        ));
    };
    assert_eq!(id, 42);
    assert_eq!(model, "claude-opus-4-5");
    Ok(())
}

#[test]
fn test_subscribe_request_roundtrip_preserves_session_takeover_flags() -> Result<()> {
    let req = Request::Subscribe {
        system_prompt: None,
        supports_user_response: true,
        supports_ask_requests: true,
        id: 89,
        working_dir: Some("/tmp/project".to_string()),
        target_session_id: Some("sess_target".to_string()),
        client_instance_id: Some("client-123".to_string()),
        client_has_local_history: true,
        allow_session_takeover: true,
        crash_on_disconnect: true,
        continue_on_disconnect: true,
        terminal_env: vec![("ZELLIJ_SESSION_NAME".to_string(), "sessionB".to_string())],
        acp: None,
    };
    let json = serde_json::to_string(&req)?;
    assert!(json.contains("\"type\":\"subscribe\""));
    let decoded = parse_request_json(&json)?;
    let Request::Subscribe {
        system_prompt: _,
        supports_user_response,
        supports_ask_requests,
        id,
        working_dir,
        target_session_id,
        client_instance_id,
        client_has_local_history,
        allow_session_takeover,
        crash_on_disconnect,
        continue_on_disconnect,
        terminal_env,
        acp: _,
    } = decoded
    else {
        return Err(anyhow!("expected Subscribe"));
    };
    assert_eq!(id, 89);
    assert!(supports_user_response);
    assert!(supports_ask_requests);
    assert_eq!(working_dir.as_deref(), Some("/tmp/project"));
    assert_eq!(target_session_id.as_deref(), Some("sess_target"));
    assert_eq!(client_instance_id.as_deref(), Some("client-123"));
    assert!(client_has_local_history);
    assert!(allow_session_takeover);
    assert!(crash_on_disconnect);
    assert!(continue_on_disconnect);
    assert_eq!(
        terminal_env,
        vec![("ZELLIJ_SESSION_NAME".to_string(), "sessionB".to_string())]
    );
    Ok(())
}

#[test]
fn test_subscribe_request_defaults_optional_flags() -> Result<()> {
    let json = r#"{"type":"subscribe","id":91}"#;
    let decoded = parse_request_json(json)?;
    let Request::Subscribe {
        system_prompt: _,
        supports_user_response,
        supports_ask_requests,
        id,
        working_dir,
        target_session_id,
        client_instance_id,
        client_has_local_history,
        allow_session_takeover,
        crash_on_disconnect,
        continue_on_disconnect,
        terminal_env,
        acp: _,
    } = decoded
    else {
        return Err(anyhow!("expected Subscribe"));
    };
    assert_eq!(id, 91);
    assert!(!supports_user_response);
    assert!(!supports_ask_requests);
    assert_eq!(working_dir, None);
    assert_eq!(target_session_id, None);
    assert_eq!(client_instance_id, None);
    assert!(!client_has_local_history);
    assert!(!allow_session_takeover);
    assert!(!crash_on_disconnect);
    assert!(!continue_on_disconnect);
    assert!(terminal_env.is_empty());
    Ok(())
}

#[test]
fn test_resume_session_defaults_sync_flags() -> Result<()> {
    let json = r#"{"type":"resume_session","id":92,"session_id":"sess_resume"}"#;
    let decoded = parse_request_json(json)?;
    let Request::ResumeSession {
        id,
        session_id,
        client_instance_id,
        client_has_local_history,
        allow_session_takeover,
    } = decoded
    else {
        return Err(anyhow!("expected ResumeSession"));
    };
    assert_eq!(id, 92);
    assert_eq!(session_id, "sess_resume");
    assert_eq!(client_instance_id, None);
    assert!(!client_has_local_history);
    assert!(!allow_session_takeover);
    Ok(())
}

#[test]
fn test_resume_all_result_event_roundtrip() -> Result<()> {
    let event = ServerEvent::ResumeAllResult {
        id: 451,
        resumed: 2,
        skipped: 1,
        resumed_sessions: vec!["fox".to_string(), "owl".to_string()],
        message: "Resuming 2 interrupted sessions: fox, owl.".to_string(),
    };
    let json = serde_json::to_string(&event)?;
    assert!(json.contains("\"type\":\"resume_all_result\""));
    let decoded = parse_event_json(&json)?;
    let ServerEvent::ResumeAllResult {
        id,
        resumed,
        skipped,
        resumed_sessions,
        message,
    } = decoded
    else {
        return Err(anyhow!("expected ResumeAllResult"));
    };
    assert_eq!(id, 451);
    assert_eq!(resumed, 2);
    assert_eq!(skipped, 1);
    assert_eq!(resumed_sessions, vec!["fox".to_string(), "owl".to_string()]);
    assert_eq!(message, "Resuming 2 interrupted sessions: fox, owl.");
    Ok(())
}

#[test]
fn test_message_request_roundtrip_preserves_images_and_system_reminder() -> Result<()> {
    let req = Request::Message {
        id: 88,
        content: "inspect this".to_string(),
        images: vec![
            ("image/png".to_string(), "AAA".to_string()),
            ("image/jpeg".to_string(), "BBB".to_string()),
        ],
        system_reminder: Some("be concise".to_string()),
        active_skill: Some("verification".to_string()),
        no_reply: true,
    };
    let json = serde_json::to_string(&req)?;
    let decoded = parse_request_json(&json)?;
    let Request::Message {
        id,
        content,
        images,
        system_reminder,
        active_skill,
        no_reply,
    } = decoded
    else {
        return Err(anyhow!("expected Message"));
    };
    assert_eq!(id, 88);
    assert_eq!(content, "inspect this");
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].0, "image/png");
    assert_eq!(images[1].0, "image/jpeg");
    assert_eq!(system_reminder.as_deref(), Some("be concise"));
    assert_eq!(active_skill.as_deref(), Some("verification"));
    assert!(no_reply);
    Ok(())
}

#[test]
fn test_provider_guardrail_event_roundtrip() -> Result<()> {
    let event = ServerEvent::ProviderGuardrail {
        stop_reason: Some("refusal".to_string()),
        message: "Provider guardrail stopped the response".to_string(),
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"provider_guardrail\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ProviderGuardrail {
        stop_reason,
        message,
    } = decoded
    else {
        return Err(anyhow!("expected ProviderGuardrail event"));
    };
    assert_eq!(stop_reason.as_deref(), Some("refusal"));
    assert_eq!(message, "Provider guardrail stopped the response");

    // stop_reason is optional on the wire.
    let decoded = parse_event_json(r#"{"type":"provider_guardrail","message":"blocked"}"#)?;
    let ServerEvent::ProviderGuardrail { stop_reason, .. } = decoded else {
        return Err(anyhow!("expected ProviderGuardrail event"));
    };
    assert!(stop_reason.is_none());
    Ok(())
}

#[test]
fn test_message_end_carries_provider_stop_reason() -> Result<()> {
    // `max_tokens` is the only signal that the output budget truncated a turn.
    // Dropping it made a truncated benchmark run look like a clean one, so the
    // reason must survive the wire round trip.
    let event = ServerEvent::MessageEnd {
        stop_reason: Some("max_tokens".to_string()),
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"message_end\""));
    assert!(json.contains("\"stop_reason\":\"max_tokens\""));

    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::MessageEnd { stop_reason } = decoded else {
        return Err(anyhow!("expected MessageEnd event"));
    };
    assert_eq!(stop_reason.as_deref(), Some("max_tokens"));

    // Older producers omit the field entirely, which must still decode.
    let decoded = parse_event_json(r#"{"type":"message_end"}"#)?;
    let ServerEvent::MessageEnd { stop_reason } = decoded else {
        return Err(anyhow!("expected MessageEnd event"));
    };
    assert!(stop_reason.is_none());

    // A reasonless end-of-turn must not add noise to the wire.
    let json = encode_event(&ServerEvent::MessageEnd { stop_reason: None });
    assert!(!json.contains("stop_reason"), "unexpected field: {json}");
    Ok(())
}

#[test]
fn test_native_ssh_pong_capability_is_backward_compatible() -> Result<()> {
    let legacy: ServerEvent = serde_json::from_str(r#"{"type":"pong","id":7}"#)?;
    assert!(matches!(
        legacy,
        ServerEvent::Pong {
            id: 7,
            native_ssh_protocol: None,
            ..
        }
    ));
    let modern = ServerEvent::Pong {
        id: 7,
        native_ssh_protocol: Some(1),
        capabilities: vec!["session_tools".into()],
        user_connected_session_ids: None,
    };
    let json = serde_json::to_value(&modern)?;
    assert_eq!(json["native_ssh_protocol"], 1);
    assert!(matches!(
        serde_json::from_value::<ServerEvent>(json)?,
        ServerEvent::Pong {
            id: 7,
            native_ssh_protocol: Some(1),
            ..
        }
    ));
    assert!(
        serde_json::to_value(&legacy)?
            .get("native_ssh_protocol")
            .is_none()
    );
    Ok(())
}

#[test]
fn tool_input_optional_id_preserves_legacy_wire_format() -> Result<()> {
    let legacy = parse_event_json(r#"{"type":"tool_input","delta":"{"}"#)?;
    assert!(matches!(&legacy, ServerEvent::ToolInput { id: None, delta } if delta == "{"));
    assert_eq!(
        serde_json::to_value(legacy)?,
        serde_json::json!({"type":"tool_input","delta":"{"})
    );
    let keyed = parse_event_json(r#"{"type":"tool_input","id":"a","delta":"{}"}"#)?;
    assert!(
        matches!(&keyed, ServerEvent::ToolInput { id: Some(id), delta } if id == "a" && delta == "{}")
    );
    assert_eq!(
        serde_json::to_value(keyed)?,
        serde_json::json!({"type":"tool_input","id":"a","delta":"{}"})
    );
    Ok(())
}
use crate::wire::acp_v2_events::{BackgroundWorkEvent, BackgroundWorkItem};

/// Encode, check the wire tag and the named fields, then decode and re-encode.
fn assert_v2_event_roundtrip(event: ServerEvent, wire_type: &str, fields: &[&str]) -> Result<serde_json::Value> {
    let wire = serde_json::to_value(&event)?;
    assert_eq!(wire["type"], wire_type, "wire tag must be stable: {wire}");
    for field in fields {
        assert!(wire.get(*field).is_some(), "missing field {field}: {wire}");
    }
    let decoded = parse_event_json(&serde_json::to_string(&event)?)?;
    assert_eq!(serde_json::to_value(&decoded)?, wire, "round trip must preserve {wire_type}");
    Ok(wire)
}

#[test]
fn acp_v2_side_channel_events_roundtrip_with_their_wire_tags() -> Result<()> {
    let cancel = assert_v2_event_roundtrip(
        ServerEvent::ToolCallCancelResult {
            id: 7,
            accepted: true,
            reason: None,
        },
        "tool_call_cancel_result",
        &["id", "accepted"],
    )?;
    assert_eq!(cancel["accepted"], true);
    assert!(cancel.get("reason").is_none(), "an accepted cancel names no reason: {cancel}");

    let refused = assert_v2_event_roundtrip(
        ServerEvent::ToolCallCancelResult {
            id: 8,
            accepted: false,
            reason: Some("final".into()),
        },
        "tool_call_cancel_result",
        &["id", "accepted", "reason"],
    )?;
    assert_eq!(refused["reason"], "final");

    assert_v2_event_roundtrip(
        ServerEvent::ToolCancelled { id: "call-1".into() },
        "tool_cancelled",
        &["id"],
    )?;

    let delta = assert_v2_event_roundtrip(
        ServerEvent::ToolOutputDelta {
            id: "call-1".into(),
            data: "AAEC".into(),
        },
        "tool_output_delta",
        &["id", "data"],
    )?;
    assert_eq!(delta["data"], "AAEC");

    // A newtype variant must flatten its payload into the same object as the
    // tag, because the enum is internally tagged.
    let started = assert_v2_event_roundtrip(
        ServerEvent::BackgroundWork(BackgroundWorkEvent {
            call_id: "call-1".into(),
            task_id: "task-1".into(),
            kind: "terminal".into(),
            state: "started".into(),
            command: Some("sleep 30".into()),
            cwd: Some("/tmp".into()),
            exit_code: None,
            signal: None,
            output: None,
        }),
        "background_work",
        &["call_id", "task_id", "kind", "state", "command", "cwd"],
    )?;
    assert_eq!(started["state"], "started");
    assert!(started.get("output").is_none(), "started work carries no output: {started}");
    assert!(started.get("exit_code").is_none(), "started work carries no exit code: {started}");

    let finished = assert_v2_event_roundtrip(
        ServerEvent::BackgroundWork(BackgroundWorkEvent {
            call_id: "call-1".into(),
            task_id: "task-1".into(),
            kind: "terminal".into(),
            state: "completed".into(),
            command: Some("true".into()),
            cwd: None,
            exit_code: Some(0),
            signal: None,
            output: Some("b2s=".into()),
        }),
        "background_work",
        &["call_id", "task_id", "kind", "state", "exit_code", "output"],
    )?;
    assert_eq!(finished["exit_code"], 0);
    assert_eq!(finished["output"], "b2s=");

    let list = assert_v2_event_roundtrip(
        ServerEvent::BackgroundWorkList {
            id: 9,
            items: vec![
                BackgroundWorkItem {
                    call_id: "call-1".into(),
                    task_id: "task-1".into(),
                    kind: "terminal".into(),
                    command: Some("sleep 30".into()),
                    cwd: Some("/tmp".into()),
                    output: Some("YQ==".into()),
                },
                BackgroundWorkItem {
                    call_id: "call-2".into(),
                    task_id: "task-2".into(),
                    kind: "tool_call".into(),
                    command: None,
                    cwd: None,
                    output: None,
                },
            ],
        },
        "background_work_list",
        &["id", "items"],
    )?;
    assert_eq!(list["items"][0]["call_id"], "call-1");
    assert_eq!(list["items"][0]["output"], "YQ==");
    assert!(list["items"][1].get("command").is_none(), "absent command stays off the wire");

    assert_v2_event_roundtrip(
        ServerEvent::AskToolCall {
            request_id: "req-1".into(),
            tool_call_id: "call-q".into(),
        },
        "ask_tool_call",
        &["request_id", "tool_call_id"],
    )?;

    let stored = assert_v2_event_roundtrip(
        ServerEvent::SoftInterruptStored {
            message_id: "row-1".into(),
            content: "stop".into(),
        },
        "soft_interrupt_stored",
        &["message_id", "content"],
    )?;
    assert_eq!(stored["content"], "stop");
    Ok(())
}

#[test]
fn existing_frames_keep_their_exact_bytes() -> Result<()> {
    for frame in [
        r#"{"type":"cancel","id":1}"#,
        r#"{"type":"ask_requested","request_id":"a","questions":[]}"#,
        r#"{"type":"message","id":1,"content":"x"}"#,
    ] {
        let value: serde_json::Value = serde_json::from_str(frame)?;
        let reencoded = if value["type"] == "cancel" || value["type"] == "message" {
            let request: Request = serde_json::from_str(frame)?;
            serde_json::to_string(&request)?
        } else {
            let event: ServerEvent = serde_json::from_str(frame)?;
            serde_json::to_string(&event)?
        };
        assert_eq!(reencoded, frame, "existing frame must serialize back byte for byte");
    }
    Ok(())
}

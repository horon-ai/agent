#[test]
fn test_request_roundtrip() -> Result<()> {
    let req = Request::Message {
        id: 1,
        content: "hello".to_string(),
        images: vec![],
        system_reminder: None,
        no_reply: false,
    };
    let json = serde_json::to_string(&req)?;
    let decoded = parse_request_json(&json)?;
    assert_eq!(decoded.id(), 1);
    Ok(())
}

#[test]
fn test_notify_auth_changed_provider_hint_is_optional() -> Result<()> {
    let legacy = r#"{"type":"notify_auth_changed","id":9}"#;
    let decoded = parse_request_json(legacy)?;
    let Request::NotifyAuthChanged { id, provider } = decoded else {
        return Err(anyhow!("wrong request type"));
    };
    assert_eq!(id, 9);
    assert_eq!(provider, None);

    let req = Request::NotifyAuthChanged {
        id: 10,
        provider: Some("azure-openai".to_string()),
    };
    let json = serde_json::to_string(&req)?;
    assert!(json.contains("\"provider\":\"azure-openai\""));
    let decoded = parse_request_json(&json)?;
    let Request::NotifyAuthChanged { id, provider } = decoded else {
        return Err(anyhow!("wrong request type"));
    };
    assert_eq!(id, 10);
    assert_eq!(provider.as_deref(), Some("azure-openai"));
    Ok(())
}

#[test]
fn test_event_roundtrip() -> Result<()> {
    let event = ServerEvent::TextDelta {
        text: "hello".to_string(),
    };
    let json = encode_event(&event);
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::TextDelta { text } = decoded else {
        return Err(anyhow!("wrong event type"));
    };
    assert_eq!(text, "hello");
    Ok(())
}

#[test]
fn test_context_message_added_event_roundtrip() -> Result<()> {
    let event = ServerEvent::ContextMessageAdded { id: 42 };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"context_message_added\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ContextMessageAdded { id } = decoded else {
        return Err(anyhow!("wrong event type"));
    };
    assert_eq!(id, 42);
    Ok(())
}

#[test]
fn test_interrupted_event_decodes_from_json() -> Result<()> {
    let json = r#"{"type":"interrupted"}"#;
    let decoded = parse_event_json(json)?;
    let ServerEvent::Interrupted = decoded else {
        return Err(anyhow!("wrong event type"));
    };
    Ok(())
}

#[test]
fn test_connection_type_event_roundtrip() -> Result<()> {
    let event = ServerEvent::ConnectionType {
        connection: "websocket".to_string(),
    };
    let json = encode_event(&event);
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ConnectionType { connection } = decoded else {
        return Err(anyhow!("wrong event type"));
    };
    assert_eq!(connection, "websocket");
    Ok(())
}

#[test]
fn test_status_detail_event_roundtrip() -> Result<()> {
    let event = ServerEvent::StatusDetail {
        detail: "reusing websocket".to_string(),
    };
    let json = encode_event(&event);
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::StatusDetail { detail } = decoded else {
        return Err(anyhow!("wrong event type"));
    };
    assert_eq!(detail, "reusing websocket");
    Ok(())
}

#[test]
fn test_generated_image_event_roundtrip() -> Result<()> {
    let event = ServerEvent::GeneratedImage {
        id: "ig_123".to_string(),
        path: "/tmp/generated.png".to_string(),
        metadata_path: Some("/tmp/generated.json".to_string()),
        output_format: "png".to_string(),
        revised_prompt: Some("A polished image prompt".to_string()),
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"generated_image\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::GeneratedImage {
        id,
        path,
        metadata_path,
        output_format,
        revised_prompt,
    } = decoded else {
        return Err(anyhow!("wrong event type"));
    };
    assert_eq!(id, "ig_123");
    assert_eq!(path, "/tmp/generated.png");
    assert_eq!(metadata_path.as_deref(), Some("/tmp/generated.json"));
    assert_eq!(output_format, "png");
    assert_eq!(revised_prompt.as_deref(), Some("A polished image prompt"));
    Ok(())
}

#[test]
fn test_interrupted_event_roundtrip() -> Result<()> {
    let event = ServerEvent::Interrupted;
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"interrupted\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::Interrupted = decoded else {
        return Err(anyhow!("wrong event type"));
    };
    Ok(())
}

#[test]
fn test_history_event_decodes_without_compaction_mode_for_older_servers() -> Result<()> {
    let json = r#"{
            "type":"history",
            "id":1,
            "session_id":"ses_test_123",
            "messages":[],
            "provider_name":"openai",
            "provider_model":"gpt-5.4",
            "available_models":["gpt-5.4"],
            "connection_type":"websocket"
        }"#;
    let decoded = parse_event_json(json)?;
    let ServerEvent::History {
        provider_name,
        provider_model,
        available_models,
        connection_type,
        compaction_mode,
        ..
    } = decoded
    else {
        return Err(anyhow!("wrong event type"));
    };
    assert_eq!(provider_name.as_deref(), Some("openai"));
    assert_eq!(provider_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(available_models, vec!["gpt-5.4"]);
    assert_eq!(connection_type.as_deref(), Some("websocket"));
    assert_eq!(compaction_mode, crate::config::CompactionMode::Reactive);
    Ok(())
}

#[test]
fn test_history_event_roundtrip_preserves_messages() -> Result<()> {
    let event = ServerEvent::History {
        id: 101,
        session_id: "ses_test_456".to_string(),
        messages: vec![HistoryMessage {
            response_stats: None,
            role: "assistant".to_string(),
            content: "hello".to_string(),
            tool_calls: None,
            tool_data: None,
        }],
        images: Vec::new(),
        provider_name: Some("openai".to_string()),
        provider_model: Some("gpt-5.4".to_string()),
        available_models: vec!["gpt-5.4".to_string()],
        available_model_routes: Vec::new(),
        mcp_servers: Vec::new(),
        skills: Vec::new(),
        total_tokens: Some((123, 45)),
        token_usage_totals: Some(TokenUsageTotals {
            cache_prompt_tokens: Some(130),
            messages_with_token_usage: 2,
            input_tokens: 123,
            output_tokens: 45,
            cache_reported_input_tokens: 100,
            cache_read_input_tokens: 80,
            cache_creation_input_tokens: 10,
        }),
        all_sessions: Vec::new(),
        client_count: None,
        reload_recovery: None,
        server_version: None,
        server_name: None,
        server_icon: None,
        server_has_update: None,
        was_interrupted: None,
        connection_type: Some("websocket".to_string()),
        status_detail: None,
        upstream_provider: None,
        resolved_credential: None,
        reasoning_effort: None,
        service_tier: None,
        subagent_model: None,
        autoreview_enabled: None,
        autojudge_enabled: None,
        compaction_mode: crate::config::CompactionMode::Reactive,
        activity: None,
        acp_protocol_version: None,
    };
    let json = encode_event(&event);
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::History {
        id,
        messages,
        provider_name,
        provider_model,
        total_tokens,
        token_usage_totals,
        ..
    } = decoded
    else {
        return Err(anyhow!("expected History event"));
    };
    assert_eq!(id, 101);
    assert_eq!(provider_name.as_deref(), Some("openai"));
    assert_eq!(provider_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(total_tokens, Some((123, 45)));
    assert_eq!(
        token_usage_totals.map(|totals| totals.cache_read_input_tokens),
        Some(80)
    );
    assert_eq!(messages.len(), 1);
    Ok(())
}

#[test]
fn test_error_event_retry_after_roundtrip() -> Result<()> {
    let event = ServerEvent::Error {
        id: 42,
        message: "rate limited".to_string(),
        retry_after_secs: Some(17),
    };
    let json = encode_event(&event);
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::Error {
        id,
        message,
        retry_after_secs,
    } = decoded
    else {
        return Err(anyhow!("wrong event type"));
    };
    assert_eq!(id, 42);
    assert_eq!(message, "rate limited");
    assert_eq!(retry_after_secs, Some(17));
    Ok(())
}

#[test]
fn test_error_event_retry_after_back_compat_default() -> Result<()> {
    let json = r#"{"type":"error","id":7,"message":"oops"}"#;
    let decoded = parse_event_json(json)?;
    let ServerEvent::Error {
        id,
        message,
        retry_after_secs,
    } = decoded
    else {
        return Err(anyhow!("wrong event type"));
    };
    assert_eq!(id, 7);
    assert_eq!(message, "oops");
    assert_eq!(retry_after_secs, None);
    Ok(())
}

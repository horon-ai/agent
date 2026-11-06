fn plain_subscribe(id: u64) -> Request {
    Request::Subscribe {
        system_prompt: None,
        supports_pdf_panels: false,
        supports_user_response: false,
        supports_ask_requests: false,
        id,
        working_dir: Some("/tmp/project".to_string()),
        target_session_id: None,
        client_instance_id: Some("acp".to_string()),
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
        acp: None,
    }
}

fn acp_subscribe(id: u64, acp: AcpSubscribeOptions) -> Request {
    let Request::Subscribe {
        system_prompt,
        supports_pdf_panels,
        supports_user_response,
        supports_ask_requests,
        working_dir,
        target_session_id,
        client_instance_id,
        client_has_local_history,
        allow_session_takeover,
        crash_on_disconnect,
        continue_on_disconnect,
        terminal_env,
        ..
    } = plain_subscribe(id)
    else {
        unreachable!("plain_subscribe always builds Subscribe");
    };
    Request::Subscribe {
        system_prompt,
        supports_pdf_panels,
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
        acp: Some(acp),
    }
}

#[test]
fn acp_subscribe_options_round_trip() -> Result<()> {
    let options = AcpSubscribeOptions {
        client_capabilities: AcpClientCapabilities {
            fs_read_text_file: true,
            fs_write_text_file: false,
            terminal: true,
        },
        permission_requests: true,
        additional_dirs: vec!["/tmp/extra".to_string()],
        protocol_version: None,
    };
    let request = acp_subscribe(501, options.clone());
    let json = serde_json::to_string(&request)?;
    println!("subscribe with acp: {json}");

    let decoded = parse_request_json(&json)?;
    let Request::Subscribe { id, acp, working_dir, .. } = decoded else {
        return Err(anyhow!("expected Subscribe"));
    };
    assert_eq!(id, 501);
    assert_eq!(working_dir.as_deref(), Some("/tmp/project"));
    assert_eq!(acp, Some(options));
    Ok(())
}

#[test]
fn subscribe_without_acp_omits_field() -> Result<()> {
    let request = plain_subscribe(502);
    let json = serde_json::to_string(&request)?;
    println!("old-style subscribe: {json}");
    assert!(json.contains("\"type\":\"subscribe\""));
    let value: serde_json::Value = serde_json::from_str(&json)?;
    assert!(
        value.get("acp").is_none(),
        "a non-ACP subscriber must not put an acp key on the wire: {json}"
    );
    Ok(())
}

#[test]
fn old_subscribe_json_still_parses() -> Result<()> {
    // Back-compat: a subscriber written before this field existed sends no
    // `acp` key and must keep decoding exactly as it did.
    let decoded = parse_request_json(r#"{"type":"subscribe","id":503,"working_dir":"/tmp/legacy"}"#)?;
    let Request::Subscribe { id, acp, working_dir, .. } = decoded else {
        return Err(anyhow!("expected Subscribe"));
    };
    assert_eq!(id, 503);
    assert_eq!(working_dir.as_deref(), Some("/tmp/legacy"));
    assert_eq!(acp, None);
    Ok(())
}

#[test]
fn subscribe_rejects_acp_non_object() {
    // Malformed input: `acp` is an object or absent, never a bare string. Both
    // the raw decoder and the daemon's `decode_request` must reject it.
    let wire = r#"{"type":"subscribe","id":1,"acp":"x"}"#;
    let raw = serde_json::from_str::<Request>(wire);
    assert!(
        raw.is_err(),
        "a string acp payload must not deserialize: {raw:?}"
    );
    let decoded = decode_request(wire);
    assert!(
        decoded.is_err(),
        "decode_request must fail for a string acp payload: {decoded:?}"
    );
}

#[test]
fn client_response_round_trip() -> Result<()> {
    let ok_response = Request::ClientResponse {
        id: 601,
        request_id: "cb-1".to_string(),
        result: Some(serde_json::json!({"content": "hello"})),
        error: None,
    };
    let json = serde_json::to_string(&ok_response)?;
    assert!(json.contains("\"type\":\"client_response\""));
    assert_eq!(ok_response.id(), 601);
    let decoded = parse_request_json(&json)?;
    let Request::ClientResponse {
        id,
        request_id,
        result,
        error,
    } = decoded
    else {
        return Err(anyhow!("expected ClientResponse"));
    };
    assert_eq!(id, 601);
    assert_eq!(request_id, "cb-1");
    assert_eq!(result, Some(serde_json::json!({"content": "hello"})));
    assert_eq!(error, None);

    let failed = Request::ClientResponse {
        id: 602,
        request_id: "cb-2".to_string(),
        result: None,
        error: Some(ClientRpcError {
            code: -32601,
            message: "method not found".to_string(),
        }),
    };
    let decoded = parse_request_json(&serde_json::to_string(&failed)?)?;
    let Request::ClientResponse { request_id, error, .. } = decoded else {
        return Err(anyhow!("expected ClientResponse"));
    };
    assert_eq!(request_id, "cb-2");
    assert_eq!(
        error,
        Some(ClientRpcError {
            code: -32601,
            message: "method not found".to_string(),
        })
    );
    Ok(())
}

#[test]
fn configure_session_mcp_round_trip() -> Result<()> {
    let request = Request::ConfigureSessionMcp {
        id: 701,
        servers: vec![
            SessionMcpServer {
                name: "local".to_string(),
                transport: "stdio".to_string(),
                command: Some("mcp-server".to_string()),
                args: vec!["--stdio".to_string()],
                env: vec![("KEY".to_string(), "value".to_string())],
                url: None,
                headers: Vec::new(),
            },
            SessionMcpServer {
                name: "remote".to_string(),
                transport: "http".to_string(),
                command: None,
                args: Vec::new(),
                env: Vec::new(),
                url: Some("https://example.test/mcp".to_string()),
                headers: vec![("Authorization".to_string(), "Bearer t".to_string())],
            },
        ],
    };
    let json = serde_json::to_string(&request)?;
    assert!(json.contains("\"type\":\"configure_session_mcp\""));
    assert_eq!(request.id(), 701);

    let decoded = parse_request_json(&json)?;
    let Request::ConfigureSessionMcp { id, servers } = decoded else {
        return Err(anyhow!("expected ConfigureSessionMcp"));
    };
    assert_eq!(id, 701);
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0].name, "local");
    assert_eq!(servers[0].transport, "stdio");
    assert_eq!(servers[0].command.as_deref(), Some("mcp-server"));
    assert_eq!(servers[0].env, vec![("KEY".to_string(), "value".to_string())]);
    assert_eq!(servers[1].transport, "http");
    assert_eq!(servers[1].url.as_deref(), Some("https://example.test/mcp"));

    let minimal = parse_request_json(
        r#"{"type":"configure_session_mcp","id":702,"servers":[{"name":"bare","transport":"sse","url":"https://example.test/sse"}]}"#,
    )?;
    let Request::ConfigureSessionMcp { servers, .. } = minimal else {
        return Err(anyhow!("expected ConfigureSessionMcp"));
    };
    assert_eq!(servers[0].args, Vec::<String>::new());
    assert_eq!(servers[0].command, None);
    assert_eq!(servers[0].headers, Vec::<(String, String)>::new());
    Ok(())
}

#[test]
fn set_session_approval_mode_round_trip() -> Result<()> {
    let request = Request::SetSessionApprovalMode {
        id: 801,
        mode: Some("write".to_string()),
    };
    let json = serde_json::to_string(&request)?;
    assert!(json.contains("\"type\":\"set_session_approval_mode\""));
    assert!(json.contains("\"mode\":\"write\""));
    assert_eq!(request.id(), 801);

    let decoded = parse_request_json(&json)?;
    let Request::SetSessionApprovalMode { id, mode } = decoded else {
        return Err(anyhow!("expected SetSessionApprovalMode"));
    };
    assert_eq!(id, 801);
    assert_eq!(mode.as_deref(), Some("write"));

    let decoded = parse_request_json(r#"{"type":"set_session_approval_mode","id":802}"#)?;
    let Request::SetSessionApprovalMode { id, mode } = decoded else {
        return Err(anyhow!("expected SetSessionApprovalMode"));
    };
    assert_eq!(id, 802);
    assert_eq!(mode, None);
    Ok(())
}

#[test]
fn client_request_event_round_trip() -> Result<()> {
    let event = ServerEvent::ClientRequest {
        request_id: "cb-9".to_string(),
        tool_call_id: Some("call-1".to_string()),
        method: "fs/read_text_file".to_string(),
        params: serde_json::json!({"path": "/tmp/project/file.txt", "line": 1, "limit": 10}),
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"client_request\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ClientRequest {
        request_id,
        tool_call_id,
        method,
        params,
    } = decoded
    else {
        return Err(anyhow!("expected ClientRequest event"));
    };
    assert_eq!(request_id, "cb-9");
    assert_eq!(tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(method, "fs/read_text_file");
    assert_eq!(params["path"], "/tmp/project/file.txt");

    let standalone = ServerEvent::ClientRequest {
        request_id: "cb-10".to_string(),
        tool_call_id: None,
        method: "terminal/create".to_string(),
        params: serde_json::json!({"command": "ls"}),
    };
    let json = encode_event(&standalone);
    assert!(
        !json.contains("tool_call_id"),
        "absent tool_call_id stays off the wire: {json}"
    );
    assert!(matches!(
        parse_event_json(json.trim())?,
        ServerEvent::ClientRequest {
            tool_call_id: None,
            ..
        }
    ));
    Ok(())
}

#[test]
fn tool_permission_requested_round_trip() -> Result<()> {
    let event = ServerEvent::ToolPermissionRequested {
        request_id: "cb-perm-1".to_string(),
        tool_call_id: "call-7".to_string(),
        tool_name: "bash".to_string(),
        title: "bash: cargo test".to_string(),
        input: serde_json::json!({"command": "cargo test"}),
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"tool_permission_requested\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ToolPermissionRequested {
        request_id,
        tool_call_id,
        tool_name,
        title,
        input,
    } = decoded
    else {
        return Err(anyhow!("expected ToolPermissionRequested event"));
    };
    assert_eq!(request_id, "cb-perm-1");
    assert_eq!(tool_call_id, "call-7");
    assert_eq!(tool_name, "bash");
    assert_eq!(title, "bash: cargo test");
    assert_eq!(input["command"], "cargo test");
    Ok(())
}

#[test]
fn plan_updated_round_trip() -> Result<()> {
    let event = ServerEvent::PlanUpdated {
        entries: vec![
            PlanEntryWire {
                content: "read the protocol crate".to_string(),
                priority: "high".to_string(),
                status: "completed".to_string(),
            },
            PlanEntryWire {
                content: "add the wire surface".to_string(),
                priority: "medium".to_string(),
                status: "in_progress".to_string(),
            },
        ],
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"plan_updated\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::PlanUpdated { entries } = decoded else {
        return Err(anyhow!("expected PlanUpdated event"));
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].content, "add the wire surface");
    assert_eq!(entries[1].priority, "medium");
    assert_eq!(entries[1].status, "in_progress");
    Ok(())
}

#[test]
fn tool_diff_round_trip() -> Result<()> {
    let event = ServerEvent::ToolDiff {
        id: "call-3".to_string(),
        diffs: vec![
            FileDiffWire {
                path: "/tmp/project/new.txt".to_string(),
                old_text: None,
                new_text: "created\n".to_string(),
                deleted: false,
                old_path: None,
                created: false,
            },
            FileDiffWire {
                path: "/tmp/project/old.txt".to_string(),
                old_text: Some("before\n".to_string()),
                new_text: "after\n".to_string(),
                deleted: false,
                old_path: None,
                created: false,
            },
        ],
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"tool_diff\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ToolDiff { id, diffs } = decoded else {
        return Err(anyhow!("expected ToolDiff event"));
    };
    assert_eq!(id, "call-3");
    assert_eq!(diffs[0].old_text, None);
    assert_eq!(diffs[0].new_text, "created\n");
    assert_eq!(diffs[1].old_text.as_deref(), Some("before\n"));
    assert_eq!(diffs[1].new_text, "after\n");
    Ok(())
}

#[test]
fn tool_permission_resolved_round_trip() -> Result<()> {
    let event = ServerEvent::ToolPermissionResolved {
        request_id: "cb-perm-1".to_string(),
        approved: false,
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"tool_permission_resolved\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ToolPermissionResolved {
        request_id,
        approved,
    } = decoded
    else {
        return Err(anyhow!("expected ToolPermissionResolved event"));
    };
    assert_eq!(request_id, "cb-perm-1");
    assert!(!approved);
    Ok(())
}

#[test]
fn v2_wire_fields_are_inert_when_absent() -> Result<()> {
    // Old JSON written before these fields existed must keep parsing, and
    // serializing the defaults must not add any of the new keys.
    let message: HistoryMessage = serde_json::from_str(r#"{"role":"user","content":"hi"}"#)?;
    assert_eq!(message.message_id, None);
    assert_eq!(message.role, "user");
    let json = serde_json::to_string(&message)?;
    println!("history message without an id: {json}");
    assert!(!json.contains("message_id"), "an absent message id stays off the wire: {json}");

    let options: AcpSubscribeOptions = serde_json::from_str(
        r#"{"client_capabilities":{"fs_read_text_file":true,"terminal":true},"permission_requests":true}"#,
    )?;
    assert_eq!(options.protocol_version, None);
    assert!(options.client_capabilities.fs_read_text_file);
    let json = serde_json::to_string(&options)?;
    println!("subscribe options without a version: {json}");
    assert!(
        !json.contains("protocol_version"),
        "a connection that asked for no version must not carry one: {json}"
    );

    let diff: FileDiffWire =
        serde_json::from_str(r#"{"path":"/tmp/project/a.txt","old_text":null,"new_text":"x\n"}"#)?;
    assert!(!diff.deleted);
    assert_eq!(diff.old_path, None);
    let json = serde_json::to_string(&diff)?;
    println!("plain file diff: {json}");
    assert!(!json.contains("deleted"), "a write is not a delete: {json}");
    assert!(!json.contains("old_path"), "a write in place carries no old path: {json}");
    Ok(())
}

#[test]
fn user_message_inserted_round_trips() -> Result<()> {
    let event = ServerEvent::UserMessageInserted {
        id: 41,
        message_id: "msg-7".to_string(),
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"user_message_inserted\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::UserMessageInserted { id, message_id } = decoded else {
        return Err(anyhow!("expected UserMessageInserted event"));
    };
    assert_eq!(id, 41);
    assert_eq!(message_id, "msg-7");
    Ok(())
}

#[test]
fn foreground_state_round_trips_and_omits_defaults() -> Result<()> {
    let running = ServerEvent::ForegroundState {
        session_id: "ses-1".to_string(),
        running: true,
        run_id: "run-1".to_string(),
        start_message_id: Some("msg-1".to_string()),
        user_content: None,
        awaiting_user: false,
        stop_reason: None,
        snapshot: false,
    };
    let json = encode_event(&running);
    assert!(json.contains("\"type\":\"foreground_state\""));
    for absent in ["user_content", "awaiting_user", "stop_reason", "snapshot"] {
        assert!(!json.contains(absent), "{absent} must stay off the wire: {json}");
    }
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ForegroundState {
        session_id,
        running,
        run_id,
        start_message_id,
        user_content,
        awaiting_user,
        stop_reason,
        snapshot,
    } = decoded
    else {
        return Err(anyhow!("expected ForegroundState event"));
    };
    assert_eq!(session_id, "ses-1");
    assert!(running);
    assert_eq!(run_id, "run-1");
    assert_eq!(start_message_id.as_deref(), Some("msg-1"));
    assert_eq!(user_content, None);
    assert!(!awaiting_user);
    assert_eq!(stop_reason, None);
    assert!(!snapshot);

    let idle = ServerEvent::ForegroundState {
        session_id: "ses-1".to_string(),
        running: false,
        run_id: "run-1".to_string(),
        start_message_id: None,
        user_content: Some("do it".to_string()),
        awaiting_user: true,
        stop_reason: Some("end_turn".to_string()),
        snapshot: true,
    };
    let json = encode_event(&idle);
    assert!(
        !json.contains("start_message_id"),
        "an absent start id stays off the wire: {json}"
    );
    assert!(json.contains("\"awaiting_user\":true"));
    assert!(json.contains("\"snapshot\":true"));
    assert!(json.contains("\"stop_reason\":\"end_turn\""));
    assert!(json.contains("\"user_content\":\"do it\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ForegroundState {
        running,
        start_message_id,
        user_content,
        awaiting_user,
        stop_reason,
        snapshot,
        ..
    } = decoded
    else {
        return Err(anyhow!("expected ForegroundState event"));
    };
    assert!(!running);
    assert_eq!(start_message_id, None);
    assert_eq!(user_content.as_deref(), Some("do it"));
    assert!(awaiting_user);
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    assert!(snapshot);
    Ok(())
}

#[test]
fn tool_exit_round_trips() -> Result<()> {
    let finished = ServerEvent::ToolExit {
        id: "call-1".to_string(),
        exit_code: Some(0),
        signal: None,
        backgrounded: false,
    };
    let json = encode_event(&finished);
    assert!(json.contains("\"type\":\"tool_exit\""));
    assert!(json.contains("\"exit_code\":0"));
    assert!(!json.contains("signal"), "an absent signal stays off the wire: {json}");
    assert!(
        !json.contains("backgrounded"),
        "a foreground exit stays off the wire: {json}"
    );
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ToolExit {
        id,
        exit_code,
        signal,
        backgrounded,
    } = decoded
    else {
        return Err(anyhow!("expected ToolExit event"));
    };
    assert_eq!(id, "call-1");
    assert_eq!(exit_code, Some(0));
    assert_eq!(signal, None);
    assert!(!backgrounded);

    let backgrounded = ServerEvent::ToolExit {
        id: "call-2".to_string(),
        exit_code: None,
        signal: Some("SIGKILL".to_string()),
        backgrounded: true,
    };
    let json = encode_event(&backgrounded);
    assert!(
        !json.contains("exit_code"),
        "an exit without a code stays off the wire: {json}"
    );
    assert!(json.contains("\"signal\":\"SIGKILL\""));
    assert!(json.contains("\"backgrounded\":true"));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ToolExit {
        exit_code,
        signal,
        backgrounded,
        ..
    } = decoded
    else {
        return Err(anyhow!("expected ToolExit event"));
    };
    assert_eq!(exit_code, None);
    assert_eq!(signal.as_deref(), Some("SIGKILL"));
    assert!(backgrounded);
    Ok(())
}

#[test]
fn withdraw_message_round_trips_and_reports_id() -> Result<()> {
    let request = Request::WithdrawMessage {
        id: 91,
        message_request_id: 90,
    };
    let json = serde_json::to_string(&request)?;
    println!("withdraw: {json}");
    assert!(json.contains("\"type\":\"withdraw_message\""));
    assert_eq!(request.id(), 91);

    let decoded = parse_request_json(&json)?;
    assert_eq!(decoded.id(), 91);
    let Request::WithdrawMessage {
        id,
        message_request_id,
    } = decoded
    else {
        return Err(anyhow!("expected WithdrawMessage"));
    };
    assert_eq!(id, 91);
    assert_eq!(message_request_id, 90);
    Ok(())
}

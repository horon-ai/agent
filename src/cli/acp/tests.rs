use super::prompt::*;
use super::*;
use crate::protocol::SessionMcpServer;
use std::path::Path;

mod golden;

#[test]
fn acp_tool_kind_maps_core_tools() {
    assert_eq!(tool_kind("read"), "read");
    assert_eq!(tool_kind("apply_patch"), "edit");
    assert_eq!(tool_kind("bash"), "execute");
    assert_eq!(tool_kind("agentgrep"), "search");
    assert_eq!(tool_kind("webfetch"), "fetch");
    assert_eq!(tool_kind("swarm"), "other");
}

#[test]
fn json_rpc_parse_errors_use_standard_codes() {
    let (code, _) = JsonRpcMessage::parse("not json").unwrap_err();
    assert_eq!(code, JSONRPC_PARSE_ERROR);

    let (code, message) = JsonRpcMessage::parse(r#"{"method":"initialize"}"#).unwrap_err();
    assert_eq!(code, JSONRPC_INVALID_REQUEST);
    assert!(message.contains("jsonrpc"));
}

#[test]
fn prompt_from_params_accepts_text_images_and_resources() {
    let params = json!({
        "sessionId": "s1",
        "prompt": [
            {"type": "text", "text": "hello"},
            {"type": "image", "mimeType": "image/png", "data": "abc"},
            {"type": "resource", "resource": {"uri": "file:///tmp/a.rs", "text": "fn main(){}"}},
            {"type": "resource_link", "uri": "file:///tmp/b.rs", "name": "b.rs"}
        ]
    });
    let (text, images) = prompt_from_params(&params).unwrap();
    assert!(text.contains("hello"));
    assert!(text.contains("Embedded resource: file:///tmp/a.rs"));
    assert!(text.contains("Resource link: b.rs"));
    assert_eq!(images, vec![("image/png".to_string(), "abc".to_string())]);
}

#[test]
fn prompt_response_reports_usage_accumulated_across_the_turn() {
    let mut usage = TurnUsage::default();
    usage.add(10, 2, Some(4), Some(5));
    usage.add(20, 3, Some(6), Some(7));

    assert_eq!(
        prompt_response("end_turn", &usage),
        json!({
            "stopReason": "end_turn",
            "_meta": {"horon-agent": {"usage": {
                "totalTokens": 57,
                "inputTokens": 30,
                "outputTokens": 5,
                "cachedReadTokens": 10,
                "cachedWriteTokens": 12,
            }}}
        })
    );
}

#[test]
fn prompt_response_omits_unreported_usage_and_cache_fields() {
    assert_eq!(
        prompt_response("end_turn", &TurnUsage::default()),
        json!({ "stopReason": "end_turn" })
    );

    let mut usage = TurnUsage::default();
    usage.add(10, 2, None, None);
    assert_eq!(
        prompt_response("cancelled", &usage),
        json!({
            "stopReason": "cancelled",
            "_meta": {"horon-agent": {"usage": {
                "totalTokens": 12,
                "inputTokens": 10,
                "outputTokens": 2,
            }}}
        })
    );
}

#[test]
fn initialize_standard_omits_horon_agent_meta() {
    let result = initialize_result(AcpProfile::Standard);
    assert_eq!(result["protocolVersion"], 1);
    assert!(result["agentCapabilities"].get("_meta").is_none());
    assert_eq!(result["agentCapabilities"]["loadSession"], true);
}

#[test]
fn initialize_full_advertises_horon_agent_extension_meta() {
    let result = initialize_result(AcpProfile::Full);
    assert_eq!(result["agentCapabilities"]["_meta"]["horon-agent"]["profile"], "full");
}

#[test]
fn event_mapper_maps_tool_lifecycle() {
    let mut mapper = EventMapper::new(AcpProfile::Standard, MapperOptions::default(), "t1".to_string());
    let start = mapper.map_event(ServerEvent::ToolStart {
        id: "tool1".to_string(),
        name: "bash".to_string(),
    });
    assert_eq!(start[0]["sessionUpdate"], "tool_call");
    assert_eq!(start[0]["kind"], "execute");

    let input = mapper.map_event(ServerEvent::ToolInput {
        id: None,
        delta: "{\"command\":\"true\"}".to_string(),
    });
    assert_eq!(input[0]["rawInput"]["command"], "true");

    let done = mapper.map_event(ServerEvent::ToolDone {
        id: "tool1".to_string(),
        name: "bash".to_string(),
        output: "ok".to_string(),
        error: None,
    });
    assert_eq!(done[0]["status"], "completed");
    assert_eq!(done[0]["content"][0]["content"]["text"], "ok");
}

fn event(value: Value) -> ServerEvent {
    serde_json::from_value(value.clone()).unwrap_or_else(|err| panic!("not a ServerEvent ({err}): {value}"))
}

fn mapper_with(options: MapperOptions) -> EventMapper {
    EventMapper::new(AcpProfile::Standard, options, "t1".to_string())
}

fn work_dir() -> MapperOptions {
    MapperOptions {
        cwd: Some(PathBuf::from("/work")),
        ..MapperOptions::default()
    }
}

#[test]
fn maps_reasoning_to_thought_chunk() {
    let mut mapper = mapper_with(MapperOptions::default());
    let updates = mapper.map_event(event(json!({"type":"reasoning_delta","text":"thinking"})));
    assert_eq!(
        updates,
        vec![json!({
            "sessionUpdate": "agent_thought_chunk",
            "messageId": "t1-thought-0",
            "content": {"type": "text", "text": "thinking"},
        })]
    );
    assert!(mapper.map_event(event(json!({"type":"reasoning_done"}))).is_empty());
}

#[test]
fn message_ids_change_after_tool_round() {
    let mut mapper = mapper_with(MapperOptions::default());
    let id_of = |updates: Vec<Value>| updates[0]["messageId"].as_str().unwrap().to_string();
    assert_eq!(
        id_of(mapper.map_event(event(json!({"type":"text_delta","text":"a"})))),
        "t1-msg-0"
    );
    assert_eq!(
        id_of(mapper.map_event(event(json!({"type":"text_delta","text":"b"})))),
        "t1-msg-0"
    );
    mapper.map_event(event(json!({"type":"tool_start","id":"c1","name":"bash"})));
    mapper.map_event(event(json!({"type":"tool_exec","id":"c1","name":"bash"})));
    mapper.map_event(event(json!({"type":"tool_done","id":"c1","name":"bash","output":"ok"})));
    assert_eq!(
        id_of(mapper.map_event(event(json!({"type":"reasoning_delta","text":"r"})))),
        "t1-thought-1"
    );
    assert_eq!(
        id_of(mapper.map_event(event(json!({"type":"text_delta","text":"c"})))),
        "t1-msg-1"
    );
    assert_eq!(
        id_of(mapper.map_event(event(json!({"type":"text_replace","text":"c"})))),
        "t1-msg-1"
    );
    mapper.map_event(event(json!({"type":"tool_done","id":"c2","name":"read","output":"x"})));
    assert_eq!(
        id_of(mapper.map_event(event(json!({"type":"text_delta","text":"d"})))),
        "t1-msg-2"
    );
}

#[test]
fn tool_call_has_absolute_locations() {
    let mut mapper = mapper_with(work_dir());
    let call = mapper.map_event(event(json!({"type":"tool_start","id":"c1","name":"read"})));
    assert_eq!(call[0]["sessionUpdate"], "tool_call");
    assert_eq!(call[0]["name"], "read");
    assert_eq!(call[0]["kind"], "read");
    assert_eq!(call[0]["status"], "pending");
    let partial = mapper.map_event(event(json!({"type":"tool_input","delta":"{\"file_path\":"})));
    assert!(partial[0].get("locations").is_none(), "{partial:?}");
    let input = mapper.map_event(event(json!({"type":"tool_input","delta":"\"src/a.rs\"}"})));
    assert_eq!(input[0]["locations"], json!([{"path": "/work/src/a.rs"}]));
    assert_eq!(input[0]["rawInput"], json!({"file_path": "src/a.rs"}));
    let exec = mapper.map_event(event(json!({"type":"tool_exec","id":"c1","name":"read"})));
    assert_eq!(exec[0]["locations"], json!([{"path": "/work/src/a.rs"}]));
    assert_eq!(exec[0]["status"], "in_progress");

    for (key, value, expected) in [
        ("path", "/etc/hosts", Some("/etc/hosts")),
        ("filePath", "b.rs", Some("/work/b.rs")),
        ("command", "true", None),
    ] {
        let id = format!("c-{key}");
        mapper.map_event(event(json!({"type":"tool_start","id":id,"name":"edit"})));
        let input = json!({ key: value }).to_string();
        let update = mapper.map_event(event(json!({"type":"tool_input","id":id,"delta":input})));
        match expected {
            Some(path) => assert_eq!(update[0]["locations"], json!([{"path": path}]), "{key}"),
            None => assert!(update[0].get("locations").is_none(), "{key}: {update:?}"),
        }
    }

    let mut no_cwd = mapper_with(MapperOptions::default());
    no_cwd.map_event(event(json!({"type":"tool_start","id":"c9","name":"read"})));
    let update = no_cwd.map_event(event(json!({"type":"tool_input","delta":"{\"path\":\"rel.rs\"}"})));
    assert!(
        update[0].get("locations").is_none(),
        "a relative path without a cwd is never reported: {update:?}"
    );
}

#[test]
fn tool_diff_becomes_diff_content() {
    let diff = json!({"type":"tool_diff","id":"c1","diffs":[
        {"path":"a.rs","old_text":null,"new_text":"new"},
        {"path":"/abs/b.rs","old_text":"old","new_text":"newer"}]});
    let expected_diffs = json!([
        {"type": "diff", "path": "/work/a.rs", "oldText": null, "newText": "new"},
        {"type": "diff", "path": "/abs/b.rs", "oldText": "old", "newText": "newer"},
    ]);
    let done = json!({"type":"tool_done","id":"c1","name":"write","output":"wrote 2 files"});
    let text = json!({"type": "content", "content": {"type": "text", "text": "wrote 2 files"}});

    let mut mapper = mapper_with(work_dir());
    let update = mapper.map_event(event(diff.clone()));
    assert_eq!(update[0]["sessionUpdate"], "tool_call_update");
    assert_eq!(update[0]["content"], expected_diffs);
    let finished = mapper.map_event(event(done.clone()));
    let mut content = expected_diffs.as_array().unwrap().clone();
    content.push(text.clone());
    assert_eq!(finished[0]["content"], Value::Array(content.clone()));
    assert_eq!(finished[0]["status"], "completed");

    let mut late = mapper_with(work_dir());
    late.map_event(event(done));
    let update = late.map_event(event(diff));
    assert_eq!(
        update[0]["content"],
        Value::Array(content),
        "a diff after the result keeps the result text"
    );
}

#[test]
fn plan_updated_becomes_plan() {
    let mut mapper = mapper_with(MapperOptions::default());
    let updates = mapper.map_event(event(json!({"type":"plan_updated","entries":[
        {"content":"read the code","priority":"high","status":"completed"},
        {"content":"write the fix","priority":"medium","status":"in_progress"}]})));
    assert_eq!(
        updates,
        vec![json!({
            "sessionUpdate": "plan",
            "entries": [
                {"content": "read the code", "priority": "high", "status": "completed"},
                {"content": "write the fix", "priority": "medium", "status": "in_progress"},
            ]
        })]
    );
    let empty = mapper.map_event(event(json!({"type":"plan_updated","entries":[]})));
    assert_eq!(empty, vec![json!({"sessionUpdate": "plan", "entries": []})]);
}

#[test]
fn notice_only_with_capability() {
    let guardrail = json!({"type":"provider_guardrail","stop_reason":"refusal","message":"The model declined."});
    let stopped = json!({"type":"turn_stopped","reason":"limit_reached","message":"Context limit exceeded"});
    let failed = json!({"type":"turn_stopped","reason":"failure","message":""});
    let notification = json!({"type":"notification","from_session":"s2","from_name":"fox",
        "notification_type":{"kind":"shared_context","key":"k","value":"v"},"message":"fox shared k"});

    for capabilities in [
        json!({}),
        json!({"session": {}}),
        json!({"session": {"notices": true}}),
        json!({"session": {"notices": null}}),
    ] {
        let mut mapper = mapper_with(MapperOptions::new(&capabilities, None));
        assert_eq!(
            mapper.map_event(event(guardrail.clone())),
            vec![json!({"sessionUpdate": "agent_message_chunk", "messageId": "t1-msg-0",
                "content": {"type": "text", "text": "The model declined."}})],
            "{capabilities}"
        );
        assert!(mapper.map_event(event(stopped.clone())).is_empty(), "{capabilities}");
        assert!(
            mapper.map_event(event(notification.clone())).is_empty(),
            "{capabilities}"
        );
    }

    let mut mapper = mapper_with(MapperOptions::new(&json!({"session": {"notices": {}}}), None));
    assert_eq!(
        mapper.map_event(event(guardrail)),
        vec![json!({"sessionUpdate": "notice", "severity": "warning", "title": "The model declined."})]
    );
    assert_eq!(
        mapper.map_event(event(stopped)),
        vec![json!({"sessionUpdate": "notice", "severity": "warning", "title": "Context limit exceeded"})]
    );
    assert_eq!(
        mapper.map_event(event(failed)),
        vec![json!({"sessionUpdate": "notice", "severity": "error", "title": "The turn stopped"})]
    );
    assert_eq!(
        mapper.map_event(event(notification)),
        vec![json!({"sessionUpdate": "notice", "severity": "info", "title": "fox shared k"})]
    );
}

#[test]
fn compaction_only_with_capability() {
    let compaction = json!({"type":"compaction","trigger":"background","pre_tokens":100});
    let mut standard = mapper_with(MapperOptions::new(&json!({"session": {"notices": {}}}), None));
    assert!(standard.map_event(event(compaction.clone())).is_empty());

    let mut extended = EventMapper::new(AcpProfile::Extended, MapperOptions::default(), "t1".to_string());
    let text = extended.map_event(event(compaction.clone()));
    assert_eq!(text[0]["sessionUpdate"], "agent_message_chunk");
    assert!(text[0]["content"]["text"].as_str().unwrap().contains("background"));

    let options = MapperOptions::new(&json!({"session": {"compaction": {}}}), None);
    let mut capable = EventMapper::new(AcpProfile::Extended, options, "t1".to_string());
    assert_eq!(
        capable.map_event(event(compaction.clone())),
        vec![json!({"sessionUpdate": "compaction_update", "compactionId": "t1-cmp-1", "status": "completed"})]
    );
    assert_eq!(
        capable.map_event(event(compaction))[0]["compactionId"],
        "t1-cmp-2",
        "every compaction gets its own id"
    );
}

/// (name, events before the end, cancel recorded, how the stream ended, expected)
type StopCase = (
    &'static str,
    Vec<ServerEvent>,
    bool,
    TurnEnd,
    Result<StopReason, String>,
);

#[test]
fn stop_reason_mapping_table() {
    let stopped = |reason: &str, provider: Option<&str>| {
        let mut value = json!({"type":"turn_stopped","reason":reason,"message":format!("{reason} stop")});
        if let Some(provider) = provider {
            value["provider_stop_reason"] = json!(provider);
        }
        event(value)
    };
    let guardrail = |stop: &str| event(json!({"type":"provider_guardrail","stop_reason":stop,"message":"m"}));
    let done = || TurnEnd::Done;
    let error = || TurnEnd::Error("provider exploded".to_string());
    let cases: Vec<StopCase> = vec![
        ("done", vec![], false, done(), Ok(StopReason::EndTurn)),
        (
            "interrupted event",
            vec![ServerEvent::Interrupted],
            false,
            done(),
            Ok(StopReason::Cancelled),
        ),
        ("cancel recorded", vec![], true, done(), Ok(StopReason::Cancelled)),
        ("error after cancel", vec![], true, error(), Ok(StopReason::Cancelled)),
        (
            "failure after cancel",
            vec![stopped("failure", None)],
            true,
            error(),
            Ok(StopReason::Cancelled),
        ),
        (
            "abandoned after cancel",
            vec![],
            true,
            TurnEnd::Abandoned,
            Ok(StopReason::Cancelled),
        ),
        (
            "guardrail stop",
            vec![stopped("provider_guardrail", None)],
            false,
            done(),
            Ok(StopReason::Refusal),
        ),
        (
            "guardrail refusal",
            vec![guardrail("refusal")],
            false,
            done(),
            Ok(StopReason::Refusal),
        ),
        (
            "guardrail content filter",
            vec![guardrail("content_filter")],
            false,
            done(),
            Ok(StopReason::Refusal),
        ),
        (
            "guardrail empty output",
            vec![guardrail("end_turn")],
            false,
            done(),
            Ok(StopReason::EndTurn),
        ),
        (
            "limit max_tokens",
            vec![stopped("limit_reached", Some("max_tokens"))],
            false,
            done(),
            Ok(StopReason::MaxTokens),
        ),
        (
            "limit max_output_tokens",
            vec![stopped("limit_reached", Some("max_output_tokens"))],
            false,
            done(),
            Ok(StopReason::MaxTokens),
        ),
        (
            "limit length",
            vec![stopped("limit_reached", Some("length"))],
            false,
            done(),
            Ok(StopReason::MaxTokens),
        ),
        (
            "limit incomplete",
            vec![stopped("limit_reached", Some("incomplete"))],
            false,
            done(),
            Ok(StopReason::MaxTokens),
        ),
        (
            "limit tool_use",
            vec![stopped("limit_reached", Some("tool_use"))],
            false,
            done(),
            Ok(StopReason::MaxTurnRequests),
        ),
        (
            "limit no provider reason",
            vec![stopped("limit_reached", None)],
            false,
            done(),
            Ok(StopReason::MaxTurnRequests),
        ),
        (
            "limit then failure then error",
            vec![stopped("limit_reached", None), stopped("failure", None)],
            false,
            error(),
            Ok(StopReason::MaxTurnRequests),
        ),
        (
            "interrupted stop",
            vec![stopped("interrupted", None)],
            false,
            done(),
            Ok(StopReason::Cancelled),
        ),
        (
            "interrupt overrides limit",
            vec![stopped("limit_reached", Some("max_tokens")), ServerEvent::Interrupted],
            false,
            done(),
            Ok(StopReason::Cancelled),
        ),
        (
            "failure",
            vec![stopped("failure", None)],
            false,
            error(),
            Err("provider exploded".to_string()),
        ),
        (
            "crash",
            vec![stopped("crash", None)],
            false,
            done(),
            Err("crash stop".to_string()),
        ),
        (
            "unknown reason",
            vec![stopped("something_new", None)],
            false,
            done(),
            Err("something_new stop".to_string()),
        ),
        (
            "plain error",
            vec![],
            false,
            error(),
            Err("provider exploded".to_string()),
        ),
    ];
    for (name, events, cancel_recorded, end, expected) in cases {
        let mut outcome = TurnOutcome::default();
        for event in &events {
            outcome.observe(event);
        }
        assert_eq!(outcome.resolve(cancel_recorded, end), expected, "{name}");
    }
    assert_eq!(
        [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::MaxTurnRequests,
            StopReason::Refusal,
            StopReason::Cancelled
        ]
        .map(StopReason::as_str),
        ["end_turn", "max_tokens", "max_turn_requests", "refusal", "cancelled"]
    );
}

#[test]
fn prompt_blocks_reject_audio_unknown_and_malformed() {
    for (block, needle) in [
        (
            json!({"type":"audio","mimeType":"audio/wav","data":"AAAA"}),
            "audio not supported",
        ),
        (json!({"type":"video","data":"x"}), "video"),
        (json!({"text":"no type"}), "missing type"),
        (json!({"type":"text"}), "missing text"),
        (json!({"type":"image","data":"abc"}), "mimeType"),
        (json!({"type":"resource","resource":{"text":"no uri"}}), "resource.uri"),
        (json!({"type":"resource_link","name":"n"}), "uri"),
    ] {
        let params = json!({"prompt": [{"type":"text","text":"ok"}, block]});
        let err = prompt_from_params(&params).unwrap_err();
        assert!(err.contains(needle), "{block}: {err}");
    }
    let blob = json!({"prompt": [{"type":"resource","resource":{"uri":"file:///x.bin","blob":"QUJD","mimeType":"application/x"}}]});
    let (text, _) = prompt_from_params(&blob).unwrap();
    assert_eq!(
        text,
        "[Embedded binary resource: file:///x.bin (application/x, 4 base64 bytes)]"
    );
}

#[test]
fn initialize_advertises_full_v1_surface() {
    let result = initialize_result(AcpProfile::Standard);
    assert_eq!(
        result["agentCapabilities"],
        json!({
            "loadSession": true,
            "promptCapabilities": {"image": true, "audio": false, "embeddedContext": true},
            "mcpCapabilities": {"http": true, "sse": true},
            "sessionCapabilities": {"close": {}, "resume": {}, "list": {}, "delete": {}, "additionalDirectories": {}},
        })
    );
    assert!(
        result["agentCapabilities"].get("auth").is_none(),
        "logout is never advertised"
    );
    assert_eq!(
        result["authMethods"],
        json!([{
            "id": "horon-agent-credentials",
            "name": "Use HoronAgent login",
            "description": "Uses the OpenAI-compatible provider profile configured in [providers.<name>]. Run 'horon-agent provider add' to create one.",
        }])
    );
}

#[test]
fn auth_methods_ignore_terminal_capability() {
    // A client's `auth.terminal` capability adds nothing: this build has no
    // login command for a terminal method to run.
    let methods = auth_methods();
    assert_eq!(methods.len(), 1, "{methods:?}");
    assert_eq!(methods[0]["id"], "horon-agent-credentials");
    assert!(methods.iter().all(|method| method.get("type").is_none()), "{methods:?}");
    let description = methods[0]["description"].as_str().expect("description");
    assert!(description.contains("[providers.<name>]"), "{description}");
    assert!(description.contains("horon-agent provider add"), "{description}");
    assert!(
        !description.contains("horon-agent login"),
        "the removed login command is never advertised: {description}"
    );
}

#[test]
fn acp_mcp_servers_map_to_session_mcp() {
    let params = json!({"mcpServers": [
        {"name": "fs", "command": "/usr/bin/mcp-fs", "args": ["--root", "/tmp"], "env": [{"name": "TOKEN", "value": "t"}]},
        {"type": "stdio", "name": "bare", "command": "/bin/mcp"},
        {"type": "http", "name": "web", "url": "https://example.com/mcp", "headers": [{"name": "Authorization", "value": "Bearer t"}]},
        {"type": "sse", "name": "events", "url": "https://example.com/sse", "headers": []},
    ]});
    assert_eq!(
        mcp_servers_from_params(&params).unwrap(),
        vec![
            SessionMcpServer {
                name: "fs".to_string(),
                transport: "stdio".to_string(),
                command: Some("/usr/bin/mcp-fs".to_string()),
                args: vec!["--root".to_string(), "/tmp".to_string()],
                env: vec![("TOKEN".to_string(), "t".to_string())],
                url: None,
                headers: Vec::new(),
            },
            SessionMcpServer {
                name: "bare".to_string(),
                transport: "stdio".to_string(),
                command: Some("/bin/mcp".to_string()),
                args: Vec::new(),
                env: Vec::new(),
                url: None,
                headers: Vec::new(),
            },
            SessionMcpServer {
                name: "web".to_string(),
                transport: "http".to_string(),
                command: None,
                args: Vec::new(),
                env: Vec::new(),
                url: Some("https://example.com/mcp".to_string()),
                headers: vec![("Authorization".to_string(), "Bearer t".to_string())],
            },
            SessionMcpServer {
                name: "events".to_string(),
                transport: "sse".to_string(),
                command: None,
                args: Vec::new(),
                env: Vec::new(),
                url: Some("https://example.com/sse".to_string()),
                headers: Vec::new(),
            },
        ]
    );
    assert!(mcp_servers_from_params(&json!({"mcpServers": []})).unwrap().is_empty());
    assert!(mcp_servers_from_params(&json!({})).unwrap().is_empty());
}

#[test]
fn acp_mcp_servers_reject_bad_shape() {
    for servers in [
        json!({}),
        json!("fs"),
        json!(["fs"]),
        json!([{"command": "/bin/mcp"}]),
        json!([{"name": "fs"}]),
        json!([{"name": "fs", "command": 7}]),
        json!([{"name": "fs", "command": "/bin/mcp", "args": "--root"}]),
        json!([{"name": "fs", "command": "/bin/mcp", "args": [1]}]),
        json!([{"name": "fs", "command": "/bin/mcp", "env": [{"name": "TOKEN"}]}]),
        json!([{"name": "fs", "command": "/bin/mcp", "env": {"TOKEN": "t"}}]),
        json!([{"type": "ws", "name": "fs", "url": "ws://localhost"}]),
        // Remote servers need a string url and name/value headers.
        json!([{"type": "http", "name": "web", "headers": []}]),
        json!([{"type": "sse", "name": "web", "url": 7, "headers": []}]),
        json!([{"type": "http", "name": "web", "url": "https://example.com/mcp", "headers": {"A": "b"}}]),
    ] {
        let params = json!({"mcpServers": servers});
        assert!(mcp_servers_from_params(&params).is_err(), "accepted {params}");
    }
}

#[test]
fn advertised_commands_cover_all_acp_daemon_model_controls() {
    let commands = acp_available_commands();
    let names: Vec<&str> = commands
        .iter()
        .map(|command| command["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["model", "models", "effort"]);
    assert_eq!(commands[0]["input"]["hint"], "model id (optional)");
    assert!(commands[1].get("input").is_none());
    assert!(commands[2]["input"]["hint"].as_str().unwrap().contains("high"));
}

#[test]
fn advertised_commands_parse_to_real_dispatch_variants() {
    assert_eq!(
        parse_acp_slash_command("/model claude-sonnet-4-5").unwrap().unwrap(),
        AcpSlashCommand::Model(Some("claude-sonnet-4-5".to_string()))
    );
    assert_eq!(
        parse_acp_slash_command("/model ").unwrap().unwrap(),
        AcpSlashCommand::Model(None)
    );
    assert_eq!(
        parse_acp_slash_command("/models").unwrap().unwrap(),
        AcpSlashCommand::Models
    );
    assert_eq!(
        parse_acp_slash_command("/effort xhigh").unwrap().unwrap(),
        AcpSlashCommand::Effort(Some("xhigh".to_string()))
    );
    assert!(parse_acp_slash_command("/models now").unwrap().is_err());
    assert!(parse_acp_slash_command("/not-advertised").is_none());
    assert!(parse_acp_slash_command(" /model literal").is_none());
    assert!(parse_acp_slash_command("ordinary prompt").is_none());
}

#[test]
fn compatibility_methods_accept_host_field_names_and_aliases() {
    assert_eq!(
        compatibility_option_value(
            &json!({"modelId": "deepseek-v4-flash"}),
            &["modelId", "model"],
            "session/set_model"
        )
        .unwrap(),
        "deepseek-v4-flash"
    );
    assert_eq!(
        compatibility_option_value(
            &json!({"reasoningEffort": "high"}),
            &["effort", "reasoningEffort"],
            "session/set_reasoning_effort"
        )
        .unwrap(),
        "high"
    );
    assert!(
        compatibility_option_value(
            &json!({"effort": ""}),
            &["effort", "reasoningEffort"],
            "session/set_reasoning_effort"
        )
        .unwrap_err()
        .contains("non-empty")
    );
}

#[test]
fn cwd_must_be_absolute() {
    for params in [
        json!({}),
        json!({"cwd": null}),
        json!({"cwd": ""}),
        json!({"cwd": "relative"}),
        json!({"cwd": 3}),
    ] {
        assert!(cwd_from_params(&params).is_err(), "accepted {params}");
    }
    let params = json!({"cwd": "/tmp"});
    assert_eq!(cwd_from_params(&params).unwrap(), Path::new("/tmp"));
}

#[test]
fn session_setup_collects_additional_directories() {
    let params = json!({"cwd": "/work", "mcpServers": [], "additionalDirectories": ["/extra/a", "/extra/b"]});
    let setup = session_setup_from_params(&params).unwrap();
    assert_eq!(setup.additional_dirs, ["/extra/a", "/extra/b"]);
    for dirs in [json!(["relative"]), json!("/extra"), json!([1])] {
        let params = json!({"cwd": "/work", "mcpServers": [], "additionalDirectories": dirs});
        assert!(session_setup_from_params(&params).is_err(), "accepted {params}");
    }
}

#[test]
fn subscribe_options_follow_advertised_client_capabilities() {
    let caps =
        json!({"fs": {"readTextFile": true, "writeTextFile": false}, "terminal": true, "elicitation": {"form": {}}});
    let options = acp_subscribe_options(&caps, vec!["/extra".to_string()]);
    assert!(options.client_capabilities.fs_read_text_file);
    assert!(!options.client_capabilities.fs_write_text_file);
    assert!(options.client_capabilities.terminal);
    assert!(options.permission_requests);
    assert_eq!(options.additional_dirs, ["/extra"]);
    assert!(supports_elicitation_form(&caps));
    assert!(!supports_elicitation_form(&json!({"elicitation": {}})));
    assert!(!supports_elicitation_form(&json!({"elicitation": {"form": null}})));
    let none = acp_subscribe_options(&json!({}), Vec::new());
    assert_eq!(none.client_capabilities, Default::default());
}

#[test]
fn readiness_requires_a_reported_model() {
    assert!(model_is_usable(Some("gpt-5"), &[]));
    assert!(model_is_usable(None, &["gpt-5".to_string()]));
    assert!(!model_is_usable(None, &[]));
    assert!(!model_is_usable(Some("  "), &[]));
}

fn list_row(id: &str, cwd: Option<&str>, last_active_at_ms: i64, archived: bool) -> ListRow {
    ListRow {
        session_id: id.to_string(),
        cwd: cwd.map(str::to_string),
        title: Some(format!("title {id}")),
        updated_at: chrono::DateTime::from_timestamp_millis(last_active_at_ms).unwrap(),
        last_active_at_ms: Some(last_active_at_ms),
        archived,
    }
}

fn page_ids(page: &Value) -> Vec<String> {
    page["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|info| info["sessionId"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn session_list_cursor_round_trip() {
    assert_eq!(encode_cursor(50), "bzo1MA", "base64url of o:50 without padding");
    for offset in [0, 1, 49, 50, 12_345, usize::MAX] {
        assert_eq!(decode_cursor(&encode_cursor(offset)), Some(offset));
    }
    let rows: Vec<ListRow> = (0..120)
        .map(|index| list_row(&format!("s{index:03}"), Some("/work"), 1_000 + index, false))
        .collect();
    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = session_list_page(rows.clone(), None, cursor.as_deref(), AcpVersion::V1).unwrap();
        pages += 1;
        seen.extend(page_ids(&page));
        match page.get("nextCursor").and_then(Value::as_str) {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    let expected: Vec<String> = (0..120).rev().map(|index| format!("s{index:03}")).collect();
    assert_eq!(seen, expected, "newest activity first, every row exactly once");
    let first = session_list_page(rows, None, None, AcpVersion::V1).unwrap();
    assert_eq!(first["sessions"][0]["cwd"], "/work");
    assert_eq!(first["sessions"][0]["title"], "title s119");
    assert_eq!(first["sessions"][0]["updatedAt"], "1970-01-01T00:00:01.119Z");
}

#[test]
fn session_list_cursor_rejects_malformed_tokens() {
    // "bzo" is o:, "eDox" is x:1, "bzot" is o:-, "bzoxYQ" is o:1a.
    for cursor in ["", "!!!!", "bzo", "eDox", "bzot", "bzoxYQ", "o:1", "A"] {
        assert_eq!(decode_cursor(cursor), None, "{cursor}");
        assert!(
            session_list_page(Vec::new(), None, Some(cursor), AcpVersion::V1).is_err(),
            "{cursor}"
        );
    }
}

#[test]
fn session_list_filters_cwd_and_archived() {
    let rows = vec![
        list_row("a", Some("/work/a"), 3, false),
        list_row("b", Some("/work/b"), 2, false),
        list_row("archived", Some("/work/a"), 5, true),
        list_row("relative", Some("work/a"), 4, false),
        list_row("no-cwd", None, 6, false),
    ];
    assert_eq!(
        page_ids(&session_list_page(rows.clone(), None, None, AcpVersion::V1).unwrap()),
        ["a", "b"]
    );
    assert_eq!(
        page_ids(&session_list_page(rows.clone(), Some("/work/a"), None, AcpVersion::V1).unwrap()),
        ["a"]
    );
    let empty = session_list_page(rows, Some("/nowhere"), None, AcpVersion::V1).unwrap();
    assert_eq!(empty, json!({"sessions": []}));

    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    let saved = (|| -> anyhow::Result<Vec<ListRow>> {
        use crate::message::{ContentBlock, Role};
        let text = |text: &str| {
            vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }]
        };
        let mut kept = crate::session::Session::create_with_id("session_acp_list_kept".to_string(), None, None);
        kept.working_dir = Some("/work/kept".to_string());
        kept.add_message(Role::User, text("kept"));
        kept.rename_title(Some("Kept work".to_string()));
        kept.save()?;
        let mut hidden = crate::session::Session::create_with_id("session_acp_list_hidden".to_string(), None, None);
        hidden.working_dir = Some("/work/kept".to_string());
        hidden.add_message(Role::User, text("hidden"));
        hidden.save()?;
        hidden.record_archive(true, "test")?;
        let mut other = crate::session::Session::create_with_id("session_acp_list_other".to_string(), None, None);
        other.working_dir = Some("/work/other".to_string());
        other.add_message(Role::User, text("other"));
        other.save()?;
        catalog_rows()
    })();
    match previous {
        Some(previous) => crate::env::set_var("HORON_AGENT_HOME", previous),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
    let rows = saved.unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    let page = session_list_page(rows.clone(), Some("/work/kept"), None, AcpVersion::V1).unwrap();
    assert_eq!(page_ids(&page), ["session_acp_list_kept"]);
    assert_eq!(page["sessions"][0]["title"], "Kept work");
    let mut all = page_ids(&session_list_page(rows, None, None, AcpVersion::V1).unwrap());
    all.sort();
    assert_eq!(all, ["session_acp_list_kept", "session_acp_list_other"]);
}

#[test]
fn config_options_include_model_selector_and_effort_ladder() {
    let state = SessionUiState {
        provider_name: Some("openai".to_string()),
        model: Some("gpt-5.2".to_string()),
        available_models: vec!["gpt-5.2".to_string(), "gpt-5.2-codex".to_string()],
        reasoning_effort: Some("high".to_string()),
    };
    let options = session_config_options(&state, AcpMode::Default);
    assert_eq!(options.len(), 3);

    let model = &options[1];
    assert_eq!(model["id"], CONFIG_ID_MODEL);
    assert_eq!(model["category"], "model");
    assert_eq!(model["type"], "select");
    assert_eq!(model["currentValue"], "gpt-5.2");
    assert_eq!(model["options"].as_array().unwrap().len(), 2);

    let effort = &options[2];
    assert_eq!(effort["id"], CONFIG_ID_EFFORT);
    assert_eq!(effort["category"], "thought_level");
    assert_eq!(effort["currentValue"], "high");
    let effort_values: Vec<&str> = effort["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["value"].as_str().unwrap())
        .collect();
    assert!(effort_values.contains(&"medium"));
    assert!(
        !effort_values.iter().any(|value| value.starts_with("swarm")),
        "swarm sentinels are TUI-only and must not leak over ACP: {effort_values:?}"
    );
}

#[test]
fn config_options_current_model_prepended_when_not_listed() {
    let state = SessionUiState {
        provider_name: Some("anthropic".to_string()),
        model: Some("claude-opus-4-6".to_string()),
        available_models: vec!["claude-sonnet-4-5".to_string()],
        reasoning_effort: None,
    };
    let options = session_config_options(&state, AcpMode::Default);
    let model_values: Vec<&str> = options[1]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["value"].as_str().unwrap())
        .collect();
    assert_eq!(model_values[0], "claude-opus-4-6");
    assert!(model_values.contains(&"claude-sonnet-4-5"));
}

#[test]
fn session_result_has_no_non_spec_models_field() {
    let state = SessionUiState {
        provider_name: Some("deepseek".to_string()),
        model: Some("deepseek-v4-flash".to_string()),
        available_models: vec!["deepseek-v4-pro".to_string()],
        reasoning_effort: Some("high".to_string()),
    };
    let mut result = json!({"sessionId": "s1"});
    insert_session_configuration(&mut result, &state, AcpMode::Default);

    assert!(result["configOptions"].is_array());
    assert!(result.get("models").is_none(), "{result}");
}

#[test]
fn config_options_only_mode_without_model_state() {
    let options = session_config_options(&SessionUiState::default(), AcpMode::Default);
    assert_eq!(options.len(), 1, "{options:?}");
    assert_eq!(options[0]["id"], CONFIG_ID_MODE);
}

#[test]
fn mode_option_is_first() {
    let state = SessionUiState {
        provider_name: Some("openai".to_string()),
        model: Some("gpt-5.2".to_string()),
        available_models: vec!["gpt-5.2".to_string(), "gpt-5.2-codex".to_string()],
        reasoning_effort: Some("high".to_string()),
    };
    for mode in AcpMode::ALL {
        let options = session_config_options(&state, mode);
        let mode_option = &options[0];
        assert_eq!(mode_option["id"], "mode");
        assert_eq!(mode_option["category"], "mode");
        assert_eq!(mode_option["type"], "select");
        assert_eq!(mode_option["currentValue"], mode.id());
        assert_eq!(select_values(mode_option), ["default", "ask-commands", "auto"]);
        assert_eq!(
            options.iter().filter(|option| option["category"] == "mode").count(),
            1,
            "exactly one mode option: {options:?}"
        );
    }
    let modes = session_modes(AcpMode::AskCommands);
    assert_eq!(modes["currentModeId"], "ask-commands");
    let ids: Vec<&str> = modes["availableModes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mode| mode["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["default", "ask-commands", "auto"]);
    assert_eq!(
        AcpMode::ALL.map(AcpMode::approval_mode),
        [None, Some("write"), Some("off")],
        "modes map onto the approval gate as it behaves"
    );
}

#[test]
fn config_options_never_empty_current_value() {
    let states = [
        SessionUiState::default(),
        SessionUiState {
            model: Some(String::new()),
            available_models: vec![String::new(), "gpt-5".to_string()],
            ..SessionUiState::default()
        },
        SessionUiState {
            model: Some("  ".to_string()),
            ..SessionUiState::default()
        },
        SessionUiState {
            provider_name: Some("openai".to_string()),
            model: Some("gpt-5.2".to_string()),
            available_models: vec![String::new(), "gpt-5.2-codex".to_string()],
            reasoning_effort: Some(String::new()),
        },
    ];
    for state in &states {
        for mode in AcpMode::ALL {
            for option in session_config_options(state, mode) {
                let current = option["currentValue"].as_str().unwrap_or_default();
                assert!(!current.trim().is_empty(), "empty currentValue: {option}");
                let values = select_values(&option);
                assert!(!values.is_empty(), "an option with no choices: {option}");
                assert!(
                    values.contains(&current),
                    "currentValue not among the choices: {option}"
                );
                for entry in option["options"].as_array().unwrap() {
                    assert!(
                        !entry["value"].as_str().unwrap_or_default().trim().is_empty()
                            && !entry["name"].as_str().unwrap_or_default().trim().is_empty(),
                        "an empty choice: {option}"
                    );
                }
                assert_ne!(option["type"], "boolean", "no boolean options: {option}");
            }
        }
    }
}

#[test]
fn responses_have_no_models_field() {
    let state = SessionUiState {
        provider_name: Some("openai".to_string()),
        model: Some("gpt-5.2".to_string()),
        available_models: vec!["gpt-5.2-codex".to_string()],
        reasoning_effort: Some("high".to_string()),
    };
    for (mut result, root_keys) in [
        (
            json!({"sessionId": "s1"}),
            ["configOptions", "modes", "sessionId"].as_slice(),
        ),
        (json!({}), ["configOptions", "modes"].as_slice()),
    ] {
        insert_session_configuration(&mut result, &state, AcpMode::Auto);
        let mut keys: Vec<&str> = result.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, root_keys, "only spec root fields: {result}");
        assert_eq!(result["modes"]["currentModeId"], "auto");
        assert_eq!(result["configOptions"][0]["currentValue"], "auto");
    }
}

#[test]
fn set_config_option_value_checks() {
    let state = SessionUiState {
        provider_name: Some("openai".to_string()),
        model: Some("gpt-5.2".to_string()),
        available_models: vec!["gpt-5.2-codex".to_string()],
        reasoning_effort: None,
    };
    let options = session_config_options(&state, AcpMode::Default);
    assert!(check_config_value(&options, "mode", "auto").is_ok());
    assert!(check_config_value(&options, "model", "gpt-5.2-codex").is_ok());
    assert!(check_config_value(&options, "mode", "plan").is_err());
    assert!(check_config_value(&options, "model", "not-offered").is_err());
    assert!(check_config_value(&options, "temperature", "1").is_err());

    let params = json!({"sessionId": "s1", "configId": "mode", "value": "auto"});
    assert_eq!(
        config_value_from_params(&params).unwrap(),
        ("mode".to_string(), "auto".to_string())
    );
    for bad in [
        json!({"sessionId": "s1", "configId": "mode", "type": "boolean", "value": true}),
        json!({"sessionId": "s1", "configId": "mode", "type": "boolean", "value": "auto"}),
        json!({"sessionId": "s1", "configId": "mode", "value": true}),
        json!({"sessionId": "s1", "value": "auto"}),
    ] {
        assert!(config_value_from_params(&bad).is_err(), "{bad}");
    }
}

#[test]
fn permission_outcomes_map_to_decisions() {
    use crate::protocol::PermissionDecision::{Allow, Deny};
    let selected = |option: &str| Ok(json!({"outcome": {"outcome": "selected", "optionId": option}}));
    let cases = [
        (selected(OPTION_ALLOW_ONCE), Allow, None),
        (selected(OPTION_ALLOW_ALWAYS), Allow, Some(true)),
        (selected(OPTION_REJECT_ONCE), Deny, None),
        (selected(OPTION_REJECT_ALWAYS), Deny, Some(false)),
        (selected("allow"), Deny, None),
        (selected(""), Deny, None),
        (Ok(json!({"outcome": {"outcome": "cancelled"}})), Deny, None),
        (Ok(json!({"outcome": {"outcome": "selected"}})), Deny, None),
        (Ok(json!({"optionId": OPTION_ALLOW_ONCE})), Deny, None),
        (Ok(json!(null)), Deny, None),
        (Err(RpcError::cancelled()), Deny, None),
        (Err(RpcError::new(JSONRPC_METHOD_NOT_FOUND, "no")), Deny, None),
    ];
    for (result, decision, remember) in cases {
        assert_eq!(
            answer_from_outcome(&result),
            Answer { decision, remember },
            "{result:?}"
        );
    }
    let params = request_permission_params("s1", "call-1", "bash", "bash: run", &json!({"command": "ls"}));
    assert_eq!(params["toolCall"]["kind"], "execute");
    assert_eq!(params["toolCall"]["status"], "pending");
    assert_eq!(params["toolCall"]["rawInput"]["command"], "ls");
    let kinds: Vec<&str> = params["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["allow_once", "allow_always", "reject_once", "reject_always"]);
}

#[test]
fn context_limit_falls_back_to_default_for_unknown_models() {
    let state = SessionUiState {
        provider_name: Some("mystery".to_string()),
        model: Some("mystery-model-9000".to_string()),
        available_models: Vec::new(),
        reasoning_effort: None,
    };
    assert_eq!(state.context_limit(), crate::provider::DEFAULT_CONTEXT_LIMIT as u64);
}

#[test]
fn delete_lookup_treats_a_busy_writer_as_present() {
    use crate::session::event_store_api as store;
    let busy = store::StoreError::busy("session writer is already owned");
    assert_eq!(
        lookup_from_row(Err(busy)).unwrap(),
        CatalogLookup::Present { cwd: None }
    );
    let other = store::StoreError::busy("x");
    let other = store::StoreError {
        code: store::StoreErrorCode::UnknownSession,
        ..other
    };
    assert!(lookup_from_row(Err(other)).is_err());

    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    let observed = (|| -> anyhow::Result<Vec<CatalogLookup>> {
        use crate::message::{ContentBlock, Role};
        let mut session = crate::session::Session::create_with_id("session_acp_delete_busy".to_string(), None, None);
        session.working_dir = Some("/work/delete".to_string());
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "persisted".to_string(),
                cache_control: None,
            }],
        );
        session.save()?;
        let mut seen = vec![catalog_lookup("session_acp_delete_busy")?];
        session.record_archive(true, "test")?;
        seen.push(catalog_lookup("session_acp_delete_busy")?);
        seen.push(catalog_lookup("session_acp_delete_never_1")?);
        Ok(seen)
    })();
    match previous {
        Some(previous) => crate::env::set_var("HORON_AGENT_HOME", previous),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
    let seen = observed.unwrap();
    assert_eq!(
        seen[0],
        CatalogLookup::Present {
            cwd: Some("/work/delete".to_string())
        }
    );
    assert_eq!(seen[1], CatalogLookup::Archived);
    assert_eq!(seen[2], CatalogLookup::Missing);
}

pub(super) mod connection_core {
    use super::*;
    use crate::transport::Stream;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    const WAIT: Duration = Duration::from_secs(5);

    pub(in crate::cli::acp) async fn within<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(WAIT, future)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
    }

    pub(in crate::cli::acp) struct Client {
        pub(in crate::cli::acp) runtime: AcpRuntime,
        input: Option<DuplexStream>,
        output: Lines<BufReader<DuplexStream>>,
        serve: JoinHandle<Result<()>>,
    }

    fn start_client() -> Client {
        start_client_with(None)
    }

    /// A client whose daemon connections come from `connector` instead of the
    /// daemon socket, so a test can script the daemon end.
    pub(in crate::cli::acp) fn start_client_with(connector: Option<DaemonConnector>) -> Client {
        start_client_bounded(connector, AttachBounds::default())
    }

    /// [`start_client_with`] whose session catalog is `catalog`, so a v2 resume
    /// reads no host-local store.
    pub(in crate::cli::acp) fn start_client_with_catalog(
        connector: DaemonConnector,
        catalog: SessionCatalog,
    ) -> Client {
        start_client_configured(Some(connector), AttachBounds::default(), Some(catalog))
    }

    /// [`start_client_with`] with the attach deadlines set before the read loop
    /// is spawned, so they apply to the runtime that serves the requests.
    fn start_client_bounded(connector: Option<DaemonConnector>, attach_bounds: AttachBounds) -> Client {
        start_client_configured(connector, attach_bounds, None)
    }

    fn start_client_configured(
        connector: Option<DaemonConnector>,
        attach_bounds: AttachBounds,
        catalog: Option<SessionCatalog>,
    ) -> Client {
        let (output_writer, output_reader) = tokio::io::duplex(1 << 20);
        let (input_writer, input_reader) = tokio::io::duplex(1 << 20);
        let mut runtime = AcpRuntime::new(
            AcpProfile::Standard,
            ProviderChoice::HoronAgent,
            None,
            None,
            Box::new(output_writer),
        );
        runtime.daemon_connector = connector;
        runtime.attach_bounds = attach_bounds;
        runtime.session_catalog = catalog;
        let serve = tokio::spawn(runtime.clone().serve(input_reader));
        Client {
            runtime,
            input: Some(input_writer),
            output: BufReader::new(output_reader).lines(),
            serve,
        }
    }

    impl Client {
        pub(super) async fn send_raw(&mut self, text: &str) {
            let input = self.input.as_mut().expect("stdin open");
            input.write_all(text.as_bytes()).await.unwrap();
            input.flush().await.unwrap();
        }

        pub(in crate::cli::acp) async fn send(&mut self, value: Value) {
            self.send_raw(&format!("{value}\n")).await;
        }

        pub(in crate::cli::acp) async fn recv(&mut self) -> Value {
            let line = within("an ACP output line", self.output.next_line())
                .await
                .unwrap()
                .expect("stdout closed");
            serde_json::from_str(&line).unwrap_or_else(|err| panic!("stdout line is not JSON ({err}): {line}"))
        }

        /// Read lines until the response for "id" arrives; return the lines
        /// before it and the response itself.
        pub(in crate::cli::acp) async fn recv_response(&mut self, id: Value) -> (Vec<Value>, Value) {
            let mut before = Vec::new();
            loop {
                let line = self.recv().await;
                if line.get("method").is_none() && line.get("id") == Some(&id) {
                    return (before, line);
                }
                before.push(line);
            }
        }

        async fn initialize(&mut self) {
            self.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1}}))
                .await;
            let response = self.recv().await;
            assert_eq!(response["result"]["protocolVersion"], 1, "{response}");
        }

        /// `initialize` selecting protocol version 2. Its v2 `capabilities`
        /// carry the v1 file and terminal markers, which a v2 connection must
        /// never pass to the daemon (IS-3), and the terminal auth marker, which
        /// this build still answers with only the credentials method.
        pub(in crate::cli::acp) async fn initialize_v2(&mut self) {
            self.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                "protocolVersion":2,
                "info":{"name":"v2-client","version":"1.0.0"},
                "capabilities":{
                    "fs":{"readTextFile":true,"writeTextFile":true},
                    "terminal":true,
                    "auth":{"terminal":{}}}}}))
                .await;
            let response = self.recv().await;
            assert_eq!(response["result"]["protocolVersion"], 2, "{response}");
            assert_eq!(
                response["result"]["authMethods"].as_array().map(Vec::len),
                Some(1),
                "the v2 auth.terminal marker adds no method: {response}"
            );
        }

        /// The kill switch, set before `initialize` answers anything.
        pub(super) fn disable_v2(&self) {
            self.runtime.lock_connection().protocol_v2 = false;
        }

        /// A request answered inline by the read loop. Every line written
        /// for input sent before it appears before its answer.
        pub(in crate::cli::acp) async fn barrier(&mut self, tag: &str) -> Vec<Value> {
            let id = json!(format!("barrier-{tag}"));
            self.send(json!({"jsonrpc":"2.0","id":id,"method":"_barrier"})).await;
            let (before, response) = self.recv_response(id).await;
            assert_eq!(response["error"]["code"], JSONRPC_METHOD_NOT_FOUND);
            before
        }

        pub(in crate::cli::acp) async fn close_stdin(mut self) {
            self.input.take();
            within("serve to return on stdin EOF", &mut self.serve)
                .await
                .unwrap()
                .unwrap();
        }
    }

    pub(in crate::cli::acp) struct FakeDaemon {
        lines: Lines<BufReader<crate::transport::ReadHalf>>,
        writer: crate::transport::WriteHalf,
    }

    impl FakeDaemon {
        pub(in crate::cli::acp) async fn recv(&mut self) -> Value {
            let line = within("a daemon request", self.lines.next_line())
                .await
                .unwrap()
                .expect("adapter closed the daemon socket");
            serde_json::from_str(&line).unwrap()
        }

        pub(in crate::cli::acp) async fn recv_type(&mut self, kind: &str) -> Value {
            loop {
                let request = self.recv().await;
                if request["type"] == kind {
                    return request;
                }
            }
        }

        pub(in crate::cli::acp) async fn emit(&mut self, event: Value) {
            let mut line = event.to_string();
            line.push('\n');
            self.writer.write_all(line.as_bytes()).await.unwrap();
            self.writer.flush().await.unwrap();
        }
    }

    /// A daemon connector that hands the test the daemon end of each
    /// connection the runtime opens, in the order it opens them.
    pub(in crate::cli::acp) fn scripted_daemon() -> (DaemonConnector, mpsc::UnboundedReceiver<FakeDaemon>) {
        let (sender, daemons) = mpsc::unbounded_channel();
        let connector: DaemonConnector = Arc::new(move || {
            let sender = sender.clone();
            Box::pin(async move {
                let (adapter_side, daemon_side) = Stream::pair()?;
                let (reader, writer) = adapter_side.into_split();
                let (daemon_reader, daemon_writer) = daemon_side.into_split();
                let _ = sender.send(FakeDaemon {
                    lines: BufReader::new(daemon_reader).lines(),
                    writer: daemon_writer,
                });
                Ok((reader, writer))
            })
        });
        (connector, daemons)
    }

    pub(in crate::cli::acp) fn daemon_pair(session_id: &str) -> (DaemonSession, FakeDaemon) {
        let (adapter_side, daemon_side) = Stream::pair().unwrap();
        let (reader, writer) = adapter_side.into_split();
        let (daemon_reader, daemon_writer) = daemon_side.into_split();
        (
            DaemonSession::new(session_id.to_string(), reader, writer, 10),
            FakeDaemon {
                lines: BufReader::new(daemon_reader).lines(),
                writer: daemon_writer,
            },
        )
    }

    pub(super) async fn attach_fake_session(client: &Client, session_id: &str) -> (Arc<DaemonSession>, FakeDaemon) {
        let (session, daemon) = daemon_pair(session_id);
        let session = Arc::new(session);
        client
            .runtime
            .sessions
            .lock()
            .await
            .insert(session_id.to_string(), Arc::clone(&session));
        (session, daemon)
    }

    /// Offer models `m0`..`m{count-1}`, so set_config_option accepts them.
    pub(in crate::cli::acp) async fn offer_models(session: &DaemonSession, count: usize) {
        *session.ui_state.lock().await = SessionUiState {
            model: Some("m0".to_string()),
            available_models: (0..count).map(|index| format!("m{index}")).collect(),
            ..SessionUiState::default()
        };
    }

    fn set_model(id: Value, session_id: &str, model: &str) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"session/set_config_option",
            "params":{"sessionId":session_id,"configId":"model","value":model}})
    }

    pub(in crate::cli::acp) fn prompt(id: Value, session_id: &str) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"session/prompt",
            "params":{"sessionId":session_id,"prompt":[{"type":"text","text":"hi"}]}})
    }

    #[test]
    fn classifies_response_frames() {
        match JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","id":"horon-1","result":{"ok":true}}"#).unwrap() {
            JsonRpcMessage::Response { id, result } => {
                assert_eq!(id, json!("horon-1"));
                assert_eq!(result.unwrap(), json!({"ok": true}));
            }
            other => panic!("expected a response, got {other:?}"),
        }
        match JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"nope"}}"#).unwrap() {
            JsonRpcMessage::Response { id, result } => {
                assert_eq!(id, json!(7));
                assert_eq!(result.unwrap_err().code, JSONRPC_METHOD_NOT_FOUND);
            }
            other => panic!("expected an error response, got {other:?}"),
        }
        assert!(matches!(
            JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","id":1,"method":"session/new"}"#).unwrap(),
            JsonRpcMessage::Request { .. }
        ));
        assert!(matches!(
            JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","method":"session/cancel"}"#).unwrap(),
            JsonRpcMessage::Notification { .. }
        ));
        for invalid in [
            r#"{"jsonrpc":"2.0","id":3}"#,
            r#"{"jsonrpc":"2.0","id":3,"result":1,"error":{"code":1,"message":"x"}}"#,
            r#"{"jsonrpc":"2.0","id":{"a":1},"method":"x"}"#,
            r#"{"jsonrpc":"2.0","id":1.5,"method":"x"}"#,
            r#"{"jsonrpc":"2.0","method":7}"#,
            r#"[{"jsonrpc":"2.0","id":1,"method":"initialize"}]"#,
        ] {
            let (code, _) = JsonRpcMessage::parse(invalid).unwrap_err();
            assert_eq!(code, JSONRPC_INVALID_REQUEST, "{invalid}");
        }
        assert_eq!(
            JsonRpcMessage::error_reply_id(r#"{"id":9,"method":"initialize"}"#),
            json!(9)
        );
        assert_eq!(JsonRpcMessage::error_reply_id("not json"), Value::Null);
        assert_eq!(JsonRpcMessage::error_reply_id(r#"{"id":[1]}"#), Value::Null);
        assert_ne!(RequestKey::of(&json!(424242)), RequestKey::of(&json!("424242")));
    }

    #[test]
    fn v2_message_id_starts_do_not_collide_for_pids_that_shared_a_slice() {
        // The old start kept only the low 16 bits of the pid, so these two
        // adapters allocated the same ids.
        assert_ne!(
            v2_message_id_start_from(1, 0),
            v2_message_id_start_from(1 + (1 << 16), 0)
        );
        // A pid's salted start leaves 2^19 ids before the next pid's slice.
        assert!(
            v2_message_id_start_from(7, ID_SALT_MASK) + (1 << 19) <= v2_message_id_start_from(8, 0),
            "slices stay disjoint"
        );
        // Ids stay below 2^53, the largest integer a JSON number carries exactly.
        assert!(v2_message_id_start_from(u32::MAX, ID_SALT_MASK) + (1 << 19) < 1 << 53);
        // Ids stay in the v2 range, above every other client's counter.
        assert!(v2_message_id_start() >= V2_MESSAGE_ID_BASE);
        assert!(V2_MESSAGE_ID_BASE > u64::from(u32::MAX));
    }

    #[tokio::test]
    async fn malformed_lines_get_error_replies_with_the_original_id() {
        let mut client = start_client();
        client.send_raw("not json\n").await;
        let parse = client.recv().await;
        assert_eq!(parse["error"]["code"], JSONRPC_PARSE_ERROR);
        assert_eq!(parse["id"], Value::Null);
        client.send_raw("{\"id\":\"s-9\",\"method\":\"initialize\"}\n").await;
        let missing_version = client.recv().await;
        assert_eq!(missing_version["error"]["code"], JSONRPC_INVALID_REQUEST);
        assert_eq!(missing_version["id"], "s-9");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn request_before_initialize_is_invalid_request() {
        let mut client = start_client();
        client
            .send(json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}))
            .await;
        let response = client.recv().await;
        assert_eq!(response["id"], 1);
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_REQUEST);
        client.send(json!({"jsonrpc":"2.0","id":"1","method":"_ext"})).await;
        let response = client.recv().await;
        assert_eq!(response["id"], "1");
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_REQUEST);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn initialize_rejects_missing_protocol_version() {
        let mut client = start_client();
        client
            .send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}))
            .await;
        assert_eq!(client.recv().await["error"]["code"], JSONRPC_INVALID_PARAMS);
        client.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).await;
        assert_eq!(client.recv().await["error"]["code"], JSONRPC_INVALID_PARAMS);
        client
            .send(json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp"}}))
            .await;
        assert_eq!(
            client.recv().await["error"]["code"],
            JSONRPC_INVALID_REQUEST,
            "a rejected initialize must not open the gate"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn initialize_rejects_string_protocol_version() {
        let mut client = start_client();
        for version in [json!("1"), json!(1.0), json!(-1), json!(65536), json!(null)] {
            client
                .send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":version}}))
                .await;
            let response = client.recv().await;
            assert_eq!(
                response["error"]["code"], JSONRPC_INVALID_PARAMS,
                "protocolVersion {version}"
            );
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn initialize_answers_one_for_65535_when_v2_off() {
        let mut client = start_client();
        client.disable_v2();
        client
            .send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":65535}}))
            .await;
        let response = client.recv().await;
        assert_eq!(response["result"]["protocolVersion"], 1);
        client
            .send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":0}}))
            .await;
        assert_eq!(client.recv().await["result"]["protocolVersion"], 1);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn initialize_selects_v2_for_65535_when_v2_on() {
        let mut client = start_client();
        client
            .send(
                json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":65535,
                "info":{"name":"v2-client","version":"1.0.0"}}}),
            )
            .await;
        let response = client.recv().await;
        assert_eq!(response["result"]["protocolVersion"], 2, "{response}");
        assert_eq!(response["result"]["info"]["name"], "horon-agent", "{response}");

        // 0 names a version no build speaks; the connection stays uninitialized.
        client
            .send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":0}}))
            .await;
        let response = client.recv().await;
        assert_eq!(response["error"]["code"], -32022, "{response}");
        assert_eq!(
            response["error"]["data"],
            json!({"requested": 0, "supported": [1, 2]}),
            "{response}"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn negotiate_selects_per_request() {
        let mut v1 = start_client();
        v1.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":1,"clientCapabilities":{"terminal":true}}}))
            .await;
        let first = v1.recv().await;
        assert_eq!(first["result"]["protocolVersion"], 1, "{first}");
        assert_eq!(first["result"]["agentCapabilities"]["loadSession"], true, "{first}");
        assert!(
            first["result"].get("info").is_none(),
            "the v1 result has no info: {first}"
        );
        // The v1 surface keeps the methods v2 removes.
        v1.send(request(
            json!(1),
            "session/set_mode",
            json!({"sessionId":"gone","modeId":"auto"}),
        ))
        .await;
        let kept = v1.recv().await;
        assert_ne!(
            kept["error"]["code"], JSONRPC_METHOD_NOT_FOUND,
            "v1 keeps session/set_mode: {kept}"
        );
        v1.close_stdin().await;

        let mut v2 = start_client();
        v2.initialize_v2().await;
        v2.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":99,
            "info":{"name":"newer","version":"9.9.9"},
            "unknown":true}}))
            .await;
        let newer = v2.recv().await;
        assert_eq!(newer["result"]["protocolVersion"], 2, "{newer}");
        assert_eq!(newer["result"]["info"]["title"], "HoronAgent", "{newer}");
        assert!(
            newer["result"]["capabilities"]["session"]["prompt"]["image"].is_object(),
            "{newer}"
        );
        assert!(
            newer["result"]["capabilities"].get("loadSession").is_none(),
            "no boolean markers on v2: {newer}"
        );
        v2.close_stdin().await;
    }

    #[tokio::test]
    async fn kill_switch_env_and_config_restore_v1_answers() {
        // Both switches reach the adapter through the config layer.
        {
            let _guard = crate::storage::lock_test_env();
            let home = tempfile::tempdir().expect("tempdir");
            let previous_home = std::env::var_os("HORON_AGENT_HOME");
            let previous_env = std::env::var_os("HORON_AGENT_ACP_PROTOCOL_V2");
            crate::env::set_var("HORON_AGENT_HOME", home.path());
            crate::env::remove_var("HORON_AGENT_ACP_PROTOCOL_V2");
            std::fs::write(home.path().join("config.toml"), "[acp]\nprotocol_v2 = false\n").expect("write config");
            crate::config::invalidate_config_cache();
            assert!(!crate::config::config().acp.protocol_v2, "[acp] protocol_v2 = false");

            crate::env::set_var("HORON_AGENT_ACP_PROTOCOL_V2", "true");
            crate::config::invalidate_config_cache();
            assert!(
                crate::config::config().acp.protocol_v2,
                "the env override wins over the file"
            );

            crate::env::set_var("HORON_AGENT_ACP_PROTOCOL_V2", "0");
            crate::config::invalidate_config_cache();
            assert!(!crate::config::config().acp.protocol_v2);

            match previous_env {
                Some(value) => crate::env::set_var("HORON_AGENT_ACP_PROTOCOL_V2", value),
                None => crate::env::remove_var("HORON_AGENT_ACP_PROTOCOL_V2"),
            }
            match previous_home {
                Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
                None => crate::env::remove_var("HORON_AGENT_HOME"),
            }
            crate::config::invalidate_config_cache();
        }

        // With the switch off every version is answered exactly as v1 answers it.
        let mut client = start_client();
        client.disable_v2();
        for (id, version) in [(0, 65535), (1, 0), (2, 2)] {
            client
                .send(json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{
                    "protocolVersion":version,
                    "info":{"name":"v2-client","version":"1.0.0"}}}))
                .await;
            let response = client.recv().await;
            assert_eq!(response["result"]["protocolVersion"], 1, "{response}");
            assert_eq!(
                response["result"]["agentCapabilities"]["loadSession"], true,
                "{response}"
            );
            assert!(response["result"].get("info").is_none(), "{response}");
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn v2_repeated_initialize_rules() {
        let mut client = start_client();
        client.initialize_v2().await;

        // Before any other request the last initialize wins.
        client
            .send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":3,
                "info":{"name":"second","version":"2.0.0"},
                "clientCapabilities":{"terminal":true}}}))
            .await;
        let again = client.recv().await;
        assert_eq!(again["result"]["protocolVersion"], 2, "{again}");

        // A v2 selection without info is invalid params and keeps the surface.
        client
            .send(json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":2}}))
            .await;
        let no_info = client.recv().await;
        assert_eq!(no_info["error"]["code"], JSONRPC_INVALID_PARAMS, "{no_info}");
        client
            .send(request(json!(3), "session/set_mode", json!({"sessionId":"gone"})))
            .await;
        let still_v2 = client.recv().await;
        assert_eq!(
            still_v2["error"]["code"], JSONRPC_METHOD_NOT_FOUND,
            "a failing repeat keeps v2: {still_v2}"
        );

        // After another request a repeat is refused and the surface stays.
        client
            .send(json!({"jsonrpc":"2.0","id":4,"method":"initialize","params":{
                "protocolVersion":2,
                "info":{"name":"late","version":"2.0.0"}}}))
            .await;
        let late = client.recv().await;
        assert_eq!(late["error"]["code"], JSONRPC_INVALID_REQUEST, "{late}");
        client.close_stdin().await;

        // A repeat that selects 1 switches this connection to the v1 surface.
        let mut client = start_client();
        client.initialize_v2().await;
        client
            .send(json!({"jsonrpc":"2.0","id":5,"method":"initialize","params":{"protocolVersion":1}}))
            .await;
        let switched = client.recv().await;
        assert_eq!(switched["result"]["protocolVersion"], 1, "{switched}");
        assert_eq!(
            switched["result"]["agentCapabilities"]["loadSession"], true,
            "{switched}"
        );
        client
            .send(request(
                json!(6),
                "session/load",
                json!({"sessionId":"missing","cwd":"/tmp","mcpServers":[]}),
            ))
            .await;
        let loaded = client.recv().await;
        assert_ne!(
            loaded["error"]["code"], JSONRPC_METHOD_NOT_FOUND,
            "the connection is back on v1: {loaded}"
        );
        client.close_stdin().await;

        // A v1 connection that already served a request cannot switch to v2:
        // its subscriptions were built for v1 (IS-1, IS-3).
        let mut client = start_client();
        client.initialize().await;
        client
            .send(request(json!(7), "session/set_mode", json!({"sessionId":"gone"})))
            .await;
        client.recv_response(json!(7)).await;
        client
            .send(
                json!({"jsonrpc":"2.0","id":8,"method":"initialize","params":{"protocolVersion":65535,
                "info":{"name":"late-v2","version":"2.0.0"}}}),
            )
            .await;
        let late_switch = client.recv().await;
        assert_eq!(late_switch["error"]["code"], JSONRPC_INVALID_REQUEST, "{late_switch}");
        // Re-initializing as v1 keeps the byte-identical v1 answer.
        client
            .send(json!({"jsonrpc":"2.0","id":9,"method":"initialize","params":{"protocolVersion":1}}))
            .await;
        let again = client.recv().await;
        assert_eq!(again["result"]["protocolVersion"], 1, "{again}");
        assert_eq!(again["result"]["agentCapabilities"]["loadSession"], true, "{again}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn v2_removed_methods_are_32601() {
        let mut client = start_client();
        client.initialize_v2().await;
        for (index, method) in [
            "session/load",
            "session/set_mode",
            "authenticate",
            "logout",
            "session/set_model",
            "session/set_reasoning_effort",
            "fs/read_text_file",
            "_horon_agent/server_event",
        ]
        .into_iter()
        .enumerate()
        {
            let id = json!(index + 10);
            client
                .send(request(id.clone(), method, json!({"sessionId":"s1","cwd":"/tmp"})))
                .await;
            let (_, response) = client.recv_response(id).await;
            assert_eq!(
                response["error"]["code"], JSONRPC_METHOD_NOT_FOUND,
                "{method}: {response}"
            );
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn v2_null_id_is_invalid_request() {
        let mut client = start_client();
        client.initialize_v2().await;
        client
            .send(json!({"jsonrpc":"2.0","id":null,"method":"session/close","params":{"sessionId":"gone"}}))
            .await;
        let response = client.recv().await;
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_REQUEST, "{response}");
        assert_eq!(response["id"], Value::Null, "{response}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn batch_rules_v2() {
        let mut client = start_client();
        client.initialize_v2().await;

        // Mixed requests and a notification answer with one array line.
        client
            .send_raw(&format!(
                "{}\n",
                json!([
                    {"jsonrpc":"2.0","id":"a","method":"session/close","params":{"sessionId":"gone"}},
                    {"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"gone"}},
                    {"jsonrpc":"2.0","id":"b","method":"session/set_mode","params":{"sessionId":"gone"}},
                ])
            ))
            .await;
        let answers = client.recv().await;
        let answers = answers.as_array().expect("one response array");
        let codes: HashMap<&str, i64> = answers
            .iter()
            .map(|answer| {
                (
                    answer["id"].as_str().expect("a batch id"),
                    answer["error"]["code"].as_i64().expect("an error code"),
                )
            })
            .collect();
        assert_eq!(answers.len(), 2, "{answers:?}");
        assert_eq!(codes.get("a"), Some(&ACP_RESOURCE_NOT_FOUND), "{answers:?}");
        assert_eq!(codes.get("b"), Some(&JSONRPC_METHOD_NOT_FOUND), "{answers:?}");

        // Every entry is answered inside the one array, an extension request and
        // an initialize entry included (the spec asks senders not to batch
        // initialize, so such an entry is a repeat by construction).
        client
            .send_raw(&format!(
                "{}\n",
                json!([
                    {"jsonrpc":"2.0","id":"ext","method":"_horon_agent/server_event","params":{}},
                    {"jsonrpc":"2.0","id":"init","method":"initialize",
                        "params":{"protocolVersion":2,"info":{"name":"late","version":"2.0.0"}}},
                    {"jsonrpc":"2.0","id":"close","method":"session/close","params":{"sessionId":"gone"}},
                ])
            ))
            .await;
        let all = client.recv().await;
        let all = all.as_array().expect("nothing is written outside the batch array");
        let codes: HashMap<&str, i64> = all
            .iter()
            .map(|answer| {
                (
                    answer["id"].as_str().expect("a batch id"),
                    answer["error"]["code"].as_i64().expect("an error code"),
                )
            })
            .collect();
        assert_eq!(all.len(), 3, "{all:?}");
        assert_eq!(codes.get("ext"), Some(&JSONRPC_METHOD_NOT_FOUND), "{all:?}");
        assert_eq!(codes.get("init"), Some(&JSONRPC_INVALID_REQUEST), "{all:?}");
        assert_eq!(codes.get("close"), Some(&ACP_RESOURCE_NOT_FOUND), "{all:?}");
        assert!(
            client.barrier("after-batch-entries").await.is_empty(),
            "a batch answer never leaves the array"
        );

        // An empty batch is one invalid request with a null id.
        client.send_raw("[]\n").await;
        let empty = client.recv().await;
        assert_eq!(empty["error"]["code"], JSONRPC_INVALID_REQUEST, "{empty}");
        assert_eq!(empty["id"], Value::Null, "{empty}");

        // Invalid entries answer per entry and valid ones still run.
        client
            .send_raw(&format!(
                "{}\n",
                json!([
                    {"jsonrpc":"2.0","id":null,"method":"session/close","params":{}},
                    7,
                    {"jsonrpc":"2.0","id":"ok","method":"session/close","params":{"sessionId":"gone"}},
                ])
            ))
            .await;
        let mixed = client.recv().await;
        let mixed = mixed.as_array().expect("one response array");
        assert_eq!(mixed.len(), 3, "{mixed:?}");
        assert_eq!(
            mixed
                .iter()
                .filter(|answer| answer["error"]["code"] == JSONRPC_INVALID_REQUEST)
                .count(),
            2,
            "{mixed:?}"
        );
        assert!(
            mixed
                .iter()
                .any(|answer| answer["id"] == json!("ok") && answer["error"]["code"] == ACP_RESOURCE_NOT_FOUND),
            "{mixed:?}"
        );

        // A notifications-only batch writes nothing at all.
        client
            .send_raw(&format!(
                "{}\n",
                json!([{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"gone"}}])
            ))
            .await;
        assert!(
            client.barrier("notifications-only-batch").await.is_empty(),
            "a batch without requests writes nothing"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn batch_does_not_block_read_loop() {
        let mut client = start_client();
        client.initialize_v2().await;
        let (session, mut daemon) = daemon_pair("s1");
        client.runtime.prepare_v2_session(&session);
        client.runtime.install_session_bridge(&session);
        offer_models(&session, 1).await;
        *session.cwd.lock().await = Some(PathBuf::from("/work"));
        let session = Arc::new(session);
        client
            .runtime
            .sessions
            .lock()
            .await
            .insert("s1".to_string(), Arc::clone(&session));
        client.runtime.start_v2_stream(&session);

        client.send_raw(&format!("{}\n", json!([prompt(json!(7), "s1")]))).await;
        let message = daemon.recv_type("message").await;
        daemon.emit(permission_requested("p1", "bash")).await;

        // The read loop is free while the batch waits: the permission answer is
        // read, and the prompt's insertion lands inside the batch's response.
        let request = client.recv().await;
        assert_eq!(request["method"], "session/request_permission", "{request}");
        assert_eq!(request["params"]["subject"]["toolCallId"], "call-p1", "{request}");
        let call = request["id"].clone();
        client.send(selected(&call, OPTION_ALLOW_ONCE)).await;
        expect_decision(&mut daemon, "p1", "allow").await;
        daemon
            .emit(json!({"type":"user_message_inserted","id":message["id"],"message_id":"m7"}))
            .await;

        let answers = loop {
            let line = client.recv().await;
            if let Some(answers) = line.as_array() {
                break answers.clone();
            }
        };
        assert_eq!(answers.len(), 1, "{answers:?}");
        assert_eq!(answers[0]["id"], 7, "{answers:?}");
        assert_eq!(answers[0]["result"], json!({"messageId":"m7"}), "{answers:?}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn batch_duplicate_id_is_invalid_and_still_completes() {
        let mut client = start_client();
        client.initialize_v2().await;

        // Two entries of one batch share an id. The second cannot overwrite
        // the first's in-flight entry: it is answered -32600, so both answers
        // reach the one array and the batch completes.
        client
            .send_raw(&format!(
                "{}\n",
                json!([
                    {"jsonrpc":"2.0","id":"dup","method":"session/close","params":{"sessionId":"gone"}},
                    {"jsonrpc":"2.0","id":"dup","method":"session/close","params":{"sessionId":"gone"}},
                ])
            ))
            .await;
        let answers = client.recv().await;
        let answers = answers.as_array().expect("one response array");
        assert_eq!(answers.len(), 2, "{answers:?}");
        assert!(answers.iter().all(|answer| answer["id"] == json!("dup")), "{answers:?}");
        assert_eq!(
            answers
                .iter()
                .filter(|answer| answer["error"]["code"] == JSONRPC_INVALID_REQUEST)
                .count(),
            1,
            "one entry is a duplicate: {answers:?}"
        );
        assert_eq!(
            answers
                .iter()
                .filter(|answer| answer["error"]["code"] == ACP_RESOURCE_NOT_FOUND)
                .count(),
            1,
            "the other entry still runs: {answers:?}"
        );
        assert!(
            client.barrier("after-duplicate-batch").await.is_empty(),
            "the batch answer never leaves its array"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn batch_reusing_an_in_flight_id_is_rejected() {
        let mut client = start_client();
        client.initialize_v2().await;
        let (session, mut daemon) = daemon_pair("s1");
        client.runtime.prepare_v2_session(&session);
        client.runtime.install_session_bridge(&session);
        offer_models(&session, 1).await;
        let session = Arc::new(session);
        client
            .runtime
            .sessions
            .lock()
            .await
            .insert("s1".to_string(), Arc::clone(&session));
        client.runtime.start_v2_stream(&session);

        client.send(prompt(json!(7), "s1")).await;
        let message = daemon.recv_type("message").await;
        // The prompt's id is still in flight when a batch entry reuses it.
        client
            .send_raw(&format!(
                "{}\n",
                json!([{"jsonrpc":"2.0","id":7,"method":"session/close","params":{"sessionId":"gone"}}])
            ))
            .await;
        let answers = loop {
            let line = client.recv().await;
            if let Some(answers) = line.as_array() {
                break answers.clone();
            }
        };
        assert_eq!(answers.len(), 1, "{answers:?}");
        assert_eq!(answers[0]["id"], 7, "{answers:?}");
        assert_eq!(
            answers[0]["error"]["code"], JSONRPC_INVALID_REQUEST,
            "the prompt keeps its in-flight entry: {answers:?}"
        );

        // The prompt itself is still answered by its insertion.
        daemon
            .emit(json!({"type":"user_message_inserted","id":message["id"],"message_id":"m7"}))
            .await;
        let response = loop {
            let line = client.recv().await;
            if line.get("method").is_none() && line["id"] == json!(7) {
                break line;
            }
        };
        assert_eq!(response["result"], json!({"messageId":"m7"}), "{response}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn arrays_before_initialize_and_on_v1_rejected_as_before() {
        let mut client = start_client();
        client
            .send_raw(&format!(
                "{}\n",
                json!([{"jsonrpc":"2.0","id":1,"method":"session/close","params":{"sessionId":"gone"}}])
            ))
            .await;
        let early = client.recv().await;
        assert_eq!(early["error"]["code"], JSONRPC_INVALID_REQUEST, "{early}");
        assert_eq!(early["id"], Value::Null, "{early}");

        client.initialize().await;
        client
            .send_raw(&format!(
                "{}\n",
                json!([{"jsonrpc":"2.0","id":2,"method":"session/close","params":{"sessionId":"gone"}}])
            ))
            .await;
        let v1 = client.recv().await;
        assert_eq!(v1["error"]["code"], JSONRPC_INVALID_REQUEST, "{v1}");
        assert_eq!(v1["id"], Value::Null, "{v1}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn v2_subscribe_disables_client_fs_and_terminal() {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        // The v1 surface passes the client's capabilities through untouched.
        client
            .send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                "protocolVersion":1,
                "clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true}}}))
            .await;
        client.recv().await;
        client
            .send(request(json!(1), "session/new", json!({"cwd":"/work","mcpServers":[]})))
            .await;
        let mut daemon = within("the daemon connection", daemons.recv()).await.unwrap();
        let subscribe = daemon.recv_type("subscribe").await;
        assert_eq!(
            subscribe["acp"]["client_capabilities"]["fs_read_text_file"], true,
            "{subscribe}"
        );
        assert_eq!(subscribe["acp"]["client_capabilities"]["terminal"], true, "{subscribe}");
        assert!(subscribe["acp"].get("protocol_version").is_none(), "{subscribe}");
        client.close_stdin().await;

        // A v2 connection advertises no client-side capability and says so.
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client.initialize_v2().await;
        client
            .send(request(json!(1), "session/new", json!({"cwd":"/work","mcpServers":[]})))
            .await;
        let mut daemon = within("the daemon connection", daemons.recv()).await.unwrap();
        let subscribe = daemon.recv_type("subscribe").await;
        assert_eq!(subscribe["acp"]["protocol_version"], 2, "{subscribe}");
        assert_eq!(
            subscribe["acp"]["client_capabilities"]["fs_read_text_file"], false,
            "{subscribe}"
        );
        assert_eq!(
            subscribe["acp"]["client_capabilities"]["fs_write_text_file"], false,
            "{subscribe}"
        );
        assert_eq!(
            subscribe["acp"]["client_capabilities"]["terminal"], false,
            "{subscribe}"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn logout_gates_session_methods_until_login() {
        let mut client = start_client();
        client.initialize_v2().await;
        client.runtime.ready.store(true, Ordering::SeqCst);

        client.send(request(json!(1), "auth/logout", json!({}))).await;
        let (_, logout) = client.recv_response(json!(1)).await;
        assert_eq!(logout["result"], json!({}), "{logout}");

        for (id, method) in [(2, "session/new"), (3, "session/resume"), (4, "session/prompt")] {
            client
                .send(request(
                    json!(id),
                    method,
                    json!({"sessionId":"s1","cwd":"/work","mcpServers":[]}),
                ))
                .await;
            let (_, response) = client.recv_response(json!(id)).await;
            assert_eq!(response["error"]["code"], ACP_AUTH_REQUIRED, "{method}: {response}");
        }

        // A second logout stays logged out; the credentials themselves are
        // never touched (no daemon call happens on this path).
        client.send(request(json!(5), "auth/logout", json!({}))).await;
        let (_, again) = client.recv_response(json!(5)).await;
        assert_eq!(again["result"], json!({}), "{again}");

        client
            .send(request(
                json!(6),
                "auth/login",
                json!({"methodId":"horon-agent-credentials"}),
            ))
            .await;
        let (_, login) = client.recv_response(json!(6)).await;
        assert_eq!(login["result"], json!({}), "{login}");
        client.send(request(json!(7), "auth/logout", json!({}))).await;
        client.recv_response(json!(7)).await;
        client
            .send(request(
                json!(8),
                "auth/login",
                json!({"methodId":"horon-agent-credentials"}),
            ))
            .await;
        let (_, relogin) = client.recv_response(json!(8)).await;
        assert_eq!(relogin["result"], json!({}), "{relogin}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn repeated_initialize_refreshes_capabilities() {
        let mut client = start_client();
        client
            .send(
                json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,
                "clientCapabilities":{"fs":{"readTextFile":true}},"clientInfo":{"name":"first"}}}),
            )
            .await;
        let first = client.recv().await;
        client
            .send(
                json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,
                "clientCapabilities":{"terminal":true},"clientInfo":{"name":"second"}}}),
            )
            .await;
        let second = client.recv().await;
        assert_eq!(first, second);
        {
            let connection = client.runtime.lock_connection();
            assert!(connection.initialized);
            assert_eq!(connection.client_capabilities, json!({"terminal": true}));
            assert_eq!(connection.client_info, Some(json!({"name": "second"})));
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn notification_never_answered() {
        let mut client = start_client();
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        client.send(json!({"jsonrpc":"2.0","method":"unknown/notice"})).await;
        client.initialize().await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"missing"}}))
            .await;
        client.send(json!({"jsonrpc":"2.0","method":"session/cancel"})).await;
        client
            .send(json!({"jsonrpc":"2.0","method":"_vendor/notice","params":{}}))
            .await;
        client
            .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":99}}))
            .await;
        client.send(json!({"jsonrpc":"2.0","method":"$/progress"})).await;
        client.send(json!({"jsonrpc":"2.0","id":"horon-999","result":{}})).await;
        let stray = client.barrier("notifications").await;
        assert!(
            stray.is_empty(),
            "notifications and stray responses were answered: {stray:?}"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn underscore_request_is_method_not_found() {
        let mut client = start_client();
        client.initialize().await;
        client
            .send(json!({"jsonrpc":"2.0","id":5,"method":"_zed.dev/workspace/buffers","params":{}}))
            .await;
        let response = client.recv().await;
        assert_eq!(response["id"], 5);
        assert_eq!(response["error"]["code"], JSONRPC_METHOD_NOT_FOUND);
        client
            .send(json!({"jsonrpc":"2.0","id":6,"method":"unknown/method"}))
            .await;
        let response = client.recv().await;
        assert_eq!(response["id"], 6);
        assert_eq!(response["error"]["code"], JSONRPC_METHOD_NOT_FOUND);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn outbound_request_resolves_on_response() {
        let mut client = start_client();
        client.initialize().await;
        let peer = Arc::clone(&client.runtime.peer);
        let call = tokio::spawn(async move {
            peer.call_client("fs/read_text_file", json!({"path": "/tmp/a"}), WAIT)
                .await
        });
        let request = client.recv().await;
        assert_eq!(request["method"], "fs/read_text_file");
        let id = request["id"].clone();
        assert!(id.as_str().is_some_and(|id| id.starts_with("horon-")), "{request}");
        client
            .send(json!({"jsonrpc":"2.0","id":"horon-unknown","result":{}}))
            .await;
        client
            .send(json!({"jsonrpc":"2.0","id":id,"result":{"content":"text"}}))
            .await;
        let result = within("call_client to resolve", call).await.unwrap();
        assert_eq!(result.unwrap(), json!({"content": "text"}));
        client
            .send(json!({"jsonrpc":"2.0","id":id,"result":{"content":"again"}}))
            .await;
        assert!(client.barrier("after-resolve").await.is_empty());
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn outbound_request_cancel_sends_cancel_request() {
        let mut client = start_client();
        client.initialize().await;
        let peer = Arc::clone(&client.runtime.peer);
        let call = tokio::spawn(async move {
            peer.call_client("terminal/wait_for_exit", json!({}), Duration::from_secs(3600))
                .await
        });
        let request = client.recv().await;
        let id = request["id"].clone();
        client.runtime.peer.cancel_outbound(&id).await;
        let cancel = client.recv().await;
        assert_eq!(cancel["method"], "$/cancel_request");
        assert_eq!(cancel["params"]["requestId"], id);
        assert!(
            cancel.get("id").is_none(),
            "$/cancel_request is a notification: {cancel}"
        );
        let result = within("the cancelled call to return", call).await.unwrap();
        assert_eq!(result.unwrap_err().code, JSONRPC_REQUEST_CANCELLED);
        client.runtime.peer.cancel_outbound(&id).await;
        client.send(json!({"jsonrpc":"2.0","id":id,"result":{}})).await;
        assert!(
            client.barrier("after-cancel").await.is_empty(),
            "a second cancel or a late response produced output"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn cancel_request_for_inflight_non_prompt_answers_32800() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_fake_session(&client, "s1").await;
        offer_models(&session, 3).await;
        client.send(set_model(json!(10), "s1", "m1")).await;
        let first = daemon.recv_type("set_model").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":10}}))
            .await;
        let cancelled = client.recv().await;
        assert_eq!(cancelled["id"], 10);
        assert_eq!(cancelled["error"]["code"], JSONRPC_REQUEST_CANCELLED);
        client
            .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":10}}))
            .await;
        client
            .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":"10"}}))
            .await;
        daemon
            .emit(json!({"type":"model_changed","id":first["id"],"model":"m1"}))
            .await;
        client.send(set_model(json!(11), "s1", "m2")).await;
        let second = daemon.recv_type("set_model").await;
        assert_eq!(second["model"], "m2");
        daemon
            .emit(json!({"type":"model_changed","id":second["id"],"model":"m2"}))
            .await;
        let (before, response) = client.recv_response(json!(11)).await;
        assert!(response.get("result").is_some(), "{response}");
        assert!(
            before.iter().all(|line| line.get("id").is_none()),
            "request 10 was answered twice: {before:?}"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn initialize_is_answered_after_its_id_was_cancelled() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_fake_session(&client, "s1").await;
        offer_models(&session, 3).await;
        client.send(set_model(json!(10), "s1", "m1")).await;
        let model = daemon.recv_type("set_model").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":10}}))
            .await;
        let cancelled = client.recv().await;
        assert_eq!(cancelled["error"]["code"], JSONRPC_REQUEST_CANCELLED, "{cancelled}");

        // `initialize` answers on the connection and owns no in-flight entry, so
        // reusing a cancelled id answers it instead of swallowing it.
        client
            .send(json!({"jsonrpc":"2.0","id":10,"method":"initialize","params":{"protocolVersion":1}}))
            .await;
        let again = client.recv().await;
        assert_eq!(again["id"], 10, "{again}");
        assert_eq!(again["result"]["protocolVersion"], 1, "{again}");

        // The cancelled call still gets no late result of its own.
        daemon
            .emit(json!({"type":"model_changed","id":model["id"],"model":"m1"}))
            .await;
        assert!(
            client.barrier("after-late-model-result").await.is_empty(),
            "the cancelled set_model never answers"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn waiter_registered_before_send() {
        let (session, mut daemon) = daemon_pair("s1");
        let session = Arc::new(session);

        let applying = tokio::spawn({
            let session = Arc::clone(&session);
            async move { apply_model(&session, "m1".to_string()).await }
        });
        let request = daemon.recv_type("set_model").await;
        let id = request["id"].as_u64().unwrap();
        let record = session.sent(id).expect("set_model went through DaemonSession::send");
        assert!(
            record.waiter_registered,
            "the reply waiter must be registered when the request is written: {record:?}"
        );
        daemon.emit(json!({"type":"model_changed","id":id,"model":"m1"})).await;
        within("apply_model to finish", applying).await.unwrap().unwrap();
        assert_eq!(session.ui_state.lock().await.model.as_deref(), Some("m1"));
        assert!(!session.has_waiter(id), "a finished waiter must deregister");

        let pending = [
            (
                "set_reasoning_effort",
                tokio::spawn({
                    let session = Arc::clone(&session);
                    async move { drop(apply_effort(&session, "high".to_string()).await) }
                }),
            ),
            (
                "get_history",
                tokio::spawn({
                    let session = Arc::clone(&session);
                    async move { drop(request_history(&session).await) }
                }),
            ),
            (
                "get_model_catalog",
                tokio::spawn({
                    let session = Arc::clone(&session);
                    async move { drop(request_model_catalog(&session).await) }
                }),
            ),
        ];
        for (kind, task) in pending {
            let request = daemon.recv_type(kind).await;
            let id = request["id"].as_u64().unwrap();
            let record = session.sent(id).expect("request went through DaemonSession::send");
            assert!(
                record.waiter_registered,
                "{kind}: waiter must be registered at write time: {record:?}"
            );
            task.abort();
        }

        let message = Request::Message {
            id: 99,
            content: "hi".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        };
        let _events = session.foreground_request(None, &message).await.unwrap();
        daemon.recv_type("message").await;
        let record = session.sent(99).expect("message went through DaemonSession::send");
        assert!(
            record.foreground_registered,
            "the prompt stream must be registered before the message is written: {record:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancel_request_racing_spawn_hits_inflight() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let daemon_task = tokio::spawn(async move {
            let mut message_id = None;
            loop {
                let request = daemon.recv().await;
                match request["type"].as_str() {
                    Some("message") => message_id = request["id"].as_u64(),
                    Some("cancel") => {
                        if let Some(id) = message_id {
                            daemon.emit(json!({"type":"interrupted"})).await;
                            daemon.emit(json!({"type":"done","id":id})).await;
                            return daemon;
                        }
                    }
                    _ => {}
                }
            }
        });
        let cancel = json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":20}});
        client
            .send_raw(&format!("{}\n{cancel}\n{cancel}\n", prompt(json!(20), "s1")))
            .await;
        let (before, response) = client.recv_response(json!(20)).await;
        assert!(before.iter().all(|line| line.get("id").is_none()), "{before:?}");
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
        let after = client.barrier("after-prompt").await;
        assert!(
            after.iter().all(|line| line.get("id").is_none()),
            "the prompt was answered twice: {after:?}"
        );
        daemon_task.abort();
        client.close_stdin().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn same_session_requests_keep_order() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_fake_session(&client, "s1").await;
        offer_models(&session, 32).await;
        let mut batch = String::new();
        for index in 0..32 {
            batch.push_str(&format!(
                "{}\n",
                set_model(json!(30 + index), "s1", &format!("m{index}"))
            ));
        }
        client.send_raw(&batch).await;
        for index in 0..32 {
            let request = daemon.recv_type("set_model").await;
            assert_eq!(
                request["model"],
                format!("m{index}"),
                "daemon saw requests out of order"
            );
            daemon
                .emit(json!({"type":"model_changed","id":request["id"],"model":request["model"]}))
                .await;
        }
        for index in 0..32 {
            let (_, response) = client.recv_response(json!(30 + index)).await;
            assert!(response.get("result").is_some(), "{response}");
        }
        client.close_stdin().await;
    }

    struct BlockingSideChannel {
        entered: tokio::sync::mpsc::UnboundedSender<String>,
    }

    impl SideChannel for BlockingSideChannel {
        fn handle(&self, _link: Arc<DaemonLink>, event: ServerEvent) -> SideFuture {
            let entered = self.entered.clone();
            Box::pin(async move {
                if let ServerEvent::AskRequested { request_id, .. } = event {
                    let _ = entered.send(request_id);
                    std::future::pending::<()>().await;
                }
            })
        }
    }

    #[tokio::test]
    async fn pump_not_blocked_by_slow_client_call() {
        let (session, mut daemon) = daemon_pair("s1");
        let (entered_tx, mut entered) = tokio::sync::mpsc::unbounded_channel();
        session.install_side_channel(Arc::new(BlockingSideChannel { entered: entered_tx }));
        let mut foreground = session.foreground(None);
        daemon
            .emit(json!({"type":"ask_requested","request_id":"ask-1","questions":[]}))
            .await;
        daemon
            .emit(json!({"type":"ask_requested","request_id":"ask-2","questions":[]}))
            .await;
        daemon.emit(json!({"type":"text_delta","text":"still flowing"})).await;
        daemon.emit(json!({"type":"done","id":42})).await;
        let mut started = vec![
            within("the first client call", entered.recv()).await.unwrap(),
            within("the second client call", entered.recv()).await.unwrap(),
        ];
        started.sort();
        assert_eq!(
            started,
            ["ask-1", "ask-2"],
            "each client round trip runs in its own task"
        );
        assert!(matches!(
            within("the text delta", foreground.next()).await.unwrap(),
            ServerEvent::TextDelta { .. }
        ));
        assert!(matches!(
            within("done", foreground.next()).await.unwrap(),
            ServerEvent::Done { id: 42 }
        ));
    }

    #[tokio::test]
    async fn client_request_answered_not_wired_off_the_prompt_stream() {
        let (session, mut daemon) = daemon_pair("s1");
        let mut foreground = session.foreground(None);
        daemon
            .emit(json!({"type":"client_request","request_id":"cr-1","tool_call_id":"t1",
                "method":"fs/read_text_file","params":{"path":"/tmp/a"}}))
            .await;
        let response = daemon.recv_type("client_response").await;
        assert_eq!(response["request_id"], "cr-1");
        assert_eq!(response["error"]["code"], JSONRPC_METHOD_NOT_FOUND);
        assert_eq!(response["error"]["message"], "client capability not wired");
        assert!(response["result"].is_null(), "{response}");
        daemon.emit(json!({"type":"done","id":7})).await;
        assert!(
            matches!(
                within("done", foreground.next()).await.unwrap(),
                ServerEvent::Done { id: 7 }
            ),
            "client_request must not reach the prompt stream"
        );
    }

    struct RecordingSideChannel {
        seen: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    }

    impl SideChannel for RecordingSideChannel {
        fn handle(&self, _link: Arc<DaemonLink>, event: ServerEvent) -> SideFuture {
            let _ = self.seen.send(event);
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn plan_and_diff_follow_the_active_prompt() {
        let (session, mut daemon) = daemon_pair("s1");
        let (seen_tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
        session.install_side_channel(Arc::new(RecordingSideChannel { seen: seen_tx }));
        let plan = json!({"type":"plan_updated","entries":[{"content":"step","priority":"high","status":"pending"}]});
        let diff = json!({"type":"tool_diff","id":"t1","diffs":[{"path":"/tmp/a","old_text":null,"new_text":"x"}]});
        let permission = json!({"type":"tool_permission_requested","request_id":"p1","tool_call_id":"t1",
            "tool_name":"bash","title":"Run","input":{}});

        let mut foreground = session.foreground(None);
        daemon.emit(plan.clone()).await;
        daemon.emit(permission.clone()).await;
        daemon.emit(diff.clone()).await;
        assert!(matches!(
            within("plan on the prompt stream", foreground.next()).await.unwrap(),
            ServerEvent::PlanUpdated { .. }
        ));
        assert!(matches!(
            within("diff on the prompt stream", foreground.next()).await.unwrap(),
            ServerEvent::ToolDiff { .. }
        ));
        assert!(
            matches!(
                within("permission on the side channel", seen.recv()).await.unwrap(),
                ServerEvent::ToolPermissionRequested { .. }
            ),
            "a permission round trip never waits on the prompt stream"
        );
        drop(foreground);

        daemon.emit(plan).await;
        daemon.emit(diff).await;
        assert!(matches!(
            within("plan on the side channel", seen.recv()).await.unwrap(),
            ServerEvent::PlanUpdated { .. }
        ));
        assert!(matches!(
            within("diff on the side channel", seen.recv()).await.unwrap(),
            ServerEvent::ToolDiff { .. }
        ));
    }

    #[tokio::test]
    async fn daemon_eof_after_cancel_is_cancelled() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(40), "s1")).await;
        daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        drop(daemon);
        let (_, response) = client.recv_response(json!(40)).await;
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
        assert!(!session.prompt_running.load(Ordering::SeqCst));
        client.close_stdin().await;
    }

    /// A cancelled prompt answers before the daemon's terminal reply, so the
    /// old turn is still streaming when the next prompt takes the foreground.
    /// None of that tail may reach it: the second prompt answers its own stop
    /// reason and carries only its own frames.
    #[tokio::test]
    async fn cancelled_turn_tail_stays_out_of_the_next_prompt() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(100), "s1")).await;
        let first = daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        daemon
            .emit(json!({"type":"turn_stopped","reason":"interrupted",
                "message":"The turn was interrupted by a cancellation request."}))
            .await;
        let (_, first_response) = client.recv_response(json!(100)).await;
        assert_eq!(first_response["result"]["stopReason"], "cancelled", "{first_response}");

        // The old turn's tail arrives while the adapter still holds it for the
        // released turn, before the next prompt is written to the daemon.
        for event in [
            json!({"type":"text_delta","text":"old tail"}),
            json!({"type":"done","id":first["id"]}),
            json!({"type":"interrupted"}),
            json!({"type":"done","id":first["id"]}),
        ] {
            daemon.emit(event).await;
        }

        client.send(prompt(json!(101), "s1")).await;
        let second = daemon.recv_type("message").await;
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, second_response) = client.recv_response(json!(101)).await;
        assert_eq!(
            second_response["result"]["stopReason"], "end_turn",
            "the second prompt inherited the cancelled turn: {second_response}"
        );
        let texts: Vec<&str> = before
            .iter()
            .filter_map(|line| line["params"]["update"]["content"]["text"].as_str())
            .collect();
        assert_eq!(texts, ["second turn"], "the old turn's frames leaked: {before:?}");
        client.close_stdin().await;
    }

    /// A cancel that reached the daemon after the turn had already finished
    /// leaves it nothing to send but one `Interrupted` and no `Done`. The turn
    /// keeps its own stop reason and the next prompt keeps every frame it
    /// streams: a note like that must never hold the stream back.
    #[tokio::test]
    async fn cancel_after_the_turn_finished_keeps_the_next_prompt_frames() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(110), "s1")).await;
        let first = daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        let (_, first_response) = client.recv_response(json!(110)).await;
        assert_eq!(first_response["result"]["stopReason"], "cancelled", "{first_response}");
        daemon.emit(json!({"type":"interrupted"})).await;

        client.send(prompt(json!(111), "s1")).await;
        let second = daemon.recv_type("message").await;
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, second_response) = client.recv_response(json!(111)).await;
        assert_eq!(
            second_response["result"]["stopReason"], "end_turn",
            "a cancel this prompt never recorded was read as its own: {second_response}"
        );
        let texts: Vec<&str> = before
            .iter()
            .filter_map(|line| line["params"]["update"]["content"]["text"].as_str())
            .collect();
        assert_eq!(texts, ["second turn"], "{before:?}");
        client.close_stdin().await;
    }

    fn verify_start_client(profile: AcpProfile) -> Client {
        let (output_writer, output_reader) = tokio::io::duplex(1 << 20);
        let (input_writer, input_reader) = tokio::io::duplex(1 << 20);
        let runtime = AcpRuntime::new(profile, ProviderChoice::HoronAgent, None, None, Box::new(output_writer));
        let serve = tokio::spawn(runtime.clone().serve(input_reader));
        Client {
            runtime,
            input: Some(input_writer),
            output: BufReader::new(output_reader).lines(),
            serve,
        }
    }

    async fn verify_try_response(client: &mut Client, id: Value, wait: Duration) -> Option<(Vec<Value>, Value)> {
        let mut before = Vec::new();
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let line = match tokio::time::timeout_at(deadline, client.output.next_line()).await {
                Ok(line) => line.unwrap().expect("stdout closed"),
                Err(_) => return None,
            };
            let value: Value = serde_json::from_str(&line).unwrap();
            if value.get("method").is_none() && value.get("id") == Some(&id) {
                return Some((before, value));
            }
            before.push(value);
        }
    }

    async fn verify_cancel_first(client: &mut Client, daemon: &mut FakeDaemon, id: Value) -> Value {
        client.send(prompt(id.clone(), "s1")).await;
        let first = daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        daemon
            .emit(json!({"type":"turn_stopped","reason":"interrupted",
                "message":"The turn was interrupted by a cancellation request."}))
            .await;
        let (_, response) = client.recv_response(id).await;
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
        first
    }

    // VERIFY-1: an interrupt this ACP client did not issue (peer connection
    // cancel, coordinator cascade) maps to cancelled per the todo 10 table.
    #[tokio::test]
    async fn verify_external_interrupt_done_is_cancelled() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(300), "s1")).await;
        let first = daemon.recv_type("message").await;
        daemon
            .emit(json!({"type":"turn_stopped","reason":"interrupted",
                "message":"Stopped because its coordinator c1 was interrupted."}))
            .await;
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        let (_, response) = client.recv_response(json!(300)).await;
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn verify_external_interrupt_error_is_cancelled() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(301), "s1")).await;
        let first = daemon.recv_type("message").await;
        daemon
            .emit(json!({"type":"turn_stopped","reason":"interrupted",
                "message":"The turn was interrupted by a cancellation request."}))
            .await;
        daemon.emit(json!({"type":"interrupted"})).await;
        daemon
            .emit(json!({"type":"error","id":first["id"],"message":"interrupted"}))
            .await;
        let (_, response) = client.recv_response(json!(301)).await;
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
        client.close_stdin().await;
    }

    // VERIFY-2: real daemon order of a cooperative cancel (pre-Done
    // TurnStopped, worker frames, the worker's own Done, then the cancel
    // path's Interrupted and duplicate Done), extended profile, with stale
    // plan/diff side events. Nothing of the old turn may reach the next
    // prompt, including _horon_agent/server_event mirrors.
    #[tokio::test]
    async fn verify_real_order_tail_extended_profile() {
        let mut client = verify_start_client(AcpProfile::Extended);
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let first = verify_cancel_first(&mut client, &mut daemon, json!(310)).await;
        // The daemon emits the old turn's tail while the adapter holds the
        // released turn, before the next prompt is written.
        for event in [
            json!({"type":"text_delta","text":"old tail"}),
            json!({"type":"plan_updated","entries":[{"content":"stale step","priority":"high","status":"pending"}]}),
            json!({"type":"tool_diff","id":"t_old","diffs":[{"path":"/tmp/stale","old_text":null,"new_text":"x"}]}),
            json!({"type":"done","id":first["id"]}),
            json!({"type":"interrupted"}),
            json!({"type":"done","id":first["id"]}),
        ] {
            daemon.emit(event).await;
        }
        client.send(prompt(json!(311), "s1")).await;
        let second = daemon.recv_type("message").await;
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, response) = client.recv_response(json!(311)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        let leaked: Vec<&Value> = before
            .iter()
            .filter(|line| {
                let text = line.to_string();
                text.contains("old tail")
                    || text.contains("stale")
                    || line["params"]["event"]["id"] == first["id"]
                    || line["params"]["event"]["type"] == "interrupted"
            })
            .collect();
        assert!(
            leaked.is_empty(),
            "old-turn frames reached the next prompt: {leaked:#?}"
        );
        client.close_stdin().await;
    }

    // VERIFY-3: a daemon that never sends the cancelled turn's terminal.
    // The next prompt's own Done must still answer it.
    #[tokio::test]
    async fn verify_missing_old_terminal_next_prompt_still_answers() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        verify_cancel_first(&mut client, &mut daemon, json!(320)).await;
        client.send(prompt(json!(321), "s1")).await;
        let second = daemon.recv_type("message").await;
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let got = verify_try_response(&mut client, json!(321), Duration::from_secs(14)).await;
        assert!(
            got.is_some(),
            "the next prompt never answered within 14 s although the daemon sent its Done"
        );
    }

    // VERIFY-4: stale side events after a released turn never reach the side
    // channel; the drain ends on the turn's own terminal.
    #[tokio::test]
    async fn verify_stale_side_events_dropped_until_terminal() {
        let (session, mut daemon) = daemon_pair("s1");
        let (seen_tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
        session.install_side_channel(Arc::new(RecordingSideChannel { seen: seen_tx }));
        let foreground = session.foreground(None);
        foreground.drain_turn(77);
        daemon
            .emit(json!({"type":"plan_updated","entries":[{"content":"stale","priority":"high","status":"pending"}]}))
            .await;
        daemon
            .emit(json!({"type":"tool_diff","id":"t1","diffs":[{"path":"/tmp/stale","old_text":null,"new_text":"x"}]}))
            .await;
        daemon.emit(json!({"type":"done","id":77})).await;
        daemon
            .emit(json!({"type":"plan_updated","entries":[{"content":"fresh","priority":"high","status":"pending"}]}))
            .await;
        let event = within("the fresh plan on the side channel", seen.recv()).await.unwrap();
        let text = serde_json::to_string(&event).unwrap();
        assert!(text.contains("fresh") && !text.contains("stale"), "{text}");
    }

    async fn probe_second_cancelled(daemon_tail: Vec<Value>) -> Option<Value> {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        // Turn 1: cancelled, answered on the pre-Done TurnStopped; its tail never comes.
        verify_cancel_first(&mut client, &mut daemon, json!(400)).await;
        client.send(prompt(json!(401), "s1")).await;
        let second = daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        for mut event in daemon_tail {
            if event["id"] == json!("SECOND") {
                event["id"] = second["id"].clone();
            }
            daemon.emit(event).await;
        }
        verify_try_response(&mut client, json!(401), Duration::from_secs(14))
            .await
            .map(|(_, response)| response)
    }

    // PROBE-3a: turn 1 tail missing; prompt 2 cancelled inside the window with
    // the daemon's full real tail.
    #[tokio::test]
    async fn probe_second_cancel_full_tail_in_window() {
        let response = probe_second_cancelled(vec![
            json!({"type":"turn_stopped","reason":"interrupted","message":"x"}),
            json!({"type":"done","id":"SECOND"}),
            json!({"type":"interrupted"}),
            json!({"type":"done","id":"SECOND"}),
        ])
        .await
        .expect("prompt 2 answered");
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
    }

    // PROBE-3b: same, but the daemon sends only the pre-Done TurnStopped for turn 2.
    #[tokio::test]
    async fn probe_second_cancel_only_turnstopped_in_window() {
        let started = std::time::Instant::now();
        let response = probe_second_cancelled(vec![
            json!({"type":"turn_stopped","reason":"interrupted","message":"x"}),
        ])
        .await
        .expect("prompt 2 answered within 14 s");
        eprintln!("PROBE-3b answered after {:?}: {response}", started.elapsed());
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
    }

    async fn probe_race_then_next(first_tail: Vec<Value>) -> (Value, Vec<Value>, Value) {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(500), "s1")).await;
        let first = daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        // The turn finished before the daemon handled the cancel.
        daemon.emit(json!({"type":"text_delta","text":"first turn"})).await;
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        for event in first_tail {
            daemon.emit(event).await;
        }
        let (_, first_response) = client.recv_response(json!(500)).await;
        client.send(prompt(json!(501), "s1")).await;
        let second = daemon.recv_type("message").await;
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, second_response) = verify_try_response(&mut client, json!(501), Duration::from_secs(14))
            .await
            .expect("prompt 2 answered");
        (first_response, before, second_response)
    }

    fn probe_texts(before: &[Value]) -> Vec<String> {
        before
            .iter()
            .filter_map(|line| line["params"]["update"]["content"]["text"].as_str().map(str::to_string))
            .collect()
    }

    // PROBE-RACE-A: client_lifecycle.rs IGNORED_FINISHED - the cancel finds the
    // worker already finished and sends nothing.
    #[tokio::test]
    async fn probe_cancel_finish_race_ignored_finished() {
        let (first, before, second) = probe_race_then_next(vec![]).await;
        eprintln!(
            "PROBE-RACE-A first={first} second={second} texts={:?}",
            probe_texts(&before)
        );
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(
            probe_texts(&before),
            ["second turn"],
            "the next turn's frames were swallowed: {before:?}"
        );
    }

    // PROBE-RACE-B: client_lifecycle.rs IDLE_NOOP - Interrupted with no Done.
    #[tokio::test]
    async fn probe_cancel_finish_race_idle_noop() {
        let (first, before, second) = probe_race_then_next(vec![json!({"type":"interrupted"})]).await;
        eprintln!(
            "PROBE-RACE-B first={first} second={second} texts={:?}",
            probe_texts(&before)
        );
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(
            probe_texts(&before),
            ["second turn"],
            "the next turn's frames were swallowed: {before:?}"
        );
    }

    // Q3: where the cancelled turn's tail lands relative to the next prompt.
    // Event driven, no clock: `before_gate` emits the tail as soon as the next
    // prompt is sent, so it ends the adapter's tail wait; otherwise the tail is
    // held back until the adapter has given up the tail wait (it probes
    // GetState, which it does only after TURN_TAIL_WAIT expired) and has written
    // the next message - later than any timed bound could place it.
    async fn q3_tail(before_gate: bool) -> (Vec<Value>, Value) {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let first = verify_cancel_first(&mut client, &mut daemon, json!(600)).await;
        let tail = [
            json!({"type":"text_delta","text":"old tail"}),
            json!({"type":"done","id":first["id"]}),
            json!({"type":"interrupted"}),
            json!({"type":"done","id":first["id"]}),
        ];
        client.send(prompt(json!(601), "s1")).await;
        if before_gate {
            for event in tail.clone() {
                daemon.emit(event).await;
            }
        }
        let mut probes = 0;
        let second = loop {
            let request = daemon.recv().await;
            if request["type"] == "message" {
                break request;
            }
            if request["type"] == "state" {
                probes += 1;
                daemon
                    .emit(json!({"type":"state","id":request["id"],"session_id":"s1","message_count":1,"is_processing":false}))
                    .await;
            }
        };
        assert!(probes >= 1, "the next message was written without the idle gate");
        if !before_gate {
            for event in tail {
                daemon.emit(event).await;
            }
        }
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, response) = verify_try_response(&mut client, json!(601), Duration::from_secs(14))
            .await
            .expect("prompt 2 answered");
        eprintln!(
            "Q3 before_gate={before_gate} probes={probes} response={response} texts={:?}",
            probe_texts(&before)
        );
        (before, response)
    }

    #[tokio::test]
    async fn q3_tail_within_bound_is_clean() {
        let (before, response) = q3_tail(true).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        assert_eq!(probe_texts(&before), ["second turn"]);
    }

    #[tokio::test]
    async fn q3_tail_after_bound_leaks() {
        let (before, response) = q3_tail(false).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        assert_eq!(probe_texts(&before), ["second turn"]);
    }

    // Q2: a cancel that raced the turn's finish, whose stray Interrupted the
    // daemon sends late: after the GetState reply, which the daemon writes
    // straight to the socket (client_state.rs:130-153). Event driven, real
    // clock, no sleeps: the fake daemon holds each State reply until the test
    // has observed the probe and decides what to answer.
    //
    // `busy_until_note`: the first probe is answered busy, the daemon then
    // handles the cancel (the note), and the next probe is answered idle.
    // Otherwise the held reply is released as idle and the note is sent only
    // after the next message - the order a slow State reply produces. The
    // adapter has no per-probe deadline below its 2 s gate bound, so a held
    // reply is waited for: the request after it must be the message, never a
    // second probe.
    async fn q2_stray_late(busy_until_note: bool) -> (Vec<Value>, Value) {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(700), "s1")).await;
        let first = daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        daemon.emit(json!({"type":"text_delta","text":"first turn"})).await;
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        let (_, first_response) = client.recv_response(json!(700)).await;
        client.send(prompt(json!(701), "s1")).await;
        let mut note_sent = false;
        let mut requests = Vec::new();
        let second = loop {
            let request = daemon.recv().await;
            requests.push(request["type"].clone());
            if request["type"] == "message" {
                break request;
            }
            assert_eq!(request["type"], "state", "unexpected daemon request {request}");
            // The probe is observed; the reply is held until now.
            let busy = busy_until_note && !note_sent;
            daemon
                .emit(
                    json!({"type":"state","id":request["id"],"session_id":"s1","message_count":1,"is_processing":busy}),
                )
                .await;
            if busy {
                // The daemon then handles the queued cancel.
                daemon.emit(json!({"type":"interrupted"})).await;
                note_sent = true;
            }
        };
        let expected: &[&str] = if busy_until_note {
            &["state", "state", "message"]
        } else {
            &["state", "message"]
        };
        assert_eq!(requests, expected, "probe sequence");
        if !note_sent {
            // The late stray note reaches the adapter after the message.
            daemon.emit(json!({"type":"interrupted"})).await;
        }
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, response) = verify_try_response(&mut client, json!(701), Duration::from_secs(14))
            .await
            .expect("prompt 2 answered");
        eprintln!(
            "Q2 busy_until_note={busy_until_note} requests={requests:?} first={first_response} response={response} texts={:?}",
            probe_texts(&before)
        );
        (before, response)
    }

    #[tokio::test]
    async fn q2_gate_answered_in_time_busy_then_idle() {
        let (before, response) = q2_stray_late(true).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        assert_eq!(probe_texts(&before), ["second turn"]);
    }

    #[tokio::test]
    async fn q2_gate_reply_slower_than_50ms_counts_idle() {
        let (before, response) = q2_stray_late(false).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        assert_eq!(probe_texts(&before), ["second turn"]);
    }

    // Q2c: after a cancelled turn the daemon answers every GetState at once with
    // is_processing:true. A working gate probes IDLE_PROBES (6) times before it
    // writes; a gate that never sees the State reply counts its first probe as
    // silent, spends its bound on it and writes after one probe.
    #[tokio::test]
    async fn q2_gate_sees_busy_reply() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let first = verify_cancel_first(&mut client, &mut daemon, json!(799)).await;
        for event in [
            json!({"type":"done","id":first["id"]}),
            json!({"type":"interrupted"}),
            json!({"type":"done","id":first["id"]}),
        ] {
            daemon.emit(event).await;
        }
        let started = tokio::time::Instant::now();
        client.send(prompt(json!(800), "s1")).await;
        let mut probes = 0;
        let message = loop {
            let request = daemon.recv().await;
            if request["type"] == "message" {
                break request;
            }
            if request["type"] == "state" {
                probes += 1;
                daemon
                    .emit(json!({"type":"state","id":request["id"],"session_id":"s1","message_count":1,"is_processing":true}))
                    .await;
            }
        };
        eprintln!("Q2c probes={probes} message after {:?}", started.elapsed());
        daemon.emit(json!({"type":"done","id":message["id"]})).await;
        let (_, response) = client.recv_response(json!(800)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn");
        assert_eq!(probes, 6, "the gate never saw the daemon's busy State reply");

        // That turn was not cancelled, so the next prompt skips the gate.
        client.send(prompt(json!(801), "s1")).await;
        let next = daemon.recv().await;
        assert_eq!(
            next["type"], "message",
            "a prompt after a clean turn probed the daemon: {next}"
        );
        daemon.emit(json!({"type":"done","id":next["id"]})).await;
        let (_, response) = client.recv_response(json!(801)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn");
    }

    // R: a cancel races a turn that ends on its own non-interrupt TurnStopped
    // (worker Err -> TurnStopped{Failure}+Error, client_lifecycle.rs:3984-4011;
    // loop guard / recovery exhausted -> TurnStopped{LimitReached}+Done,
    // turn_streaming_mpsc.rs:1652, :2349). The prompt answers Abandoned on that
    // TurnStopped; the daemon's cancel then finds the worker finished
    // (IGNORED_FINISHED: nothing, IDLE_NOOP: lone Interrupted) or live
    // (TurnStopped{Interrupted}, worker terminal, Interrupted, Done).
    async fn r_probe(first_tail: Vec<Value>) -> (Value, Vec<Value>, Value, Duration) {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(900), "s1")).await;
        let first = daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        for mut event in first_tail {
            if event["id"] == json!("FIRST") {
                event["id"] = first["id"].clone();
            }
            daemon.emit(event).await;
        }
        let (_, first_response) = client.recv_response(json!(900)).await;
        let started = tokio::time::Instant::now();
        client.send(prompt(json!(901), "s1")).await;
        let second = loop {
            let request = daemon.recv().await;
            if request["type"] == "message" {
                break request;
            }
            if request["type"] == "state" {
                daemon
                    .emit(json!({"type":"state","id":request["id"],"session_id":"s1","message_count":1,"is_processing":false}))
                    .await;
            }
        };
        let written = started.elapsed();
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, second_response) = verify_try_response(&mut client, json!(901), Duration::from_secs(14))
            .await
            .expect("prompt 2 answered");
        eprintln!(
            "R first={first_response} second={second_response} message_after={written:?} texts={:?}",
            probe_texts(&before)
        );
        (first_response, before, second_response, written)
    }

    #[tokio::test]
    async fn r_failure_then_ignored_finished() {
        let (_, before, second, _) = r_probe(vec![
            json!({"type":"turn_stopped","reason":"failure","message":"provider error"}),
            json!({"type":"error","id":"FIRST","message":"provider error"}),
        ])
        .await;
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(probe_texts(&before), ["second turn"], "next turn content lost");
    }

    #[tokio::test]
    async fn r_failure_then_idle_noop() {
        let (_, before, second, _) = r_probe(vec![
            json!({"type":"turn_stopped","reason":"failure","message":"provider error"}),
            json!({"type":"error","id":"FIRST","message":"provider error"}),
            json!({"type":"interrupted"}),
        ])
        .await;
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(probe_texts(&before), ["second turn"], "next turn content lost");
    }

    #[tokio::test]
    async fn r_limit_then_ignored_finished() {
        let (_, before, second, _) = r_probe(vec![
            json!({"type":"turn_stopped","reason":"limit_reached","message":"loop guard"}),
            json!({"type":"done","id":"FIRST"}),
        ])
        .await;
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(probe_texts(&before), ["second turn"], "next turn content lost");
    }

    #[tokio::test]
    async fn r_failure_then_live_cancel_tail() {
        let (_, before, second, _) = r_probe(vec![
            json!({"type":"turn_stopped","reason":"failure","message":"provider error"}),
            json!({"type":"turn_stopped","reason":"interrupted","message":"x"}),
            json!({"type":"error","id":"FIRST","message":"provider error"}),
            json!({"type":"interrupted"}),
            json!({"type":"done","id":"FIRST"}),
        ])
        .await;
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(probe_texts(&before), ["second turn"], "next turn content lost");
    }

    // S: the stray drain must not swallow the next turn's first content event.
    #[tokio::test]
    async fn s_stray_first_event_kept() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(950), "s1")).await;
        let first = daemon.recv_type("message").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        daemon.recv_type("cancel").await;
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        let (_, first_response) = client.recv_response(json!(950)).await;
        assert_eq!(first_response["result"]["stopReason"], "cancelled");
        client.send(prompt(json!(951), "s1")).await;
        let second = loop {
            let request = daemon.recv().await;
            if request["type"] == "message" {
                break request;
            }
            if request["type"] == "state" {
                daemon
                    .emit(json!({"type":"state","id":request["id"],"session_id":"s1","message_count":1,"is_processing":false}))
                    .await;
            }
        };
        // IGNORED_FINISHED: no note at all; the next turn's first event is content.
        daemon.emit(json!({"type":"text_delta","text":"first event"})).await;
        daemon
            .emit(json!({"type":"plan_updated","entries":[{"content":"p","priority":"high","status":"pending"}]}))
            .await;
        daemon.emit(json!({"type":"text_delta","text":" second event"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, response) = client.recv_response(json!(951)).await;
        eprintln!(
            "S texts={:?} kinds={:?}",
            probe_texts(&before),
            before
                .iter()
                .map(|l| l["params"]["update"]["sessionUpdate"].clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(response["result"]["stopReason"], "end_turn");
        assert_eq!(probe_texts(&before), ["first event", " second event"]);
    }

    // T: a fast cooperative cancel. The daemon's whole real tail
    // (TurnStopped{Interrupted}, worker Done, Interrupted, Done) arrives back
    // to back, before the prompt loop has released its stream.
    #[tokio::test]
    async fn t_fast_cooperative_cancel_tail_back_to_back() {
        let (_, before, second, written) = r_probe(vec![
            json!({"type":"turn_stopped","reason":"interrupted","message":"x"}),
            json!({"type":"done","id":"FIRST"}),
            json!({"type":"interrupted"}),
            json!({"type":"done","id":"FIRST"}),
        ])
        .await;
        eprintln!("T message_after={written:?}");
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(probe_texts(&before), ["second turn"], "next turn content lost");
    }

    /// `session/cancel` twice and `$/cancel_request` for the same prompt send
    /// the daemon exactly one `Cancel`: a second one would hit the daemon's
    /// idle path and answer with a lone `Interrupted`.
    #[tokio::test]
    async fn repeated_cancels_send_one_daemon_cancel() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(1000), "s1")).await;
        let first = daemon.recv_type("message").await;
        let cancel = json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}});
        client.send(cancel.clone()).await;
        client.send(cancel).await;
        client
            .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":1000}}))
            .await;
        assert_eq!(daemon.recv().await["type"], "cancel");
        for event in [
            json!({"type":"turn_stopped","reason":"interrupted","message":"x"}),
            json!({"type":"done","id":first["id"]}),
            json!({"type":"interrupted"}),
            json!({"type":"done","id":first["id"]}),
        ] {
            daemon.emit(event).await;
        }
        let (_, response) = client.recv_response(json!(1000)).await;
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
        client.send(prompt(json!(1001), "s1")).await;
        let mut requests = Vec::new();
        let second = loop {
            let request = daemon.recv().await;
            requests.push(request["type"].clone());
            if request["type"] == "message" {
                break request;
            }
            if request["type"] == "state" {
                daemon
                    .emit(json!({"type":"state","id":request["id"],"session_id":"s1","message_count":1,"is_processing":false}))
                    .await;
            }
        };
        assert!(
            !requests.iter().any(|kind| kind == "cancel"),
            "a second daemon cancel was sent: {requests:?}"
        );
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (_, response) = client.recv_response(json!(1001)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    }

    /// The real daemon's cancel sends bookkeeping (the swarm member status,
    /// client_lifecycle.rs:4118-4130) between the worker's `Done` and its own
    /// `Interrupted`, and its `GetState` reply can overtake that `Interrupted`.
    /// The bookkeeping must not end the stray drain: the note still belongs to
    /// the cancelled turn even when it lands after the next message.
    #[tokio::test]
    async fn cancel_bookkeeping_before_the_note_keeps_the_stray_drain() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let first = verify_cancel_first(&mut client, &mut daemon, json!(1020)).await;
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        daemon.emit(json!({"type":"swarm_status","members":[]})).await;
        client.send(prompt(json!(1021), "s1")).await;
        let second = loop {
            let request = daemon.recv().await;
            if request["type"] == "message" {
                break request;
            }
            if request["type"] == "state" {
                daemon
                    .emit(json!({"type":"state","id":request["id"],"session_id":"s1","message_count":1,"is_processing":false}))
                    .await;
            }
        };
        daemon.emit(json!({"type":"interrupted"})).await;
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        daemon.emit(json!({"type":"text_delta","text":"second turn"})).await;
        daemon.emit(json!({"type":"done","id":second["id"]})).await;
        let (before, response) = client.recv_response(json!(1021)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        assert_eq!(probe_texts(&before), ["second turn"]);
    }

    /// A `session/cancel` with no prompt in flight is a no-op: nothing reaches
    /// the daemon, so its idle path cannot queue a note for the next prompt.
    #[tokio::test]
    async fn cancel_without_a_prompt_in_flight_reaches_no_daemon() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(1010), "s1")).await;
        let first = daemon.recv_type("message").await;
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        let (_, response) = client.recv_response(json!(1010)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        client.send(prompt(json!(1011), "s1")).await;
        let next = daemon.recv().await;
        assert_eq!(next["type"], "message", "an idle cancel reached the daemon: {next}");
        daemon.emit(json!({"type":"done","id":next["id"]})).await;
        let (_, response) = client.recv_response(json!(1011)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    }

    // V5-ii: a peer connection's cancel interrupts the turn first
    // (NO_LOCAL_TASK branch fans TurnStopped{Interrupted} to every session
    // member, client_lifecycle.rs:4166-4174, and sends its Interrupted only to
    // the peer), the worker ends and sends its own terminal, and this
    // connection's own recorded cancel then finds the worker finished.
    #[tokio::test]
    async fn v5_peer_interrupt_then_own_cancel_idle_noop() {
        let (_, before, second, written) = r_probe(vec![
            json!({"type":"turn_stopped","reason":"interrupted","message":"peer"}),
            json!({"type":"done","id":"FIRST"}),
            json!({"type":"interrupted"}),
        ])
        .await;
        eprintln!("V5-ii-idle message_after={written:?}");
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(probe_texts(&before), ["second turn"], "next turn content lost");
    }

    #[tokio::test]
    async fn v5_peer_interrupt_then_own_cancel_ignored_finished() {
        let (_, before, second, written) = r_probe(vec![
            json!({"type":"turn_stopped","reason":"interrupted","message":"peer"}),
            json!({"type":"done","id":"FIRST"}),
        ])
        .await;
        eprintln!("V5-ii-ignored message_after={written:?}");
        assert_eq!(second["result"]["stopReason"], "end_turn", "{second}");
        assert_eq!(probe_texts(&before), ["second turn"], "next turn content lost");
    }

    #[tokio::test]
    async fn daemon_eof_without_cancel_is_internal_error() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(41), "s1")).await;
        daemon.recv_type("message").await;
        drop(daemon);
        let (_, response) = client.recv_response(json!(41)).await;
        assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn busy_prompt_is_invalid_params() {
        let mut client = start_client();
        client.initialize().await;
        let (session, _daemon) = attach_fake_session(&client, "s1").await;
        session.prompt_running.store(true, Ordering::SeqCst);
        client.send(prompt(json!(50), "s1")).await;
        let response = client.recv().await;
        assert_eq!(response["id"], 50);
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS);
        client.send(set_model(json!(51), "s1", "m1")).await;
        let response = client.recv().await;
        assert_eq!(response["id"], 51);
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn prompt_daemon_error_is_internal_error() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(60), "s1")).await;
        let message = daemon.recv_type("message").await;
        daemon
            .emit(json!({"type":"error","id":message["id"],"message":"provider failed"}))
            .await;
        let (_, response) = client.recv_response(json!(60)).await;
        assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
        client.close_stdin().await;
    }

    fn request(id: Value, method: &str, params: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
    }

    fn assert_session_not_found(response: &Value, session_id: &str) {
        assert_eq!(response["error"]["code"], ACP_RESOURCE_NOT_FOUND, "{response}");
        assert_eq!(response["error"]["data"]["sessionId"], session_id, "{response}");
    }

    #[tokio::test]
    async fn authenticate_unknown_method_invalid_params() {
        let mut client = start_client();
        client.initialize().await;
        // The removed terminal method is now just an unknown id: `authenticate`
        // answers -32602 and the later requests prove the process stayed alive.
        for (id, params, expected) in [
            (1, json!({"methodId": "nope"}), "Unknown authentication method: nope"),
            (
                2,
                json!({"methodId": "horon-agent-terminal-login"}),
                "Unknown authentication method: horon-agent-terminal-login",
            ),
            (3, json!({}), "authenticate requires a string methodId"),
            (4, json!({"methodId": 7}), "authenticate requires a string methodId"),
        ] {
            client.send(request(json!(id), "authenticate", params)).await;
            let (_, response) = client.recv_response(json!(id)).await;
            assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
            assert_eq!(response["error"]["message"], expected, "{response}");
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn authenticate_answers_from_cached_readiness() {
        let mut client = start_client();
        client.initialize().await;
        client.runtime.ready.store(true, Ordering::SeqCst);
        for id in 0..10 {
            client
                .send(request(
                    json!(id + 10),
                    "authenticate",
                    json!({"methodId": "horon-agent-credentials"}),
                ))
                .await;
            let (_, response) = client.recv_response(json!(id + 10)).await;
            assert_eq!(response["result"], json!({}), "{response}");
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn session_new_requires_absolute_cwd() {
        let mut client = start_client();
        client.initialize().await;
        for (id, params) in [
            (1, json!({"mcpServers": []})),
            (2, json!({"cwd": "relative/dir", "mcpServers": []})),
            (3, json!({"cwd": "", "mcpServers": []})),
            (4, json!({"cwd": 5, "mcpServers": []})),
        ] {
            client.send(request(json!(id), "session/new", params)).await;
            let (_, response) = client.recv_response(json!(id)).await;
            assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn session_new_rejects_relative_additional_directory() {
        let mut client = start_client();
        client.initialize().await;
        let params = json!({"cwd": "/tmp", "mcpServers": [], "additionalDirectories": ["extra"]});
        client.send(request(json!(1), "session/new", params)).await;
        let (_, response) = client.recv_response(json!(1)).await;
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
        let params = json!({"cwd": "/tmp", "mcpServers": [{"name": "fs"}]});
        client.send(request(json!(2), "session/new", params)).await;
        let (_, response) = client.recv_response(json!(2)).await;
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn unknown_session_is_resource_not_found() {
        let mut client = start_client();
        client.initialize().await;
        let requests = [
            prompt(json!(1), "nope"),
            set_model(json!(2), "nope", "m1"),
            request(
                json!(3),
                "session/set_model",
                json!({"sessionId": "nope", "modelId": "m1"}),
            ),
            request(
                json!(4),
                "session/set_reasoning_effort",
                json!({"sessionId": "nope", "effort": "high"}),
            ),
            request(json!(5), "session/close", json!({"sessionId": "nope"})),
        ];
        for (index, message) in requests.into_iter().enumerate() {
            let id = message["id"].clone();
            client.send(message).await;
            let (_, response) = client.recv_response(id).await;
            assert_session_not_found(&response, "nope");
            assert_eq!(index + 1, response["id"].as_u64().unwrap() as usize);
        }
        client.close_stdin().await;
    }

    fn history(id: &Value, session_id: &str) -> Value {
        json!({"type":"history","id":id,"session_id":session_id,"provider_model":"gpt-5",
            "messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}]})
    }

    /// Answers the configuration requests a reload sends with the error a
    /// daemon that does not serve them yet returns.
    async fn refuse_config_requests(daemon: &mut FakeDaemon) -> Vec<Value> {
        let mut seen = Vec::new();
        for kind in ["configure_session_mcp", "set_session_approval_mode"] {
            let request = daemon.recv_type(kind).await;
            daemon
                .emit(json!({"type":"error","id":request["id"],"message":"This request is only served on an ACP bridge connection"}))
                .await;
            seen.push(request);
        }
        seen
    }

    #[tokio::test]
    async fn reload_settles_on_ack_well_under_the_config_bound() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        for (request_id, method) in [(7, "session/load"), (8, "session/resume")] {
            client
                .send(request(
                    json!(request_id),
                    method,
                    json!({"sessionId": "s1", "cwd": "/tmp", "mcpServers": []}),
                ))
                .await;
            let get_history = daemon.recv_type("get_history").await;
            daemon.emit(history(&get_history["id"], "s1")).await;
            // The daemon answers the MCP configuration with Done and the
            // approval mode with Ack, exactly as it does on a real connection.
            let mcp = daemon.recv_type("configure_session_mcp").await;
            daemon.emit(json!({"type":"done","id":mcp["id"]})).await;
            let mode = daemon.recv_type("set_session_approval_mode").await;
            daemon.emit(json!({"type":"ack","id":mode["id"]})).await;
            let started = std::time::Instant::now();
            let (_, response) = tokio::time::timeout(Duration::from_secs(2), client.recv_response(json!(request_id)))
                .await
                .unwrap_or_else(|_| panic!("{method} waited past the Ack instead of settling on it"));
            assert!(response["result"].is_object(), "{response}");
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{method} of an active session answered in {:?}",
                started.elapsed()
            );
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn is4_load_active_session_replays_once() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let params = json!({"sessionId": "s1", "cwd": "/tmp", "mcpServers": []});
        client.send(request(json!(7), "session/load", params)).await;
        let get_history = daemon.recv_type("get_history").await;
        daemon.emit(history(&get_history["id"], "s1")).await;
        let config = refuse_config_requests(&mut daemon).await;
        assert_eq!(config[0]["servers"], json!([]));
        let (before, response) = client.recv_response(json!(7)).await;
        assert!(response["result"].is_object(), "{response}");
        let replayed: Vec<&str> = before
            .iter()
            .filter(|line| line["method"] == "session/update")
            .map(|line| line["params"]["update"]["sessionUpdate"].as_str().unwrap())
            .collect();
        assert_eq!(replayed, ["user_message_chunk", "agent_message_chunk"], "{before:?}");
        let after = client.barrier("after-load").await;
        assert!(
            after
                .iter()
                .all(|line| line["params"]["update"]["sessionUpdate"] == "available_commands_update"),
            "nothing replays after the response: {after:?}"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn is4_resume_active_session_skips_replay() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let params = json!({"sessionId": "s1", "cwd": "/tmp", "mcpServers": []});
        client.send(request(json!(8), "session/resume", params)).await;
        let get_history = daemon.recv_type("get_history").await;
        daemon.emit(history(&get_history["id"], "s1")).await;
        refuse_config_requests(&mut daemon).await;
        let (before, response) = client.recv_response(json!(8)).await;
        assert!(response["result"].is_object(), "{response}");
        assert!(
            before.iter().all(|line| line["method"] != "session/update"),
            "resume must not replay: {before:?}"
        );
        client.close_stdin().await;
    }

    /// The events that attach an `s1` target subscribe, as a scripted daemon
    /// answers them: the History for the target, then the request's Done.
    async fn settle_attach(daemon: &mut FakeDaemon, subscribe: &Value) {
        daemon.emit(history(&subscribe["id"], "s1")).await;
        daemon.emit(json!({"type":"done","id":subscribe["id"]})).await;
        let mode = daemon.recv_type("set_session_approval_mode").await;
        daemon.emit(json!({"type":"ack","id":mode["id"]})).await;
    }

    fn load_s1(id: Value) -> Value {
        request(
            id,
            "session/load",
            json!({"sessionId": "s1", "cwd": "/tmp", "mcpServers": []}),
        )
    }

    /// The daemon's refusal of an attach whose session writer is still owned
    /// elsewhere (`StoreError::busy`, as the daemon's Error frame carries it).
    const BUSY_REFUSAL: &str =
        "Failed to restore session: store error [internal:busy] retryable session writer is already owned";

    /// F3 D-1: the daemon refuses an attach with the store's retryable busy
    /// error and closes the connection. The adapter attaches again on one fresh
    /// connection and answers at once.
    #[tokio::test]
    async fn attach_retries_a_retryable_busy_refusal_once() {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client.initialize().await;
        client.send(load_s1(json!(6))).await;

        let mut busy = daemons.recv().await.expect("the first attach's connection");
        let subscribe = busy.recv_type("subscribe").await;
        assert_eq!(subscribe["target_session_id"], "s1", "{subscribe}");
        busy.emit(json!({"type":"ack","id":subscribe["id"]})).await;
        busy.emit(json!({"type":"error","id":subscribe["id"],"message":BUSY_REFUSAL}))
            .await;
        drop(busy);

        let mut retry = within("the retry's connection", daemons.recv())
            .await
            .expect("the retry's connection");
        let subscribe = retry.recv_type("subscribe").await;
        settle_attach(&mut retry, &subscribe).await;

        let (_, response) = tokio::time::timeout(Duration::from_secs(2), client.recv_response(json!(6)))
            .await
            .expect("the retry answers the load");
        assert!(response["result"].is_object(), "{response}");
        assert!(daemons.try_recv().is_err(), "exactly one retry");
        client.close_stdin().await;
    }

    /// The retry gets one attempt: a second busy refusal is the client's
    /// answer, with the daemon's reason, and no third connection.
    #[tokio::test]
    async fn attach_answers_a_busy_refusal_that_repeats_after_one_retry() {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client.initialize().await;
        client.send(load_s1(json!(6))).await;

        for attempt in ["the first attach's connection", "the retry's connection"] {
            let mut daemon = within(attempt, daemons.recv()).await.expect(attempt);
            let subscribe = daemon.recv_type("subscribe").await;
            daemon
                .emit(json!({"type":"error","id":subscribe["id"],"message":BUSY_REFUSAL}))
                .await;
        }

        let (_, response) = tokio::time::timeout(Duration::from_secs(2), client.recv_response(json!(6)))
            .await
            .expect("the repeated refusal is answered");
        assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("s1") && message.contains("session writer is already owned"),
            "{message}"
        );
        assert!(daemons.try_recv().is_err(), "no third connection");
        client.close_stdin().await;
    }

    /// A restore slower than attempt 1's 2 s bound is not a refusal: the
    /// daemon answers it with the History when it is done. Holding the History
    /// past that bound must open no second connection, and the load answers
    /// once with the session.
    #[tokio::test]
    async fn attach_waits_for_a_slow_history_without_retrying() {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client.initialize().await;
        client.send(load_s1(json!(6))).await;

        let mut slow = daemons.recv().await.expect("the attach's connection");
        let subscribe = slow.recv_type("subscribe").await;
        slow.emit(json!({"type":"ack","id":subscribe["id"]})).await;
        // The History is held until this window, past attempt 1's bound,
        // closes; a retry opens its connection inside it.
        let held = Duration::from_millis(2500);
        assert!(
            tokio::time::timeout(held, daemons.recv()).await.is_err(),
            "a slow restore was taken for a refusal and retried"
        );
        settle_attach(&mut slow, &subscribe).await;

        let (before, response) = within("the slow load's answer", client.recv_response(json!(6))).await;
        assert!(response["result"].is_object(), "{response}");
        let replayed = before
            .iter()
            .filter(|line| line["params"]["update"]["sessionUpdate"] == "user_message_chunk")
            .count();
        assert!(replayed <= 1, "the history was replayed {replayed} times: {before:?}");
        assert!(daemons.try_recv().is_err(), "one connection only");
        client.close_stdin().await;
    }

    /// A refusal a retry would meet again is answered at once: a store error
    /// that is not the busy writer, and the live-attach conflict that names
    /// another client holding the session, `retry_after_secs` and all.
    #[tokio::test]
    async fn attach_does_not_retry_a_refusal_that_repeats() {
        for (refusal, retry_after, reason) in [
            (
                "Failed to restore session: permission denied",
                None,
                "permission denied",
            ),
            (
                "Session 's1' is already live but could not be shared safely with this connection.",
                Some(1),
                "could not be shared safely",
            ),
        ] {
            let (connector, mut daemons) = scripted_daemon();
            let mut client = start_client_with(Some(connector));
            client.initialize().await;
            client.send(load_s1(json!(6))).await;

            let mut daemon = daemons.recv().await.expect("the attach's connection");
            let subscribe = daemon.recv_type("subscribe").await;
            daemon
                .emit(json!({"type":"error","id":subscribe["id"],"message":refusal,"retry_after_secs":retry_after}))
                .await;

            let (_, response) = tokio::time::timeout(Duration::from_secs(2), client.recv_response(json!(6)))
                .await
                .expect("the refusal is answered");
            assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
            let message = response["error"]["message"].as_str().expect("message");
            assert!(message.contains(reason), "{message}");
            assert!(daemons.try_recv().is_err(), "no second connection for {refusal}");
            client.close_stdin().await;
        }
    }

    /// A daemon that closes the connection instead of answering is answered at
    /// once: the pump fails the attach waiter on EOF, and a close is not the
    /// busy refusal, so nothing is retried.
    #[tokio::test]
    async fn attach_answers_at_once_when_the_daemon_closes() {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client.initialize().await;
        client.send(load_s1(json!(6))).await;

        let mut closed = daemons.recv().await.expect("the attach's connection");
        let subscribe = closed.recv_type("subscribe").await;
        closed.emit(json!({"type":"ack","id":subscribe["id"]})).await;
        drop(closed);

        let (_, response) = tokio::time::timeout(Duration::from_secs(2), client.recv_response(json!(6)))
            .await
            .expect("the close is answered");
        assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
        let message = response["error"]["message"].as_str().expect("message");
        assert!(message.contains("daemon disconnected"), "{message}");
        assert!(daemons.try_recv().is_err(), "no second connection");
        client.close_stdin().await;
    }

    /// A daemon that acknowledges and then hangs is answered at the safety
    /// bound, with that reason, and not retried.
    #[tokio::test]
    async fn attach_answers_a_hung_daemon_at_the_safety_bound() {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_bounded(
            Some(connector),
            AttachBounds {
                reply: Duration::from_millis(50),
            },
        );
        client.initialize().await;
        client.send(load_s1(json!(6))).await;

        let mut hung = daemons.recv().await.expect("the attach's connection");
        let subscribe = hung.recv_type("subscribe").await;
        hung.emit(json!({"type":"ack","id":subscribe["id"]})).await;

        let (_, response) = tokio::time::timeout(Duration::from_secs(2), client.recv_response(json!(6)))
            .await
            .expect("the hung daemon is answered");
        assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("s1") && message.contains("did not answer within"),
            "{message}"
        );
        assert!(daemons.try_recv().is_err(), "no second connection");
        drop(hung);
        client.close_stdin().await;
    }

    /// The tail a cancelled turn still owes can never arrive once the daemon
    /// closed the connection: a prompt already waiting on it stops waiting at
    /// the close instead of spending [`TURN_TAIL_WAIT`].
    #[tokio::test]
    async fn daemon_close_ends_a_cancelled_turns_tail_wait() {
        let (session, daemon) = daemon_pair("s1");
        session.foreground(Some(20)).drain_cancelled_turn(20);
        session.mark_cancelled_turn();
        let settle = session.settle_cancelled_turn();
        tokio::pin!(settle);
        // One poll registers the wait for the tail before the close.
        assert!(
            tokio::time::timeout(Duration::ZERO, &mut settle).await.is_err(),
            "the drain is armed, so the prompt waits for the tail"
        );
        let closed_at = std::time::Instant::now();
        drop(daemon);
        tokio::time::timeout(TURN_TAIL_WAIT, &mut settle)
            .await
            .expect("the close ends the tail wait");
        assert!(
            closed_at.elapsed() < TURN_TAIL_WAIT,
            "waited {:?} after the close",
            closed_at.elapsed()
        );
    }

    #[tokio::test]
    async fn close_waits_for_inflight_load_then_session_is_gone() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client
            .send(request(
                json!(1),
                "session/load",
                json!({"sessionId": "s1", "cwd": "/tmp", "mcpServers": []}),
            ))
            .await;
        client
            .send(request(json!(2), "session/close", json!({"sessionId": "s1"})))
            .await;
        let get_history = daemon.recv_type("get_history").await;
        daemon.emit(history(&get_history["id"], "s1")).await;
        refuse_config_requests(&mut daemon).await;
        let (_, load) = client.recv_response(json!(1)).await;
        assert!(
            load["result"].is_object(),
            "the load finishes before the close runs: {load}"
        );
        let (_, close) = client.recv_response(json!(2)).await;
        assert_eq!(close["result"], json!({}), "{close}");
        assert!(!client.runtime.sessions.lock().await.contains_key("s1"));

        client
            .send(request(json!(3), "session/close", json!({"sessionId": "s1"})))
            .await;
        let (_, again) = client.recv_response(json!(3)).await;
        assert_session_not_found(&again, "s1");
        client.send(prompt(json!(4), "s1")).await;
        let (_, prompt_after_close) = client.recv_response(json!(4)).await;
        assert_session_not_found(&prompt_after_close, "s1");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn session_fifos_retire_once_their_session_is_gone() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        assert_eq!(client.runtime.session_queue_len(), 0, "no session-named request yet");
        client
            .send(request(
                json!(1),
                "session/load",
                json!({"sessionId": "s1", "cwd": "/tmp", "mcpServers": []}),
            ))
            .await;
        let get_history = daemon.recv_type("get_history").await;
        assert_eq!(client.runtime.session_queue_len(), 1, "the load runs on s1's FIFO");
        daemon.emit(history(&get_history["id"], "s1")).await;
        refuse_config_requests(&mut daemon).await;
        let (_, load) = client.recv_response(json!(1)).await;
        assert!(load["result"].is_object(), "{load}");
        assert_eq!(client.runtime.session_queue_len(), 1, "a live session keeps its FIFO");
        client
            .send(request(json!(2), "session/close", json!({"sessionId": "s1"})))
            .await;
        let (_, close) = client.recv_response(json!(2)).await;
        assert_eq!(close["result"], json!({}), "{close}");
        within("s1's FIFO to retire after the close", async {
            while client.runtime.session_queue_len() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        client
            .send(request(json!(3), "session/close", json!({"sessionId": "nope"})))
            .await;
        let (_, missing) = client.recv_response(json!(3)).await;
        assert_session_not_found(&missing, "nope");
        within("the unknown id's FIFO to retire", async {
            while client.runtime.session_queue_len() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn session_list_bad_cursor_invalid_params() {
        let mut client = start_client();
        client.initialize().await;
        for (id, params) in [
            (1, json!({"cursor": "!!!!"})),
            (2, json!({"cursor": "eDox"})),
            (3, json!({"cursor": 3})),
            (4, json!({"cwd": 3})),
        ] {
            client.send(request(json!(id), "session/list", params)).await;
            let (_, response) = client.recv_response(json!(id)).await;
            assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn close_answers_after_the_prompt_cancelled_response() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(1), "s1")).await;
        let message = daemon.recv_type("message").await;
        client
            .send(request(json!(2), "session/close", json!({"sessionId": "s1"})))
            .await;
        daemon.recv_type("cancel").await;
        let early = client.barrier("close-pending").await;
        assert!(
            early.iter().all(|line| line.get("id").is_none()),
            "neither the prompt nor the close has answered yet: {early:?}"
        );
        daemon.emit(json!({"type":"done","id":message["id"]})).await;
        let (before, prompt_response) = client.recv_response(json!(1)).await;
        assert_eq!(
            prompt_response["result"]["stopReason"], "cancelled",
            "{prompt_response}"
        );
        assert!(before.iter().all(|line| line.get("id").is_none()), "{before:?}");
        let (_, close) = client.recv_response(json!(2)).await;
        assert_eq!(close["result"], json!({}), "{close}");
        assert!(!client.runtime.sessions.lock().await.contains_key("s1"));
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn error_after_cancel_is_cancelled() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let session_cancel = json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}});
        let cases = [
            (session_cancel.clone(), false),
            (
                json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":71}}),
                false,
            ),
            (session_cancel, true),
        ];
        for (index, (cancel, failure_first)) in cases.into_iter().enumerate() {
            let id = json!(70 + index);
            client.send(prompt(id.clone(), "s1")).await;
            let message = daemon.recv_type("message").await;
            client.send(cancel).await;
            daemon.recv_type("cancel").await;
            if failure_first {
                daemon
                    .emit(json!({"type":"turn_stopped","reason":"failure","message":"boom"}))
                    .await;
            }
            daemon
                .emit(json!({"type":"error","id":message["id"],"message":"aborted by cancel"}))
                .await;
            let (_, response) = client.recv_response(id).await;
            assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
            assert!(response.get("error").is_none(), "{response}");
        }
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn audio_block_invalid_params() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        for (id, block) in [
            (80, json!({"type":"audio","mimeType":"audio/wav","data":"AAAA"})),
            (81, json!({"type":"hologram","data":"x"})),
            (82, json!({"type":"text"})),
        ] {
            let message = json!({"jsonrpc":"2.0","id":id,"method":"session/prompt",
                "params":{"sessionId":"s1","prompt":[{"type":"text","text":"hi"}, block]}});
            client.send(message).await;
            let (_, response) = client.recv_response(json!(id)).await;
            assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
        }
        client.send(prompt(json!(83), "s1")).await;
        let first = daemon.recv().await;
        assert_eq!(
            first["type"], "message",
            "a rejected prompt never reaches the daemon: {first}"
        );
        assert_eq!(first["content"], "hi");
        daemon.emit(json!({"type":"done","id":first["id"]})).await;
        let (_, response) = client.recv_response(json!(83)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn prompt_response_has_no_root_usage() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client.send(prompt(json!(90), "s1")).await;
        let message = daemon.recv_type("message").await;
        daemon
            .emit(json!({"type":"tokens","input":100,"output":7,"cache_read_input":20}))
            .await;
        daemon.emit(json!({"type":"text_delta","text":"hello"})).await;
        daemon.emit(json!({"type":"done","id":message["id"]})).await;
        let (before, response) = client.recv_response(json!(90)).await;
        let result = response["result"].as_object().expect("a result object");
        let mut keys: Vec<&str> = result.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["_meta", "stopReason"], "{response}");
        assert_eq!(result["stopReason"], "end_turn");
        assert_eq!(
            result["_meta"]["horon-agent"]["usage"],
            json!({"totalTokens": 127, "inputTokens": 100, "outputTokens": 7, "cachedReadTokens": 20})
        );
        let kinds: Vec<&str> = before
            .iter()
            .filter_map(|line| line["params"]["update"]["sessionUpdate"].as_str())
            .collect();
        assert_eq!(kinds, ["usage_update", "agent_message_chunk"], "{before:?}");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn close_during_prompt_resolves_cancelled_before_drop() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_fake_session(&client, "s1").await;
        let held = Arc::downgrade(&session);
        drop(session);
        client.send(prompt(json!(1), "s1")).await;
        let message = daemon.recv_type("message").await;
        client
            .send(request(json!(2), "session/close", json!({"sessionId": "s1"})))
            .await;
        daemon.recv_type("cancel").await;
        assert!(
            held.upgrade().is_some(),
            "the daemon connection outlives the unanswered prompt"
        );
        daemon.emit(json!({"type":"text_delta","text":"last words"})).await;
        daemon
            .emit(json!({"type":"error","id":message["id"],"message":"cancelled mid-stream"}))
            .await;
        let (before, prompt_response) = client.recv_response(json!(1)).await;
        assert_eq!(
            prompt_response["result"]["stopReason"], "cancelled",
            "{prompt_response}"
        );
        assert!(
            before
                .iter()
                .any(|line| line["params"]["update"]["content"]["text"] == "last words"),
            "updates of the turn precede its response: {before:?}"
        );
        let (between, close) = client.recv_response(json!(2)).await;
        assert_eq!(close["result"], json!({}), "{close}");
        assert!(
            between.iter().all(|line| line["method"] != "session/update"),
            "no update after the prompt response: {between:?}"
        );
        loop {
            match within("the daemon connection to close", daemon.lines.next_line())
                .await
                .unwrap()
            {
                None => break,
                Some(line) => assert!(!line.contains("\"message\""), "{line}"),
            }
        }
        assert!(held.upgrade().is_none(), "close dropped the daemon session");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn prompt_streams_turn_updates_before_true_stop_reason() {
        let mut client = start_client();
        client
            .send(
                json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,
                "clientCapabilities":{"session":{"notices":{}}}}}),
            )
            .await;
        client.recv().await;
        let (session, mut daemon) = attach_fake_session(&client, "s1").await;
        *session.cwd.lock().await = Some(PathBuf::from("/work"));
        client.send(prompt(json!(3), "s1")).await;
        let message = daemon.recv_type("message").await;
        for event in [
            json!({"type":"reasoning_delta","text":"plan it"}),
            json!({"type":"plan_updated","entries":[{"content":"edit a.rs","priority":"high","status":"in_progress"}]}),
            json!({"type":"tool_start","id":"c1","name":"edit"}),
            json!({"type":"tool_input","delta":"{\"file_path\":\"a.rs\"}"}),
            json!({"type":"tool_exec","id":"c1","name":"edit"}),
            json!({"type":"tool_diff","id":"c1","diffs":[{"path":"/work/a.rs","old_text":"x","new_text":"y"}]}),
            json!({"type":"tool_done","id":"c1","name":"edit","output":"edited"}),
            json!({"type":"text_delta","text":"done"}),
            json!({"type":"turn_stopped","reason":"limit_reached","message":"Output limit","provider_stop_reason":"max_tokens"}),
            json!({"type":"done","id":message["id"]}),
        ] {
            daemon.emit(event).await;
        }
        let (before, response) = client.recv_response(json!(3)).await;
        assert_eq!(response["result"]["stopReason"], "max_tokens", "{response}");
        let updates: Vec<&Value> = before.iter().map(|line| &line["params"]["update"]).collect();
        let kinds: Vec<&str> = updates
            .iter()
            .map(|update| update["sessionUpdate"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "agent_thought_chunk",
                "plan",
                "tool_call",
                "tool_call_update",
                "tool_call_update",
                "tool_call_update",
                "tool_call_update",
                "agent_message_chunk",
                "notice",
            ],
            "{before:?}"
        );
        assert_eq!(updates[3]["locations"], json!([{"path": "/work/a.rs"}]));
        assert_eq!(updates[5]["content"][0]["type"], "diff");
        assert_eq!(updates[6]["status"], "completed");
        assert_eq!(updates[6]["content"][0]["type"], "diff");
        assert_eq!(updates[6]["content"][1]["content"]["text"], "edited");
        assert_ne!(updates[0]["messageId"], updates[7]["messageId"]);
        assert_eq!(updates[8]["severity"], "warning");
        daemon.emit(json!({"type":"text_delta","text":"stray"})).await;
        daemon.emit(json!({"type":"plan_updated","entries":[]})).await;
        let after = client.barrier("after-turn").await;
        assert!(after.is_empty(), "frames after the response: {after:?}");
        client.close_stdin().await;
    }

    pub(super) async fn attach_bridged_session(client: &Client, session_id: &str) -> (Arc<DaemonSession>, FakeDaemon) {
        let (session, daemon) = attach_fake_session(client, session_id).await;
        client.runtime.install_session_bridge(&session);
        (session, daemon)
    }

    pub(super) fn permission_requested(request_id: &str, tool_name: &str) -> Value {
        json!({"type":"tool_permission_requested","request_id":request_id,"tool_call_id":format!("call-{request_id}"),
            "tool_name":tool_name,"title":format!("{tool_name}: approval required"),"input":{"command":"echo hi"}})
    }

    fn selected(id: &Value, option_id: &str) -> Value {
        json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"selected","optionId":option_id}}})
    }

    /// The next frame must be a session/request_permission for `request_id`.
    async fn expect_permission_request(client: &mut Client, request_id: &str, tool_name: &str) -> Value {
        let request = client.recv().await;
        assert_eq!(request["method"], "session/request_permission", "{request}");
        let params = &request["params"];
        assert_eq!(params["sessionId"], "s1");
        assert_eq!(params["toolCall"]["toolCallId"], format!("call-{request_id}"));
        assert_eq!(params["toolCall"]["kind"], tool_kind(tool_name));
        assert_eq!(params["toolCall"]["status"], "pending");
        assert_eq!(params["toolCall"]["rawInput"]["command"], "echo hi");
        let option_ids: Vec<&str> = params["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option["optionId"].as_str().unwrap())
            .collect();
        assert_eq!(
            option_ids,
            [
                OPTION_ALLOW_ONCE,
                OPTION_ALLOW_ALWAYS,
                OPTION_REJECT_ONCE,
                OPTION_REJECT_ALWAYS
            ]
        );
        request["id"].clone()
    }

    async fn expect_decision(daemon: &mut FakeDaemon, request_id: &str, decision: &str) {
        let answer = daemon.recv_type("permission_decision").await;
        assert_eq!(answer["request_id"], request_id, "{answer}");
        assert_eq!(answer["decision"], decision, "{answer}");
        daemon.emit(json!({"type":"ack","id":answer["id"]})).await;
    }

    #[tokio::test]
    async fn permission_allow_once_maps_allow() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon.emit(permission_requested("p1", "bash")).await;
        let call = expect_permission_request(&mut client, "p1", "bash").await;
        client.send(selected(&call, OPTION_ALLOW_ONCE)).await;
        expect_decision(&mut daemon, "p1", "allow").await;

        daemon.emit(permission_requested("p2", "bash")).await;
        let call = expect_permission_request(&mut client, "p2", "bash").await;
        client.send(selected(&call, OPTION_REJECT_ONCE)).await;
        expect_decision(&mut daemon, "p2", "deny").await;

        daemon.emit(permission_requested("p3", "bash")).await;
        let call = expect_permission_request(&mut client, "p3", "bash").await;
        client.send(selected(&call, OPTION_ALLOW_ONCE)).await;
        expect_decision(&mut daemon, "p3", "allow").await;
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn permission_always_remembered_for_session() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon.emit(permission_requested("p1", "bash")).await;
        let call = expect_permission_request(&mut client, "p1", "bash").await;
        client.send(selected(&call, OPTION_ALLOW_ALWAYS)).await;
        expect_decision(&mut daemon, "p1", "allow").await;

        daemon.emit(permission_requested("p2", "write")).await;
        let call = expect_permission_request(&mut client, "p2", "write").await;
        client.send(selected(&call, OPTION_REJECT_ALWAYS)).await;
        expect_decision(&mut daemon, "p2", "deny").await;

        daemon.emit(permission_requested("p3", "bash")).await;
        expect_decision(&mut daemon, "p3", "allow").await;
        daemon.emit(permission_requested("p4", "write")).await;
        expect_decision(&mut daemon, "p4", "deny").await;
        assert!(
            client.barrier("remembered").await.is_empty(),
            "a remembered answer never asks the client again"
        );

        daemon.emit(permission_requested("p5", "webfetch")).await;
        let call = expect_permission_request(&mut client, "p5", "webfetch").await;
        client.send(selected(&call, OPTION_ALLOW_ONCE)).await;
        expect_decision(&mut daemon, "p5", "allow").await;
        assert_eq!(session.permissions.remembered("webfetch"), None);

        let (_other, mut other_daemon) = attach_bridged_session(&client, "s2").await;
        other_daemon.emit(permission_requested("q1", "bash")).await;
        let request = client.recv().await;
        assert_eq!(request["method"], "session/request_permission");
        assert_eq!(
            request["params"]["sessionId"], "s2",
            "remembered answers are per session"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn permission_cancelled_maps_deny() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon.emit(permission_requested("p1", "bash")).await;
        let call = expect_permission_request(&mut client, "p1", "bash").await;
        client
            .send(json!({"jsonrpc":"2.0","id":call,"result":{"outcome":{"outcome":"cancelled"}}}))
            .await;
        expect_decision(&mut daemon, "p1", "deny").await;
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn permission_unknown_option_denies() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        let answers = [
            selected(&json!(null), "allow"),
            selected(&json!(null), "ALLOW-ONCE"),
            json!({"jsonrpc":"2.0","id":null,"result":{"outcome":"selected","optionId":OPTION_ALLOW_ONCE}}),
            json!({"jsonrpc":"2.0","id":null,"result":{}}),
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"client failed"}}),
        ];
        for (index, mut answer) in answers.into_iter().enumerate() {
            let request_id = format!("p{index}");
            daemon.emit(permission_requested(&request_id, "bash")).await;
            let call = expect_permission_request(&mut client, &request_id, "bash").await;
            answer["id"] = call;
            client.send(answer).await;
            expect_decision(&mut daemon, &request_id, "deny").await;
        }
        assert_eq!(session.permissions.remembered("bash"), None);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn permission_pending_cancel_resolves_cancelled() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        client.send(prompt(json!(1), "s1")).await;
        let message = daemon.recv_type("message").await;
        daemon.emit(permission_requested("p1", "bash")).await;
        let call = expect_permission_request(&mut client, "p1", "bash").await;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        let withdrawn = client.recv().await;
        assert_eq!(withdrawn["method"], "$/cancel_request", "{withdrawn}");
        assert_eq!(withdrawn["params"]["requestId"], call);
        let first = daemon.recv().await;
        let second = daemon.recv().await;
        let mut kinds = [first["type"].as_str().unwrap(), second["type"].as_str().unwrap()];
        kinds.sort_unstable();
        assert_eq!(kinds, ["cancel", "permission_decision"], "{first} {second}");
        let decision = if first["type"] == "permission_decision" {
            &first
        } else {
            &second
        };
        assert_eq!(decision["request_id"], "p1");
        assert_eq!(decision["decision"], "deny");
        daemon
            .emit(json!({"type":"tool_permission_resolved","request_id":"p1","approved":false}))
            .await;
        daemon.emit(json!({"type":"done","id":message["id"]})).await;
        let (_, response) = client.recv_response(json!(1)).await;
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
        client.send(selected(&call, OPTION_ALLOW_ONCE)).await;
        assert!(
            client.barrier("late-answer").await.is_empty(),
            "a late answer to a cancelled permission request changes nothing"
        );
        assert_eq!(session.permissions.open_len(), 0);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn permission_resolved_elsewhere_withdraws_prompt() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon.emit(permission_requested("p1", "bash")).await;
        let call = expect_permission_request(&mut client, "p1", "bash").await;
        daemon
            .emit(json!({"type":"tool_permission_resolved","request_id":"p1","approved":true}))
            .await;
        let withdrawn = client.recv().await;
        assert_eq!(withdrawn["method"], "$/cancel_request", "{withdrawn}");
        assert_eq!(withdrawn["params"]["requestId"], call);
        client.send(selected(&call, OPTION_REJECT_ALWAYS)).await;

        daemon
            .emit(json!({"type":"tool_permission_resolved","request_id":"p2","approved":false}))
            .await;
        daemon.emit(permission_requested("p2", "bash")).await;
        daemon.emit(permission_requested("p3", "bash")).await;
        let call = expect_permission_request(&mut client, "p3", "bash").await;
        client.send(selected(&call, OPTION_ALLOW_ONCE)).await;
        expect_decision(&mut daemon, "p3", "allow").await;
        assert_eq!(
            session.permissions.remembered("bash"),
            None,
            "a withdrawn prompt's late answer is not remembered"
        );
        assert_eq!(session.permissions.open_len(), 0);
        client.close_stdin().await;
    }

    fn set_mode_request(id: Value, session_id: &str, mode_id: Value) -> Value {
        request(
            id,
            "session/set_mode",
            json!({"sessionId": session_id, "modeId": mode_id}),
        )
    }

    fn set_option(id: Value, params: Value) -> Value {
        request(id, "session/set_config_option", params)
    }

    #[tokio::test]
    async fn set_mode_maps_to_approval_override() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        session.prompt_running.store(true, Ordering::SeqCst);
        for (index, (mode_id, approval)) in [
            ("ask-commands", json!("write")),
            ("auto", json!("off")),
            ("default", Value::Null),
        ]
        .into_iter()
        .enumerate()
        {
            let id = json!(10 + index);
            client.send(set_mode_request(id.clone(), "s1", json!(mode_id))).await;
            let applied = daemon.recv_type("set_session_approval_mode").await;
            assert_eq!(
                applied.get("mode").cloned().unwrap_or(Value::Null),
                approval,
                "{applied}"
            );
            daemon.emit(json!({"type":"ack","id":applied["id"]})).await;
            let (_, response) = client.recv_response(id).await;
            assert_eq!(response["result"], json!({}), "{response}");
            let mode_update = client.recv().await;
            assert_eq!(mode_update["params"]["update"]["sessionUpdate"], "current_mode_update");
            assert_eq!(mode_update["params"]["update"]["currentModeId"], mode_id);
            let options_update = client.recv().await;
            assert_eq!(
                options_update["params"]["update"]["sessionUpdate"],
                "config_option_update"
            );
            assert_eq!(
                options_update["params"]["update"]["configOptions"][0]["currentValue"],
                mode_id
            );
            assert_eq!(session.mode.lock().await.id(), mode_id);
        }

        client
            .send(set_option(
                json!(20),
                json!({"sessionId":"s1","configId":"mode","value":"auto"}),
            ))
            .await;
        let applied = daemon.recv_type("set_session_approval_mode").await;
        assert_eq!(applied["mode"], "off");
        daemon.emit(json!({"type":"ack","id":applied["id"]})).await;
        let (_, response) = client.recv_response(json!(20)).await;
        assert_eq!(response["result"]["configOptions"][0]["id"], "mode", "{response}");
        assert_eq!(response["result"]["configOptions"][0]["currentValue"], "auto");
        assert_eq!(
            client.recv().await["params"]["update"]["sessionUpdate"],
            "current_mode_update"
        );
        assert_eq!(
            client.recv().await["params"]["update"]["sessionUpdate"],
            "config_option_update"
        );

        client
            .send(set_mode_request(json!(21), "s1", json!("ask-commands")))
            .await;
        let refused = daemon.recv_type("set_session_approval_mode").await;
        daemon
            .emit(json!({"type":"error","id":refused["id"],"message":"approval mode refused"}))
            .await;
        let (_, response) = client.recv_response(json!(21)).await;
        assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
        assert_eq!(
            *session.mode.lock().await,
            AcpMode::Auto,
            "a refused switch keeps the mode"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn set_mode_unknown_invalid_params() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        for (id, mode_id) in [
            (1, json!("plan")),
            (2, json!("Auto")),
            (3, json!("")),
            (4, json!(1)),
            (5, Value::Null),
        ] {
            client.send(set_mode_request(json!(id), "s1", mode_id)).await;
            let (_, response) = client.recv_response(json!(id)).await;
            assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
        }
        client.send(set_mode_request(json!(6), "missing", json!("auto"))).await;
        let (_, response) = client.recv_response(json!(6)).await;
        assert_session_not_found(&response, "missing");
        client
            .send(set_option(
                json!(7),
                json!({"sessionId":"s1","configId":"mode","value":"plan"}),
            ))
            .await;
        let (_, response) = client.recv_response(json!(7)).await;
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
        assert_eq!(*session.mode.lock().await, AcpMode::Default);

        client.send(set_mode_request(json!(8), "s1", json!("auto"))).await;
        let first = daemon.recv().await;
        assert_eq!(
            first["type"], "set_session_approval_mode",
            "refused ids never reach the daemon: {first}"
        );
        assert_eq!(first["mode"], "off");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn set_config_option_boolean_rejected() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        *session.ui_state.lock().await = SessionUiState {
            provider_name: Some("openai".to_string()),
            model: Some("gpt-5.2".to_string()),
            available_models: vec!["gpt-5.2".to_string(), "gpt-5.2-codex".to_string()],
            reasoning_effort: None,
        };
        let refused = [
            json!({"sessionId":"s1","configId":"mode","type":"boolean","value":true}),
            json!({"sessionId":"s1","configId":"model","type":"boolean","value":false}),
            json!({"sessionId":"s1","configId":"mode","value":false}),
            json!({"sessionId":"s1","configId":"temperature","value":"1"}),
            json!({"sessionId":"s1","configId":"model","value":"not-offered"}),
            json!({"sessionId":"s1","value":"auto"}),
        ];
        for (index, params) in refused.into_iter().enumerate() {
            let id = json!(30 + index);
            client.send(set_option(id.clone(), params.clone())).await;
            let (_, response) = client.recv_response(id).await;
            assert_eq!(
                response["error"]["code"], JSONRPC_INVALID_PARAMS,
                "{params} -> {response}"
            );
        }

        session.prompt_running.store(true, Ordering::SeqCst);
        client
            .send(set_option(
                json!(40),
                json!({"sessionId":"s1","configId":"model","value":"gpt-5.2-codex"}),
            ))
            .await;
        let (_, response) = client.recv_response(json!(40)).await;
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("while a prompt is running"),
            "{response}"
        );
        session.prompt_running.store(false, Ordering::SeqCst);

        client
            .send(set_option(
                json!(41),
                json!({"sessionId":"s1","configId":"model","value":"gpt-5.2-codex"}),
            ))
            .await;
        let set_model = daemon.recv().await;
        assert_eq!(
            set_model["type"], "set_model",
            "refused requests never reach the daemon: {set_model}"
        );
        daemon
            .emit(json!({"type":"model_changed","id":set_model["id"],"model":"gpt-5.2-codex","provider_name":"openai"}))
            .await;
        let (_, response) = client.recv_response(json!(41)).await;
        let options = response["result"]["configOptions"].as_array().unwrap();
        assert_eq!(options[0]["id"], "mode", "{response}");
        assert_eq!(options[1]["currentValue"], "gpt-5.2-codex", "{response}");
        let update = client.recv().await;
        assert_eq!(update["params"]["update"]["sessionUpdate"], "config_option_update");
        assert_eq!(update["params"]["update"]["configOptions"], json!(options));
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn model_change_outside_a_request_updates_config_options() {
        let mut client = start_client();
        client.initialize().await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        *session.ui_state.lock().await = SessionUiState {
            provider_name: Some("openai".to_string()),
            model: Some("gpt-5.2".to_string()),
            available_models: vec!["gpt-5.2".to_string(), "gpt-5.2-codex".to_string()],
            reasoning_effort: None,
        };
        daemon
            .emit(json!({"type":"model_changed","id":999,"model":"gpt-5.2-codex","provider_name":"openai"}))
            .await;
        let update = client.recv().await;
        assert_eq!(
            update["params"]["update"]["sessionUpdate"], "config_option_update",
            "{update}"
        );
        assert_eq!(update["params"]["update"]["configOptions"][0]["id"], "mode");
        assert_eq!(
            update["params"]["update"]["configOptions"][1]["currentValue"],
            "gpt-5.2-codex"
        );
        client.close_stdin().await;
    }

    const ALL_CLIENT_CAPABILITIES: &str =
        r#"{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true,"elicitation":{"form":{}}}"#;

    async fn initialize_with(client: &mut Client, capabilities: &str) {
        let capabilities: Value = serde_json::from_str(capabilities).unwrap();
        client
            .send(json!({"jsonrpc":"2.0","id":0,"method":"initialize",
                "params":{"protocolVersion":1,"clientCapabilities":capabilities}}))
            .await;
        let response = client.recv().await;
        assert_eq!(response["result"]["protocolVersion"], 1, "{response}");
    }

    fn client_request(request_id: &str, tool_call_id: Option<&str>, method: &str, params: Value) -> Value {
        json!({"type":"client_request","request_id":request_id,"tool_call_id":tool_call_id,
            "method":method,"params":params})
    }

    /// The next `client_response` the daemon sees, acknowledged.
    async fn expect_client_response(daemon: &mut FakeDaemon, request_id: &str) -> Value {
        let response = daemon.recv_type("client_response").await;
        assert_eq!(response["request_id"], request_id, "{response}");
        daemon.emit(json!({"type":"ack","id":response["id"]})).await;
        response
    }

    fn answered(id: &Value, result: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"result":result})
    }

    #[tokio::test]
    async fn client_request_without_capability_answers_32601() {
        let mut client = start_client();
        initialize_with(
            &mut client,
            r#"{"fs":{"readTextFile":true,"writeTextFile":false},"terminal":false}"#,
        )
        .await;
        let (_session, mut daemon) = attach_bridged_session(&client, "s1").await;
        let methods = [
            "fs/write_text_file",
            "terminal/create",
            "terminal/output",
            "terminal/wait_for_exit",
            "terminal/kill",
            "terminal/release",
            "elicitation/create",
            "fs/delete_file",
        ];
        for (index, method) in methods.iter().enumerate() {
            let request_id = format!("cr-{index}");
            daemon
                .emit(client_request(
                    &request_id,
                    Some("call-1"),
                    method,
                    json!({"path":"/w/a"}),
                ))
                .await;
            let response = expect_client_response(&mut daemon, &request_id).await;
            assert_eq!(
                response["error"]["code"], JSONRPC_METHOD_NOT_FOUND,
                "{method}: {response}"
            );
            assert!(response["result"].is_null(), "{response}");
        }
        assert!(
            client.barrier("ungated").await.is_empty(),
            "a method the client did not advertise is never sent to it"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn client_request_forwards_session_id_and_result() {
        let mut client = start_client();
        initialize_with(&mut client, ALL_CLIENT_CAPABILITIES).await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon
            .emit(client_request(
                "cr-1",
                Some("call-1"),
                "fs/read_text_file",
                json!({"path":"/w/a.txt","line":2}),
            ))
            .await;
        let request = client.recv().await;
        assert_eq!(request["method"], "fs/read_text_file", "{request}");
        assert_eq!(request["params"], json!({"path":"/w/a.txt","line":2,"sessionId":"s1"}));
        client
            .send(answered(&request["id"], json!({"content":"overlay"})))
            .await;
        let response = expect_client_response(&mut daemon, "cr-1").await;
        assert_eq!(response["result"], json!({"content":"overlay"}), "{response}");
        assert!(response["error"].is_null(), "{response}");

        daemon
            .emit(client_request(
                "cr-2",
                Some("call-2"),
                "fs/write_text_file",
                json!({"path":"/w/b.txt","content":"x"}),
            ))
            .await;
        let request = client.recv().await;
        assert_eq!(request["method"], "fs/write_text_file", "{request}");
        assert_eq!(request["params"]["sessionId"], "s1");
        client
            .send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32002,"message":"no such file"}}))
            .await;
        let response = expect_client_response(&mut daemon, "cr-2").await;
        assert_eq!(
            response["error"],
            json!({"code":-32002,"message":"no such file"}),
            "{response}"
        );
        assert_eq!(session.client_calls.open_len(), 0);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn terminal_create_embeds_terminal_content() {
        let mut client = start_client();
        initialize_with(&mut client, r#"{"terminal":true}"#).await;
        let (_session, mut daemon) = attach_bridged_session(&client, "s1").await;
        client.send(prompt(json!(1), "s1")).await;
        let message = daemon.recv_type("message").await;
        daemon
            .emit(json!({"type":"tool_start","id":"call-1","name":"bash"}))
            .await;
        daemon
            .emit(client_request(
                "cr-1",
                Some("call-1"),
                "terminal/create",
                json!({"command":"bash","args":["-c","echo hi"]}),
            ))
            .await;
        let mut frames = Vec::new();
        let create = loop {
            let frame = client.recv().await;
            if frame["method"] == "terminal/create" {
                break frame;
            }
            frames.push(frame);
        };
        assert_eq!(create["params"]["sessionId"], "s1");
        client
            .send(answered(&create["id"], json!({"terminalId":"term-1"})))
            .await;
        let response = expect_client_response(&mut daemon, "cr-1").await;
        assert_eq!(response["result"]["terminalId"], "term-1", "{response}");
        // The update was written before the daemon heard the result, so it is
        // already on stdout when the barrier is answered.
        frames.extend(client.barrier("after-create").await);
        let updates: Vec<&Value> = frames
            .iter()
            .filter(|frame| frame["method"] == "session/update")
            .map(|frame| &frame["params"]["update"])
            .collect();
        assert_eq!(updates.len(), 2, "{frames:?}");
        assert_eq!(updates[0]["sessionUpdate"], "tool_call", "the call is announced first");
        assert_eq!(
            updates[1],
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"call-1",
                "content":[{"type":"terminal","terminalId":"term-1"}]})
        );

        daemon
            .emit(json!({"type":"tool_done","id":"call-1","name":"bash","output":"hi\n"}))
            .await;
        daemon.emit(json!({"type":"done","id":message["id"]})).await;
        let (before, response) = client.recv_response(json!(1)).await;
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        let completed = before
            .iter()
            .map(|frame| &frame["params"]["update"])
            .find(|update| update["status"] == "completed")
            .expect("the completed tool call");
        assert_eq!(
            completed["content"][0],
            json!({"type":"terminal","terminalId":"term-1"}),
            "the result keeps the terminal: {completed}"
        );
        assert_eq!(completed["content"][1]["content"]["text"], "hi\n");
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn terminal_create_without_prompt_forwards_without_update() {
        let mut client = start_client();
        initialize_with(&mut client, r#"{"terminal":true}"#).await;
        let (_session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon
            .emit(client_request(
                "cr-1",
                Some("call-1"),
                "terminal/create",
                json!({"command":"bash"}),
            ))
            .await;
        let create = client.recv().await;
        assert_eq!(create["method"], "terminal/create", "{create}");
        client
            .send(answered(&create["id"], json!({"terminalId":"term-1"})))
            .await;
        let response = expect_client_response(&mut daemon, "cr-1").await;
        assert_eq!(response["result"]["terminalId"], "term-1");
        assert!(
            client.barrier("no-prompt").await.is_empty(),
            "no turn is running, so no update is written"
        );
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn cancel_cancels_outstanding_client_calls() {
        let mut client = start_client();
        initialize_with(&mut client, ALL_CLIENT_CAPABILITIES).await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        client.send(prompt(json!(1), "s1")).await;
        let message = daemon.recv_type("message").await;
        daemon
            .emit(client_request(
                "cr-read",
                Some("call-1"),
                "fs/read_text_file",
                json!({"path":"/w/a"}),
            ))
            .await;
        daemon
            .emit(client_request(
                "cr-wait",
                None,
                "terminal/wait_for_exit",
                json!({"terminalId":"term-1"}),
            ))
            .await;
        daemon
            .emit(client_request(
                "cr-release",
                None,
                "terminal/release",
                json!({"terminalId":"term-0"}),
            ))
            .await;
        let mut asked = HashMap::new();
        for _ in 0..3 {
            let request = client.recv().await;
            asked.insert(request["method"].as_str().unwrap().to_string(), request["id"].clone());
        }
        assert_eq!(session.client_calls.open_len(), 3);

        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
            .await;
        let mut withdrawn = Vec::new();
        for _ in 0..2 {
            let notification = client.recv().await;
            assert_eq!(notification["method"], "$/cancel_request", "{notification}");
            withdrawn.push(notification["params"]["requestId"].to_string());
        }
        withdrawn.sort();
        let mut expected = vec![
            asked["fs/read_text_file"].to_string(),
            asked["terminal/wait_for_exit"].to_string(),
        ];
        expected.sort();
        assert_eq!(withdrawn, expected, "terminal/release is cleanup and stays open");

        assert_eq!(
            daemon.recv().await["type"],
            "cancel",
            "the daemon hears the cancel first"
        );
        let mut cancelled = HashMap::new();
        for _ in 0..2 {
            let response = daemon.recv_type("client_response").await;
            daemon.emit(json!({"type":"ack","id":response["id"]})).await;
            cancelled.insert(response["request_id"].as_str().unwrap().to_string(), response);
        }
        for request_id in ["cr-read", "cr-wait"] {
            assert_eq!(
                cancelled[request_id]["error"]["code"], JSONRPC_REQUEST_CANCELLED,
                "{request_id}: {:?}",
                cancelled[request_id]
            );
        }

        client.send(answered(&asked["terminal/release"], json!({}))).await;
        let released = expect_client_response(&mut daemon, "cr-release").await;
        assert_eq!(released["result"], json!({}), "{released}");
        daemon.emit(json!({"type":"done","id":message["id"]})).await;
        let (_, response) = client.recv_response(json!(1)).await;
        assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
        client
            .send(answered(&asked["fs/read_text_file"], json!({"content":"late"})))
            .await;
        assert!(
            client.barrier("late-answer").await.is_empty(),
            "a late answer to a withdrawn call changes nothing"
        );
        assert_eq!(session.client_calls.open_len(), 0);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn close_cancels_outstanding_client_calls() {
        let mut client = start_client();
        initialize_with(&mut client, ALL_CLIENT_CAPABILITIES).await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon
            .emit(client_request(
                "cr-1",
                Some("call-1"),
                "fs/read_text_file",
                json!({"path":"/w/a"}),
            ))
            .await;
        let read = client.recv().await;
        assert_eq!(read["method"], "fs/read_text_file");
        client
            .send(request(json!(5), "session/close", json!({"sessionId":"s1"})))
            .await;
        let withdrawn = client.recv().await;
        assert_eq!(withdrawn["method"], "$/cancel_request", "{withdrawn}");
        assert_eq!(withdrawn["params"]["requestId"], read["id"]);
        let (_, closed) = client.recv_response(json!(5)).await;
        assert_eq!(closed["result"], json!({}), "{closed}");
        let response = expect_client_response(&mut daemon, "cr-1").await;
        assert_eq!(response["error"]["code"], JSONRPC_REQUEST_CANCELLED, "{response}");
        assert_eq!(session.client_calls.open_len(), 0);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn client_request_timeout_cancels_and_answers_32800() {
        let mut client = start_client();
        client.runtime.call_bounds = CallBounds {
            fs: Duration::from_millis(50),
            ..CallBounds::default()
        };
        initialize_with(&mut client, ALL_CLIENT_CAPABILITIES).await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon
            .emit(client_request(
                "cr-1",
                Some("call-1"),
                "fs/read_text_file",
                json!({"path":"/w/a"}),
            ))
            .await;
        let request = client.recv().await;
        assert_eq!(request["method"], "fs/read_text_file");
        let withdrawn = client.recv().await;
        assert_eq!(withdrawn["method"], "$/cancel_request", "{withdrawn}");
        assert_eq!(withdrawn["params"]["requestId"], request["id"]);
        let response = expect_client_response(&mut daemon, "cr-1").await;
        assert_eq!(response["error"]["code"], JSONRPC_REQUEST_CANCELLED, "{response}");
        assert!(
            response["error"]["message"].as_str().unwrap().contains("timed out"),
            "{response}"
        );
        client.send(answered(&request["id"], json!({"content":"late"}))).await;
        assert!(client.barrier("after-timeout").await.is_empty());
        assert_eq!(session.client_calls.open_len(), 0);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn client_request_malformed_result_is_internal_error() {
        let mut client = start_client();
        initialize_with(&mut client, ALL_CLIENT_CAPABILITIES).await;
        let (_session, mut daemon) = attach_bridged_session(&client, "s1").await;
        for (index, result) in [json!("oops"), Value::Null, json!([1])].into_iter().enumerate() {
            let request_id = format!("cr-{index}");
            daemon
                .emit(client_request(
                    &request_id,
                    None,
                    "terminal/output",
                    json!({"terminalId":"term-1"}),
                ))
                .await;
            let request = client.recv().await;
            assert_eq!(request["method"], "terminal/output");
            client.send(answered(&request["id"], result.clone())).await;
            let response = expect_client_response(&mut daemon, &request_id).await;
            assert_eq!(
                response["error"]["code"], JSONRPC_INTERNAL_ERROR,
                "{result}: {response}"
            );
            assert!(response["result"].is_null(), "{response}");
        }
        client.close_stdin().await;
    }

    fn ask_requested(request_id: &str) -> Value {
        json!({"type":"ask_requested","request_id":request_id,"questions":[
            {"id":"db","question":"Which database?","header":"Storage",
                "options":[{"label":"Postgres"},{"label":"SQLite","description":"local file"}],"recommended":1},
            {"id":"q2","question":"Which targets?",
                "options":[{"label":"linux"},{"label":"macos"},{"label":"windows"}],"multi":true,"recommended":0}
        ]})
    }

    #[tokio::test]
    async fn ask_maps_to_form_elicitation_schema() {
        let mut client = start_client();
        initialize_with(&mut client, r#"{"elicitation":{"form":{}}}"#).await;
        let (session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon.emit(ask_requested("ask-1")).await;
        let request = client.recv().await;
        assert_eq!(request["method"], "elicitation/create", "{request}");
        assert_eq!(
            request["params"],
            json!({
                "sessionId": "s1",
                "mode": "form",
                "message": "Storage: Which database?\nWhich targets?",
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "db": {"type":"string","title":"Storage","description":"Which database?",
                            "enum":["Postgres","SQLite"],"default":"SQLite"},
                        "q2": {"type":"array","description":"Which targets?",
                            "items":{"type":"string","enum":["linux","macos","windows"]},"default":["linux"]}
                    },
                    "required": ["db", "q2"]
                }
            })
        );
        client
            .send(answered(
                &request["id"],
                json!({"action":"accept","content":{"db":"Postgres","q2":["macos","windows"]}}),
            ))
            .await;
        let answer = daemon.recv_type("ask_response").await;
        assert_eq!(answer["request_id"], "ask-1");
        assert_eq!(
            answer["answers"],
            json!([{"question_id":"db","selected":["Postgres"]},
                {"question_id":"q2","selected":["macos","windows"]}])
        );
        assert!(answer.get("answer_id").is_none(), "{answer}");
        daemon.emit(json!({"type":"ack","id":answer["id"]})).await;
        assert_eq!(session.client_calls.open_len(), 0);
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn elicitation_decline_sends_empty_answers() {
        let mut client = start_client();
        initialize_with(&mut client, r#"{"elicitation":{"form":{}}}"#).await;
        let (_session, mut daemon) = attach_bridged_session(&client, "s1").await;
        let replies = [
            json!({"jsonrpc":"2.0","id":null,"result":{"action":"decline"}}),
            json!({"jsonrpc":"2.0","id":null,"result":{"action":"cancel"}}),
            json!({"jsonrpc":"2.0","id":null,"result":{"action":"accept"}}),
            json!({"jsonrpc":"2.0","id":null,"result":{"action":"_horon_other"}}),
            json!({"jsonrpc":"2.0","id":null,"result":"accept"}),
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"form crashed"}}),
        ];
        for (index, mut reply) in replies.into_iter().enumerate() {
            let request_id = format!("ask-{index}");
            daemon.emit(ask_requested(&request_id)).await;
            let request = client.recv().await;
            assert_eq!(request["method"], "elicitation/create", "{request}");
            reply["id"] = request["id"].clone();
            client.send(reply.clone()).await;
            let answer = daemon.recv_type("ask_response").await;
            assert_eq!(answer["request_id"], request_id.as_str());
            assert_eq!(answer["answers"], json!([]), "{reply}: {answer}");
            daemon.emit(json!({"type":"ack","id":answer["id"]})).await;
        }
        client.close_stdin().await;

        let mut client = start_client();
        initialize_with(&mut client, r#"{"elicitation":{}}"#).await;
        let (_session, mut daemon) = attach_bridged_session(&client, "s1").await;
        daemon.emit(ask_requested("ask-no-form")).await;
        let answer = daemon.recv_type("ask_response").await;
        assert_eq!(answer["answers"], json!([]), "{answer}");
        assert!(
            client.barrier("no-form").await.is_empty(),
            "a client without elicitation.form is never asked"
        );
        client.close_stdin().await;
    }

    /// A turn as the daemon renders it: prompt, reasoning plus text, a read
    /// call and the answer after it.
    fn rich_history(id: &Value, session_id: &str) -> Value {
        let thought = format!("{}\n", crate::session::reasoning_line_markup("checking the file"));
        json!({"type":"history","id":id,"session_id":session_id,"provider_model":"gpt-5",
        "messages":[
            {"role":"user","content":"read a.rs"},
            {"role":"assistant","content": format!("{thought}I will read it."),"tool_calls":["read"]},
            {"role":"tool","content":"fn main() {}","tool_data":{"id":"call_1","name":"read","input":{"file_path":"a.rs"}}},
            {"role":"assistant","content":"done"}
        ]})
    }

    /// History updates only: the commands update a previous answer wrote is
    /// not part of a replay.
    fn update_kinds(lines: &[Value]) -> Vec<String> {
        lines
            .iter()
            .filter(|line| line["method"] == "session/update")
            .map(|line| line["params"]["update"]["sessionUpdate"].as_str().unwrap().to_string())
            .filter(|kind| kind != "available_commands_update")
            .collect()
    }

    #[tokio::test]
    async fn resume_does_not_replay() {
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        let params = json!({"sessionId": "s1", "cwd": "/work", "mcpServers": []});
        client.send(request(json!(8), "session/resume", params.clone())).await;
        let get_history = daemon.recv_type("get_history").await;
        daemon.emit(rich_history(&get_history["id"], "s1")).await;
        refuse_config_requests(&mut daemon).await;
        let (before, response) = client.recv_response(json!(8)).await;
        assert!(response["result"].is_object(), "{response}");
        assert!(update_kinds(&before).is_empty(), "resume must not replay: {before:?}");

        // The same history does replay on load, all of it before the answer.
        client.send(request(json!(9), "session/load", params)).await;
        let get_history = daemon.recv_type("get_history").await;
        daemon.emit(rich_history(&get_history["id"], "s1")).await;
        refuse_config_requests(&mut daemon).await;
        let (before, response) = client.recv_response(json!(9)).await;
        assert!(response["result"].is_object(), "{response}");
        assert_eq!(
            update_kinds(&before),
            [
                "user_message_chunk",
                "agent_thought_chunk",
                "agent_message_chunk",
                "tool_call",
                "tool_call_update",
                "agent_message_chunk"
            ],
            "{before:?}"
        );
        let call = before
            .iter()
            .find(|line| line["params"]["update"]["sessionUpdate"] == "tool_call")
            .unwrap();
        assert_eq!(call["params"]["update"]["locations"], json!([{"path": "/work/a.rs"}]));
        client.close_stdin().await;
    }

    #[tokio::test]
    async fn load_replays_large_history_in_order_before_response() {
        const TURNS: usize = 400;
        let mut client = start_client();
        client.initialize().await;
        let (_session, mut daemon) = attach_fake_session(&client, "s1").await;
        client
            .send(request(
                json!(7),
                "session/load",
                json!({"sessionId": "s1", "cwd": "/work", "mcpServers": []}),
            ))
            .await;
        let get_history = daemon.recv_type("get_history").await;
        let messages: Vec<Value> = (0..TURNS)
            .flat_map(|turn| {
                [
                    json!({"role":"user","content": format!("question {turn}")}),
                    json!({"role":"assistant","content": format!("answer {turn} {}", "x".repeat(512))}),
                ]
            })
            .collect();
        daemon
            .emit(json!({"type":"history","id":get_history["id"],"session_id":"s1","messages":messages}))
            .await;
        refuse_config_requests(&mut daemon).await;
        let (before, response) = client.recv_response(json!(7)).await;
        assert!(response["result"].is_object(), "{response}");
        let replayed: Vec<&Value> = before
            .iter()
            .filter(|line| line["method"] == "session/update")
            .map(|line| &line["params"]["update"])
            .collect();
        assert_eq!(replayed.len(), TURNS * 2);
        for (index, update) in replayed.iter().enumerate() {
            assert_eq!(
                update["messageId"].as_str().unwrap().split('-').nth(1),
                Some(index.to_string().as_str())
            );
        }
        assert_eq!(
            replayed[TURNS * 2 - 2]["content"]["text"],
            format!("question {}", TURNS - 1)
        );
        client.close_stdin().await;
    }
}

mod replay_rows {
    use super::*;
    use crate::protocol::HistoryMessage;
    use crate::session::{RenderedImage, reasoning_line_markup};

    fn rows(value: Value) -> Vec<HistoryMessage> {
        serde_json::from_value(value).expect("history rows")
    }

    fn kinds(updates: &[Value]) -> Vec<&str> {
        updates
            .iter()
            .map(|update| update["sessionUpdate"].as_str().unwrap())
            .collect()
    }

    /// Reasoning exactly as `render.rs` formats it: every line through the
    /// shared markup, then one terminating blank line.
    fn rendered_reasoning(text: &str) -> String {
        let mut out: String = text.split('\n').map(reasoning_line_markup).collect();
        out.push('\n');
        out
    }

    #[test]
    fn replay_splits_reasoning_markup() {
        let reasoning = "weigh *both* options\n\nthen pick `a_b` [x] & <y>\n";
        let content = format!("{}The answer is **a**.", rendered_reasoning(reasoning));
        assert_eq!(
            split_reasoning(&content),
            [
                Part::Thought(reasoning.to_string()),
                Part::Text("The answer is **a**.".to_string())
            ]
        );
        assert_eq!(
            split_reasoning("plain *markdown* answer"),
            [Part::Text("plain *markdown* answer".to_string())]
        );
        assert_eq!(
            split_reasoning(&rendered_reasoning("only thinking")),
            [Part::Thought("only thinking".to_string())]
        );

        let updates = replay_updates(
            &rows(json!([{"role": "assistant", "content": content}])),
            &[],
            &MapperOptions::default(),
        );
        assert_eq!(
            updates,
            [
                json!({"sessionUpdate": "agent_thought_chunk", "messageId": "replay-0-thought",
                    "content": {"type": "text", "text": reasoning}}),
                json!({"sessionUpdate": "agent_message_chunk", "messageId": "replay-0-msg",
                    "content": {"type": "text", "text": "The answer is **a**."}}),
            ]
        );
    }

    #[test]
    fn replay_tool_rows_become_completed_tool_calls() {
        let todo_input = json!({"todos": [
            {"content": "edit", "status": "in_progress", "priority": "high", "id": "1", "confidence": "plausible"},
            {"content": "drop", "status": "cancelled", "priority": "low", "id": "2", "confidence": "plausible"}
        ]});
        let history = rows(json!([
            {"role": "user", "content": "fix it"},
            {"role": "assistant", "content": "On it.", "tool_calls": ["read", "todo"]},
            {"role": "tool", "content": "fn main() {}", "tool_data": {"id": "call_read", "name": "read", "input": {"file_path": "src/a.rs"}}},
            {"role": "tool", "content": "[...]", "tool_data": {"id": "call_todo", "name": "todo", "input": todo_input}},
            {"role": "tool", "content": "orphan output"},
            {"role": "assistant", "content": "Fixed."}
        ]));
        let updates = replay_updates(&history, &[], &work_dir());
        assert_eq!(
            kinds(&updates),
            [
                "user_message_chunk",
                "agent_message_chunk",
                "tool_call",
                "tool_call_update",
                "tool_call",
                "plan",
                "tool_call_update",
                "agent_message_chunk",
                "agent_message_chunk"
            ]
        );
        assert_eq!(updates[0]["messageId"], "replay-0");
        assert_eq!(
            updates[2],
            json!({"sessionUpdate": "tool_call", "toolCallId": "call_read", "name": "read",
                "title": tool_title("read"), "kind": "read", "status": "completed",
                "locations": [{"path": "/work/src/a.rs"}], "rawInput": {"file_path": "src/a.rs"}})
        );
        assert_eq!(
            updates[3],
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "call_read",
                "title": tool_title("read"), "kind": "read", "status": "completed",
                "content": [{"type": "content", "content": {"type": "text", "text": "fn main() {}"}}],
                "rawOutput": {"output": "fn main() {}"}})
        );
        assert_eq!(
            updates[5],
            json!({"sessionUpdate": "plan", "entries": [{"content": "edit", "priority": "high", "status": "in_progress"}]})
        );
        assert_eq!(updates[7]["content"]["text"], "orphan output");

        // A todo read shows no plan, and neither does input the tool rejects.
        for input in [json!({}), json!({"todos": [{"content": "x", "status": "bogus"}]})] {
            let history = rows(json!([
                {"role": "tool", "content": "[]", "tool_data": {"id": "c", "name": "todo", "input": input}}
            ]));
            assert_eq!(
                kinds(&replay_updates(&history, &[], &work_dir())),
                ["tool_call", "tool_call_update"]
            );
        }
        // A goals-only write keeps the stored list its result text opens with.
        let stored = json!([{"content": "kept", "status": "pending", "priority": "medium", "id": "1"}]);
        let history = rows(json!([
            {"role": "tool", "content": format!("{stored:#}\n\nGoals:\n[]"),
                "tool_data": {"id": "c", "name": "todo", "input": {"goals": []}}}
        ]));
        assert_eq!(
            replay_updates(&history, &[], &work_dir())[1]["entries"],
            json!([{"content": "kept", "priority": "medium", "status": "pending"}])
        );
    }

    #[test]
    fn replay_skips_system_rows() {
        let notice = "Earlier conversation compacted - showing all 3 compacted historical messages. Redraw may be slower while this view is open.";
        assert!(crate::session::is_compacted_history_notice(notice));
        let history = rows(json!([
            {"role": "system", "content": notice},
            {"role": "user", "content": "go on"},
            {"role": "system", "content": "Gate check ran"},
            {"role": "background_task", "content": "task finished"},
            {"role": "assistant", "content": "ok"}
        ]));
        let plain = replay_updates(&history, &[], &MapperOptions::default());
        assert_eq!(
            kinds(&plain),
            ["user_message_chunk", "agent_message_chunk", "agent_message_chunk"]
        );
        assert_eq!(plain[1]["content"]["text"], "task finished");

        let capable = MapperOptions::new(&json!({"session": {"compaction": {}}}), None);
        let updates = replay_updates(&history, &[], &capable);
        assert_eq!(
            updates[0],
            json!({"sessionUpdate": "compaction_update", "compactionId": "replay-0-cmp", "status": "completed"})
        );
        assert_eq!(
            updates.len(),
            plain.len() + 1,
            "only the compaction notice is added: {updates:?}"
        );
    }

    #[test]
    fn replay_user_images_follow_their_prompt() {
        let images: Vec<RenderedImage> = serde_json::from_value(json!([
            {"media_type": "image/png", "data": "AAA", "label": null, "source": {"kind": "user_input"},
                "anchor": {"kind": "user_prompt", "ordinal": 1}},
            {"media_type": "image/png", "data": "BBB", "label": null,
                "source": {"kind": "tool_result", "tool_name": "read"}, "anchor": {"kind": "tool_call", "id": "c"}}
        ]))
        .expect("images");
        let history = rows(json!([
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "a"},
            {"role": "user", "content": "second, with a screenshot"},
            {"role": "tool", "content": "read an image", "tool_data": {"id": "c", "name": "read", "input": {"file_path": "/x.png"}}}
        ]));
        let updates = replay_updates(&history, &images, &MapperOptions::default());
        assert_eq!(
            updates[3],
            json!({"sessionUpdate": "user_message_chunk", "messageId": "replay-2",
                "content": {"type": "image", "mimeType": "image/png", "data": "AAA"}})
        );
        assert!(
            !updates.iter().any(|update| update.to_string().contains("BBB")),
            "a tool image was never shown live: {updates:?}"
        );
    }
}

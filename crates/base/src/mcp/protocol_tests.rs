use super::*;

/// An isolated agent home for tests that make a workspace trusted.
struct HomeSandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl HomeSandbox {
    fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home tempdir");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", home.path());
        crate::workspace_trust::set_trust_file_for_tests(home.path().join("workspace-trust.json"));
        Self {
            _guard: guard,
            _home: home,
            previous,
        }
    }
}

impl Drop for HomeSandbox {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
    }
}

/// Load project-local config the way a trusted launch does.
fn load_trusted_project_locals(project: &std::path::Path) -> McpConfig {
    crate::workspace_trust::trust_for_tests(project);
    McpConfig::load_authorized_project_locals(project)
}

#[test]
fn issue_790_load_uses_process_cwd_while_unbound_load_for_dir_does_not() {
    let _guard = crate::storage::lock_test_env();
    let original_cwd = std::env::current_dir().expect("current cwd");
    let previous_home = std::env::var_os("HORON_AGENT_HOME");
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let other_project = tempfile::tempdir().expect("other project tempdir");
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    std::env::set_current_dir(project.path()).expect("set project cwd");
    std::fs::write(
        project.path().join(".mcp.json"),
        r#"{"mcpServers":{"cwd-only":{"command":"cwd-server"}}}"#,
    )
    .expect("write project MCP config");
    std::fs::write(
        other_project.path().join(".mcp.json"),
        r#"{"mcpServers":{"other-cwd-only":{"command":"other-cwd-server"}}}"#,
    )
    .expect("write other project MCP config");
    crate::workspace_trust::trust_for_tests(project.path());
    crate::workspace_trust::trust_for_tests(other_project.path());

    let result = std::panic::catch_unwind(|| {
        let unbound = McpConfig::load_for_dir(None);
        assert!(!unbound.servers.contains_key("cwd-only"));

        let default = McpConfig::load();
        assert!(default.servers.contains_key("cwd-only"));
        assert!(!default.servers.contains_key("other-cwd-only"));

        std::env::set_current_dir(other_project.path()).expect("set other project cwd");
        let other_default = McpConfig::load();
        assert!(!other_default.servers.contains_key("cwd-only"));
        assert!(other_default.servers.contains_key("other-cwd-only"));

        let bound = McpConfig::load_for_dir(Some(project.path()));
        assert!(bound.servers.contains_key("cwd-only"));
        assert!(!bound.servers.contains_key("other-cwd-only"));
    });

    std::env::set_current_dir(original_cwd).expect("restore cwd");
    if let Some(previous_home) = previous_home {
        crate::env::set_var("HORON_AGENT_HOME", previous_home);
    } else {
        crate::env::remove_var("HORON_AGENT_HOME");
    }
    result.expect("MCP cwd isolation assertions");
}

#[test]
fn test_json_rpc_request_serialization() {
    let request = JsonRpcRequest::new(1, "tools/list", None);
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"jsonrpc\":\"2.0\""));
    assert!(json.contains("\"id\":1"));
    assert!(json.contains("\"method\":\"tools/list\""));
}

#[test]
fn test_json_rpc_notification_serialization_omits_id() {
    let notification = JsonRpcNotification::new("notifications/initialized", None);
    let value = serde_json::to_value(notification).unwrap();
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["method"], "notifications/initialized");
    assert!(value.get("id").is_none());
    assert!(value.get("params").is_none());
}

#[test]
fn test_json_rpc_response_deserialization() {
    let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
    let response: JsonRpcResponse = serde_json::from_str(json).unwrap();
    assert_eq!(response.id, Some(1));
    assert!(response.result.is_some());
    assert!(response.error.is_none());
}

#[test]
fn test_json_rpc_error_response() {
    let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid Request"}}"#;
    let response: JsonRpcResponse = serde_json::from_str(json).unwrap();
    assert!(response.error.is_some());
    let err = response.error.unwrap();
    assert_eq!(err.code, -32600);
    assert_eq!(err.message, "Invalid Request");
}

#[test]
fn test_mcp_config_deserialization() {
    let json = r#"{
            "servers": {
                "test-server": {
                    "command": "/usr/bin/test-mcp",
                    "args": ["--port", "8080"],
                    "env": {"API_KEY": "secret"}
                }
            }
        }"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.servers.len(), 1);
    let server = config.servers.get("test-server").unwrap();
    assert_eq!(server.command, "/usr/bin/test-mcp");
    assert_eq!(server.args, vec!["--port", "8080"]);
    assert_eq!(server.env.get("API_KEY"), Some(&"secret".to_string()));
}

#[test]
fn test_mcp_config_timeout_secs_defaults_to_none_and_accepts_override() {
    // Issues #802 / #1174: per-server reply timeout. Absent keeps the 30s
    // default; an explicit value is honored.
    let json = r#"{
            "mcpServers": {
                "fast": {"command": "fast-mcp"},
                "slow": {"command": "slow-mcp", "timeout_secs": 120}
            }
        }"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    let fast = config.servers.get("fast").unwrap();
    let slow = config.servers.get("slow").unwrap();
    assert_eq!(fast.timeout_secs, None);
    assert_eq!(slow.timeout_secs, Some(120));
    assert_eq!(
        crate::mcp::request_timeout_for(fast),
        crate::mcp::DEFAULT_MCP_REQUEST_TIMEOUT
    );
    assert_eq!(
        crate::mcp::request_timeout_for(slow),
        std::time::Duration::from_secs(120)
    );
    // Zero is treated as "unset" rather than an instant timeout.
    let zero = McpServerConfig {
        timeout_secs: Some(0),
        ..slow.clone()
    };
    assert_eq!(
        crate::mcp::request_timeout_for(&zero),
        crate::mcp::DEFAULT_MCP_REQUEST_TIMEOUT
    );
}

#[test]
fn test_mcp_config_empty() {
    let json = r#"{}"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert!(config.servers.is_empty());
}

#[test]
fn test_mcp_config_accepts_claude_mcp_servers_key() {
    // Claude Code uses `mcpServers`, not `servers`.
    let json = r#"{
            "mcpServers": {
                "claude-server": {
                    "command": "npx",
                    "args": ["-y", "some-mcp"]
                }
            }
        }"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.servers.len(), 1);
    let server = config.servers.get("claude-server").unwrap();
    assert_eq!(server.command, "npx");
    assert!(server.is_stdio());
}

#[test]
fn test_mcp_http_server_is_not_stdio() {
    let json = r#"{
            "mcpServers": {
                "remote": {
                    "type": "http",
                    "url": "https://example.com/mcp"
                }
            }
        }"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    let server = config.servers.get("remote").unwrap();
    assert!(!server.is_stdio());
    assert_eq!(server.url.as_deref(), Some("https://example.com/mcp"));
    assert!(server.is_runnable());
    assert_eq!(server.remote_transport(), Some(RemoteTransport::StreamableHttp));
}

#[test]
fn remote_transport_inference_respects_explicit_type() {
    let parse = |value| serde_json::from_value::<McpServerConfig>(value).unwrap();
    assert_eq!(
        parse(serde_json::json!({"url": "https://example.com/mcp"})).remote_transport(),
        Some(RemoteTransport::StreamableHttp)
    );
    assert_eq!(
        parse(serde_json::json!({"type": " SSE ", "url": "https://example.com/mcp"})).remote_transport(),
        Some(RemoteTransport::LegacySse)
    );
    for kind in ["stdio", "websocket", "unknown"] {
        let config = parse(serde_json::json!({"type": kind, "url": "https://example.com/mcp"}));
        assert!(!config.is_runnable(), "explicit {kind} must not infer HTTP");
        assert!(config.remote_transport().is_none());
    }
    let unknown = parse(serde_json::json!({"type": "unknown", "command": "do-not-spawn"}));
    assert!(!unknown.is_runnable());
    let local = parse(serde_json::json!({"command": "local-server", "url": "https://example.com/mcp"}));
    assert!(local.is_stdio());
    assert!(local.remote_transport().is_none());
    assert!(!parse(serde_json::json!({"type": "http"})).is_runnable());
}

#[test]
fn remote_display_target_does_not_expose_url_credentials() {
    let config: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "https://user:secret@example.com/mcp?token=secret#secret",
    }))
    .unwrap();
    assert_eq!(config.display_target(), "http https://example.com/mcp");
    let opaque: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "mailto:user:secret@example.com?token=secret",
    }))
    .unwrap();
    assert_eq!(opaque.display_target(), "http [invalid URL]");
}

#[test]
fn merged_config_retains_remote_servers_and_expands_headers() {
    let _guard = crate::storage::lock_test_env();
    let previous_home = std::env::var_os("HORON_AGENT_HOME");
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    std::fs::write(project.path().join(".mcp.json"), r#"{
        "mcpServers": {
            "remote-http": {"url": "https://example.com/mcp", "headers": {"Authorization": "Bearer ${MCP_TEST_TOKEN_MISSING:-fixture-token}"}},
            "remote-sse": {"type": "sse", "url": "https://example.com/events"},
            "unknown": {"type": "websocket", "url": "https://example.com/ws"},
            "missing-url": {"type": "http"}
        }
    }"#).unwrap();
    crate::workspace_trust::trust_for_tests(project.path());
    let config = McpConfig::load_for_dir(Some(project.path()));
    match previous_home {
        Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
    assert_eq!(config.servers.len(), 2);
    assert!(config.servers["remote-http"].is_remote());
    assert_eq!(
        config.servers["remote-http"].headers["Authorization"],
        "Bearer fixture-token"
    );
    assert_eq!(
        config.servers["remote-sse"].remote_transport(),
        Some(RemoteTransport::LegacySse)
    );
}

#[test]
fn environment_expansion_matches_claude_syntax_across_config_fields() {
    let json = r#"{
        "mcpServers": {
            "probe": {
                "command": "${BIN_DIR}/probe",
                "args": ["--token=${TOKEN:-fallback-token}", "${MISSING_DEFAULT:-fallback}", "${MISSING}"],
                "env": {
                    "PROBE_PATH": "${BIN_DIR}/marker",
                    "STILL_MISSING": "prefix-${MISSING}"
                },
                "type": "http",
                "url": "${BASE_URL:-https://example.test}/mcp",
                "headers": {"Authorization": "Bearer ${TOKEN}"}
            }
        }
    }"#;
    let mut config: McpConfig = serde_json::from_str(json).unwrap();

    let warnings = config.expand_environment_variables_with(|variable| match variable {
        "BIN_DIR" => Some("/opt/tools".to_string()),
        "TOKEN" => Some("secret-token".to_string()),
        _ => None,
    });

    let server = config.servers.get("probe").unwrap();
    assert_eq!(server.command, "/opt/tools/probe");
    assert_eq!(server.args, vec!["--token=secret-token", "fallback", "${MISSING}"]);
    assert_eq!(server.env["PROBE_PATH"], "/opt/tools/marker");
    assert_eq!(server.env["STILL_MISSING"], "prefix-${MISSING}");
    assert_eq!(server.url.as_deref(), Some("https://example.test/mcp"));
    assert_eq!(server.headers["Authorization"], "Bearer secret-token");
    assert_eq!(
        warnings,
        vec![UnresolvedEnvironmentVariable {
            server: "probe".to_string(),
            variable: "MISSING".to_string(),
        }],
        "an unresolved variable is preserved and warned once per server"
    );
}

#[test]
fn environment_expansion_preserves_malformed_and_unclosed_expressions() {
    let mut unresolved = std::collections::BTreeSet::new();
    let expanded = expand_environment_string(
        "${1INVALID} ${:-no} ${UNCLOSED",
        &|_| Some("unexpected".to_string()),
        &mut unresolved,
    );

    assert_eq!(expanded, "${1INVALID} ${:-no} ${UNCLOSED");
    assert!(unresolved.is_empty());
}

#[test]
fn expansion_after_merge_ignores_shadowed_references() {
    let mut merged = McpConfig::default();
    merged.servers.insert(
        "same-name".to_string(),
        serde_json::from_value(serde_json::json!({"command": "${WINNER}"})).unwrap(),
    );
    let lower = serde_json::from_value(serde_json::json!({
        "same-name": {"command": "${SHADOWED}"}
    }))
    .unwrap();
    super::super::sources::merge_lower(&mut merged.servers, lower);

    let warnings = merged
        .expand_environment_variables_with(|variable| (variable == "WINNER").then(|| "winning-command".to_string()));

    assert_eq!(merged.servers["same-name"].command, "winning-command");
    assert!(warnings.is_empty(), "shadowed definitions must not warn");
}

#[test]
fn load_for_dir_expands_the_winning_merged_definition() {
    let _guard = crate::storage::lock_test_env();
    let previous_home = std::env::var_os("HORON_AGENT_HOME");
    let previous_value = std::env::var_os("HORON_AGENT_MCP_EXPANSION_TEST_VALUE");
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    crate::env::set_var("HORON_AGENT_MCP_EXPANSION_TEST_VALUE", "expanded-value");

    std::fs::write(
        home.path().join("mcp.json"),
        r#"{"mcpServers":{"same-name":{"command":"${SHADOWED_MISSING}"}}}"#,
    )
    .unwrap();
    std::fs::write(
        project.path().join(".mcp.json"),
        r#"{"mcpServers":{"same-name":{"command":"project-bin","args":["${HORON_AGENT_MCP_EXPANSION_TEST_VALUE}"]}}}"#,
    )
    .unwrap();
    crate::workspace_trust::trust_for_tests(project.path());

    let result = std::panic::catch_unwind(|| {
        let config = McpConfig::load_for_dir(Some(project.path()));
        let server = &config.servers["same-name"];
        assert_eq!(server.command, "project-bin");
        assert_eq!(server.args, ["expanded-value"]);
    });

    match previous_home {
        Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
    match previous_value {
        Some(value) => crate::env::set_var("HORON_AGENT_MCP_EXPANSION_TEST_VALUE", value),
        None => crate::env::remove_var("HORON_AGENT_MCP_EXPANSION_TEST_VALUE"),
    }
    result.expect("merged config expansion assertions");
}

#[test]
fn expanded_values_invalidate_schema_cache_fingerprint() {
    let raw = r#"{"mcpServers":{"srv":{"command":"node","args":["${SCRIPT}"],"env":{"TOKEN":"${TOKEN}"}}}}"#;
    let mut first: McpConfig = serde_json::from_str(raw).unwrap();
    first.expand_environment_variables_with(|variable| match variable {
        "SCRIPT" => Some("first.js".to_string()),
        "TOKEN" => Some("token-one".to_string()),
        _ => None,
    });
    let mut second: McpConfig = serde_json::from_str(raw).unwrap();
    second.expand_environment_variables_with(|variable| match variable {
        "SCRIPT" => Some("second.js".to_string()),
        "TOKEN" => Some("token-two".to_string()),
        _ => None,
    });

    assert_ne!(
        crate::mcp::schema_cache::fingerprint_config(&first.servers["srv"]),
        crate::mcp::schema_cache::fingerprint_config(&second.servers["srv"]),
        "changing expanded environment values must invalidate cached schemas"
    );
}

#[test]
fn test_load_project_locals_resolves_against_given_dir_not_cwd() {
    let _home = HomeSandbox::new();
    // Issue #420: remote/client sessions must load project-local MCP config
    // from the session working directory, not the server process cwd.
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("client-project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"example-server":{"command":"npx","args":["-y","some-mcp-server"]}}}"#,
    )
    .unwrap();

    // The process cwd (this repo) is unrelated to `project`; resolution must
    // come from the explicit path.
    let config = load_trusted_project_locals(&project);
    assert_eq!(config.servers.len(), 1);
    let server = config.servers.get("example-server").unwrap();
    assert_eq!(server.command, "npx");
    assert!(server.is_stdio());

    // A directory with no project-local config yields nothing.
    let empty = temp.path().join("empty-project");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(load_trusted_project_locals(&empty).servers.is_empty());
}

#[test]
fn test_load_project_locals_merge_order() {
    let _home = HomeSandbox::new();
    // Highest priority first: `.horon-agent/mcp.json`, then `.mcp.json`, then
    // `.claude/mcp.json`. An earlier file keeps a same-named server.
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path();
    std::fs::create_dir_all(project.join(".horon-agent")).unwrap();
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(
        project.join(".horon-agent/mcp.json"),
        r#"{"servers":{"shared-name":{"command":"horon-agent-bin"},"horon-agent-only":{"command":"a"}}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"shared-name":{"command":"claude-bin"}}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".claude/mcp.json"),
        r#"{"mcpServers":{"legacy-only":{"command":"c"}}}"#,
    )
    .unwrap();

    let config = load_trusted_project_locals(project);
    assert_eq!(config.servers.len(), 3);
    assert_eq!(
        config.servers.get("shared-name").unwrap().command,
        "horon-agent-bin",
        ".horon-agent/mcp.json must win over .mcp.json for same-named servers"
    );
    assert!(config.servers.contains_key("horon-agent-only"));
    assert!(config.servers.contains_key("legacy-only"));
}

#[test]
fn test_server_enabled_defaults_true() {
    // Existing configs without the flag keep current behavior (issue #436).
    let json = r#"{"servers":{"srv":{"command":"bin"}}}"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert!(config.servers.get("srv").unwrap().is_enabled());
}

#[test]
fn test_server_enabled_false_opencode_style() {
    let json = r#"{"servers":{"srv":{"command":"bin","enabled":false}}}"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert!(!config.servers.get("srv").unwrap().is_enabled());
}

#[test]
fn test_server_disabled_true_claude_style() {
    let json = r#"{"mcpServers":{"srv":{"command":"bin","disabled":true}}}"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert!(!config.servers.get("srv").unwrap().is_enabled());
}

#[test]
fn test_server_disabled_wins_over_enabled() {
    // `disabled` (Claude Code style) wins when both spellings are present.
    let json = r#"{"servers":{"srv":{"command":"bin","enabled":true,"disabled":true}}}"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert!(!config.servers.get("srv").unwrap().is_enabled());

    let json = r#"{"servers":{"srv":{"command":"bin","enabled":false,"disabled":false}}}"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert!(config.servers.get("srv").unwrap().is_enabled());
}

#[test]
fn test_disabled_server_survives_save_roundtrip() {
    // Disabled servers must stay in config (kept, not spawned), including
    // through a save/load cycle.
    let json = r#"{"servers":{"off":{"command":"bin","enabled":false}}}"#;
    let config: McpConfig = serde_json::from_str(json).unwrap();
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("mcp.json");
    config.save_to_file(&path).unwrap();
    let reloaded = McpConfig::load_from_file(&path).unwrap();
    assert!(!reloaded.servers.get("off").unwrap().is_enabled());
}

#[test]
fn test_tool_def_deserialization() {
    let json = r#"{
            "name": "read_file",
            "description": "Read a file from disk",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }"#;
    let tool: McpToolDef = serde_json::from_str(json).unwrap();
    assert_eq!(tool.name, "read_file");
    assert_eq!(tool.description, Some("Read a file from disk".to_string()));
}

#[test]
fn test_tool_call_result_text() {
    let json = r#"{
            "content": [{"type": "text", "text": "File contents here"}],
            "isError": false
        }"#;
    let result: ToolCallResult = serde_json::from_str(json).unwrap();
    assert!(!result.is_error);
    assert_eq!(result.content.len(), 1);
    match &result.content[0] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "File contents here"),
        _ => panic!("Expected text block"),
    }
}

#[test]
fn test_tool_call_result_error() {
    let json = r#"{
            "content": [{"type": "text", "text": "File not found"}],
            "isError": true
        }"#;
    let result: ToolCallResult = serde_json::from_str(json).unwrap();
    assert!(result.is_error);
}

#[test]
fn test_initialize_result() {
    let json = r#"{
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {"listChanged": true}
            },
            "serverInfo": {
                "name": "test-server",
                "version": "1.0.0"
            }
        }"#;
    let result: InitializeResult = serde_json::from_str(json).unwrap();
    assert_eq!(result.protocol_version, "2024-11-05");
    assert!(result.server_info.is_some());
}

#[test]
fn http_entry_does_not_displace_a_working_stdio_server_of_the_same_name() {
    let _home = HomeSandbox::new();
    // A `type: http` entry from a lower-precedence config used to overwrite the
    // stdio definition and then get dropped by the non-stdio filter, silently
    // losing a working server (issue #653).
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path();
    std::fs::create_dir_all(project.join(".horon-agent")).unwrap();
    std::fs::write(
        project.join(".horon-agent/mcp.json"),
        r#"{"servers":{"github":{"type":"stdio","command":"npx","args":["-y","mcp-remote"]}}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"github":{"type":"http","url":"https://api.githubcopilot.com/mcp/"}}}"#,
    )
    .unwrap();

    let config = load_trusted_project_locals(project);
    let github = config
        .servers
        .get("github")
        .expect("stdio github server must survive the http entry");
    assert_eq!(github.command, "npx");
    assert!(github.is_stdio());
}

#[test]
fn stdio_entry_still_overrides_an_existing_http_entry() {
    let _home = HomeSandbox::new();
    // A stdio definition wins over a remote one in both directions, so a
    // lower-priority stdio entry still replaces a higher-priority http entry.
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path();
    std::fs::create_dir_all(project.join(".horon-agent")).unwrap();
    std::fs::write(
        project.join(".horon-agent/mcp.json"),
        r#"{"servers":{"github":{"type":"http","url":"https://example.invalid/mcp/"}}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"github":{"type":"stdio","command":"npx"}}}"#,
    )
    .unwrap();

    let config = load_trusted_project_locals(project);
    assert_eq!(config.servers.get("github").unwrap().command, "npx");
}

#[test]
fn same_transport_collision_keeps_the_higher_priority_entry() {
    let _home = HomeSandbox::new();
    // Same-transport collisions keep the first, highest-priority definition.
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path();
    std::fs::create_dir_all(project.join(".horon-agent")).unwrap();
    std::fs::write(
        project.join(".horon-agent/mcp.json"),
        r#"{"servers":{"github":{"command":"old-bin"}}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"github":{"command":"new-bin"}}}"#,
    )
    .unwrap();

    let config = load_trusted_project_locals(project);
    assert_eq!(config.servers.get("github").unwrap().command, "old-bin");
}

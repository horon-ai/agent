//! Every v2 builder is validated against the vendored `schema-v2.0.0-alpha.10`
//! `$defs` entry it produces, plus the IS-1/IS-6/IS-12/IS-15 rules that schema
//! validation cannot express.

use super::super::auth::{AUTH_METHOD_CREDENTIALS, AUTH_METHOD_TERMINAL};
use super::super::session_configuration::{acp_available_commands, insert_available_commands_v2};
use super::super::util::ACP_PROTOCOL_VERSION;
use super::negotiate::{ACP_V2_PROTOCOL_VERSION, AcpVersion, InitError, select};
use super::shapes::{
    ChunkKind, ConfigValue, MessageKind, ToolCallPatch, auth_methods, available_commands, command_subject,
    config_options, diff_content, initialize_response, mcp_servers, message_chunk, message_ids,
    parse_set_config_option, permission_request, plan_update, session_info, state_idle, state_requires_action,
    state_running, terminal_content, terminal_finished, terminal_output, terminal_started, tool_call_content_chunk,
    tool_call_subject, tool_call_update, user_message, whole_message,
};
use crate::protocol::{FileDiffWire, PlanEntryWire, SessionMcpServer};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

const SCHEMA_JSON: &str = include_str!("../../../../tests/fixtures/acp/schema-v2.0.0-alpha.10.json");

fn validator(def_name: &str) -> Arc<jsonschema::Validator> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<jsonschema::Validator>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().expect("the v2 schema cache is not poisoned");
    if let Some(cached) = cache.get(def_name) {
        return Arc::clone(cached);
    }
    let schema: Value = serde_json::from_str(SCHEMA_JSON).expect("the vendored v2 schema is valid JSON");
    let defs = schema.get("$defs").cloned().expect("the vendored v2 schema has $defs");
    let wrapper = json!({ "$ref": format!("#/$defs/{def_name}"), "$defs": defs });
    let compiled = Arc::new(jsonschema::validator_for(&wrapper).expect("the v2 $def compiles"));
    cache.insert(def_name.to_string(), Arc::clone(&compiled));
    compiled
}

#[track_caller]
pub(super) fn assert_matches(def_name: &str, value: &Value) {
    if let Err(error) = validator(def_name).validate(value) {
        panic!("{def_name} rejected {value}: {error}");
    }
}

#[track_caller]
pub(super) fn assert_update_matches(update: &Value) {
    assert_matches("SessionUpdate", update);
    assert_matches(
        "UpdateSessionNotification",
        &json!({ "sessionId": "sess-1", "update": update }),
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    V1,
    V2,
    Invalid,
    Unsupported,
}

#[track_caller]
fn check_selection(params: &Value, v2_enabled: bool, expect: Expect, label: &str) {
    let actual = select(params, v2_enabled);
    let matched = match (&actual, expect) {
        (Ok(AcpVersion::V1), Expect::V1) => true,
        (Ok(AcpVersion::V2), Expect::V2) => true,
        (Err(InitError::InvalidParams(_)), Expect::Invalid) => true,
        (
            Err(InitError::Unsupported {
                requested: 0,
                supported,
            }),
            Expect::Unsupported,
        ) => supported == &[1, ACP_V2_PROTOCOL_VERSION],
        _ => false,
    };
    assert!(
        matched,
        "{label}: expected {expect:?} with v2 {v2_enabled}, got {actual:?}"
    );
}

#[test]
fn select_table() {
    let info = json!({ "name": "client", "title": "Client", "version": "1.0.0" });
    let with_info = |version: Value| json!({ "protocolVersion": version, "info": info });
    let cases: [(&str, Value, Expect, Expect); 9] = [
        ("1", with_info(json!(1)), Expect::V1, Expect::V1),
        ("2", with_info(json!(2)), Expect::V2, Expect::V1),
        ("3", with_info(json!(3)), Expect::V2, Expect::V1),
        ("65535", with_info(json!(65535)), Expect::V2, Expect::V1),
        ("0", with_info(json!(0)), Expect::Unsupported, Expect::V1),
        ("\"2\"", with_info(json!("2")), Expect::Invalid, Expect::Invalid),
        ("missing", json!({ "info": info }), Expect::Invalid, Expect::Invalid),
        ("70000", with_info(json!(70000)), Expect::Invalid, Expect::Invalid),
        (
            "2 without info",
            json!({ "protocolVersion": 2 }),
            Expect::Invalid,
            Expect::V1,
        ),
    ];
    for (label, params, v2_on, v2_off) in &cases {
        check_selection(params, true, *v2_on, label);
        check_selection(params, false, *v2_off, label);
    }
    assert_eq!(
        ACP_PROTOCOL_VERSION, 1,
        "the v1 surface a disabled switch falls back to"
    );
    assert_eq!(ACP_V2_PROTOCOL_VERSION, 2);

    let invalid = select(&json!({ "protocolVersion": "2" }), true).expect_err("bad version");
    let rpc = invalid.to_rpc();
    assert_eq!(rpc.code, -32602);
    let unsupported = select(&with_info(json!(0)), true).expect_err("no common version");
    let rpc = unsupported.to_rpc();
    assert_eq!(rpc.code, -32022);
    assert_eq!(rpc.message, "Unsupported protocol version");
    assert_eq!(rpc.data, Some(json!({ "requested": 0, "supported": [1, 2] })));
}

#[test]
fn initialize_response_matches_schema() {
    let v1_style = json!({ "fs": { "readTextFile": true }, "terminal": true });
    let v2_style = json!({ "auth": { "terminal": {} }, "elicitation": { "form": {} } });
    for profile in [
        super::super::util::AcpProfile::Standard,
        super::super::util::AcpProfile::Extended,
        super::super::util::AcpProfile::Full,
    ] {
        assert_matches("InitializeResponse", &initialize_response(profile, &v1_style, true));
        assert_matches("InitializeResponse", &initialize_response(profile, &v2_style, false));
    }

    let response = initialize_response(super::super::util::AcpProfile::Standard, &v2_style, true);
    assert_eq!(response["protocolVersion"], 2);
    assert_eq!(response["info"]["name"], "horon-agent");
    assert_eq!(response["info"]["title"], "HoronAgent");
    assert!(!response["info"]["version"].as_str().unwrap().is_empty());
    assert_eq!(
        response["capabilities"]["session"]["prompt"],
        json!({ "image": {}, "embeddedContext": {} })
    );
    assert_eq!(
        response["capabilities"]["session"]["mcp"],
        json!({ "stdio": {}, "http": {} })
    );
    assert_eq!(response["capabilities"]["session"]["delete"], json!({}));
    assert_eq!(response["capabilities"]["session"]["additionalDirectories"], json!({}));
    for removed in [
        "loadSession",
        "promptCapabilities",
        "mcpCapabilities",
        "sessionCapabilities",
    ] {
        assert!(
            response["capabilities"].get(removed).is_none(),
            "{removed} is gone on v2: {response}"
        );
    }
    assert!(response["capabilities"].get("_meta").is_none());
    assert_eq!(response["authMethods"].as_array().unwrap().len(), 2);

    let without_http = initialize_response(super::super::util::AcpProfile::Standard, &v1_style, false);
    assert!(without_http["capabilities"]["session"]["mcp"].get("http").is_none());
    assert_eq!(
        without_http["authMethods"].as_array().unwrap().len(),
        1,
        "a v1 boolean terminal capability never advertises the v2 terminal method"
    );
}

#[test]
fn auth_methods_matches_schema() {
    for method in auth_methods(&json!({ "auth": { "terminal": {} } })) {
        assert_matches("AuthMethod", &method);
    }
    let methods = auth_methods(&json!({ "auth": { "terminal": {} } }));
    assert_eq!(methods[0]["methodId"], AUTH_METHOD_CREDENTIALS);
    assert_eq!(methods[0]["type"], "agent");
    assert_eq!(methods[1]["methodId"], AUTH_METHOD_TERMINAL);
    assert_eq!(methods[1]["type"], "terminal");
    assert_eq!(auth_methods(&json!({ "auth": { "terminal": null } })).len(), 1);
    assert_eq!(auth_methods(&json!({ "auth": { "terminal": false } })).len(), 1);
}

#[test]
fn terminal_auth_args_are_login_only() {
    let methods = auth_methods(&json!({ "auth": { "terminal": {} } }));
    let terminal = methods
        .iter()
        .find(|method| method["type"] == "terminal")
        .expect("the terminal method is advertised");
    assert_eq!(terminal["methodId"], AUTH_METHOD_TERMINAL);
    assert_eq!(terminal["name"], "Log in from the terminal");
    assert_eq!(
        terminal["description"],
        "Runs 'horon-agent acp --login' in a terminal to log in."
    );
    assert_eq!(terminal["args"], json!(["--login"]), "the client appends only --login");
    assert_eq!(terminal["env"], json!([]), "v2 env is an array, not an object");
}

#[test]
fn config_options_matches_schema() {
    let v1_options = json!([
        {
            "type": "select",
            "id": "mode",
            "name": "Session Mode",
            "category": "mode",
            "currentValue": "ask",
            "options": [{ "value": "ask", "name": "Ask" }, { "value": "code", "name": "Code" }],
        },
        {
            "type": "select",
            "id": "model",
            "name": "Model",
            "category": "model",
            "currentValue": "gpt",
            "options": [{ "value": "gpt", "name": "gpt" }],
        },
    ]);
    let options = config_options(v1_options.as_array().unwrap());
    assert_eq!(options.len(), 2);
    for option in &options {
        assert_matches("SessionConfigOption", option);
        assert!(option.get("id").is_none(), "v2 uses configId: {option}");
    }
    assert_eq!(options[0]["configId"], "mode", "order is preserved");
    assert_eq!(options[1]["configId"], "model");
    assert_eq!(options[0]["category"], "mode");
    assert_eq!(options[0]["currentValue"], "ask");
    assert_eq!(options[0]["options"][1]["value"], "code");

    let boolean = json!([{ "type": "boolean", "id": "toggle", "currentValue": true }]);
    assert!(
        config_options(boolean.as_array().unwrap()).is_empty(),
        "only select options are v2 config options today"
    );

    let grouped = json!([{
        "type": "select",
        "id": "model",
        "name": "Model",
        "category": "model",
        "currentValue": "a",
        "options": [
            { "group": "g1", "name": "Provider A", "options": [{ "value": "a", "name": "A" }] },
            { "group": "g2", "name": "Provider B", "options": [{ "value": "b", "name": "B" }] },
        ],
    }]);
    let grouped_options = config_options(grouped.as_array().unwrap());
    assert_eq!(grouped_options.len(), 1);
    assert_matches("SessionConfigOption", &grouped_options[0]);
    for (index, group) in ["g1", "g2"].iter().enumerate() {
        assert_eq!(grouped_options[0]["options"][index]["groupId"], *group);
        assert!(
            grouped_options[0]["options"][index].get("group").is_none(),
            "v2 names a select group groupId: {}",
            grouped_options[0]["options"][index]
        );
    }
    assert_eq!(
        grouped_options[0]["options"][0]["options"][0]["value"], "a",
        "a group keeps its own option list"
    );

    assert_matches("SetSessionConfigOptionResponse", &json!({ "configOptions": options }));
    assert_matches(
        "ConfigOptionUpdate",
        &json!({ "configOptions": config_options(v1_options.as_array().unwrap()) }),
    );
}

#[test]
fn parse_set_config_option_requires_type() {
    let parsed = parse_set_config_option(&json!({
        "sessionId": "sess-1",
        "configId": "model",
        "type": "id",
        "value": "gpt",
    }));
    assert_eq!(
        parsed.expect("a typed id value"),
        ("model".to_string(), ConfigValue::Id("gpt".to_string()))
    );
    let parsed = parse_set_config_option(&json!({
        "configId": "feature",
        "type": "boolean",
        "value": true,
    }));
    assert_eq!(
        parsed.expect("a typed boolean value"),
        ("feature".to_string(), ConfigValue::Boolean(true))
    );

    for rejected in [
        json!({ "configId": "model", "value": "gpt" }),
        json!({ "configId": "model", "type": "id", "value": true }),
        json!({ "configId": "model", "type": "boolean", "value": "true" }),
        json!({ "configId": "model", "type": "string", "value": "gpt" }),
        json!({ "configId": "model", "type": "id" }),
        json!({ "configId": "", "type": "id", "value": "gpt" }),
        json!({ "type": "id", "value": "gpt" }),
    ] {
        let error = parse_set_config_option(&rejected).expect_err("rejected");
        assert_eq!(error.code, -32602, "{rejected}");
    }
}

#[test]
fn available_commands_matches_schema() {
    let commands = available_commands(&acp_available_commands());
    assert_eq!(commands.len(), 3);
    let names: Vec<&str> = commands
        .iter()
        .map(|command| command["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["model", "models", "effort"]);
    for command in &commands {
        assert_matches("AvailableCommand", command);
        assert_eq!(command["type"], "command");
    }
    assert_eq!(
        commands[0]["input"],
        json!({ "type": "text", "hint": "model id (optional)" })
    );
    assert!(
        commands[1].get("input").is_none(),
        "a command without an argument advertises no input"
    );
    assert_matches(
        "AvailableCommandsUpdate",
        &json!({ "availableCommands": available_commands(&acp_available_commands()) }),
    );
}

#[test]
fn empty_commands_omitted() {
    let mut empty = json!({ "sessionId": "sess-1" });
    insert_available_commands_v2(&mut empty, &[]);
    assert!(
        empty.get("availableCommands").is_none(),
        "an empty list is omitted, never null: {empty}"
    );
    assert_matches("NewSessionResponse", &empty);
    let mut advertised = json!({ "sessionId": "sess-1" });
    insert_available_commands_v2(&mut advertised, &acp_available_commands());
    assert_eq!(advertised["availableCommands"].as_array().unwrap().len(), 3);
    assert_eq!(advertised["availableCommands"][0]["type"], "command");
    assert_matches("NewSessionResponse", &advertised);
}

fn server_wire(server: &SessionMcpServer) -> Value {
    if server.transport == "stdio" {
        json!({
            "type": "stdio",
            "name": server.name,
            "command": server.command,
            "args": server.args,
            "env": server
                .env
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect::<Vec<Value>>(),
        })
    } else {
        json!({
            "type": "http",
            "name": server.name,
            "url": server.url,
            "headers": server
                .headers
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect::<Vec<Value>>(),
        })
    }
}

#[test]
fn mcp_servers_matches_schema() {
    let servers = json!([
        {
            "type": "stdio",
            "name": "fs",
            "command": "/usr/local/bin/mcp-fs",
            "args": ["--root", "/w"],
            "env": [{ "name": "TOKEN", "value": "t" }],
        },
        {
            "type": "http",
            "name": "web",
            "url": "https://example.com/mcp",
            "headers": [{ "name": "A", "value": "b" }],
        },
        { "type": "stdio", "name": "bare", "command": "/bin/bare" },
    ]);
    assert_matches("NewSessionRequest", &json!({ "cwd": "/w", "mcpServers": servers }));
    let params = json!({ "mcpServers": servers });
    let parsed = mcp_servers(&params, true).expect("both advertised transports parse");
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].transport, "stdio");
    assert_eq!(parsed[0].command.as_deref(), Some("/usr/local/bin/mcp-fs"));
    assert_eq!(parsed[0].args, ["--root", "/w"]);
    assert_eq!(parsed[0].env, [("TOKEN".to_string(), "t".to_string())]);
    assert_eq!(parsed[1].transport, "http");
    assert_eq!(parsed[1].url.as_deref(), Some("https://example.com/mcp"));
    assert_eq!(parsed[1].headers, [("A".to_string(), "b".to_string())]);
    assert!(parsed[2].args.is_empty() && parsed[2].env.is_empty());
    for server in &parsed {
        assert_matches("McpServer", &server_wire(server));
    }

    for rejected in [
        json!([{ "type": "sse", "name": "web", "url": "https://example.com/sse" }]),
        json!([{ "name": "fs", "command": "/bin/mcp" }]),
        json!([{ "type": "ws", "name": "fs", "url": "ws://localhost" }]),
        json!([{ "type": "stdio", "name": "fs" }]),
        json!([{ "type": "http", "name": "web" }]),
    ] {
        let error = mcp_servers(&json!({ "mcpServers": rejected }), true).expect_err("rejected");
        assert_eq!(error.code, -32602, "{rejected}");
    }
    let error = mcp_servers(
        &json!({ "mcpServers": [{ "type": "http", "name": "web", "url": "https://e.com/mcp" }] }),
        false,
    )
    .expect_err("http is not advertised");
    assert_eq!(error.code, -32602);
    assert!(mcp_servers(&json!({}), true).expect("absent servers").is_empty());
}

#[test]
fn permission_request_matches_schema() {
    let options = json!([
        { "optionId": "allow_once", "name": "Allow once", "kind": "allow_once" },
        { "optionId": "reject_once", "name": "Deny", "kind": "reject_once" },
    ]);
    let options = options.as_array().unwrap();
    let request = permission_request(
        "sess-1",
        "Allow HoronAgent to run Bash?",
        Some("HoronAgent wants to run `echo hi` in /w"),
        Some(command_subject("echo hi", Path::new("/w"), Some("call-1"))),
        options,
    );
    assert_matches("RequestPermissionRequest", &request);
    assert_eq!(request["title"], "Allow HoronAgent to run Bash?");
    assert_eq!(request["subject"]["type"], "command");
    assert_eq!(request["subject"]["toolCallId"], "call-1");
    assert_eq!(request["options"][0]["kind"], "allow_once");

    let minimal = permission_request("sess-1", "Allow Edit?", None, None, options);
    assert_matches("RequestPermissionRequest", &minimal);
    assert!(minimal.get("description").is_none() && minimal.get("subject").is_none());
}

#[test]
fn command_subject_matches_schema() {
    let subject = command_subject("cargo test", Path::new("/w"), None);
    assert_matches("CommandPermissionSubject", &subject);
    assert_eq!(subject["command"], "cargo test");
    assert_eq!(subject["cwd"], "/w");
    assert!(subject.get("toolCallId").is_none());
}

#[test]
fn tool_call_subject_matches_schema() {
    let tool_call = tool_call_update(
        "call-1",
        &ToolCallPatch::created("Reading /w/config.json", "read", "read"),
    );
    let subject = tool_call_subject(tool_call);
    assert_matches("ToolCallPermissionSubject", &subject);
    assert_eq!(subject["toolCall"]["toolCallId"], "call-1");
}

#[test]
fn plan_update_matches_schema() {
    let update = plan_update(&[
        PlanEntryWire {
            content: "Check for syntax errors".to_string(),
            priority: "high".to_string(),
            status: "pending".to_string(),
        },
        PlanEntryWire {
            content: "Run the tests".to_string(),
            priority: "medium".to_string(),
            status: "in_progress".to_string(),
        },
    ]);
    assert_matches("PlanUpdate", &update);
    assert_update_matches(&update);
    assert_eq!(update["plan"]["type"], "items");
    assert_eq!(update["plan"]["planId"], "todo");
    assert_eq!(update["plan"]["entries"][1]["content"], "Run the tests");
}

#[test]
fn diff_content_matches_schema() {
    let diffs = [
        FileDiffWire {
            path: "/w/added.txt".to_string(),
            old_text: None,
            new_text: "one\n".to_string(),
            deleted: false,
            old_path: None,
            created: false,
        },
        FileDiffWire {
            path: "modified.txt".to_string(),
            old_text: Some("old\n".to_string()),
            new_text: "new\n".to_string(),
            deleted: false,
            old_path: None,
            created: false,
        },
        FileDiffWire {
            path: "/w/gone.txt".to_string(),
            old_text: Some("gone\n".to_string()),
            new_text: String::new(),
            deleted: true,
            old_path: None,
            created: false,
        },
        FileDiffWire {
            path: "/w/new-name.txt".to_string(),
            old_text: Some("same\n".to_string()),
            new_text: "same\n".to_string(),
            deleted: false,
            old_path: Some("/w/old-name.txt".to_string()),
            created: false,
        },
        // apply_patch reads a missing file as empty text: the marker, not the
        // absent old text, makes it an add.
        FileDiffWire {
            path: "/w/patched-in.txt".to_string(),
            old_text: Some(String::new()),
            new_text: "two\n".to_string(),
            deleted: false,
            old_path: None,
            created: true,
        },
    ];
    let content = diff_content(&diffs, Path::new("/w"));
    assert_matches("ToolCallContent", &content);
    assert_eq!(
        content["changes"],
        json!([
            { "operation": "add", "path": "/w/added.txt", "fileType": "text" },
            { "operation": "modify", "path": "/w/modified.txt", "fileType": "text" },
            { "operation": "delete", "path": "/w/gone.txt", "fileType": "text" },
            {
                "operation": "move",
                "oldPath": "/w/old-name.txt",
                "path": "/w/new-name.txt",
                "fileType": "text"
            },
            { "operation": "add", "path": "/w/patched-in.txt", "fileType": "text" },
        ])
    );
    assert!(
        content.get("oldText").is_none() && content.get("newText").is_none(),
        "v2 carries no snippet text: {content}"
    );
    assert_eq!(content["patch"]["format"], "git_patch");
    let patch = content["patch"]["text"].as_str().expect("patch text");
    assert!(patch.contains("diff --git /w/added.txt /w/added.txt"), "{patch}");
    assert!(patch.contains("new file mode 100644"), "{patch}");
    assert!(
        patch.contains("diff --git /w/patched-in.txt /w/patched-in.txt\nnew file mode 100644\n"),
        "{patch}"
    );
    assert!(patch.contains("--- /dev/null\n+++ /w/patched-in.txt"), "{patch}");
    assert!(patch.contains("deleted file mode 100644"), "{patch}");
    assert!(patch.contains("rename from /w/old-name.txt"), "{patch}");
    assert!(patch.contains("+++ /dev/null"), "{patch}");
    assert!(!patch.contains("a/w/") && !patch.contains("b/w/"), "{patch}");
}

#[test]
fn terminal_content_matches_schema() {
    let content = terminal_content("term-1");
    assert_matches("ToolCallContent", &content);
    assert_eq!(content, json!({ "type": "terminal", "terminalId": "term-1" }));
}

#[test]
fn terminal_started_matches_schema() {
    let started = terminal_started("term-1", "cargo test", Path::new("/w"));
    assert_matches("TerminalUpdate", &started);
    assert_update_matches(&started);
    assert_eq!(started["command"], "cargo test");
    assert_eq!(started["cwd"], "/w");
    assert!(started.get("output").is_none() && started.get("exitStatus").is_none());
}

#[test]
fn terminal_output_matches_schema() {
    let output = terminal_output("term-1", "hello\n");
    assert_matches("TerminalUpdate", &output);
    assert_eq!(output["output"]["data"], "aGVsbG8K");
    assert!(
        output.get("exitStatus").is_none(),
        "a background snapshot carries no exit status"
    );
}

#[test]
fn terminal_finished_matches_schema() {
    let finished = terminal_finished("term-1", "hello\n", Some(0), None);
    assert_matches("TerminalUpdate", &finished);
    assert_eq!(finished["output"]["data"], "aGVsbG8K");
    assert_eq!(finished["exitStatus"], json!({ "exitCode": 0, "signal": null }));
    let signalled = terminal_finished("term-1", "", None, Some("SIGTERM"));
    assert_matches("TerminalUpdate", &signalled);
    assert_eq!(
        signalled["exitStatus"],
        json!({ "exitCode": null, "signal": "SIGTERM" })
    );
}

#[test]
fn state_running_matches_schema() {
    let running = state_running();
    assert_matches("StateUpdate", &running);
    assert_update_matches(&running);
    assert_eq!(running["state"], "running");
}

#[test]
fn state_requires_action_matches_schema() {
    let waiting = state_requires_action();
    assert_matches("StateUpdate", &waiting);
    assert_update_matches(&waiting);
    assert_eq!(waiting["state"], "requires_action");
}

#[test]
fn state_idle_matches_schema() {
    let ended = state_idle(Some("end_turn"), None);
    assert_matches("StateUpdate", &ended);
    assert_update_matches(&ended);
    assert_eq!(ended["state"], "idle");
    assert_eq!(ended["stopReason"], "end_turn");

    let failed = state_idle(Some("error"), Some((-32000, "no usable model".to_string())));
    assert_matches("StateUpdate", &failed);
    assert_eq!(failed["error"], json!({ "code": -32000, "message": "no usable model" }));

    let bare = state_idle(None, None);
    assert_matches("StateUpdate", &bare);
    assert!(bare.get("stopReason").is_none());
}

#[test]
fn message_ids_advance_per_tool_round() {
    let mut ids = message_ids("msg-1");
    assert_eq!(ids.agent(), "msg-1:a1");
    assert_eq!(ids.thought(), "msg-1:t1");
    assert_eq!(ids.agent(), "msg-1:a1", "chunks of one round share its id");
    ids.advance_after_tool_round();
    assert_eq!(ids.agent(), "msg-1:a2");
    assert_eq!(ids.thought(), "msg-1:t2");
    ids.advance_after_tool_round();
    assert_eq!(ids.agent(), "msg-1:a3");
    let replay = message_ids("msg-1");
    assert_eq!(replay.agent(), "msg-1:a1", "replay starts from the same ids");
}

#[test]
fn user_message_matches_schema() {
    let message = user_message("msg-1", &[json!({ "type": "text", "text": "hi" })]);
    assert_update_matches(&message);
    assert_eq!(message["messageId"], "msg-1");
    assert_eq!(message["content"], json!([{ "type": "text", "text": "hi" }]));
}

#[test]
fn message_chunk_matches_schema() {
    for (kind, expected) in [
        (ChunkKind::AgentMessage, "agent_message_chunk"),
        (ChunkKind::AgentThought, "agent_thought_chunk"),
    ] {
        let chunk = message_chunk(kind, "msg-1:a1", "hello");
        assert_update_matches(&chunk);
        assert_eq!(chunk["sessionUpdate"], expected);
        assert_eq!(chunk["messageId"], "msg-1:a1");
        assert_eq!(chunk["content"], json!({ "type": "text", "text": "hello" }));
    }
}

#[test]
fn whole_message_matches_schema() {
    for (kind, expected) in [
        (MessageKind::UserMessage, "user_message"),
        (MessageKind::AgentMessage, "agent_message"),
        (MessageKind::AgentThought, "agent_thought"),
    ] {
        let message = whole_message(kind, "msg-1:a1", &[json!({ "type": "text", "text": "hi" })]);
        assert_update_matches(&message);
        assert_eq!(message["sessionUpdate"], expected);
        assert_eq!(message["messageId"], "msg-1:a1");
    }
}

#[test]
fn tool_call_update_matches_schema() {
    let created = tool_call_update(
        "call-1",
        &ToolCallPatch::created("Reading /w/config.json", "read", "read"),
    );
    assert_update_matches(&created);
    assert_eq!(created["title"], "Reading /w/config.json");
    assert_eq!(created["name"], "read");
    assert_eq!(created["kind"], "read");
    assert_eq!(created["status"], "pending");

    let patch = ToolCallPatch {
        status: Some("in_progress".to_string()),
        locations: Some(vec![json!({ "path": "/w/config.json", "line": 3 })]),
        raw_input: Some(json!({ "path": "/w/config.json" })),
        content: Some(vec![terminal_content("term-1")]),
        ..ToolCallPatch::default()
    };
    let updated = tool_call_update("call-1", &patch);
    assert_update_matches(&updated);
    assert!(
        updated.get("title").is_none() && updated.get("name").is_none(),
        "an omitted field keeps the stored value: {updated}"
    );
    assert_eq!(updated["content"][0]["type"], "terminal");
    assert_eq!(updated["locations"][0]["path"], "/w/config.json");
}

#[test]
fn tool_call_content_chunk_matches_schema() {
    let content = terminal_content("term-1");
    let chunk = tool_call_content_chunk("call-1", content);
    assert_update_matches(&chunk);
    assert_eq!(chunk["content"]["type"], "terminal");

    let diffs = [FileDiffWire {
        path: "/w/a.txt".to_string(),
        old_text: Some("a\n".to_string()),
        new_text: "b\n".to_string(),
        deleted: false,
        old_path: None,
        created: false,
    }];
    let chunk = tool_call_content_chunk("call-1", diff_content(&diffs, Path::new("/w")));
    assert_update_matches(&chunk);
    assert_eq!(chunk["content"]["type"], "diff");
}

#[test]
fn session_info_matches_schema() {
    let row = session_info(
        "sess-1",
        Path::new("/w"),
        Some("Fix the build"),
        Some("2026-10-27T00:00:00Z"),
        &[],
    );
    assert_matches("SessionInfo", &row);
    assert_eq!(row["sessionId"], "sess-1");
    assert_eq!(row["cwd"], "/w");
    assert_eq!(row["title"], "Fix the build");
    assert!(row.get("additionalDirectories").is_none());

    let full = session_info("sess-2", Path::new("/w"), None, None, &["/w/extra".to_string()]);
    assert_matches("SessionInfo", &full);
    assert_eq!(full["additionalDirectories"], json!(["/w/extra"]));
    assert!(full.get("title").is_none() && full.get("updatedAt").is_none());
    assert_matches("ListSessionsResponse", &json!({ "sessions": [row, full] }));
}

#![cfg(unix)]
//! ACP v1 test infrastructure: the vendored schema validator, the mock client
//! parser, the committed fixtures and the scripted provider turns.

#[path = "support/acp_client.rs"]
mod acp_client;
#[path = "support/acp_daemon.rs"]
mod acp_daemon;
#[path = "support/acp_schema.rs"]
mod acp_schema;
#[path = "support/lifecycle.rs"]
mod lifecycle;

use acp_client::{
    AcpClient, AcpClientError, AcpClientOptions, ClientCapabilities, ElicitationAction, Frame, FrameParser,
    PermissionOutcome, StateTracker,
};
use acp_schema::{Direction, FrameValidator};
use lifecycle::{PLAIN_TEXT, Script, ScriptedProvider};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

fn hex_sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn readme_sha256(readme: &str, file_name: &str) -> String {
    let line = readme
        .lines()
        .find(|line| line.contains(file_name))
        .unwrap_or_else(|| panic!("the fixtures README records no sha256 for {file_name}"));
    line.split('`')
        .find(|token| token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit()))
        .unwrap_or_else(|| panic!("no 64-hex sha256 on the {file_name} row: {line}"))
        .to_string()
}

#[test]
fn fixture_sha256_matches_readme() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("tests/fixtures/acp/README.md")).expect("fixtures README");
    for fixture in [acp_schema::SCHEMA_FIXTURE_PATH, acp_schema::META_FIXTURE_PATH] {
        let file_name = fixture.rsplit('/').next().expect("file name");
        let bytes = std::fs::read(root.join(fixture)).unwrap_or_else(|error| panic!("read {fixture}: {error}"));
        assert_eq!(
            hex_sha256(&bytes),
            readme_sha256(&readme, file_name),
            "{fixture} does not match the sha256 recorded in the fixtures README"
        );
    }
    assert!(readme.contains("schema-v1.26.0"), "the README names the release tag");
    assert!(
        readme.contains("releases/download/schema-v1.26.0/schema.json"),
        "the README records the schema download URL"
    );
    assert!(
        readme.contains("e2d83349bc9e28a4059ea3acefa1c91c966f74e2"),
        "the README records the source commit"
    );
}

#[test]
fn schema_accepts_spec_initialize_response() {
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": false},
            "authMethods": [],
            "agentInfo": {"name": "horon-agent", "version": "0.98.1"},
            "_meta": {"horon-agent": {"usage": {"inputTokens": 3}}},
        }
    });
    let violations = acp_schema::validate_response_to("initialize", &response, Direction::AgentToClient);
    assert!(
        violations.is_empty(),
        "a spec-shaped initialize response must validate: {violations:?}"
    );

    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": true}}},
    });
    let violations = acp_schema::validate_outgoing(&request, Direction::ClientToAgent);
    assert!(
        violations.is_empty(),
        "a spec-shaped initialize request must validate: {violations:?}"
    );

    let notification = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}},
    });
    let violations = acp_schema::validate_outgoing(&notification, Direction::AgentToClient);
    assert!(
        violations.is_empty(),
        "a spec-shaped session/update must validate: {violations:?}"
    );
}

#[test]
fn schema_rejects_session_new_response_without_session_id() {
    let response = json!({"jsonrpc": "2.0", "id": 7, "result": {}});
    let violations = acp_schema::validate_response_to("session/new", &response, Direction::AgentToClient);
    assert!(
        !violations.is_empty(),
        "a session/new response without sessionId must be rejected"
    );
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("sessionId")),
        "the violation names the missing sessionId: {violations:?}"
    );

    let envelope =
        json!({"jsonrpc": "2.0", "id": 7, "result": {"sessionId": "s1"}, "error": {"code": -1, "message": "x"}});
    let violations = acp_schema::validate_response_to("session/new", &envelope, Direction::AgentToClient);
    assert!(
        violations
            .iter()
            .any(|violation| violation.message.contains("both result and error")),
        "a response carrying both result and error must be rejected: {violations:?}"
    );

    let error = json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32602, "message": "bad cwd", "sessionId": "s1"}});
    let violations = acp_schema::validate_response_to("session/new", &error, Direction::AgentToClient);
    assert!(
        violations.iter().any(|violation| violation.path == "/error/sessionId"),
        "an Error object allows only code, message and data: {violations:?}"
    );
}

#[test]
fn schema_flags_unknown_root_key() {
    let with_extra = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "result": {"sessionId": "s1", "models": [{"id": "gpt-5"}]},
    });
    let violations = acp_schema::validate_response_to("session/new", &with_extra, Direction::AgentToClient);
    assert!(
        violations
            .iter()
            .any(|violation| violation.path == "/models" && violation.schema_path.contains("root-keys")),
        "a non-spec root key must be flagged: {violations:?}"
    );

    let with_meta = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "result": {"sessionId": "s1", "_meta": {"horon-agent": {"x": 1}}},
    });
    let violations = acp_schema::validate_response_to("session/new", &with_meta, Direction::AgentToClient);
    assert!(violations.is_empty(), "_meta is always allowed: {violations:?}");

    let request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/new",
        "params": {"cwd": "/tmp", "mcpServers": [], "models": []},
    });
    let violations = acp_schema::validate_outgoing(&request, Direction::ClientToAgent);
    assert!(
        violations.iter().any(|violation| violation.path == "/models"),
        "an unknown request params key must be flagged: {violations:?}"
    );
}

#[test]
fn mock_client_rejects_non_json_stdout_line() {
    let mut parser = FrameParser::new();
    let error = parser.parse("not json").expect_err("a non-JSON line must be rejected");
    assert!(
        matches!(error, AcpClientError::NonJson { .. }),
        "a non-JSON line is a framing error, not a schema error: {error}"
    );
    let error = parser.parse("[1,2,3]").expect_err("a JSON array must be rejected");
    assert!(matches!(error, AcpClientError::NonJson { .. }), "{error}");
    let error = parser
        .parse(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
        .expect_err("a request without protocolVersion must be rejected");
    assert!(matches!(error, AcpClientError::Schema { .. }), "{error}");
}

#[test]
fn mock_client_reports_schema_violation() {
    let mut parser = FrameParser::new();
    parser.note_request(&json!(1), "initialize");
    let error = parser
        .parse(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
        .expect_err("a response without protocolVersion must be rejected");
    match error {
        AcpClientError::Schema { violations, .. } => {
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.to_string().contains("protocolVersion")),
                "the schema violation names protocolVersion: {violations:?}"
            );
        }
        other => panic!("expected a schema violation, got {other}"),
    }
    let error = parser
        .parse(r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"models":[]}}"#)
        .expect_err("an unknown root key must be rejected");
    assert!(matches!(error, AcpClientError::Schema { .. }), "{error}");

    parser.note_request(&json!(2), "session/new");
    let frame = parser
        .parse(r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s1"}}"#)
        .expect("a spec-shaped session/new response parses");
    match frame {
        Frame::Response { result: Ok(result), .. } => assert_eq!(result["sessionId"], json!("s1")),
        other => panic!("expected a successful response, got {other:?}"),
    }
}

#[test]
fn v2_schema_flags_v1_tool_call_variant() {
    let v1_tool_call = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "s1",
            "update": {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read the file", "kind": "read", "status": "pending"},
        },
    });
    let violations = acp_schema::validate_outgoing(&v1_tool_call, Direction::AgentToClient);
    assert!(
        violations.is_empty(),
        "the v1 tool_call variant is valid under the v1 set: {violations:?}"
    );

    let violations = acp_schema::validate_outgoing_version(2, &v1_tool_call, Direction::AgentToClient);
    assert!(
        !violations.is_empty(),
        "a session/update with sessionUpdate \"tool_call\" must be refused under the v2 set"
    );
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("sessionUpdate")),
        "the v2 violation names sessionUpdate: {violations:?}"
    );

    let v2_tool_call = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "s1",
            "update": {"sessionUpdate": "tool_call_update", "toolCallId": "t1", "title": "Read the file", "status": "pending"},
        },
    });
    let violations = acp_schema::validate_outgoing_version(2, &v2_tool_call, Direction::AgentToClient);
    assert!(
        violations.is_empty(),
        "the v2 tool_call_update variant is valid under the v2 set: {violations:?}"
    );

    let extension = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "s1",
            "update": {"sessionUpdate": "_horon-agent_extension"},
        },
    });
    let violations = acp_schema::validate_outgoing_version(2, &extension, Direction::AgentToClient);
    assert!(
        violations.is_empty(),
        "an implementation-specific `_` update value stays valid under the v2 set: {violations:?}"
    );

    let unknown = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": "s1", "update": {"sessionUpdate": "bogus"}},
    });
    let violations = acp_schema::validate_outgoing(&unknown, Direction::AgentToClient);
    assert_eq!(
        violations.len(),
        1,
        "the v1 set refuses an unknown variant once, from its closed union alone: {violations:?}"
    );
    let violations = acp_schema::validate_outgoing_version(2, &unknown, Direction::AgentToClient);
    assert!(
        violations
            .iter()
            .any(|violation| violation.schema_path == "acp-schema-002:update-variants"),
        "the v2 set adds the variant check for an unknown value that does not start with `_`: {violations:?}"
    );
}

#[test]
fn v2_schema_flags_boolean_capability_marker() {
    let boolean_marker = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": 2,
            "info": {"name": "horon-agent", "title": "HoronAgent", "version": "0.98.1"},
            "capabilities": {"session": {"prompt": {"image": true}}},
        },
    });
    let violations =
        acp_schema::validate_response_to_version(2, "initialize", &boolean_marker, Direction::AgentToClient);
    assert!(
        !violations.is_empty(),
        "a v1-style boolean capability marker must be refused under the v2 set"
    );
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("image")),
        "the v2 violation names the capability: {violations:?}"
    );

    let marker = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": 2,
            "info": {"name": "horon-agent", "title": "HoronAgent", "version": "0.98.1"},
            "capabilities": {"session": {"prompt": {"image": {}}}},
        },
    });
    let violations = acp_schema::validate_response_to_version(2, "initialize", &marker, Direction::AgentToClient);
    assert!(
        violations.is_empty(),
        "the v2 {{}} capability marker validates: {violations:?}"
    );

    let params = AcpClientOptions::new(Vec::new())
        .protocol_version(2)
        .auth_terminal(true)
        .capabilities(ClientCapabilities::all())
        .initialize_params();
    assert_eq!(params["protocolVersion"], json!(2), "{params}");
    assert_eq!(params["info"]["name"], json!("mock-acp-client"), "{params}");
    assert_eq!(params["capabilities"]["auth"]["terminal"], json!({}), "{params}");
    assert_eq!(params["capabilities"]["elicitation"]["form"], json!({}), "{params}");
    assert!(
        params.get("clientCapabilities").is_none(),
        "the v2 initialize carries capabilities, not clientCapabilities: {params}"
    );

    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params});
    let violations = acp_schema::validate_outgoing_version(2, &request, Direction::ClientToAgent);
    assert!(
        violations.is_empty(),
        "the mock client's v2 initialize params validate against the v2 set: {violations:?}"
    );
    let violations = acp_schema::validate_outgoing(&request, Direction::ClientToAgent);
    assert!(
        !violations.is_empty(),
        "the same params are not a v1 initialize request: {violations:?}"
    );
}

#[test]
fn v2_client_tracks_state_updates() {
    let mut parser = FrameParser::for_version(2);
    let mut tracker = StateTracker::default();
    let frames = [
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {"sessionUpdate": "state_update", "state": "running"}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {"sessionUpdate": "state_update", "state": "requires_action"}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s2", "update": {"sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": {"type": "text", "text": "hi"}}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {"sessionUpdate": "state_update", "state": "running"}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {"sessionUpdate": "state_update", "state": "idle", "stopReason": "end_turn"}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s2", "update": {"sessionUpdate": "state_update", "state": "idle", "stopReason": "cancelled"}}}),
    ];
    for (index, frame) in frames.iter().enumerate() {
        let line = frame.to_string();
        let parsed = parser
            .parse(&line)
            .unwrap_or_else(|error| panic!("frame {index} must validate under the v2 set: {error}"));
        match parsed {
            Frame::Notification { method, params } => {
                let recorded = tracker.record(&method, &params);
                let is_state_update = params["update"]["sessionUpdate"] == json!("state_update");
                assert_eq!(
                    recorded, is_state_update,
                    "frame {index} is recorded only when it carries a state_update"
                );
            }
            other => panic!("frame {index} is a notification, not {other:?}"),
        }
    }

    let states = |session_id: &str| -> Vec<&str> {
        tracker
            .transitions(session_id)
            .iter()
            .map(|update| update.state.as_str())
            .collect()
    };
    assert_eq!(states("s1"), vec!["running", "requires_action", "running", "idle"]);
    assert_eq!(states("s2"), vec!["idle"]);
    assert!(states("s3").is_empty(), "an unknown session has no transitions");
    assert_eq!(tracker.transitions("s1")[3].stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(tracker.transitions("s2")[0].stop_reason.as_deref(), Some("cancelled"));
    assert_eq!(tracker.cursor("s1"), 4);
    assert_eq!(tracker.idle_since("s1", 0).as_deref(), Some("end_turn"));
    assert_eq!(tracker.idle_since("s1", 2).as_deref(), Some("end_turn"));
    assert_eq!(
        tracker.idle_since("s1", 4),
        None,
        "a cursor after the idle waits for the next one"
    );

    let v1_chunk = json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}}});
    let line = v1_chunk.to_string();
    let error = parser
        .parse(&line)
        .expect_err("a chunk without messageId is a v1 frame, not a v2 one");
    assert!(matches!(error, AcpClientError::Schema { .. }), "{error}");
    assert!(
        !tracker.record(
            "session/update",
            &json!({"sessionId": "s1", "update": {"sessionUpdate": "plan_update"}})
        ),
        "only a state_update is recorded"
    );
}

#[test]
fn v2_batch_answers_interleaved_agent_request() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": "perm-1",
        "method": "session/request_permission",
        "params": {
            "sessionId": "batch-session",
            "title": "Allow the batch probe",
            "options": [{"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"}],
        },
    });
    let answer = json!([{"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "s1"}}]);
    let script = format!(
        "printf '%s\\n' '{request}' '{answer}'\nwhile read -r _; do :; done\n",
        request = request,
        answer = answer,
    );
    let mut client = AcpClient::spawn_peer(
        "sh",
        &["-c", script.as_str()],
        AcpClientOptions::new(Vec::new()).protocol_version(2).deadline(WAIT),
    );

    let entries = [json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": {"cwd": "/tmp"}})];
    let answers = client
        .send_batch(&entries)
        .unwrap_or_else(|error| panic!("send_batch: {error}\nstdin: {:?}", client.stdin_lines()));
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert_eq!(answers[0]["result"]["sessionId"], json!("s1"), "{answers:?}");

    let recorded = client.handlers().permission.requests();
    assert_eq!(
        recorded.len(),
        1,
        "the permission request that arrived before the batch array is recorded: {recorded:?}"
    );
    assert_eq!(recorded[0]["sessionId"], json!("batch-session"), "{recorded:?}");

    let answered = client
        .stdin_lines()
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| line["id"] == json!("perm-1"))
        .expect("the interleaved permission request is answered on stdin");
    assert_eq!(
        answered["result"]["outcome"]["outcome"],
        json!("selected"),
        "{answered}"
    );
    assert_eq!(
        answered["result"]["outcome"]["optionId"],
        json!("allow_once"),
        "{answered}"
    );
}

fn provider_host(provider: &ScriptedProvider) -> String {
    provider
        .base_url()
        .trim_start_matches("http://")
        .trim_end_matches("/v1")
        .to_string()
}

fn connect(provider: &ScriptedProvider) -> TcpStream {
    TcpStream::connect(provider_host(provider)).expect("connect to the scripted provider")
}

fn send_prompt(stream: &mut TcpStream, provider: &ScriptedProvider, body: &str) {
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nhost: {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        provider_host(provider),
        body.len(),
        body
    );
    stream
        .write_all(request.as_bytes())
        .expect("write the provider request");
    stream.flush().expect("flush the provider request");
}

fn read_until(stream: &mut TcpStream, received: &mut String, needle: &str, deadline: Duration) {
    let end = Instant::now() + deadline;
    let mut buffer = [0u8; 4096];
    loop {
        if received.contains(needle) {
            return;
        }
        let remaining = end.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "the provider never wrote {needle:?}; saw: {received}"
        );
        stream
            .set_read_timeout(Some(remaining.min(Duration::from_millis(250))))
            .expect("read timeout");
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => received.push_str(&String::from_utf8_lossy(&buffer[..read])),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("read the provider response: {error}"),
        }
    }
}

fn read_to_eof(stream: &mut TcpStream, deadline: Duration) -> String {
    let end = Instant::now() + deadline;
    let mut response = String::new();
    let mut buffer = [0u8; 4096];
    loop {
        let remaining = end.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "the provider never closed the response: {response}"
        );
        stream
            .set_read_timeout(Some(remaining.min(Duration::from_millis(250))))
            .expect("read timeout");
        match stream.read(&mut buffer) {
            Ok(0) => return response,
            Ok(read) => response.push_str(&String::from_utf8_lossy(&buffer[..read])),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("read the provider response: {error}"),
        }
    }
}

fn prompt_body(text: &str) -> String {
    json!({
        "model": "scripted-model",
        "stream": true,
        "messages": [{"role": "user", "content": text}],
    })
    .to_string()
}

fn tool_output_body(text: &str, call_id: &str, name: &str) -> String {
    json!({
        "model": "scripted-model",
        "stream": true,
        "messages": [
            {"role": "user", "content": text},
            {"role": "assistant", "content": Value::Null, "tool_calls": [
                {"id": call_id, "type": "function", "function": {"name": name, "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": call_id, "content": "tool result"},
        ],
    })
    .to_string()
}

fn sse_frames(response: &str) -> Vec<Value> {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(response);
    body.split("\n\n")
        .filter_map(|chunk| chunk.trim().strip_prefix("data: "))
        .filter(|frame| *frame != "[DONE]")
        .map(|frame| serde_json::from_str::<Value>(frame).expect("each SSE data line is JSON"))
        .collect()
}

#[test]
fn scripted_provider_tool_call_script() {
    let provider = ScriptedProvider::start();
    let needle = "T14_SCRIPTED_TOOL_CALL";
    let arguments = json!({"path": "/tmp/t14-tool.txt"}).to_string();
    let follow_up = "t14 follow-up after the tool result";
    provider.script(
        needle,
        Script::ToolCall {
            name: "read".to_string(),
            arguments_json: arguments.clone(),
            follow_up: follow_up.to_string(),
        },
    );

    let mut stream = connect(&provider);
    send_prompt(&mut stream, &provider, &prompt_body(needle));
    let response = read_to_eof(&mut stream, WAIT);
    // The direct Chat Completions parser reads one delta that opens the call
    // and one that carries its arguments, then the finish reason.
    let frames = sse_frames(&response);
    assert_eq!(
        frames.len(),
        3,
        "one opening delta, one arguments delta, one finish: {response}"
    );
    let call = &frames[0]["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!(call["type"], json!("function"));
    assert_eq!(call["function"]["name"], json!("read"));
    assert_eq!(call["function"]["arguments"], json!(""));
    let delta = &frames[1]["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!(delta["function"]["arguments"], json!(arguments));
    assert_eq!(frames[2]["choices"][0]["finish_reason"], json!("tool_calls"));
    let call_id = call["id"].as_str().expect("call_id").to_string();

    let mut stream = connect(&provider);
    send_prompt(&mut stream, &provider, &tool_output_body(needle, &call_id, "read"));
    let response = read_to_eof(&mut stream, WAIT);
    let frames = sse_frames(&response);
    assert_eq!(
        frames[0]["choices"][0]["delta"]["content"],
        json!(follow_up),
        "the request carrying this call's tool result gets the follow-up reply: {response}"
    );

    let mut stream = connect(&provider);
    send_prompt(&mut stream, &provider, &prompt_body(needle));
    let response = read_to_eof(&mut stream, WAIT);
    assert!(
        response.contains("tool_calls"),
        "a script stays registered, so the same needle replays the tool call: {response}"
    );
}

#[test]
fn scripted_provider_reasoning_script() {
    let provider = ScriptedProvider::start();
    let needle = "T14_SCRIPTED_REASONING";
    provider.script(
        needle,
        Script::Reasoning {
            summary: "checked the fixture".to_string(),
            text: "thinking about the fixture".to_string(),
        },
    );

    let mut stream = connect(&provider);
    send_prompt(&mut stream, &provider, &prompt_body(needle));
    let response = read_to_eof(&mut stream, WAIT);
    // The direct Chat Completions parser reads `reasoning_content` deltas.
    let frames = sse_frames(&response);
    assert!(
        frames
            .iter()
            .any(|frame| frame["choices"][0]["delta"]["reasoning_content"] == json!("thinking about the fixture")),
        "the reasoning text streams: {response}"
    );
    assert_eq!(
        frames.last().expect("a finish frame")["choices"][0]["finish_reason"],
        json!("stop")
    );
}

#[test]
fn scripted_provider_hold_open_keeps_stream_until_close() {
    let provider = ScriptedProvider::start();
    let needle = "T14_HOLD_OPEN";
    let delta = "t14 hold-open delta";
    provider.script(needle, Script::HoldOpen(delta.to_string()));

    let mut stream = connect(&provider);
    send_prompt(&mut stream, &provider, &prompt_body(needle));
    let mut received = String::new();
    read_until(&mut stream, &mut received, "\r\n\r\n", WAIT);
    let head = received.clone();
    assert!(head.starts_with("HTTP/1.1 200"), "the hold-open reply is a 200: {head}");
    assert!(
        !head[..head.find("\r\n\r\n").expect("header terminator")]
            .to_ascii_lowercase()
            .contains("content-length"),
        "the hold-open reply must be close-delimited, not length-framed: {head}"
    );
    read_until(&mut stream, &mut received, delta, WAIT);
    assert!(
        received.contains("chat.completion.chunk"),
        "the first delta is written: {received}"
    );
    provider
        .wait_for_hold_open_opened(WAIT)
        .expect("the provider reports the open stream");

    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .expect("read timeout");
    let mut probe = [0u8; 64];
    match stream.read(&mut probe) {
        Ok(0) => panic!("the stream closed before the client closed it"),
        Ok(read) => panic!("the agent wrote {} unexpected bytes while holding", read),
        Err(error) => assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "the stream must still be open: {error}"
        ),
    }
    assert!(
        provider.hold_open_is_still_open(),
        "the provider has not seen the peer close yet"
    );

    drop(stream);
    provider
        .wait_for_hold_open_closed(WAIT)
        .expect("the provider observes the client closing the stream");
    assert!(!provider.hold_open_is_still_open());
}

/// One real ACP prompt reaches the scripted provider through the daemon's
/// OpenAI-compatible provider profile.
///
/// The daemon owns an isolated home whose `[providers.*]` profile is pointed at
/// the scripted upstream, so the text the client receives can only have come back
/// through that profile: no other model transport is configured in this home.
#[test]
fn acp_prompt_reaches_scripted_provider() {
    let daemon = acp_daemon::AcpDaemon::start("acp");
    let capabilities = ClientCapabilities::default();
    // The adapter answers `session/new` with its configuration options at the root today;
    // the strict validator refuses anything the spec object does not declare unless the run
    // names the extension it knows about.
    let mut client = AcpClient::spawn(
        AcpClientOptions::new(daemon.env_pairs())
            .capabilities(capabilities)
            .extension_root_keys(["configOptions".to_string(), "models".to_string()]),
    );

    let initialized = client
        .request(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": capabilities.to_json()}),
        )
        .expect("initialize");
    assert_eq!(initialized["protocolVersion"], json!(1), "{initialized}");

    let created = client
        .request(
            "session/new",
            json!({"cwd": daemon.home.to_str().expect("a home path"), "mcpServers": []}),
        )
        .unwrap_or_else(|error| panic!("session/new: {error}"));
    let session_id = created["sessionId"].as_str().expect("a session id").to_string();

    let prompted = client
        .request(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "hello"}]}),
        )
        .expect("session/prompt");
    assert_eq!(prompted["stopReason"], json!("end_turn"), "{prompted}");

    let mut text = String::new();
    for notification in client.drain_notifications() {
        if notification["method"] != json!("session/update") {
            continue;
        }
        let update = &notification["params"]["update"];
        if update["sessionUpdate"] == json!("agent_message_chunk")
            && let Some(chunk) = update["content"]["text"].as_str()
        {
            text.push_str(chunk);
        }
    }
    assert!(
        text.contains(PLAIN_TEXT),
        "the scripted reply reached the client as an agent message chunk: {text:?}\ndaemon log:\n{}",
        daemon.logs()
    );
    assert!(
        daemon.provider.request_count() > 0,
        "the scripted provider served the turn:\n{}",
        daemon.logs()
    );
}

const STREAM_PROMPT: &str = "T10_STREAM reason, plan, then edit the fixture";
const STREAM_FINAL_TEXT: &str = "t10 stream turn complete";

static TOOL_CALL_INDEX: AtomicU64 = AtomicU64::new(0);

/// One function call as the Chat Completions stream announces it: a delta
/// that opens the call, then a delta carrying its arguments.
fn function_call_frames(_item_id: &str, call_id: &str, name: &str, arguments: &Value) -> Vec<Value> {
    let index = TOOL_CALL_INDEX.fetch_add(1, Ordering::Relaxed);
    let arguments = arguments.to_string();
    vec![
        lifecycle::chat_chunk(
            json!({"role": "assistant", "tool_calls": [{
                "index": index, "id": call_id, "type": "function",
                "function": {"name": name, "arguments": ""}}]}),
            None,
        ),
        lifecycle::chat_chunk(
            json!({"tool_calls": [{"index": index, "function": {"arguments": arguments}}]}),
            None,
        ),
    ]
}

fn completed() -> Value {
    lifecycle::chat_chunk(json!({}), Some("tool_calls"))
}

/// A turn that reasons, writes a to-do plan and edits a file in one response,
/// then answers with text once the tool results come back.
fn stream_script(file: &str) -> Script {
    let mut frames = vec![lifecycle::chat_chunk(
        json!({"role": "assistant", "reasoning_content": "t10 thinking about the edit"}),
        None,
    )];
    frames.extend(function_call_frames(
        "fc_t10_todo",
        "call_t10_todo",
        "todo",
        &json!({"todos": [
            {"content": "edit the fixture", "status": "in_progress", "priority": "high", "id": "1", "confidence": "plausible"},
            {"content": "report back", "status": "pending", "priority": "low", "id": "2", "confidence": "plausible"}
        ]}),
    ));
    frames.extend(function_call_frames(
        "fc_t10_write",
        "call_t10_write",
        "write",
        &json!({"file_path": file, "content": "after\n"}),
    ));
    frames.push(completed());
    Script::Frames {
        frames,
        follow_up: ("call_t10_todo".to_string(), STREAM_FINAL_TEXT.to_string()),
    }
}

fn session_updates(notifications: &[Value]) -> Vec<&Value> {
    notifications
        .iter()
        .filter(|note| note["method"] == json!("session/update"))
        .map(|note| &note["params"]["update"])
        .collect()
}

/// A real daemon turn against the scripted provider: every stdout frame of the run is
/// valid against the v1.26.0 schema, and the turn streams a thought, a plan,
/// a tool call with an absolute location, a diff and text before `end_turn`.
#[test]
fn acp_prompt_stream_frames_match_schema() {
    let daemon = acp_daemon::AcpDaemon::start("acp");
    let work = daemon.root.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let fixture = work.join("t10-qa.txt");
    std::fs::write(&fixture, "before\n").expect("fixture");
    daemon.provider.script("T10_STREAM", stream_script("t10-qa.txt"));
    let mut client = AcpClient::spawn(AcpClientOptions::new(daemon.env_pairs()));
    client
        .request(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": ClientCapabilities::default().to_json()}),
        )
        .expect("initialize");
    let created = client
        .request(
            "session/new",
            json!({"cwd": work.to_str().expect("a work path"), "mcpServers": []}),
        )
        .unwrap_or_else(|error| panic!("session/new: {error}\n{}", daemon.logs()));
    let session_id = created["sessionId"].as_str().expect("a session id").to_string();
    let prompted = client
        .request(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": STREAM_PROMPT}]}),
        )
        .unwrap_or_else(|error| panic!("session/prompt: {error}\n{}", daemon.logs()));
    assert_eq!(prompted["stopReason"], "end_turn", "{prompted}");
    assert!(prompted.get("usage").is_none(), "no root usage: {prompted}");

    let lines = client.stdout_lines();
    let mut validator = FrameValidator::new(Direction::AgentToClient);
    for (id, method) in [(1, "initialize"), (2, "session/new"), (3, "session/prompt")] {
        validator.note_request(&json!(id), method);
    }
    let mut violations = 0;
    for line in &lines {
        eprintln!("T10-FRAME {line}");
        let value: Value = serde_json::from_str(line).expect("every stdout line is JSON");
        for violation in validator.validate(&value) {
            violations += 1;
            eprintln!("T10-VIOLATION {violation} in {line}");
        }
    }
    eprintln!("T10-SCHEMA frames={} violations={violations}", lines.len());
    assert_eq!(violations, 0, "schema violations in {} frames", lines.len());
    assert!(
        lines.last().is_some_and(|line| line.contains("\"stopReason\"")),
        "the prompt response is the last frame"
    );

    let notifications = client.drain_notifications();
    let updates = session_updates(&notifications);
    let kinds: Vec<&str> = updates
        .iter()
        .filter_map(|update| update["sessionUpdate"].as_str())
        .collect();
    let fixture_path = fixture.display().to_string();
    assert!(
        updates
            .iter()
            .any(|update| update["sessionUpdate"] == "agent_thought_chunk" && update["messageId"].is_string()),
        "{kinds:?}"
    );
    assert!(
        updates.iter().any(|update| update["sessionUpdate"] == "plan"
            && update["entries"][0]
                == json!({"content": "edit the fixture", "priority": "high", "status": "in_progress"})),
        "{updates:?}"
    );
    assert!(
        updates
            .iter()
            .any(|update| update["sessionUpdate"] == "tool_call" && update["name"] == "write"),
        "{updates:?}"
    );
    assert!(
        updates
            .iter()
            .any(|update| update["locations"] == json!([{"path": fixture_path}])),
        "an absolute location for the edited file: {updates:?}"
    );
    let diff = json!({"type": "diff", "path": fixture_path, "oldText": "before\n", "newText": "after\n"});
    assert!(
        updates
            .iter()
            .any(|update| update["sessionUpdate"] == "tool_call_update"
                && update["status"] == "completed"
                && update["content"][0] == diff),
        "the completed write carries its diff: {updates:?}"
    );
    let text: String = updates
        .iter()
        .filter(|update| update["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|update| update["content"]["text"].as_str())
        .collect();
    assert!(text.contains(STREAM_FINAL_TEXT), "{text:?}\n{}", daemon.logs());
    assert_eq!(std::fs::read_to_string(&fixture).expect("fixture"), "after\n");
}

/// `session/cancel` while the provider still holds the response stream open:
/// the prompt answers `cancelled` and nothing of the turn is written after
/// that response.
#[test]
fn acp_cancel_mid_stream_answers_cancelled_without_later_frames() {
    let daemon = acp_daemon::AcpDaemon::start("acp");
    daemon
        .provider
        .script("T10_HOLD", Script::HoldOpen("t10 partial ".to_string()));
    let mut client = AcpClient::spawn(AcpClientOptions::new(daemon.env_pairs()));
    client
        .request("initialize", json!({"protocolVersion": 1}))
        .expect("initialize");
    let created = client
        .request(
            "session/new",
            json!({"cwd": daemon.home.to_str().expect("a home path"), "mcpServers": []}),
        )
        .expect("session/new");
    let session_id = created["sessionId"].as_str().expect("a session id").to_string();
    let prompt_id = json!(client.next_id());
    client
        .send_request(
            prompt_id.clone(),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "T10_HOLD keep streaming"}]}),
        )
        .expect("send the prompt");
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    client
        .notify("session/cancel", json!({"sessionId": session_id}))
        .expect("cancel");
    let response = client
        .await_response("session/prompt", &prompt_id, WAIT)
        .expect("the prompt response");
    assert_eq!(response, json!({"stopReason": "cancelled"}));
    daemon
        .provider
        .wait_for_hold_open_closed(WAIT)
        .expect("the daemon drops the provider stream");
    let barrier = client.request("_horon/barrier", json!({}));
    assert!(
        matches!(&barrier, Err(AcpClientError::Rpc { error, .. }) if error["code"] == json!(-32601)),
        "{barrier:?}"
    );
    let lines = client.stdout_lines();
    let answered_at = lines
        .iter()
        .rposition(|line| line.contains("\"stopReason\""))
        .expect("the prompt response on stdout");
    let late: Vec<&String> = lines[answered_at + 1..]
        .iter()
        .filter(|line| line.contains("session/update"))
        .collect();
    assert!(late.is_empty(), "frames after the cancelled response: {late:?}");
}

/// Printed only when the scripted bash call actually ran: the command text
/// carries `$((6*7))`, never `42`.
const T11_BASH_RAN: &str = "t11-42-ran";
const T11_WAIT: Duration = Duration::from_secs(120);

fn t11_bash_script() -> Script {
    Script::ToolCall {
        name: "bash".to_string(),
        arguments_json: json!({"command": "echo t11-$((6*7))-ran", "intent": "t11 permission fixture"}).to_string(),
        follow_up: "t11 bash turn complete".to_string(),
    }
}

/// One ACP client against a daemon backed by the scripted provider, with every request it
/// sends recorded so the whole stdout can be schema-validated afterwards.
struct T11Run {
    daemon: acp_daemon::AcpDaemon,
    client: AcpClient,
    session_id: String,
    work: std::path::PathBuf,
    sent: Vec<(Value, String)>,
}

impl T11Run {
    fn start(config_approval_mode: Option<&str>) -> Self {
        Self::start_with(config_approval_mode, ClientCapabilities::default())
    }

    fn start_with(config_approval_mode: Option<&str>, capabilities: ClientCapabilities) -> Self {
        let mut daemon = acp_daemon::AcpDaemon::start("acp");
        if let Some(mode) = config_approval_mode {
            let config_path = daemon.home.join("config.toml");
            let mut config = std::fs::read_to_string(&config_path).expect("daemon config");
            assert!(!config.contains("[tools]"), "{config}");
            config.push_str(&format!("\n[tools]\napproval_mode = \"{mode}\"\n"));
            std::fs::write(&config_path, config).expect("write daemon config");
            daemon.restart();
        }
        let work = daemon.root.join("work");
        std::fs::create_dir_all(&work).expect("work dir");
        for needle in [
            "T11_BASH_DEFAULT",
            "T11_BASH_REJECT",
            "T11_BASH_UNKNOWN",
            "T11_BASH_ALLOW",
            "T11_BASH_AUTO",
            "T11_BASH_HOLD",
        ] {
            daemon.provider.script(needle, t11_bash_script());
        }
        daemon.provider.script(
            "T11_WRITE",
            Script::ToolCall {
                name: "write".to_string(),
                arguments_json: json!({"file_path": "t11-write.txt", "content": "t11 written\n"}).to_string(),
                follow_up: "t11 write turn complete".to_string(),
            },
        );
        let client = AcpClient::spawn(AcpClientOptions::new(daemon.env_pairs()).capabilities(capabilities));
        let mut run = Self {
            daemon,
            client,
            session_id: String::new(),
            work,
            sent: Vec::new(),
        };
        run.call(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": capabilities.to_json()}),
        );
        run.new_session();
        run
    }

    /// A fresh session per scripted turn: the scripted provider answers any
    /// request whose history carries an earlier call's output with that
    /// call's follow-up text, so a second tool turn needs an empty history.
    fn new_session(&mut self) {
        let work = self.work.to_str().expect("a work path").to_string();
        let created = self.call("session/new", json!({"cwd": work, "mcpServers": []}));
        assert_eq!(created["modes"]["currentModeId"], "default", "{created}");
        let mode_ids: Vec<&str> = created["modes"]["availableModes"]
            .as_array()
            .expect("availableModes")
            .iter()
            .map(|mode| mode["id"].as_str().expect("a mode id"))
            .collect();
        assert_eq!(mode_ids, ["default", "ask-commands", "auto"], "{created}");
        assert_eq!(created["configOptions"][0]["id"], "mode", "{created}");
        assert_eq!(created["configOptions"][0]["currentValue"], "default", "{created}");
        self.session_id = created["sessionId"].as_str().expect("a session id").to_string();
    }

    fn send(&mut self, method: &str, params: Value) -> Value {
        let id = json!(self.client.next_id());
        self.sent.push((id.clone(), method.to_string()));
        self.client
            .send_request(id.clone(), method, params)
            .expect("send a request");
        id
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.client
            .await_response(method, &id, T11_WAIT)
            .unwrap_or_else(|error| panic!("{method}: {error}\n{}", self.daemon.logs()))
    }

    /// Prompt and return the stop reason with the notifications of the turn.
    fn prompt(&mut self, text: &str) -> (String, Vec<Value>) {
        let params = json!({"sessionId": self.session_id, "prompt": [{"type": "text", "text": text}]});
        let result = self.call("session/prompt", params);
        let stop = result["stopReason"].as_str().expect("a stop reason").to_string();
        (stop, self.client.drain_notifications())
    }

    fn set_mode(&mut self, mode_id: &str) {
        let params = json!({"sessionId": self.session_id, "modeId": mode_id});
        assert_eq!(self.call("session/set_mode", params), json!({}));
    }

    /// The next `session/update` of kind `kind`, skipping others.
    fn next_update(&mut self, kind: &str) -> Value {
        loop {
            let note = self
                .client
                .next_notification(T11_WAIT)
                .unwrap_or_else(|error| panic!("waiting for {kind}: {error}"));
            if note["params"]["update"]["sessionUpdate"] == kind {
                return note["params"]["update"].clone();
            }
        }
    }

    fn permission_requests(&self) -> usize {
        self.client.handlers().permission.requests().len()
    }

    /// Validate every stdout line against the vendored schema and print it.
    fn validate_frames(&self, label: &str) -> usize {
        let lines = self.client.stdout_lines();
        let mut validator = FrameValidator::new(Direction::AgentToClient);
        for (id, method) in &self.sent {
            validator.note_request(id, method);
        }
        let mut violations = 0;
        for line in &lines {
            eprintln!("{label}-FRAME {line}");
            let value: Value = serde_json::from_str(line).expect("every stdout line is JSON");
            for violation in validator.validate(&value) {
                violations += 1;
                eprintln!("{label}-VIOLATION {violation} in {line}");
            }
        }
        eprintln!("{label}-SCHEMA frames={} violations={violations}", lines.len());
        violations
    }
}

fn mentions(notifications: &[Value], needle: &str) -> bool {
    notifications.iter().any(|note| note.to_string().contains(needle))
}

/// IS-7/IS-8 against a real daemon: the three modes gate a bash call exactly as
/// the approval gate behaves, a rejected or unknown answer never runs it, and
/// every stdout frame of the run is valid against the v1.26.0 schema.
#[test]
fn acp_permission_modes_real_daemon() {
    let mut run = T11Run::start(None);

    let (stop, updates) = run.prompt("T11_BASH_DEFAULT run it");
    assert_eq!(stop, "end_turn");
    assert_eq!(
        run.permission_requests(),
        0,
        "default follows the daemon config (approval off)"
    );
    assert!(mentions(&updates, T11_BASH_RAN), "{updates:?}\n{}", run.daemon.logs());

    run.new_session();
    run.set_mode("ask-commands");
    let mode_update = run.next_update("current_mode_update");
    assert_eq!(mode_update["currentModeId"], "ask-commands", "{mode_update}");
    let options_update = run.next_update("config_option_update");
    assert_eq!(
        options_update["configOptions"][0]["currentValue"], "ask-commands",
        "{options_update}"
    );
    run.client
        .handlers()
        .permission
        .script([PermissionOutcome::Selected("reject-once".to_string())]);
    let (stop, updates) = run.prompt("T11_BASH_REJECT run it");
    assert_eq!(stop, "end_turn");
    let requests = run.client.handlers().permission.requests();
    assert_eq!(requests.len(), 1, "ask-commands asks before bash");
    assert_eq!(requests[0]["sessionId"], run.session_id.as_str());
    assert_eq!(requests[0]["toolCall"]["kind"], "execute", "{}", requests[0]);
    assert_eq!(requests[0]["toolCall"]["status"], "pending");
    assert!(
        !mentions(&updates, T11_BASH_RAN),
        "a rejected command never runs: {updates:?}"
    );
    assert!(
        updates.iter().any(|note| {
            let update = &note["params"]["update"];
            update["sessionUpdate"] == "tool_call_update"
                && update["status"] == "failed"
                && update.to_string().contains("denied")
        }),
        "the refused call fails with the denial text: {updates:?}"
    );

    run.new_session();
    run.set_mode("ask-commands");
    run.client
        .handlers()
        .permission
        .script([PermissionOutcome::Selected("allow".to_string())]);
    let (_, updates) = run.prompt("T11_BASH_UNKNOWN run it");
    assert_eq!(run.permission_requests(), 2);
    assert!(
        !mentions(&updates, T11_BASH_RAN),
        "an unknown optionId never approves: {updates:?}"
    );

    run.new_session();
    run.set_mode("ask-commands");
    run.client
        .handlers()
        .permission
        .script([PermissionOutcome::Selected("allow-once".to_string())]);
    let (_, updates) = run.prompt("T11_BASH_ALLOW run it");
    assert_eq!(run.permission_requests(), 3);
    assert!(
        mentions(&updates, T11_BASH_RAN),
        "allow-once runs the command: {updates:?}"
    );

    run.new_session();
    run.set_mode("ask-commands");
    let (_, _) = run.prompt("T11_WRITE write the file");
    assert_eq!(run.permission_requests(), 4, "ask-commands asks before a file mutation");
    assert_eq!(
        std::fs::read_to_string(run.work.join("t11-write.txt")).expect("the write ran"),
        "t11 written\n"
    );

    run.new_session();
    let session_id = run.session_id.clone();
    let options = run.call(
        "session/set_config_option",
        json!({"sessionId": session_id, "configId": "mode", "value": "auto"}),
    );
    assert_eq!(options["configOptions"][0]["currentValue"], "auto", "{options}");
    let (_, updates) = run.prompt("T11_BASH_AUTO run it");
    assert_eq!(run.permission_requests(), 3, "auto never asks");
    assert!(mentions(&updates, T11_BASH_RAN), "{updates:?}");

    let unknown = run
        .client
        .request("session/set_mode", json!({"sessionId": session_id, "modeId": "plan"}));
    assert!(
        matches!(&unknown, Err(AcpClientError::Rpc { error, .. }) if error["code"] == json!(-32602)),
        "{unknown:?}"
    );
    assert_eq!(run.validate_frames("T11"), 0);
}

/// A permission request still open when the prompt is cancelled: the client
/// gets `$/cancel_request` for it, the prompt answers `cancelled`, the command
/// never runs, and the session keeps answering. `default` follows a daemon config
/// of `write`, so bash asks without any mode switch.
#[test]
fn acp_permission_cancel_while_pending_real_daemon() {
    let mut run = T11Run::start(Some("write"));
    run.client.handlers().permission.script([PermissionOutcome::Hold]);
    let session_id = run.session_id.clone();
    let prompt_id = run.send(
        "session/prompt",
        json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "T11_BASH_HOLD run it"}]}),
    );
    let permission_id = run
        .client
        .await_agent_request("session/request_permission", T11_WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", run.daemon.logs()));
    run.client
        .notify("session/cancel", json!({"sessionId": session_id}))
        .expect("cancel");
    let response = run
        .client
        .await_response("session/prompt", &prompt_id, T11_WAIT)
        .expect("the prompt response");
    assert_eq!(response["stopReason"], "cancelled", "{response}");
    let notifications = run.client.drain_notifications();
    assert!(
        notifications
            .iter()
            .any(|note| note["method"] == "$/cancel_request" && note["params"]["requestId"] == permission_id),
        "the open permission request is cancelled: {notifications:?}"
    );
    assert!(!mentions(&notifications, T11_BASH_RAN), "{notifications:?}");

    let mode = run.call(
        "session/set_config_option",
        json!({"sessionId": session_id, "configId": "mode", "value": "auto"}),
    );
    assert_eq!(
        mode["configOptions"][0]["currentValue"], "auto",
        "the session still answers: {mode}"
    );
    assert!(
        run.client
            .stdout_lines()
            .iter()
            .all(|line| !line.contains(T11_BASH_RAN)),
        "the held command never ran"
    );
    assert_eq!(run.permission_requests(), 1);
    assert_eq!(run.validate_frames("T11C"), 0);
}

const T12_BASH_RAN: &str = "t12-42-ran";
const T12_OVERLAY: &str = "t12 overlay text\n";
const T12_DISK: &str = "t12 disk text\n";

/// A T11Run with the todo-12 scripts registered and the two fixture files on
/// disk: `t12-read.txt` (read) and `t12-write.txt` (overwritten).
fn t12_run(capabilities: ClientCapabilities) -> T11Run {
    let run = T11Run::start_with(None, capabilities);
    std::fs::write(run.work.join("t12-read.txt"), T12_DISK).expect("read fixture");
    std::fs::write(run.work.join("t12-write.txt"), "before\n").expect("write fixture");
    let read_path = run.work.join("t12-read.txt").display().to_string();
    let provider = &run.daemon.provider;
    provider.script(
        "T12_READ",
        Script::ToolCall {
            name: "read".to_string(),
            arguments_json: json!({"file_path": read_path}).to_string(),
            follow_up: "t12 read turn complete".to_string(),
        },
    );
    provider.script(
        "T12_WRITE",
        Script::ToolCall {
            name: "write".to_string(),
            arguments_json: json!({"file_path": "t12-write.txt", "content": "t12 written\n"}).to_string(),
            follow_up: "t12 write turn complete".to_string(),
        },
    );
    for needle in ["T12_BASH", "T12_BASH_HOLD"] {
        provider.script(
            needle,
            Script::ToolCall {
                name: "bash".to_string(),
                arguments_json: json!({"command": "echo t12-$((6*7))-ran", "intent": "t12 terminal fixture"})
                    .to_string(),
                follow_up: "t12 bash turn complete".to_string(),
            },
        );
    }
    provider.script(
        "T12_ASK",
        Script::ToolCall {
            name: "ask".to_string(),
            arguments_json: json!({"questions": [{"question": "Pick a color", "header": "Color",
                "options": [{"label": "Red"}, {"label": "Blue"}], "recommended": 0}]})
            .to_string(),
            follow_up: "t12 ask turn complete".to_string(),
        },
    );
    run
}

fn update_of(notifications: &[Value], matches: impl Fn(&Value) -> bool) -> Option<&Value> {
    notifications
        .iter()
        .map(|note| &note["params"]["update"])
        .find(|update| matches(update))
}

/// IS-9/IS-10/IS-11 against a real daemon with a client that advertises fs,
/// terminal and form elicitation: text reads and writes go through `fs/*`, a
/// bash call runs in the client terminal and the tool call embeds it, an `ask`
/// is answered through `elicitation/create`, and every stdout frame of the run
/// is valid against the v1.26.0 schema.
#[test]
fn acp_client_requests_real_daemon() {
    let mut run = t12_run(ClientCapabilities::all());
    let read_path = run.work.join("t12-read.txt");
    run.client.handlers().fs.insert(read_path.clone(), T12_OVERLAY);

    let (stop, updates) = run.prompt("T12_READ read the fixture");
    assert_eq!(stop, "end_turn");
    assert_eq!(run.client.handlers().fs.read_paths(), std::slice::from_ref(&read_path));
    assert!(
        mentions(&updates, "t12 overlay text"),
        "the overlay is what the tool read: {updates:?}"
    );
    assert!(!mentions(&updates, "t12 disk text"), "{updates:?}");

    run.new_session();
    let (stop, _) = run.prompt("T12_WRITE write the fixture");
    assert_eq!(stop, "end_turn");
    let write_path = run.work.join("t12-write.txt");
    assert_eq!(
        run.client.handlers().fs.writes(),
        [(write_path.clone(), "t12 written\n".to_string())]
    );
    assert_eq!(
        std::fs::read_to_string(&write_path).expect("fixture"),
        "before\n",
        "the client holds the write, the disk is untouched"
    );

    run.new_session();
    let (stop, updates) = run.prompt("T12_BASH run it");
    assert_eq!(stop, "end_turn");
    assert_eq!(
        run.client.handlers().terminal.calls(),
        ["create", "wait_for_exit", "output", "release"]
    );
    let record = run
        .client
        .handlers()
        .terminal
        .record("term-1")
        .expect("the client terminal");
    assert!(record.released, "{record:?}");
    assert!(record.output.contains(T12_BASH_RAN), "{record:?}");
    let terminal = json!({"type": "terminal", "terminalId": "term-1"});
    let embedded = update_of(&updates, |update| {
        update["sessionUpdate"] == "tool_call_update" && update["content"] == json!([terminal])
    });
    assert!(embedded.is_some(), "the tool call embeds the terminal: {updates:?}");
    let completed = update_of(&updates, |update| update["status"] == "completed").expect("completed");
    assert_eq!(completed["content"][0], terminal, "{completed}");
    assert!(mentions(&updates, T12_BASH_RAN), "{updates:?}");

    run.new_session();
    run.client
        .handlers()
        .elicitation
        .script([ElicitationAction::Accept(json!({"q1": "Blue"}))]);
    let (stop, updates) = run.prompt("T12_ASK ask me");
    assert_eq!(stop, "end_turn");
    let asked = run.client.handlers().elicitation.requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["mode"], "form");
    assert_eq!(asked[0]["sessionId"], run.session_id.as_str());
    assert_eq!(
        asked[0]["requestedSchema"]["properties"]["q1"]["enum"],
        json!(["Red", "Blue"])
    );
    assert!(
        mentions(&updates, "-> Blue"),
        "the human answer reached the tool: {updates:?}"
    );
    assert_eq!(run.validate_frames("T12"), 0);
}

/// The same scripts with a client that advertises nothing: no client request
/// is ever sent, the read sees disk, bash runs locally and `ask` resolves to
/// its recommendation.
#[test]
fn acp_client_requests_fall_back_without_capabilities_real_daemon() {
    let mut run = t12_run(ClientCapabilities::default());
    let (stop, updates) = run.prompt("T12_READ read the fixture");
    assert_eq!(stop, "end_turn");
    assert!(mentions(&updates, "t12 disk text"), "{updates:?}");

    run.new_session();
    let (stop, _) = run.prompt("T12_WRITE write the fixture");
    assert_eq!(stop, "end_turn");
    assert_eq!(
        std::fs::read_to_string(run.work.join("t12-write.txt")).expect("fixture"),
        "t12 written\n"
    );

    run.new_session();
    let (stop, updates) = run.prompt("T12_BASH run it");
    assert_eq!(stop, "end_turn");
    assert!(mentions(&updates, T12_BASH_RAN), "the command ran locally: {updates:?}");
    assert!(!mentions(&updates, "\"terminal\""), "{updates:?}");

    run.new_session();
    let (stop, updates) = run.prompt("T12_ASK ask me");
    assert_eq!(stop, "end_turn");
    assert!(mentions(&updates, "-> Red"), "the recommendation answers: {updates:?}");

    let handlers = run.client.handlers();
    assert!(handlers.fs.read_paths().is_empty() && handlers.fs.writes().is_empty());
    assert!(handlers.terminal.calls().is_empty());
    assert!(handlers.elicitation.requests().is_empty());
    let lines = run.client.stdout_lines();
    for method in ["fs/", "terminal/", "elicitation/"] {
        assert!(
            lines
                .iter()
                .all(|line| !line.contains(&format!("\"method\":\"{method}"))),
            "no {method} request without the capability"
        );
    }
    assert_eq!(run.validate_frames("T12F"), 0);
}

/// `session/cancel` while `terminal/wait_for_exit` is still open: the client
/// gets `$/cancel_request` for it, the prompt answers `cancelled`, the terminal
/// is still released, and the session keeps answering.
#[test]
fn acp_client_terminal_cancel_while_pending_real_daemon() {
    let mut run = t12_run(ClientCapabilities::all());
    run.client.handlers().hold("terminal/wait_for_exit");
    let session_id = run.session_id.clone();
    let prompt_id = run.send(
        "session/prompt",
        json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "T12_BASH_HOLD run it"}]}),
    );
    let wait_id = run
        .client
        .await_agent_request("terminal/wait_for_exit", T11_WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", run.daemon.logs()));
    run.client
        .notify("session/cancel", json!({"sessionId": session_id}))
        .expect("cancel");
    let response = run
        .client
        .await_response("session/prompt", &prompt_id, T11_WAIT)
        .expect("the prompt response");
    assert_eq!(response["stopReason"], "cancelled", "{response}");
    let notifications = run.client.drain_notifications();
    assert!(
        notifications
            .iter()
            .any(|note| note["method"] == "$/cancel_request" && note["params"]["requestId"] == wait_id),
        "the open wait is withdrawn: {notifications:?}"
    );
    if !run
        .client
        .handlers()
        .terminal
        .calls()
        .iter()
        .any(|call| call == "release")
    {
        run.client
            .await_agent_request("terminal/release", T11_WAIT)
            .unwrap_or_else(|error| panic!("{error}\n{}", run.daemon.logs()));
    }
    let record = run
        .client
        .handlers()
        .terminal
        .record("term-1")
        .expect("the client terminal");
    assert!(
        record.released,
        "a cancelled call still releases its terminal: {record:?}"
    );

    let mode = run.call(
        "session/set_config_option",
        json!({"sessionId": session_id, "configId": "mode", "value": "auto"}),
    );
    assert_eq!(mode["configOptions"][0]["currentValue"], "auto", "{mode}");
    assert_eq!(run.validate_frames("T12C"), 0);
}

fn initialized_client(daemon: &acp_daemon::AcpDaemon, capabilities: Value) -> AcpClient {
    let mut client = AcpClient::spawn(AcpClientOptions::new(daemon.env_pairs()));
    client
        .request(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": capabilities}),
        )
        .expect("initialize");
    client
}

fn new_session(client: &mut AcpClient, daemon: &acp_daemon::AcpDaemon, work: &std::path::Path) -> String {
    let created = client
        .request(
            "session/new",
            json!({"cwd": work.to_str().expect("a work path"), "mcpServers": []}),
        )
        .unwrap_or_else(|error| panic!("session/new: {error}\n{}", daemon.logs()));
    created["sessionId"].as_str().expect("a session id").to_string()
}

fn prompt_text(client: &mut AcpClient, daemon: &acp_daemon::AcpDaemon, session_id: &str, text: &str) -> Value {
    client
        .request(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]}),
        )
        .unwrap_or_else(|error| panic!("session/prompt: {error}\n{}", daemon.logs()))
}

/// The last lines of the daemon's own log files, for a failure message.
fn home_log_tail(daemon: &acp_daemon::AcpDaemon) -> String {
    let mut logs: Vec<std::path::PathBuf> = std::fs::read_dir(daemon.home.join("logs"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    logs.sort();
    logs.iter()
        .map(|log| {
            let text = std::fs::read_to_string(log).unwrap_or_default();
            let lines: Vec<&str> = text.lines().filter(|line| !line.contains("TIMING")).collect();
            format!(
                "--- {} ---\n{}",
                log.display(),
                lines[lines.len().saturating_sub(40)..].join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `session/load` on a fresh client. Returns the updates written before the
/// load response (the client queues notifications until the response).
fn load_session(
    client: &mut AcpClient,
    daemon: &acp_daemon::AcpDaemon,
    session_id: &str,
    work: &std::path::Path,
) -> Vec<Value> {
    let loaded = client
        .request(
            "session/load",
            json!({"sessionId": session_id, "cwd": work.to_str().expect("a work path"), "mcpServers": []}),
        )
        .unwrap_or_else(|error| panic!("session/load: {error}\n{}\n{}", daemon.logs(), home_log_tail(daemon)));
    assert!(loaded.is_object(), "{loaded}");
    session_updates(&client.drain_notifications())
        .into_iter()
        .cloned()
        .collect()
}

/// Every stdout line of `client`, schema-validated with its requests noted.
fn count_violations(client: &AcpClient, label: &str, requests: &[(i64, &str)]) -> usize {
    let mut validator = FrameValidator::new(Direction::AgentToClient);
    for (id, method) in requests {
        validator.note_request(&json!(id), method);
    }
    let lines = client.stdout_lines();
    let mut violations = 0;
    for line in &lines {
        eprintln!("{label}-FRAME {line}");
        let value: Value = serde_json::from_str(line).expect("every stdout line is JSON");
        for violation in validator.validate(&value) {
            violations += 1;
            eprintln!("{label}-VIOLATION {violation} in {line}");
        }
    }
    eprintln!("{label}-SCHEMA frames={} violations={violations}", lines.len());
    violations
}

/// The `session/load` response line comes after every replayed update.
fn assert_replay_precedes_response(client: &AcpClient, load_id: i64) {
    let lines: Vec<Value> = client
        .stdout_lines()
        .iter()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect();
    let response = lines
        .iter()
        .position(|line| line.get("method").is_none() && line["id"] == json!(load_id))
        .expect("the load response");
    let last_replay = lines.iter().rposition(|line| {
        line["method"] == "session/update" && line["params"]["update"]["sessionUpdate"] != "available_commands_update"
    });
    assert!(
        last_replay.is_none_or(|last| last < response),
        "a replayed update came after the load response: {lines:?}"
    );
}

fn of_kind<'a>(updates: &'a [Value], kind: &str) -> Vec<&'a Value> {
    updates
        .iter()
        .filter(|update| update["sessionUpdate"] == kind)
        .collect()
}

fn joined_text(updates: &[Value], kind: &str) -> String {
    of_kind(updates, kind)
        .iter()
        .filter_map(|update| update["content"]["text"].as_str())
        .collect()
}

/// A live turn with reasoning, a plan, an edit with a diff and text, then a
/// second connection loads the session: the replay carries the same structure
/// before the load response, and both connections' frames are schema-valid.
#[test]
fn acp_load_replay_matches_live_turn() {
    let daemon = acp_daemon::AcpDaemon::start("acp");
    let work = daemon.root.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let fixture = work.join("t10-qa.txt");
    std::fs::write(&fixture, "before\n").expect("fixture");
    daemon.provider.script("T10_STREAM", stream_script("t10-qa.txt"));

    let mut live_client = initialized_client(&daemon, ClientCapabilities::default().to_json());
    let session_id = new_session(&mut live_client, &daemon, &work);
    let prompted = prompt_text(&mut live_client, &daemon, &session_id, STREAM_PROMPT);
    assert_eq!(prompted["stopReason"], "end_turn", "{prompted}");
    let live: Vec<Value> = session_updates(&live_client.drain_notifications())
        .into_iter()
        .cloned()
        .collect();
    let live_violations = count_violations(
        &live_client,
        "T13-LIVE",
        &[(1, "initialize"), (2, "session/new"), (3, "session/prompt")],
    );
    live_client.close_stdin();
    live_client.wait_exit(WAIT).expect("the live client exits");

    let mut loader = initialized_client(&daemon, ClientCapabilities::default().to_json());
    let replay = load_session(&mut loader, &daemon, &session_id, &work);
    let replay_violations = count_violations(&loader, "T13-REPLAY", &[(1, "initialize"), (2, "session/load")]);
    assert_replay_precedes_response(&loader, 2);
    assert_eq!((live_violations, replay_violations), (0, 0));

    let kinds = |updates: &[Value]| -> Vec<String> {
        updates
            .iter()
            .map(|update| update["sessionUpdate"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    eprintln!("T13-LIVE-KINDS {:?}", kinds(&live));
    eprintln!("T13-REPLAY-KINDS {:?}", kinds(&replay));

    assert_eq!(replay[0]["sessionUpdate"], "user_message_chunk", "{replay:?}");
    assert_eq!(replay[0]["content"]["text"], STREAM_PROMPT);

    let thought = joined_text(&live, "agent_thought_chunk");
    assert!(!thought.is_empty(), "the live turn reasoned: {live:?}");
    assert_eq!(joined_text(&replay, "agent_thought_chunk"), thought);
    assert_eq!(
        joined_text(&replay, "agent_message_chunk"),
        joined_text(&live, "agent_message_chunk")
    );

    let call_ids = |updates: &[Value]| -> Vec<String> {
        of_kind(updates, "tool_call")
            .iter()
            .map(|update| update["toolCallId"].as_str().expect("toolCallId").to_string())
            .collect()
    };
    assert_eq!(call_ids(&replay), call_ids(&live));
    assert_eq!(call_ids(&replay), ["call_t10_todo", "call_t10_write"]);

    let live_plan = of_kind(&live, "plan").last().map(|update| update["entries"].clone());
    let replay_plan = of_kind(&replay, "plan").last().map(|update| update["entries"].clone());
    assert!(live_plan.is_some(), "{live:?}");
    assert_eq!(replay_plan, live_plan);

    let fixture_path = fixture.display().to_string();
    let replayed_write = of_kind(&replay, "tool_call")
        .into_iter()
        .find(|update| update["toolCallId"] == "call_t10_write")
        .expect("the replayed write call");
    assert_eq!(replayed_write["locations"], json!([{"path": fixture_path}]));
    assert_eq!(replayed_write["kind"], "edit");
    let live_input = live
        .iter()
        .rfind(|update| update["toolCallId"] == "call_t10_write" && update.get("rawInput").is_some())
        .map(|update| update["rawInput"].clone())
        .expect("the live write input");
    assert_eq!(replayed_write["rawInput"], live_input);

    let finals = |updates: &[Value], id: &str| -> Value {
        updates
            .iter()
            .rfind(|update| update["sessionUpdate"] == "tool_call_update" && update["toolCallId"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no final update for {id}: {updates:?}"))
    };
    for id in ["call_t10_todo", "call_t10_write"] {
        assert_eq!(finals(&replay, id)["status"], finals(&live, id)["status"]);
    }
    let live_write = finals(&live, "call_t10_write");
    let replay_write = finals(&replay, "call_t10_write");
    assert_eq!(live_write["content"][0]["type"], "diff");
    let stored_result = replay_write["content"][0]["content"]["text"]
        .as_str()
        .expect("the stored result text");
    assert!(
        stored_result.contains("+after"),
        "the stored result keeps the unified diff text: {stored_result}"
    );
    eprintln!(
        "T13-COMPARE live_diff={} replay_content={}",
        live_write["content"], replay_write["content"]
    );
}

/// A second connection that loads an attached session gets the replay; the
/// connection already attached gets nothing, and a resume replays nothing.
#[test]
fn acp_load_replays_only_to_the_loading_connection() {
    let daemon = acp_daemon::AcpDaemon::start("acp");
    let work = daemon.root.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let mut attached = initialized_client(&daemon, ClientCapabilities::default().to_json());
    let session_id = new_session(&mut attached, &daemon, &work);
    let prompted = prompt_text(&mut attached, &daemon, &session_id, "T13 attached hello");
    assert_eq!(prompted["stopReason"], "end_turn", "{prompted}");
    attached.drain_notifications();

    let mut loader = initialized_client(&daemon, ClientCapabilities::default().to_json());
    let replay = load_session(&mut loader, &daemon, &session_id, &work);
    assert_eq!(
        of_kind(&replay, "user_message_chunk")[0]["content"]["text"],
        "T13 attached hello"
    );
    assert!(!of_kind(&replay, "agent_message_chunk").is_empty(), "{replay:?}");
    assert_replay_precedes_response(&loader, 2);

    // A request round trip on the attached connection flushes anything the
    // load caused there.
    attached
        .request("session/list", json!({"cwd": work.to_str().expect("a work path")}))
        .expect("session/list");
    let leaked = session_updates(&attached.drain_notifications()).len();
    eprintln!("T13-OTHER-CLIENT updates_after_foreign_load={leaked}");
    assert_eq!(leaked, 0, "the attached client saw the other connection's replay");

    let mut resumer = initialized_client(&daemon, ClientCapabilities::default().to_json());
    let resumed = resumer
        .request(
            "session/resume",
            json!({"sessionId": session_id, "cwd": work.to_str().expect("a work path"), "mcpServers": []}),
        )
        .unwrap_or_else(|error| panic!("session/resume: {error}\n{}", daemon.logs()));
    assert!(resumed.is_object(), "{resumed}");
    let resumed_updates = session_updates(&resumer.drain_notifications()).len();
    eprintln!("T13-RESUME updates_before_response={resumed_updates}");
    assert_eq!(resumed_updates, 0, "resume must not replay");
    assert_eq!(
        count_violations(&loader, "T13-OTHER-LOAD", &[(1, "initialize"), (2, "session/load")]),
        0
    );
}

/// A turn cancelled mid-stream is stored; loading the session replays it and
/// answers.
#[test]
fn acp_load_replays_a_cancelled_turn() {
    let daemon = acp_daemon::AcpDaemon::start("acp");
    let work = daemon.root.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    daemon
        .provider
        .script("T13_HOLD", Script::HoldOpen("t13 partial ".to_string()));
    let mut client = initialized_client(&daemon, ClientCapabilities::default().to_json());
    let session_id = new_session(&mut client, &daemon, &work);
    let prompt_id = json!(client.next_id());
    client
        .send_request(
            prompt_id.clone(),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "T13_HOLD cancel me"}]}),
        )
        .expect("send the prompt");
    daemon
        .provider
        .wait_for_hold_open_opened(WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    client
        .notify("session/cancel", json!({"sessionId": session_id}))
        .expect("cancel");
    let answered = client
        .await_response("session/prompt", &prompt_id, WAIT)
        .unwrap_or_else(|error| panic!("prompt: {error}\n{}", daemon.logs()));
    assert_eq!(answered["stopReason"], "cancelled", "{answered}");
    client.close_stdin();
    client.wait_exit(WAIT).expect("the client exits");

    let mut loader = initialized_client(&daemon, ClientCapabilities::default().to_json());
    let replay = load_session(&mut loader, &daemon, &session_id, &work);
    eprintln!("T13-CANCELLED-REPLAY {replay:?}");
    assert_eq!(
        of_kind(&replay, "user_message_chunk")[0]["content"]["text"],
        "T13_HOLD cancel me"
    );
    assert_replay_precedes_response(&loader, 2);
    assert_eq!(
        count_violations(&loader, "T13-CANCELLED", &[(1, "initialize"), (2, "session/load")]),
        0
    );
}

/// A persisted session of `turns` question/answer pairs whose first
/// `covered` messages are compacted, written through the store the way the
/// daemon writes one, since the scripted provider offers no manual compaction.
fn persisted_compacted_session(
    home: &std::path::Path,
    work: &std::path::Path,
    session_id: &str,
    turns: usize,
    covered: usize,
) {
    use horon_agent::session::event_store_api as store;
    let paths = store::SessionPaths::new(home.join("sessions"));
    let id = store::SessionId::parse(session_id).expect("session id");
    let mut writer = store::BlockingWriter::open(paths.clone(), id.clone()).expect("session writer");
    let metadata = store::SessionMetadata {
        working_dir: Some(work.display().to_string()),
        ..store::SessionMetadata::default()
    };
    let root = store::EventDraft::new(
        id.clone(),
        store::EventId::new_v4(),
        None,
        store::OriginId::new_v4(),
        metadata.created_at,
        store::EventKind::new(store::KnownEventKind::SessionCreated),
        json!(store::SessionCreatedPayload {
            branch_root: false,
            reset_provenance: None,
            metadata,
        }),
    )
    .seal()
    .expect("root event");
    writer.append(&root).expect("append root");
    let mut head = root.envelope().event_id.clone();
    let mut message_ids = Vec::new();
    for turn in 0..turns {
        for (role, text) in [
            ("user", format!("T13 question {turn}")),
            ("assistant", format!("T13 answer {turn}")),
        ] {
            head = lifecycle::append_message(&mut writer, &id, &head, &format!("m{}", message_ids.len()), role, &text);
            message_ids.push(head.clone());
        }
    }
    let covered_id = message_ids[covered - 1].clone();
    let graph = store::EventGraph::open(&paths, &id).expect("event graph");
    let payload = store::CompactedPayload {
        summary_text: "T13 summary of the early turns".to_string(),
        source_head_id: Some(head.clone()),
        source_prefix_digest: graph.prefix_digest(&head, &covered_id, None).expect("prefix digest"),
        covered_through_event_id: covered_id,
        first_kept_event_id: message_ids.get(covered).cloned(),
        prior_compaction_id: None,
        provider: "openai".to_string(),
        model: "gpt-5".to_string(),
        route: "responses".to_string(),
        settings_digest: "0".repeat(64),
        generation: 0,
        input_tokens: None,
        output_tokens: None,
        outcome: store::CompactionOutcome::Summary,
        native_window: Vec::new(),
        restoration: None,
    };
    let compacted = store::EventDraft::new(
        id.clone(),
        store::EventId::new_v4(),
        Some(head),
        store::OriginId::new_v4(),
        store::SessionMetadata::default().created_at,
        store::EventKind::new(store::KnownEventKind::Compacted),
        json!(payload),
    )
    .seal()
    .expect("compacted event");
    writer.append(&compacted).expect("append the compaction");
}

fn user_prompts(updates: &[Value]) -> Vec<String> {
    of_kind(updates, "user_message_chunk")
        .iter()
        .filter_map(|update| update["content"]["text"].as_str().map(str::to_string))
        .collect()
}

/// A large compacted session: the replay is bounded by the daemon's rendered
/// window (render.rs: 64 compacted rows once the prefix holds more than 80
/// renderable messages and 5 turns) plus the uncompacted tail, in order. A
/// plain client sees no compaction marker; a client that advertised
/// `session.compaction` sees one `compaction_update` first.
#[test]
fn acp_load_replays_a_compacted_session() {
    const TURNS: usize = 120;
    const COVERED: usize = 200;
    let daemon = acp_daemon::AcpDaemon::start("acp");
    let work = daemon.root.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let session_id = "session_acpreplay_1700000000000_0123456789abcdef";
    persisted_compacted_session(&daemon.home, &work, session_id, TURNS, COVERED);

    let mut plain = initialized_client(&daemon, ClientCapabilities::default().to_json());
    let replay = load_session(&mut plain, &daemon, session_id, &work);
    let prompts = user_prompts(&replay);
    eprintln!(
        "T13-COMPACTED-PLAIN updates={} user_prompts={} first={:?} last={:?} stored_messages={}",
        replay.len(),
        prompts.len(),
        prompts.first(),
        prompts.last(),
        TURNS * 2
    );
    assert!(of_kind(&replay, "compaction_update").is_empty());
    assert!(
        replay.len() < TURNS * 2,
        "the compacted prefix is bounded by the render window: {}",
        replay.len()
    );
    let first: usize = prompts[0]
        .strip_prefix("T13 question ")
        .and_then(|turn| turn.parse().ok())
        .expect("a numbered prompt");
    let expected: Vec<String> = (first..TURNS).map(|turn| format!("T13 question {turn}")).collect();
    assert_eq!(
        prompts, expected,
        "a contiguous, ordered window ending at the last turn"
    );
    assert_eq!(
        first,
        (COVERED - 64) / 2,
        "64 compacted rows before the uncompacted tail"
    );
    assert_replay_precedes_response(&plain, 2);
    assert_eq!(
        count_violations(&plain, "T13-COMPACTED-PLAIN", &[(1, "initialize"), (2, "session/load")]),
        0
    );
    plain.close_stdin();
    plain.wait_exit(WAIT).expect("the plain client exits");

    let mut capable = initialized_client(&daemon, json!({"session": {"compaction": {}}}));
    let capable_replay = load_session(&mut capable, &daemon, session_id, &work);
    eprintln!("T13-COMPACTED-CAPABLE first={}", capable_replay[0]);
    assert_eq!(
        capable_replay[0]["sessionUpdate"], "compaction_update",
        "{capable_replay:?}"
    );
    assert_eq!(of_kind(&capable_replay, "compaction_update").len(), 1);
    assert_eq!(capable_replay.len(), replay.len() + 1);
    assert_replay_precedes_response(&capable, 2);
    assert_eq!(
        count_violations(
            &capable,
            "T13-COMPACTED-CAPABLE",
            &[(1, "initialize"), (2, "session/load")]
        ),
        0
    );
}

/// IS-5 (todo 10a) against a real daemon: the leak the todo 11 verifier
/// reproduced. A turn cancelled while its permission request is still open
/// answers `cancelled` on the daemon's pre-Done `TurnStopped`
/// (client_lifecycle.rs publishes it before signalling the worker), so the old
/// turn's withdrawn `tool_call_update` and its `Interrupted`/`Done` are still
/// in flight. The prompt sent immediately after must answer its own
/// `end_turn` and carry none of the old turn's frames. Three rounds, no
/// sleeps: every wait is driven by the frame or the stream event it needs.
#[test]
fn acp_cancel_with_permission_pending_keeps_the_next_prompt_clean_real_daemon() {
    const NEXT: &str = "T10A_NEXT";
    let mut run = T11Run::start(Some("write"));
    run.daemon
        .provider
        .script(NEXT, Script::Text("t10a next turn text".to_string()));
    let mut stops = Vec::new();
    for round in 1..=3 {
        // A fresh session per round: the scripted provider answers a request
        // that carries an earlier call's output with that call's follow-up.
        run.new_session();
        let session_id = run.session_id.clone();
        run.client.handlers().permission.script([PermissionOutcome::Hold]);
        let held = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "T11_BASH_HOLD run it"}]}),
        );
        run.client
            .await_agent_request("session/request_permission", T11_WAIT)
            .unwrap_or_else(|error| panic!("round {round}: {error}\n{}", run.daemon.logs()));
        run.client
            .notify("session/cancel", json!({"sessionId": session_id}))
            .expect("cancel");
        let cancelled = run
            .client
            .await_response("session/prompt", &held, T11_WAIT)
            .unwrap_or_else(|error| panic!("round {round}: the cancelled prompt: {error}\n{}", run.daemon.logs()));
        assert_eq!(cancelled["stopReason"], "cancelled", "round {round}: {cancelled}");
        // Clear the frames the cancelled turn already wrote; whether its
        // withdrawn call is reported before or after this response depends on
        // which side wins the cancel, so only the next turn's window is judged.
        let _ = run.client.drain_notifications();

        // Immediately, while the old turn's tail is still in flight.
        let next = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": format!("{NEXT} round {round}")}]}),
        );
        let response = run
            .client
            .await_response("session/prompt", &next, T11_WAIT)
            .unwrap_or_else(|error| {
                panic!(
                    "round {round}: the prompt after the cancel: {error}\n{}",
                    run.daemon.logs()
                )
            });
        assert_eq!(
            response["stopReason"],
            "end_turn",
            "round {round}: the prompt after a cancel inherited the cancelled turn: {response}\n{}",
            run.daemon.logs()
        );
        stops.push(response["stopReason"].clone());
        let next_turn = run.client.drain_notifications();
        let stale: Vec<&Value> = next_turn
            .iter()
            .filter(|note| {
                let text = note.to_string();
                text.contains("tool_call_update")
                    || text.contains("cancelled before it was answered")
                    || text.contains(T11_BASH_RAN)
            })
            .collect();
        assert!(
            stale.is_empty(),
            "round {round}: the old turn's frames reached the next prompt: {stale:?}"
        );
        let second_text = next_turn
            .iter()
            .any(|note| note["params"]["update"]["sessionUpdate"] == json!("agent_message_chunk"));
        assert!(
            second_text,
            "round {round}: the second turn streamed its own text: {next_turn:?}"
        );
    }
    assert_eq!(stops, [json!("end_turn"), json!("end_turn"), json!("end_turn")]);
    assert_eq!(run.validate_frames("T10A"), 0);
    run.client.close_stdin();
}

fn verify_window(lines: &[String], session_id: &str, from_id: &Value, to_id: &Value) -> (Vec<Value>, Vec<Value>) {
    let parsed: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("json"))
        .collect();
    let is_response = |value: &Value, id: &Value| value.get("method").is_none() && value.get("id") == Some(id);
    let from = parsed
        .iter()
        .position(|value| is_response(value, from_id))
        .expect("from response");
    let to = parsed
        .iter()
        .position(|value| is_response(value, to_id))
        .expect("to response");
    let of_session = |value: &&Value| value["method"] == "session/update" && value["params"]["sessionId"] == session_id;
    let between = parsed[from + 1..to].iter().filter(of_session).cloned().collect();
    let after = parsed[to + 1..].iter().filter(of_session).cloned().collect();
    (between, after)
}

fn verify_barrier(run: &mut T11Run) {
    let id = run.send("_horon/barrier", json!({}));
    let barrier = run.client.await_response("_horon/barrier", &id, T11_WAIT);
    assert!(
        matches!(&barrier, Err(AcpClientError::Rpc { error, .. }) if error["code"] == json!(-32601)),
        "{barrier:?}"
    );
}

#[test]
fn verify_t10a_exact_repro_strict() {
    const NEXT: &str = "VERIFY_NEXT";
    let mut run = T11Run::start(Some("write"));
    run.daemon
        .provider
        .script(NEXT, Script::Text("verify next text".to_string()));
    let mut rounds = Vec::new();
    for round in 1..=3 {
        run.new_session();
        let session_id = run.session_id.clone();
        run.client.handlers().permission.script([PermissionOutcome::Hold]);
        let held = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "T11_BASH_HOLD run it"}]}),
        );
        run.client
            .await_agent_request("session/request_permission", T11_WAIT)
            .unwrap_or_else(|error| panic!("round {round}: {error}\n{}", run.daemon.logs()));
        run.client
            .notify("session/cancel", json!({"sessionId": session_id}))
            .expect("cancel");
        let cancelled = run
            .client
            .await_response("session/prompt", &held, T11_WAIT)
            .unwrap_or_else(|error| panic!("round {round}: {error}\n{}", run.daemon.logs()));
        assert_eq!(cancelled["stopReason"], "cancelled", "round {round}: {cancelled}");
        let next = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": format!("{NEXT} round {round}")}]}),
        );
        let response = run
            .client
            .await_response("session/prompt", &next, T11_WAIT)
            .unwrap_or_else(|error| panic!("round {round}: {error}\n{}", run.daemon.logs()));
        eprintln!("VERIFY-R1 round {round} first={cancelled} second={response}");
        assert_eq!(
            response["stopReason"],
            "end_turn",
            "round {round}: {response}\n{}",
            run.daemon.logs()
        );
        rounds.push((session_id, held, next));
    }
    verify_barrier(&mut run);
    let lines = run.client.stdout_lines();
    for (index, (session_id, held, next)) in rounds.iter().enumerate() {
        let round = index + 1;
        let (between, after) = verify_window(&lines, session_id, held, next);
        eprintln!(
            "VERIFY-R1 round {round} between={} after={}",
            between.len(),
            after.len()
        );
        for value in &between {
            eprintln!("VERIFY-R1 round {round} between {value}");
        }
        let stale: Vec<&Value> = between
            .iter()
            .filter(|value| {
                let text = value.to_string();
                text.contains("tool_call")
                    || text.contains("cancelled before it was answered")
                    || text.contains(T11_BASH_RAN)
            })
            .collect();
        assert!(
            stale.is_empty(),
            "round {round}: old frames after the cancelled response: {stale:?}"
        );
        assert!(
            after.is_empty(),
            "round {round}: frames after the end_turn response: {after:?}"
        );
    }
    let requests = run.daemon.provider.requests();
    let next_requests = requests.iter().filter(|body| body.contains(NEXT)).count();
    let hold_only = requests
        .iter()
        .filter(|body| body.contains("T11_BASH_HOLD") && !body.contains(NEXT))
        .count();
    eprintln!(
        "VERIFY-R1 provider requests total={} next={next_requests} hold_only={hold_only}",
        requests.len()
    );
    assert_eq!(run.validate_frames("VR1"), 0);
    run.client.close_stdin();
}

#[test]
fn verify_t10a_hold_open_cancel_then_immediate_prompt() {
    let mut run = T11Run::start(None);
    run.daemon
        .provider
        .script("VERIFY_HOLD", Script::HoldOpen("verify partial ".to_string()));
    run.daemon
        .provider
        .script("VERIFY_AFTER_HOLD", Script::Text("verify after hold".to_string()));
    let session_id = run.session_id.clone();
    let held = run.send(
        "session/prompt",
        json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "VERIFY_HOLD keep streaming"}]}),
    );
    run.daemon
        .provider
        .wait_for_hold_open_opened(T11_WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", run.daemon.logs()));
    run.client
        .notify("session/cancel", json!({"sessionId": session_id}))
        .expect("cancel");
    let cancelled = run
        .client
        .await_response("session/prompt", &held, T11_WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", run.daemon.logs()));
    assert_eq!(cancelled, json!({"stopReason": "cancelled"}));
    let next = run.send(
        "session/prompt",
        json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "VERIFY_AFTER_HOLD now"}]}),
    );
    let response = run
        .client
        .await_response("session/prompt", &next, T11_WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", run.daemon.logs()));
    eprintln!("VERIFY-R2 first={cancelled} second={response}");
    assert_eq!(response["stopReason"], "end_turn", "{response}\n{}", run.daemon.logs());
    run.daemon
        .provider
        .wait_for_hold_open_closed(T11_WAIT)
        .expect("the daemon drops the held provider stream");
    verify_barrier(&mut run);
    let lines = run.client.stdout_lines();
    let (between, after) = verify_window(&lines, &session_id, &held, &next);
    for value in &between {
        eprintln!("VERIFY-R2 between {value}");
    }
    let texts: Vec<&str> = between
        .iter()
        .filter_map(|value| value["params"]["update"]["content"]["text"].as_str())
        .collect();
    assert_eq!(texts, ["verify after hold"], "{between:?}");
    assert!(!between.iter().any(|value| value.to_string().contains("verify partial")));
    assert!(after.is_empty(), "frames after the end_turn response: {after:?}");
    assert_eq!(run.validate_frames("VR2"), 0);
    run.client.close_stdin();
}

#[test]
fn probe_real_daemon_cancel_finish_race() {
    let mut run = T11Run::start(None);
    run.daemon
        .provider
        .script("RACE_FIRST", Script::Text("race first text".to_string()));
    run.daemon
        .provider
        .script("RACE_SECOND", Script::Text("race second text".to_string()));
    let mut cancelled_rounds = 0;
    let mut lost = Vec::new();
    for round in 1..=30 {
        run.new_session();
        let session_id = run.session_id.clone();
        let first = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": format!("RACE_FIRST {round}")}]}),
        );
        let _ = run.next_update("agent_message_chunk");
        run.client
            .notify("session/cancel", json!({"sessionId": session_id}))
            .expect("cancel");
        let first_response = run
            .client
            .await_response("session/prompt", &first, T11_WAIT)
            .expect("first response");
        let _ = run.client.drain_notifications();
        let second = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": format!("RACE_SECOND {round}")}]}),
        );
        let second_response = run
            .client
            .await_response("session/prompt", &second, T11_WAIT)
            .expect("second response");
        let notes = run.client.drain_notifications();
        let has_second = mentions(&notes, "race second text");
        if first_response["stopReason"] == "cancelled" {
            cancelled_rounds += 1;
        }
        eprintln!(
            "PROBE-REAL round {round} first={} second={} second_text={has_second}",
            first_response["stopReason"], second_response["stopReason"]
        );
        if !has_second {
            lost.push(round);
        }
    }
    eprintln!("PROBE-REAL cancelled_first={cancelled_rounds}/30 lost_second_text={lost:?}");
    eprintln!(
        "PROBE-REAL-LOGS {}",
        run.daemon
            .logs()
            .lines()
            .filter(|line| line.contains("SERVER_INTERRUPT_CANCEL"))
            .collect::<Vec<_>>()
            .join("\nPROBE-REAL-LOGS ")
    );
    assert!(
        lost.is_empty(),
        "rounds whose next turn lost its streamed text: {lost:?}"
    );
    run.client.close_stdin();
}

#[test]
fn t_real_daemon_hold_open_cancel_rounds() {
    let mut run = T11Run::start(None);
    run.daemon
        .provider
        .script("TROUND_HOLD", Script::HoldOpen("t partial ".to_string()));
    run.daemon
        .provider
        .script("TROUND_NEXT", Script::Text("t next text".to_string()));
    let mut slow = Vec::new();
    let mut lost = Vec::new();
    for round in 1..=20 {
        run.new_session();
        let session_id = run.session_id.clone();
        let held = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": format!("TROUND_HOLD {round}")}]}),
        );
        run.daemon
            .provider
            .wait_for_request(&format!("TROUND_HOLD {round}"), T11_WAIT)
            .expect("provider got the held request");
        run.client
            .notify("session/cancel", json!({"sessionId": session_id}))
            .expect("cancel");
        let first = run
            .client
            .await_response("session/prompt", &held, T11_WAIT)
            .expect("first");
        let _ = run.client.drain_notifications();
        let started = std::time::Instant::now();
        let next = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": format!("TROUND_NEXT {round}")}]}),
        );
        let second = run
            .client
            .await_response("session/prompt", &next, T11_WAIT)
            .unwrap_or_else(|error| panic!("round {round}: {error}\n{}", run.daemon.logs()));
        let elapsed = started.elapsed();
        let notes = run.client.drain_notifications();
        let has_text = mentions(&notes, "t next text");
        eprintln!(
            "T-REAL round {round} first={} second={} elapsed={elapsed:?} text={has_text}",
            first["stopReason"], second["stopReason"]
        );
        if elapsed >= Duration::from_millis(2900) {
            slow.push(round);
        }
        if !has_text {
            lost.push(round);
        }
    }
    eprintln!("T-REAL slow={slow:?} lost={lost:?}");
    assert!(lost.is_empty(), "rounds whose next turn lost its text: {lost:?}");
    run.client.close_stdin();
}

/// Single client, real daemon: the editor pattern "session/cancel, then the
/// next session/prompt" right after the previous turn already finished, and a
/// double session/cancel during a held turn followed by the next prompt.
#[test]
fn v5_real_daemon_cancel_note_then_prompt() {
    let mut run = T11Run::start(None);
    run.daemon
        .provider
        .script("V5_DONE", Script::Text("v5 done text".to_string()));
    run.daemon
        .provider
        .script("V5_NEXT", Script::Text("v5 next text".to_string()));
    run.daemon
        .provider
        .script("V5_HOLD", Script::HoldOpen("v5 partial ".to_string()));
    let mut wrong_after_idle_cancel = Vec::new();
    let mut wrong_after_double_cancel = Vec::new();
    for round in 1..=15 {
        // A: the turn finished, then cancel + next prompt back to back.
        run.new_session();
        let session_id = run.session_id.clone();
        let (stop, _) = run.prompt(&format!("V5_DONE {round}"));
        assert_eq!(stop, "end_turn");
        run.client
            .notify("session/cancel", json!({"sessionId": session_id}))
            .expect("cancel");
        let (next_stop, notes) = run.prompt(&format!("V5_NEXT a{round}"));
        let has_text = mentions(&notes, "v5 next text");
        eprintln!("V5-REAL A round {round} next={next_stop} text={has_text}");
        if next_stop != "end_turn" || !has_text {
            wrong_after_idle_cancel.push(round);
        }

        // B: double cancel of a held turn, then the next prompt at once.
        run.new_session();
        let session_id = run.session_id.clone();
        let held = run.send(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": format!("V5_HOLD {round}")}]}),
        );
        run.daemon
            .provider
            .wait_for_request(&format!("V5_HOLD {round}"), T11_WAIT)
            .expect("held request");
        run.client
            .notify("session/cancel", json!({"sessionId": session_id}))
            .expect("cancel 1");
        run.client
            .notify("session/cancel", json!({"sessionId": session_id}))
            .expect("cancel 2");
        let first = run
            .client
            .await_response("session/prompt", &held, T11_WAIT)
            .expect("held answered");
        let _ = run.client.drain_notifications();
        let (next_stop, notes) = run.prompt(&format!("V5_NEXT b{round}"));
        let has_text = mentions(&notes, "v5 next text");
        eprintln!(
            "V5-REAL B round {round} first={} next={next_stop} text={has_text}",
            first["stopReason"]
        );
        if next_stop != "end_turn" || !has_text {
            wrong_after_double_cancel.push(round);
        }
    }
    eprintln!(
        "V5-REAL idle_cancel_wrong={wrong_after_idle_cancel:?} double_cancel_wrong={wrong_after_double_cancel:?}"
    );
    assert!(wrong_after_idle_cancel.is_empty() && wrong_after_double_cancel.is_empty());
    run.client.close_stdin();
}

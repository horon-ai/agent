//! IS-10, IS-12, IS-14 and IS-15 over a scripted daemon: v2 permission
//! requests, config options, form elicitation and the auth methods.

use super::super::tests::connection_core::*;
use super::super::*;
use super::state::V2Session;
use super::tests::{assert_matches, assert_update_matches};

const SID: &str = "s1";
const CWD: &str = "/work";

/// A v2 client that sent `capabilities` (the v2 field name) in `initialize`,
/// and the `initialize` result it was answered.
async fn v2_client_with(capabilities: Value) -> (Client, Value) {
    let mut client = start_client_with(None);
    client
        .send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":2,
            "info":{"name":"t9-client","version":"1.0.0"},
            "capabilities":capabilities}}))
        .await;
    let response = client.recv().await;
    assert_eq!(response["result"]["protocolVersion"], 2, "{response}");
    (client, response["result"].clone())
}

async fn v2_client() -> Client {
    v2_client_with(json!({})).await.0
}

/// A v2 session on a scripted daemon with cwd `/work`, wired as `session/new`
/// wires one.
async fn attach_v2(client: &Client) -> (Arc<DaemonSession>, FakeDaemon) {
    let (session, daemon) = daemon_pair(SID);
    *session.cwd.lock().await = Some(PathBuf::from(CWD));
    let events = session.open_v2_stream();
    assert!(
        session
            .v2
            .set(Arc::new(V2Session::new(events, PROMPT_CANCEL_BOUND)))
            .is_ok()
    );
    client.runtime.install_session_bridge(&session);
    offer_models(&session, 2).await;
    let session = Arc::new(session);
    client
        .runtime
        .sessions
        .lock()
        .await
        .insert(SID.to_string(), Arc::clone(&session));
    client.runtime.start_v2_stream(&session);
    (session, daemon)
}

fn permission_requested(request_id: &str, tool_name: &str, input: Value) -> Value {
    json!({"type":"tool_permission_requested","request_id":request_id,"tool_call_id":format!("call-{request_id}"),
        "tool_name":tool_name,"title":format!("{tool_name}: approval required"),"input":input})
}

fn answered(call: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":call,"result":result})
}

fn selected(option_id: &str) -> Value {
    json!({"outcome":{"outcome":"selected","optionId":option_id}})
}

fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

/// The next frame must be a v2 `session/request_permission`; returns it.
async fn expect_permission_request(client: &mut Client) -> Value {
    let request = client.recv().await;
    assert_eq!(request["method"], "session/request_permission", "{request}");
    assert_matches("RequestPermissionRequest", &request["params"]);
    request
}

async fn expect_decision(daemon: &mut FakeDaemon, request_id: &str, decision: &str) {
    let answer = daemon.recv_type("permission_decision").await;
    assert_eq!(answer["request_id"], request_id, "{answer}");
    assert_eq!(answer["decision"], decision, "{answer}");
    daemon.emit(json!({"type":"ack","id":answer["id"]})).await;
}

/// Every line the session's stream wrote for daemon events sent before this
/// point (the marker's `session_info_update` follows them).
async fn daemon_barrier(client: &mut Client, daemon: &mut FakeDaemon, tag: &str) -> Vec<Value> {
    daemon
        .emit(json!({"type":"session_renamed","session_id":SID,"display_title":tag}))
        .await;
    let mut before = Vec::new();
    loop {
        let line = client.recv().await;
        if line["params"]["update"]["sessionUpdate"] == "session_info_update"
            && line["params"]["update"]["title"] == tag
        {
            return before;
        }
        before.push(line);
    }
}

fn kinds(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line["method"] == "session/update")
        .map(|line| {
            let update = &line["params"]["update"];
            let kind = update["sessionUpdate"].as_str().unwrap_or_default().to_string();
            match update["state"].as_str() {
                Some(state) => format!("{kind}:{state}"),
                None => kind,
            }
        })
        .collect()
}

/// Every option is keyed by `configId`, none by the v1 `id`.
fn assert_no_v1_option_ids(options: &Value) {
    for option in options.as_array().expect("configOptions is an array") {
        assert!(option.get("id").is_none(), "v1 `id` on a v2 option: {option}");
        assert!(option["configId"].is_string(), "{option}");
    }
}

#[tokio::test]
async fn permission_v2_has_title_and_subject() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon
        .emit(permission_requested(
            "p1",
            "write",
            json!({"file_path":"a.txt","content":"x"}),
        ))
        .await;
    let request = expect_permission_request(&mut client).await;
    let params = &request["params"];
    assert_eq!(params["sessionId"], SID);
    assert_eq!(params["title"], "Allow writing file?", "{params}");
    assert_eq!(params["description"], "HoronAgent wants to run write.", "{params}");
    assert!(params.get("toolCall").is_none(), "no v1 toolCall root: {params}");
    assert_eq!(
        params["subject"],
        json!({"type":"tool_call","toolCall":{
            "toolCallId":"call-p1","name":"write","title":"Writing file","kind":"edit","status":"pending",
            "rawInput":{"file_path":"a.txt","content":"x"}}}),
        "{params}"
    );
    let option_ids: Vec<&str> = params["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["optionId"].as_str().unwrap())
        .collect();
    // This request names one path, so a v2 client also gets the two scoped
    // answers; v1 stays at the four below (golden-v1-frames.jsonl).
    assert_eq!(
        option_ids,
        [
            OPTION_ALLOW_ONCE,
            OPTION_ALLOW_ALWAYS,
            OPTION_ALLOW_PATH,
            OPTION_ALLOW_FOLDER,
            OPTION_REJECT_ONCE,
            OPTION_REJECT_ALWAYS
        ]
    );
    client
        .send(answered(&request["id"], selected(OPTION_ALLOW_ALWAYS)))
        .await;
    expect_decision(&mut daemon, "p1", "allow").await;

    // The remembered answer is shared with v1: no second request.
    daemon
        .emit(permission_requested("p2", "write", json!({"file_path":"b.txt"})))
        .await;
    expect_decision(&mut daemon, "p2", "allow").await;
    assert!(client.barrier("remembered").await.is_empty());
    client.close_stdin().await;
}

#[tokio::test]
async fn bash_permission_uses_command_subject() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client).await;
    daemon
        .emit(permission_requested(
            "p1",
            "bash",
            json!({"command":"echo hi","intent":"t"}),
        ))
        .await;
    let request = expect_permission_request(&mut client).await;
    let params = &request["params"];
    assert_eq!(params["title"], "Allow running shell command?", "{params}");
    assert_eq!(params["description"], "HoronAgent wants to run bash.", "{params}");
    assert_eq!(
        params["subject"],
        json!({"type":"command","command":"echo hi","cwd":CWD,"toolCallId":"call-p1"}),
        "{params}"
    );
    client
        .send(answered(&request["id"], selected(OPTION_REJECT_ONCE)))
        .await;
    expect_decision(&mut daemon, "p1", "deny").await;

    // The subject follows the session's current cwd.
    *session.cwd.lock().await = Some(PathBuf::from("/elsewhere"));
    daemon
        .emit(permission_requested("p2", "bash", json!({"command":"ls"})))
        .await;
    let request = expect_permission_request(&mut client).await;
    assert_eq!(request["params"]["subject"]["cwd"], "/elsewhere", "{request}");
    client.send(answered(&request["id"], selected(OPTION_ALLOW_ONCE))).await;
    expect_decision(&mut daemon, "p2", "allow").await;

    // A bash call without a command has nothing to show as a command.
    daemon.emit(permission_requested("p3", "bash", json!({}))).await;
    let request = expect_permission_request(&mut client).await;
    assert_eq!(request["params"]["subject"]["type"], "tool_call", "{request}");
    client.send(answered(&request["id"], selected(OPTION_ALLOW_ONCE))).await;
    expect_decision(&mut daemon, "p3", "allow").await;
    client.close_stdin().await;
}

#[tokio::test]
async fn unknown_outcome_denies_v2() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client).await;
    let replies = [
        json!({"jsonrpc":"2.0","id":null,"result":{"outcome":{"outcome":"_custom"}}}),
        json!({"jsonrpc":"2.0","id":null,"result":{"outcome":"_custom"}}),
        json!({"jsonrpc":"2.0","id":null,"result":{"outcome":{"outcome":"_custom","optionId":OPTION_ALLOW_ONCE}}}),
        json!({"jsonrpc":"2.0","id":null,"result":{"outcome":{"outcome":"selected","optionId":"allow"}}}),
        json!({"jsonrpc":"2.0","id":null,"result":{"outcome":{"outcome":"cancelled"}}}),
        json!({"jsonrpc":"2.0","id":null,"result":{}}),
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"client failed"}}),
    ];
    for (index, mut reply) in replies.into_iter().enumerate() {
        let request_id = format!("p{index}");
        daemon
            .emit(permission_requested(&request_id, "bash", json!({"command":"rm -rf x"})))
            .await;
        let request = expect_permission_request(&mut client).await;
        reply["id"] = request["id"].clone();
        client.send(reply.clone()).await;
        expect_decision(&mut daemon, &request_id, "deny").await;
    }
    assert_eq!(session.permissions.remembered("bash"), None);
    client.close_stdin().await;
}

#[tokio::test]
async fn permission_reports_requires_action() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon
        .emit(
            json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":"r1",
            "start_message_id":"peer-1"}),
        )
        .await;
    assert_eq!(
        kinds(&daemon_barrier(&mut client, &mut daemon, "running").await),
        ["state_update:running"]
    );

    daemon
        .emit(permission_requested("p1", "bash", json!({"command":"echo hi"})))
        .await;
    let mut lines = Vec::new();
    let request = loop {
        let line = client.recv().await;
        if line["method"] == "session/request_permission" {
            break line;
        }
        lines.push(line);
    };
    assert_matches("RequestPermissionRequest", &request["params"]);
    while kinds(&lines).is_empty() {
        lines.push(client.recv().await);
    }
    assert_eq!(kinds(&lines), ["state_update:requires_action"], "{lines:?}");
    assert_update_matches(&lines[0]["params"]["update"]);

    client.send(answered(&request["id"], selected(OPTION_ALLOW_ONCE))).await;
    expect_decision(&mut daemon, "p1", "allow").await;
    daemon.emit(json!({"type":"text_delta","text":"ran"})).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "answered").await;
    assert_eq!(
        kinds(&lines),
        ["state_update:running", "agent_message_chunk"],
        "{lines:?}"
    );
    client.close_stdin().await;
}

#[test]
fn config_options_use_config_id_mode_first() {
    let state = SessionUiState {
        model: Some("m0".to_string()),
        available_models: vec!["m0".to_string(), "m1".to_string()],
        ..SessionUiState::default()
    };
    let mut result = json!({"sessionId": SID});
    insert_session_configuration_v2(&mut result, &state, AcpMode::AskCommands);
    assert_matches("NewSessionResponse", &result);
    let options = &result["configOptions"];
    assert_no_v1_option_ids(options);
    assert_eq!(options[0]["configId"], CONFIG_ID_MODE);
    assert_eq!(options[0]["category"], "mode");
    assert_eq!(options[0]["currentValue"], "ask-commands");
    assert_eq!(options[1]["configId"], CONFIG_ID_MODEL);
    assert!(
        options
            .as_array()
            .unwrap()
            .iter()
            .all(|option| option["type"] == "select"),
        "no boolean option is emitted: {options}"
    );
    assert_matches("ConfigOptionUpdate", &json!({"configOptions": options}));
}

fn set_option(id: Value, params: Value) -> Value {
    request(id, "session/set_config_option", params)
}

#[tokio::test]
async fn set_config_option_v2_requires_type() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client).await;
    let rejected = [
        json!({"sessionId":SID,"configId":"mode","value":"auto"}),
        json!({"sessionId":SID,"configId":"mode","type":"boolean","value":true}),
        json!({"sessionId":SID,"configId":"mode","type":"id","value":true}),
        json!({"sessionId":SID,"configId":"mode","type":"text","value":"auto"}),
        json!({"sessionId":SID,"configId":"mode","type":"id","value":"nope"}),
        json!({"sessionId":SID,"configId":"nope","type":"id","value":"auto"}),
    ];
    for (index, params) in rejected.into_iter().enumerate() {
        let id = json!(10 + index);
        client.send(set_option(id.clone(), params.clone())).await;
        let (before, response) = client.recv_response(id).await;
        assert!(before.is_empty(), "{params}: {before:?}");
        assert_eq!(
            response["error"]["code"], JSONRPC_INVALID_PARAMS,
            "{params}: {response}"
        );
    }
    assert_eq!(
        *session.mode.lock().await,
        AcpMode::Default,
        "no rejected change applied"
    );

    client
        .send(set_option(
            json!(20),
            json!({"sessionId":SID,"configId":"mode","type":"id","value":"auto"}),
        ))
        .await;
    let applied = daemon.recv_type("set_session_approval_mode").await;
    assert_eq!(applied["mode"], "off", "{applied}");
    daemon.emit(json!({"type":"ack","id":applied["id"]})).await;
    let (_, response) = client.recv_response(json!(20)).await;
    assert_matches("SetSessionConfigOptionResponse", &response["result"]);
    assert_no_v1_option_ids(&response["result"]["configOptions"]);
    assert_eq!(response["result"]["configOptions"][0]["configId"], "mode");
    assert_eq!(response["result"]["configOptions"][0]["currentValue"], "auto");
    assert_eq!(*session.mode.lock().await, AcpMode::Auto);
    client.close_stdin().await;
}

#[tokio::test]
async fn no_modes_in_v2_responses() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;

    // A mode change reports the options only, never `current_mode_update`.
    client
        .send(set_option(
            json!(1),
            json!({"sessionId":SID,"configId":"mode","type":"id","value":"ask-commands"}),
        ))
        .await;
    let applied = daemon.recv_type("set_session_approval_mode").await;
    daemon.emit(json!({"type":"ack","id":applied["id"]})).await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert!(response["result"].get("modes").is_none(), "{response}");
    let after = client.barrier("mode-changed").await;
    assert_eq!(kinds(&after), ["config_option_update"], "{after:?}");
    let update = &after[0]["params"]["update"];
    assert_update_matches(update);
    assert_no_v1_option_ids(&update["configOptions"]);
    assert_eq!(update["configOptions"][0]["currentValue"], "ask-commands");

    // `session/resume` answers v2 options and no `modes`.
    client
        .send(request(
            json!(2),
            "session/resume",
            json!({"sessionId":SID,"cwd":CWD,"mcpServers":[]}),
        ))
        .await;
    let get_history = daemon.recv_type("get_history").await;
    daemon
        .emit(
            json!({"type":"history","id":get_history["id"],"session_id":SID,"provider_model":"m0",
            "available_models":["m0","m1"],"messages":[],"acp_protocol_version":2}),
        )
        .await;
    let mcp = daemon.recv_type("configure_session_mcp").await;
    daemon.emit(json!({"type":"done","id":mcp["id"]})).await;
    let mode = daemon.recv_type("set_session_approval_mode").await;
    daemon.emit(json!({"type":"ack","id":mode["id"]})).await;
    let (before, response) = client.recv_response(json!(2)).await;
    let result = &response["result"];
    assert!(result.get("modes").is_none(), "{response}");
    assert_matches("ResumeSessionResponse", result);
    assert_no_v1_option_ids(&result["configOptions"]);
    assert_eq!(result["configOptions"][0]["configId"], "mode");
    let after = client.barrier("resumed").await;
    for line in before.iter().chain(&after) {
        if line["method"] == "session/update" {
            assert_update_matches(&line["params"]["update"]);
            assert_ne!(line["params"]["update"]["sessionUpdate"], "current_mode_update");
        }
    }
    client.close_stdin().await;
}

fn ask_requested(request_id: &str) -> Value {
    json!({"type":"ask_requested","request_id":request_id,"questions":[
        {"id":"db","question":"Which database?","header":"Storage",
            "options":[{"label":"Postgres"},{"label":"SQLite"}],"recommended":1}
    ]})
}

#[tokio::test]
async fn elicitation_v2_carries_session_id() {
    let (mut client, _) = v2_client_with(json!({"elicitation":{"form":{}}})).await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(ask_requested("ask-1")).await;
    let request = client.recv().await;
    assert_eq!(request["method"], "elicitation/create", "{request}");
    let params = &request["params"];
    assert_matches("CreateElicitationRequest", params);
    assert_eq!(params["sessionId"], SID);
    assert_eq!(params["mode"], "form");
    assert!(params.get("requestId").is_none(), "{params}");
    assert_eq!(params["requestedSchema"]["properties"]["db"]["default"], "SQLite");
    client
        .send(answered(
            &request["id"],
            json!({"action":"accept","content":{"db":"Postgres"}}),
        ))
        .await;
    let answer = daemon.recv_type("ask_response").await;
    assert_eq!(answer["answers"], json!([{"question_id":"db","selected":["Postgres"]}]));
    daemon.emit(json!({"type":"ack","id":answer["id"]})).await;
    client.close_stdin().await;

    // Form support is read from the v2 `capabilities` only: the v1 field name
    // on a v2 connection advertises nothing, so the ask is never sent.
    let mut client = start_client_with(None);
    client
        .send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":2,"info":{"name":"t9","version":"1"},
            "clientCapabilities":{"elicitation":{"form":{}}}}}))
        .await;
    assert_eq!(client.recv().await["result"]["protocolVersion"], 2);
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(ask_requested("ask-2")).await;
    let answer = daemon.recv_type("ask_response").await;
    assert_eq!(answer["answers"], json!([]), "{answer}");
    daemon.emit(json!({"type":"ack","id":answer["id"]})).await;
    assert!(client.barrier("no-form").await.is_empty());
    client.close_stdin().await;
}

#[tokio::test]
async fn elicitation_unknown_action_is_cancel() {
    let (mut client, _) = v2_client_with(json!({"elicitation":{"form":{}}})).await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon
        .emit(
            json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":"r1",
            "start_message_id":"peer-1"}),
        )
        .await;
    daemon_barrier(&mut client, &mut daemon, "running").await;
    let replies = [
        json!({"action":"_later","content":{"db":"Postgres"}}),
        json!({"action":"cancel"}),
        json!({"action":"decline"}),
    ];
    for (index, reply) in replies.into_iter().enumerate() {
        let request_id = format!("ask-{index}");
        daemon.emit(ask_requested(&request_id)).await;
        let mut lines = Vec::new();
        let request = loop {
            let line = client.recv().await;
            if line["method"] == "elicitation/create" {
                break line;
            }
            lines.push(line);
        };
        while kinds(&lines).is_empty() {
            lines.push(client.recv().await);
        }
        assert_eq!(kinds(&lines), ["state_update:requires_action"], "{reply}: {lines:?}");
        client.send(answered(&request["id"], reply.clone())).await;
        let answer = daemon.recv_type("ask_response").await;
        assert_eq!(answer["request_id"], request_id.as_str());
        assert_eq!(answer["answers"], json!([]), "{reply}: {answer}");
        daemon.emit(json!({"type":"ack","id":answer["id"]})).await;
        // The answered ask lets the turn report `running` before its next event.
        let lines = daemon_barrier(&mut client, &mut daemon, &format!("answered-{index}")).await;
        assert_eq!(kinds(&lines), ["state_update:running"], "{reply}: {lines:?}");
    }
    client.close_stdin().await;
}

#[tokio::test]
async fn auth_methods_v2_shape() {
    // A client that advertises terminal auth gets the one credentials method:
    // this build has no terminal login to launch.
    for capabilities in [
        json!({"auth":{"terminal":{}}}),
        json!({"auth":{"terminal":true}}),
        json!({"auth":{}}),
        json!({}),
    ] {
        let (client, result) = v2_client_with(capabilities.clone()).await;
        assert_matches("InitializeResponse", &result);
        let methods = result["authMethods"].as_array().expect("authMethods");
        assert_eq!(methods.len(), 1, "{capabilities}: {result}");
        for method in methods {
            assert_matches("AuthMethod", method);
            assert!(method.get("id").is_none(), "no v1 `id`: {method}");
            assert!(method.get("args").is_none(), "no terminal method: {method}");
        }
        assert_eq!(methods[0]["methodId"], AUTH_METHOD_CREDENTIALS);
        assert_eq!(methods[0]["type"], "agent");
        client.close_stdin().await;
    }
}

#[tokio::test]
async fn auth_login_and_logout_flow() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    client.runtime.ready.store(true, Ordering::SeqCst);

    client.send(request(json!(1), "auth/logout", json!({}))).await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert_eq!(response["result"], json!({}), "{response}");
    assert_matches("LogoutAuthResponse", &response["result"]);

    let gated = [
        request(json!(2), "session/new", json!({"cwd":CWD})),
        request(json!(3), "session/resume", json!({"sessionId":SID,"cwd":CWD})),
        prompt(json!(4), SID),
    ];
    for message in gated {
        let id = message["id"].clone();
        client.send(message).await;
        let (_, response) = client.recv_response(id).await;
        assert_eq!(response["error"]["code"], ACP_AUTH_REQUIRED, "{response}");
    }

    // The removed terminal method and malformed logins leave the connection
    // logged out.
    for (id, params) in [
        (5, json!({"methodId": "horon-agent-terminal-login"})),
        (6, json!({"methodId": "nope"})),
        (7, json!({})),
    ] {
        client.send(request(json!(id), "auth/login", params.clone())).await;
        let (_, response) = client.recv_response(json!(id)).await;
        assert_eq!(
            response["error"]["code"], JSONRPC_INVALID_PARAMS,
            "{params}: {response}"
        );
    }
    client.send(prompt(json!(8), SID)).await;
    let (_, response) = client.recv_response(json!(8)).await;
    assert_eq!(response["error"]["code"], ACP_AUTH_REQUIRED, "{response}");

    client
        .send(request(
            json!(9),
            "auth/login",
            json!({"methodId": AUTH_METHOD_CREDENTIALS}),
        ))
        .await;
    let (_, response) = client.recv_response(json!(9)).await;
    assert_eq!(response["result"], json!({}), "{response}");
    assert_matches("LoginAuthResponse", &response["result"]);

    // Logged in again: the prompt reaches the daemon and is acknowledged.
    client.send(prompt(json!(10), SID)).await;
    let message = daemon.recv_type("message").await;
    daemon
        .emit(json!({"type":"user_message_inserted","id":message["id"],"message_id":"m1"}))
        .await;
    let (_, response) = client.recv_response(json!(10)).await;
    assert_eq!(response["result"], json!({"messageId":"m1"}), "{response}");
    client.close_stdin().await;
}

fn prompt_with(id: Value, blocks: Value) -> Value {
    request(id, "session/prompt", json!({"sessionId": SID, "prompt": blocks}))
}

/// IS-6: v2 advertises `audio`, takes a wav block as a daemon audio attachment
/// and refuses an audio format the endpoint cannot take before insertion.
#[tokio::test]
async fn audio_prompt_v2_reaches_daemon_and_other_formats_are_invalid() {
    let (mut client, initialized) = v2_client_with(json!({})).await;
    assert_eq!(
        initialized["capabilities"]["session"]["prompt"]["audio"],
        json!({}),
        "{initialized}"
    );
    let (_session, mut daemon) = attach_v2(&client).await;

    client
        .send(prompt_with(
            json!(1),
            json!([{"type":"audio","mimeType":"audio/wav","data":"AAAA"},{"type":"text","text":"hi"}]),
        ))
        .await;
    let message = daemon.recv_type("message").await;
    assert_eq!(message["images"], json!([["audio/wav", "AAAA"]]), "{message}");
    assert_eq!(message["content"], "hi", "{message}");
    daemon
        .emit(json!({"type":"user_message_inserted","id":message["id"],"message_id":"m1"}))
        .await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert_eq!(response["result"], json!({"messageId":"m1"}), "{response}");
    // The turn ends, so the next prompt is not held behind it.
    for running in [true, false] {
        daemon
            .emit(
                json!({"type":"foreground_state","session_id":SID,"running":running,"run_id":"r1",
                "start_message_id":"m1","stop_reason":"end_turn"}),
            )
            .await;
    }

    client
        .send(prompt_with(
            json!(2),
            json!([{"type":"audio","mimeType":"audio/mp3","data":"BBBB"}]),
        ))
        .await;
    let message = daemon.recv_type("message").await;
    assert_eq!(message["images"], json!([["audio/mpeg", "BBBB"]]), "{message}");
    daemon
        .emit(json!({"type":"user_message_inserted","id":message["id"],"message_id":"m2"}))
        .await;
    client.recv_response(json!(2)).await;

    client
        .send(prompt_with(
            json!(3),
            json!([{"type":"audio","mimeType":"audio/ogg","data":"CCCC"}]),
        ))
        .await;
    let (_, response) = client.recv_response(json!(3)).await;
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), daemon.recv_type("message"))
            .await
            .is_err(),
        "an unsupported audio format never reaches the daemon"
    );
    client.close_stdin().await;
}

/// IS-6: the daemon's `ask_tool_call` ties the following question to its `ask`
/// call; a question the daemon named no call for goes without `toolCallId`.
#[tokio::test]
async fn elicitation_v2_names_the_ask_tool_call() {
    let (mut client, _) = v2_client_with(json!({"elicitation":{"form":{}}})).await;
    let (_session, mut daemon) = attach_v2(&client).await;
    for (request_id, tool_call_id) in [("ask-1", Some("call-q")), ("ask-2", None)] {
        if let Some(tool_call_id) = tool_call_id {
            daemon
                .emit(json!({"type":"ask_tool_call","request_id":request_id,"tool_call_id":tool_call_id}))
                .await;
        }
        daemon.emit(ask_requested(request_id)).await;
        let request = loop {
            let line = client.recv().await;
            if line["method"] == "elicitation/create" {
                break line;
            }
        };
        assert_matches("CreateElicitationRequest", &request["params"]);
        assert_eq!(
            request["params"].get("toolCallId"),
            tool_call_id.map(|id| json!(id)).as_ref(),
            "{request}"
        );
        client.send(answered(&request["id"], json!({"action":"cancel"}))).await;
        let answer = daemon.recv_type("ask_response").await;
        assert_eq!(answer["request_id"], request_id, "{answer}");
        daemon.emit(json!({"type":"ack","id":answer["id"]})).await;
    }
    client.close_stdin().await;
}

/// IS-6: a soft interrupt the daemon stored mid-turn is reported as its own
/// `user_message` under the stored row id.
#[tokio::test]
async fn soft_interrupt_v2_is_a_user_message() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon
        .emit(
            json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":"r1",
            "start_message_id":"peer-1"}),
        )
        .await;
    daemon
        .emit(json!({"type":"soft_interrupt_stored","message_id":"row-7","content":"also check tests"}))
        .await;
    let lines = daemon_barrier(&mut client, &mut daemon, "interrupt").await;
    let user = lines
        .iter()
        .map(|line| &line["params"]["update"])
        .find(|update| update["sessionUpdate"] == "user_message")
        .unwrap_or_else(|| panic!("no user_message: {lines:?}"));
    assert_update_matches(user);
    assert_eq!(
        user,
        &json!({"sessionUpdate":"user_message","messageId":"row-7",
            "content":[{"type":"text","text":"also check tests"}]}),
    );
    client.close_stdin().await;
}

/// IS-6: `session/cancel` names the run it saw, so a peer turn that starts
/// before the daemon reads it is not stopped.
#[tokio::test]
async fn session_cancel_v2_targets_the_tracked_run() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon
        .emit(
            json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":"run-9",
            "start_message_id":"peer-1"}),
        )
        .await;
    daemon_barrier(&mut client, &mut daemon, "running").await;
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":SID}}))
        .await;
    let cancel = daemon.recv().await;
    assert_eq!(cancel["type"], "cancel_turn", "{cancel}");
    assert_eq!(cancel["run_id"], "run-9", "{cancel}");
    client.close_stdin().await;
}

/// IS-6: `usage_update` keeps its place among the turn's updates: written
/// where the daemon reported it, between the chunks around it and before the
/// idle state.
#[tokio::test]
async fn usage_update_keeps_daemon_order() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    for event in [
        json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":"r1","start_message_id":"peer-1"}),
        json!({"type":"text_delta","text":"before "}),
        json!({"type":"tokens","input":120,"output":4}),
        json!({"type":"text_delta","text":"after"}),
        json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":"r1",
            "start_message_id":"peer-1","stop_reason":"end_turn"}),
    ] {
        daemon.emit(event).await;
    }
    let lines = daemon_barrier(&mut client, &mut daemon, "usage").await;
    assert_eq!(
        kinds(&lines),
        [
            "state_update:running",
            "agent_message_chunk",
            "usage_update",
            "agent_message_chunk",
            "state_update:idle"
        ],
        "{lines:?}"
    );
    client.close_stdin().await;
}

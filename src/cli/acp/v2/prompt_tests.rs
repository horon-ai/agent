//! IS-4 and IS-5 over a scripted daemon: v2 prompt acknowledgment, pre-insertion
//! refusals, withdrawal, foreground state updates and `session/close`.

use super::super::tests::connection_core::*;
use super::super::*;
use super::prompt::SESSION_CLOSED;
use super::state::{Control, V2Session};

const SID: &str = "s1";

async fn v2_client() -> Client {
    let mut client = start_client_with(None);
    client.initialize_v2().await;
    client
}

/// A v2 session on a scripted daemon, wired as `session/new` wires one: v2
/// state, side channel, a usable model, then its stream task.
async fn attach_v2(client: &Client, close_bound: Duration) -> (Arc<DaemonSession>, FakeDaemon) {
    let (session, daemon) = daemon_pair(SID);
    let events = session.open_v2_stream();
    assert!(session.v2.set(Arc::new(V2Session::new(events, close_bound))).is_ok());
    client.runtime.install_session_bridge(&session);
    offer_models(&session, 1).await;
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

fn inserted(id: u64, message_id: &str) -> Value {
    json!({"type":"user_message_inserted","id":id,"message_id":message_id})
}

fn running(run_id: &str, start: &str, user_content: Option<&str>) -> Value {
    let mut event = json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":run_id,
        "start_message_id":start});
    if let Some(text) = user_content {
        event["user_content"] = json!(text);
    }
    event
}

fn idle(run_id: &str, start: &str, stop_reason: &str) -> Value {
    json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":run_id,
        "start_message_id":start,"stop_reason":stop_reason})
}

fn text(delta: &str) -> Value {
    json!({"type":"text_delta","text":delta})
}

/// The id of the next `message` request the adapter writes.
async fn next_message(daemon: &mut FakeDaemon) -> u64 {
    let message = daemon.recv_type("message").await;
    let id = message["id"].as_u64().expect("a message id");
    assert!(
        id >= V2_MESSAGE_ID_BASE,
        "v2 message ids come from the high range: {message}"
    );
    id
}

/// Every line the adapter wrote for daemon events sent before this point: the
/// session's stream task writes the marker's `session_info_update` after them.
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

/// `sessionUpdate`, with the state of a `state_update`, of every update line.
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

fn updates(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| line["method"] == "session/update")
        .map(|line| &line["params"]["update"])
        .collect()
}

#[tokio::test]
async fn prompt_answers_message_id_on_insert_not_on_done() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;
    assert!(
        daemon_barrier(&mut client, &mut daemon, "queued").await.is_empty(),
        "nothing is answered before insertion"
    );

    daemon.emit(inserted(id, "m1")).await;
    let (before, response) = client.recv_response(json!(1)).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["result"], json!({"messageId":"m1"}), "{response}");

    daemon.emit(running("r1", "m1", Some("hi"))).await;
    daemon.emit(text("hello")).await;
    daemon.emit(json!({"type":"done","id":id})).await;
    daemon.emit(idle("r1", "m1", "end_turn")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "turn").await;
    assert_eq!(
        kinds(&lines),
        [
            "user_message",
            "state_update:running",
            "agent_message_chunk",
            "state_update:idle"
        ],
        "{lines:?}"
    );
    let updates = updates(&lines);
    assert_eq!(updates[0]["messageId"], "m1");
    assert_eq!(updates[0]["content"], json!([{"type":"text","text":"hi"}]));
    assert_eq!(updates[3]["stopReason"], "end_turn");
    assert!(
        lines.iter().all(|line| line.get("id").is_none()),
        "the turn's Done answers nothing: {lines:?}"
    );
    client.close_stdin().await;
}

/// alpha.13 `PromptRequest.prompt` is `minItems: 1`: the adapter refuses an
/// empty array with `-32602` before the daemon sees a `message`, so no turn
/// starts and no empty `user_message` is ever written.
#[tokio::test]
async fn empty_prompt_is_invalid_params_and_never_reaches_the_daemon() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client
        .send(json!({"jsonrpc":"2.0","id":"empty","method":"session/prompt",
            "params":{"sessionId":SID,"prompt":[]}}))
        .await;
    let (before, response) = client.recv_response(json!("empty")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
    assert!(
        response["error"].get("data").is_none(),
        "an empty prompt is a plain invalid-params error: {response}"
    );
    super::tests::assert_matches("Error", &response["error"]);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), daemon.recv())
            .await
            .is_err(),
        "an empty prompt writes no daemon message"
    );
    client.close_stdin().await;
}

#[tokio::test]
async fn pre_insert_errors_have_no_updates() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;

    // The daemon is busy with this connection's own message.
    client.send(prompt(json!("busy"), SID)).await;
    let id = next_message(&mut daemon).await;
    daemon
        .emit(json!({"type":"error","id":id,"message":"Already processing a message"}))
        .await;
    let (before, response) = client.recv_response(json!("busy")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");

    // The daemon is reloading: refused at once.
    client.send(prompt(json!("reloading"), SID)).await;
    next_message(&mut daemon).await;
    daemon.emit(json!({"type":"reloading"})).await;
    let (before, response) = client.recv_response(json!("reloading")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");

    // Logged out on this connection.
    client.runtime.lock_connection().logged_out = true;
    client.send(prompt(json!("logged-out"), SID)).await;
    let (before, response) = client.recv_response(json!("logged-out")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], ACP_AUTH_REQUIRED, "{response}");
    client.runtime.lock_connection().logged_out = false;

    // No usable model.
    *session.ui_state.lock().await = SessionUiState::default();
    client.send(prompt(json!("no-model"), SID)).await;
    let (before, response) = client.recv_response(json!("no-model")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], ACP_AUTH_REQUIRED, "{response}");
    offer_models(&session, 1).await;

    // Another prompt of this editor is still waiting for insertion.
    client.send(prompt(json!("first"), SID)).await;
    let first = next_message(&mut daemon).await;
    client.send(prompt(json!("second"), SID)).await;
    let (before, response) = client.recv_response(json!("second")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");

    // Any other daemon error for the prompt.
    daemon
        .emit(json!({"type":"error","id":first,"message":"Skill 'x' is not installed on the server"}))
        .await;
    let (before, response) = client.recv_response(json!("first")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");

    let lines = daemon_barrier(&mut client, &mut daemon, "end").await;
    assert!(kinds(&lines).is_empty(), "a refused prompt writes no update: {lines:?}");
    client.close_stdin().await;
}

#[tokio::test]
async fn peer_error_with_same_low_id_does_not_fail_our_prompt() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;
    // The id this connection's own counter hands out next, as another
    // connection's turn terminal would carry it.
    let low = session.next_id();
    daemon
        .emit(json!({"type":"error","id":low,"message":"peer turn failed"}))
        .await;
    daemon.emit(json!({"type":"done","id":low})).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "peer").await;
    assert!(
        lines.iter().all(|line| line.get("id").is_none()),
        "a peer's terminal answers nothing: {lines:?}"
    );

    daemon.emit(inserted(id, "m1")).await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert_eq!(response["result"], json!({"messageId":"m1"}), "{response}");
    client.close_stdin().await;
}

#[tokio::test]
async fn post_insert_failure_is_idle_error() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    for (round, (failure, code)) in [
        ("provider exploded", JSONRPC_INTERNAL_ERROR),
        ("401 Unauthorized: not logged in", ACP_AUTH_REQUIRED),
    ]
    .into_iter()
    .enumerate()
    {
        let rpc_id = json!(round);
        let start = format!("m{round}");
        let run = format!("r{round}");
        client.send(prompt(rpc_id.clone(), SID)).await;
        let id = next_message(&mut daemon).await;
        daemon.emit(inserted(id, &start)).await;
        let (_, response) = client.recv_response(rpc_id).await;
        assert_eq!(response["result"]["messageId"], start.as_str(), "{response}");
        daemon.emit(running(&run, &start, None)).await;
        daemon.emit(json!({"type":"error","id":id,"message":failure})).await;
        daemon.emit(idle(&run, &start, "error")).await;
        let lines = daemon_barrier(&mut client, &mut daemon, &format!("failed-{round}")).await;
        assert!(
            lines.iter().all(|line| line.get("id").is_none()),
            "an inserted prompt is never answered with an error: {lines:?}"
        );
        let last = *updates(&lines).last().expect("an idle update");
        assert_eq!(
            last,
            &json!({"sessionUpdate":"state_update","state":"idle","stopReason":"error",
                "error":{"code":code,"message":failure}})
        );
    }
    client.close_stdin().await;
}

#[tokio::test]
async fn cancel_request_before_insert_withdraws_32800_after_insert_ignored() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;

    client.send(prompt(json!("a"), SID)).await;
    let first = next_message(&mut daemon).await;
    client
        .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":"a"}}))
        .await;
    let withdraw = daemon.recv().await;
    assert_eq!(
        withdraw["type"], "withdraw_message",
        "never a daemon cancel: {withdraw}"
    );
    assert_eq!(withdraw["message_request_id"], first, "{withdraw}");
    daemon.emit(json!({"type":"done","id":withdraw["id"]})).await;
    let (before, response) = client.recv_response(json!("a")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_REQUEST_CANCELLED, "{response}");

    client.send(prompt(json!("b"), SID)).await;
    let second = next_message(&mut daemon).await;
    daemon.emit(inserted(second, "m2")).await;
    let (_, response) = client.recv_response(json!("b")).await;
    assert_eq!(response["result"], json!({"messageId":"m2"}), "{response}");
    daemon.emit(running("r2", "m2", None)).await;
    daemon_barrier(&mut client, &mut daemon, "running").await;
    client
        .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":"b"}}))
        .await;
    client
        .send(json!({"jsonrpc":"2.0","id":"cancel","method":"session/cancel","params":{"sessionId":SID}}))
        .await;
    let (before, response) = client.recv_response(json!("cancel")).await;
    assert!(
        before.iter().all(|line| line.get("id").is_none()),
        "the inserted prompt is not answered again: {before:?}"
    );
    assert_eq!(response["result"], json!({}), "{response}");
    let next = daemon.recv().await;
    assert_eq!(
        next["type"], "cancel",
        "$/cancel_request of an inserted prompt reaches no daemon: {next}"
    );
    client.close_stdin().await;
}

/// A peer turn's terminals carry ids from this connection's low counter; only
/// the reply to our own `withdraw_message`, whose id is in the v2 range,
/// settles the withdrawal.
#[tokio::test]
async fn peer_terminals_with_low_ids_settle_no_withdraw() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client.send(prompt(json!(1), SID)).await;
    next_message(&mut daemon).await;
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":SID}}))
        .await;
    let withdraw = daemon.recv().await;
    assert_eq!(withdraw["type"], "withdraw_message", "{withdraw}");
    let withdraw_id = withdraw["id"].as_u64().expect("a withdraw id");
    assert!(
        withdraw_id >= V2_MESSAGE_ID_BASE,
        "a withdrawal id comes from the v2 range: {withdraw}"
    );

    let low = session.next_id();
    daemon.emit(json!({"type":"done","id":low})).await;
    daemon
        .emit(json!({"type":"error","id":low,"message":"already inserted"}))
        .await;
    let lines = daemon_barrier(&mut client, &mut daemon, "peer").await;
    assert!(
        lines.iter().all(|line| line.get("id").is_none()),
        "a peer's terminal settles no withdrawal and answers nothing: {lines:?}"
    );

    daemon.emit(json!({"type":"done","id":withdraw_id})).await;
    let (before, response) = client.recv_response(json!(1)).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_REQUEST_CANCELLED, "{response}");
    client.send(prompt(json!(2), SID)).await;
    let next = daemon.recv().await;
    assert_eq!(
        next["type"], "message",
        "a withdrawn prompt leaves no turn for a daemon cancel: {next}"
    );
    client.close_stdin().await;
}

/// Every event of a v2 session goes through the v2 mapper: ids from the
/// run's start id, tool calls created by `tool_call_update`, and a call the
/// cancelled run left open reported cancelled before the idle update.
#[tokio::test]
async fn stream_maps_v2_updates_and_cancels_open_tools() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    daemon.emit(running("r1", "peer-1", None)).await;
    daemon.emit(text("look")).await;
    daemon.emit(json!({"type":"tool_start","id":"c1","name":"bash"})).await;
    daemon.emit(json!({"type":"interrupted"})).await;
    daemon.emit(idle("r1", "peer-1", "cancelled")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "cancelled").await;
    assert_eq!(
        kinds(&lines),
        [
            "state_update:running",
            "agent_message_chunk",
            "tool_call_update",
            "tool_call_update",
            "state_update:idle"
        ],
        "{lines:?}"
    );
    let updates = updates(&lines);
    assert_eq!(updates[1]["messageId"], "peer-1:a1");
    assert_eq!(updates[2]["status"], "pending");
    assert_eq!(
        updates[3],
        &json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"cancelled"})
    );
    assert_eq!(updates[4]["stopReason"], "cancelled");
    client.close_stdin().await;
}

/// A run first seen through the attach snapshot gets its own mapper: its ids
/// come from its start id, and a call it leaves open ends cancelled with it.
#[tokio::test]
async fn snapshot_attached_run_maps_with_its_start_id() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    daemon
        .emit(
            json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":"r1",
            "start_message_id":"peer-1","snapshot":true}),
        )
        .await;
    daemon.emit(text("look")).await;
    daemon.emit(json!({"type":"tool_start","id":"c1","name":"bash"})).await;
    daemon.emit(json!({"type":"interrupted"})).await;
    daemon.emit(idle("r1", "peer-1", "cancelled")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "cancelled").await;
    assert_eq!(
        kinds(&lines),
        [
            "agent_message_chunk",
            "tool_call_update",
            "tool_call_update",
            "state_update:idle"
        ],
        "{lines:?}"
    );
    let updates = updates(&lines);
    assert_eq!(updates[0]["messageId"], "peer-1:a1");
    assert_eq!(
        updates[2],
        &json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"cancelled"})
    );
    assert_eq!(updates[3]["stopReason"], "cancelled");
    client.close_stdin().await;
}

/// A model change outside any request updates the selector in v2 shapes.
#[tokio::test]
async fn model_change_writes_v2_config_options() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    daemon
        .emit(json!({"type":"model_changed","id":0,"model":"model-0"}))
        .await;
    let update = loop {
        let line = client.recv().await;
        if line["params"]["update"]["sessionUpdate"] == "config_option_update" {
            break line["params"]["update"].clone();
        }
    };
    let options = update["configOptions"].as_array().expect("config options");
    assert!(!options.is_empty(), "{update}");
    assert!(
        options
            .iter()
            .all(|option| option.get("configId").is_some() && option.get("id").is_none()),
        "{update}"
    );
    client.close_stdin().await;
}

/// The end of a turn the daemon cancelled, and what the editor sees of it.
async fn finish_cancelled(client: &mut Client, daemon: &mut FakeDaemon, id: u64, run: &str, start: &str) -> Vec<Value> {
    daemon.emit(json!({"type":"interrupted"})).await;
    daemon.emit(json!({"type":"done","id":id})).await;
    daemon.emit(idle(run, start, "cancelled")).await;
    daemon_barrier(client, daemon, "cancelled").await
}

#[tokio::test]
async fn cancel_after_refused_withdraw_cancels_the_inserted_turn() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":SID}}))
        .await;
    let withdraw = daemon.recv().await;
    assert_eq!(
        withdraw["type"], "withdraw_message",
        "a queued prompt is withdrawn first: {withdraw}"
    );
    // The turn claimed the message before the withdrawal reached the daemon.
    daemon.emit(inserted(id, "m1")).await;
    daemon.emit(running("r1", "m1", None)).await;
    daemon
        .emit(json!({"type":"error","id":withdraw["id"],"message":"already inserted"}))
        .await;
    let cancel = daemon.recv().await;
    assert_eq!(cancel["type"], "cancel", "the inserted turn is cancelled: {cancel}");

    let lines = finish_cancelled(&mut client, &mut daemon, id, "r1", "m1").await;
    let answer = lines.iter().find(|line| line["id"] == 1).expect("the prompt answer");
    assert_eq!(answer["result"], json!({"messageId":"m1"}), "{answer}");
    assert_eq!(
        kinds(&lines),
        ["user_message", "state_update:running", "state_update:idle"],
        "{lines:?}"
    );
    assert_eq!(updates(&lines)[2]["stopReason"], "cancelled");
    client.close_stdin().await;
}

#[tokio::test]
async fn refused_withdraw_after_our_turn_ended_cancels_no_peer_turn() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":SID}}))
        .await;
    let withdraw = daemon.recv().await;
    assert_eq!(withdraw["type"], "withdraw_message", "{withdraw}");
    // Our turn ran to its end and a peer turn started before the daemon
    // handled the withdrawal, which it then refuses.
    daemon.emit(inserted(id, "m1")).await;
    daemon.emit(running("r1", "m1", None)).await;
    daemon.emit(json!({"type":"done","id":id})).await;
    daemon.emit(idle("r1", "m1", "end_turn")).await;
    daemon.emit(running("r2", "p2", Some("peer text"))).await;
    daemon
        .emit(json!({"type":"error","id":withdraw["id"],"message":"already inserted"}))
        .await;
    let lines = daemon_barrier(&mut client, &mut daemon, "refused").await;
    assert_eq!(
        kinds(&lines),
        [
            "user_message",
            "state_update:running",
            "state_update:idle",
            "state_update:running",
            "user_message",
        ],
        "{lines:?}"
    );

    client.send(prompt(json!(2), SID)).await;
    let next = daemon.recv().await;
    assert_eq!(
        next["type"], "message",
        "a refusal for a turn that already ended cancels nothing, least of all the peer's: {next}"
    );
    client.close_stdin().await;
}

#[tokio::test]
async fn cancel_between_answer_and_running_cancels_the_turn() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;
    daemon.emit(inserted(id, "m1")).await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert_eq!(response["result"], json!({"messageId":"m1"}), "{response}");
    // The turn has not reported `running` yet when the editor cancels it.
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":SID}}))
        .await;
    let cancel = daemon.recv().await;
    assert_eq!(
        cancel["type"], "cancel",
        "an inserted prompt is cancelled, never withdrawn: {cancel}"
    );

    daemon.emit(running("r1", "m1", None)).await;
    let lines = finish_cancelled(&mut client, &mut daemon, id, "r1", "m1").await;
    assert_eq!(
        kinds(&lines),
        ["user_message", "state_update:running", "state_update:idle"],
        "{lines:?}"
    );
    assert_eq!(updates(&lines)[2]["stopReason"], "cancelled");
    assert!(
        lines.iter().all(|line| line.get("error").is_none()),
        "no error answer: {lines:?}"
    );
    client.close_stdin().await;
}

#[test]
fn auth_failure_matches_whole_tokens() {
    for message in [
        "HTTP 401 from the provider",
        "Unauthorized",
        "You are not logged in",
        "login required",
        "Invalid API key provided",
        "no credentials found",
        "Authentication failed",
    ] {
        assert_eq!(super::foreground::error_code(message), ACP_AUTH_REQUIRED, "{message}");
    }
    for message in [
        "request 4011 failed",
        "provider exploded",
        "authentication of the tool server failed",
        "nologgedin",
    ] {
        assert_eq!(
            super::foreground::error_code(message),
            JSONRPC_INTERNAL_ERROR,
            "{message}"
        );
    }
}

#[tokio::test]
async fn peer_turn_reports_user_message_and_states() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    daemon
        .emit(running("peer-run", "peer-1", Some("from the desktop")))
        .await;
    daemon.emit(text("answer")).await;
    daemon.emit(json!({"type":"done","id":7})).await;
    daemon.emit(idle("peer-run", "peer-1", "end_turn")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "peer").await;
    assert_eq!(
        kinds(&lines),
        [
            "state_update:running",
            "user_message",
            "agent_message_chunk",
            "state_update:idle"
        ],
        "{lines:?}"
    );
    let updates = updates(&lines);
    assert_eq!(
        updates[1],
        &json!({"sessionUpdate":"user_message","messageId":"peer-1",
            "content":[{"type":"text","text":"from the desktop"}]})
    );
    assert_eq!(updates[3]["stopReason"], "end_turn");
    client.close_stdin().await;
}

#[tokio::test]
async fn requires_action_then_running_before_next_mapped_event() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    daemon.emit(running("r1", "peer-1", None)).await;
    daemon_barrier(&mut client, &mut daemon, "running").await;

    daemon
        .emit(
            json!({"type":"tool_permission_requested","request_id":"p1","tool_call_id":"call-1",
            "tool_name":"bash","title":"Run bash","input":{"command":"echo hi"}}),
        )
        .await;
    let mut lines = Vec::new();
    let call = loop {
        let line = client.recv().await;
        if line["method"] == "session/request_permission" {
            break line["id"].clone();
        }
        lines.push(line);
    };
    // `requires_action` may follow the request it reports.
    while kinds(&lines).is_empty() {
        lines.push(client.recv().await);
    }
    assert_eq!(kinds(&lines), ["state_update:requires_action"], "{lines:?}");

    client
        .send(json!({"jsonrpc":"2.0","id":call,"result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}}))
        .await;
    let decision = daemon.recv_type("permission_decision").await;
    daemon.emit(json!({"type":"ack","id":decision["id"]})).await;
    assert!(
        kinds(&daemon_barrier(&mut client, &mut daemon, "answered").await) == ["state_update:running"],
        "the marker is the next mapped event, so running precedes it"
    );

    daemon.emit(text("after")).await;
    daemon.emit(idle("r1", "peer-1", "end_turn")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "end").await;
    assert_eq!(kinds(&lines), ["agent_message_chunk", "state_update:idle"], "{lines:?}");
    client.close_stdin().await;
}

#[tokio::test]
async fn idle_once_per_run_and_after_last_update() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    daemon.emit(running("r1", "peer-1", None)).await;
    daemon.emit(running("r1", "peer-1", None)).await;
    daemon.emit(text("one")).await;
    daemon.emit(text("two")).await;
    daemon.emit(idle("r1", "peer-1", "end_turn")).await;
    daemon.emit(idle("r1", "peer-1", "end_turn")).await;
    daemon.emit(json!({"type":"interrupted"})).await;
    daemon.emit(running("r2", "peer-2", None)).await;
    daemon.emit(idle("r2", "peer-2", "max_tokens")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "end").await;
    assert_eq!(
        kinds(&lines),
        [
            "state_update:running",
            "agent_message_chunk",
            "agent_message_chunk",
            "state_update:idle",
            "state_update:running",
            "state_update:idle",
        ],
        "{lines:?}"
    );
    let updates = updates(&lines);
    assert_eq!(updates[3]["stopReason"], "end_turn");
    assert_eq!(updates[5]["stopReason"], "max_tokens");
    client.close_stdin().await;
}

/// IS-5 across two runs: the idle of a peer run that arrives only after this
/// editor's run started neither ends that run nor takes its message ids. The
/// peer run is reported over when the own run starts, once, without a stop
/// reason, and the own run keeps its state until its own idle.
#[tokio::test]
async fn late_peer_idle_does_not_end_the_own_run() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    daemon
        .emit(running("peer-run", "peer-1", Some("from the desktop")))
        .await;
    daemon.emit(text("peer answer")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "peer").await;
    assert_eq!(
        kinds(&lines),
        ["state_update:running", "user_message", "agent_message_chunk"],
        "{lines:?}"
    );

    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;
    daemon.emit(inserted(id, "m1")).await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert_eq!(response["result"], json!({"messageId":"m1"}), "{response}");
    daemon.emit(running("own-run", "m1", Some("hi"))).await;
    daemon.emit(text("own one")).await;
    daemon.emit(idle("peer-run", "peer-1", "end_turn")).await;
    daemon.emit(text("own two")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "own").await;
    assert_eq!(
        kinds(&lines),
        [
            "user_message",
            "state_update:idle",
            "state_update:running",
            "agent_message_chunk",
            "agent_message_chunk",
        ],
        "{lines:?}"
    );
    let own = updates(&lines);
    assert_eq!(
        own[1],
        &json!({"sessionUpdate":"state_update","state":"idle"}),
        "the superseded peer run ends without a stop reason it does not know"
    );
    let own_agent = super::shapes::message_ids("m1").agent();
    for chunk in &own[3..] {
        assert_eq!(chunk["messageId"], json!(own_agent), "{chunk}");
    }

    daemon.emit(json!({"type":"done","id":id})).await;
    daemon.emit(idle("own-run", "m1", "end_turn")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "end").await;
    assert_eq!(kinds(&lines), ["state_update:idle"], "{lines:?}");
    assert_eq!(updates(&lines)[0]["stopReason"], "end_turn");
    client.close_stdin().await;
}

#[tokio::test]
async fn close_writes_idle_cancelled_before_response() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;
    daemon.emit(inserted(id, "m1")).await;
    daemon.emit(running("r1", "m1", None)).await;
    daemon_barrier(&mut client, &mut daemon, "running").await;

    client
        .send(json!({"jsonrpc":"2.0","id":"close","method":"session/close","params":{"sessionId":SID}}))
        .await;
    let cancel = daemon.recv().await;
    assert_eq!(cancel["type"], "cancel", "{cancel}");
    daemon.emit(text("last words")).await;
    daemon.emit(json!({"type":"interrupted"})).await;
    daemon.emit(json!({"type":"done","id":id})).await;
    daemon.emit(idle("r1", "m1", "cancelled")).await;
    let (before, response) = client.recv_response(json!("close")).await;
    assert_eq!(response["result"], json!({}), "{response}");
    assert_eq!(
        kinds(&before),
        ["agent_message_chunk", "state_update:idle"],
        "{before:?}"
    );
    assert_eq!(updates(&before)[1]["stopReason"], "cancelled");
    assert!(client.runtime.sessions.lock().await.is_empty());
    client.close_stdin().await;
}

#[tokio::test]
async fn close_bound_synthesizes_idle_and_drops_late_events() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client, Duration::from_millis(200)).await;
    daemon.emit(running("r1", "peer-1", None)).await;
    daemon_barrier(&mut client, &mut daemon, "running").await;

    client
        .send(json!({"jsonrpc":"2.0","id":"close","method":"session/close","params":{"sessionId":SID}}))
        .await;
    assert_eq!(daemon.recv().await["type"], "cancel");
    let (before, response) = client.recv_response(json!("close")).await;
    assert_eq!(response["result"], json!({}), "{response}");
    assert_eq!(
        updates(&before),
        [&json!({"sessionUpdate":"state_update","state":"idle","stopReason":"cancelled"})],
        "{before:?}"
    );

    daemon.emit(text("too late")).await;
    daemon.emit(idle("r1", "peer-1", "cancelled")).await;
    daemon
        .emit(json!({"type":"session_renamed","session_id":SID,"display_title":"late"}))
        .await;
    let v2 = Arc::clone(session.v2.get().expect("v2 state"));
    assert!(v2.is_closed());
    assert!(
        client.barrier("late").await.is_empty(),
        "the closed session writes nothing more"
    );
    client.close_stdin().await;
}

#[tokio::test]
async fn close_withdraws_uninserted_prompt_32800() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;

    client
        .send(json!({"jsonrpc":"2.0","id":"close","method":"session/close","params":{"sessionId":SID}}))
        .await;
    let withdraw = daemon.recv().await;
    assert_eq!(withdraw["type"], "withdraw_message", "{withdraw}");
    assert_eq!(withdraw["message_request_id"], id);
    daemon.emit(json!({"type":"done","id":withdraw["id"]})).await;
    let mut answers = HashMap::new();
    let mut lines = Vec::new();
    while answers.len() < 2 {
        let line = client.recv().await;
        match line.get("id") {
            Some(id) => {
                answers.insert(id.to_string(), line);
            }
            None => lines.push(line),
        }
    }
    assert_eq!(answers["1"]["error"]["code"], JSONRPC_REQUEST_CANCELLED, "{answers:?}");
    assert_eq!(answers["\"close\""]["result"], json!({}), "{answers:?}");
    assert!(
        kinds(&lines).is_empty(),
        "an uninserted prompt writes no update: {lines:?}"
    );
    client.close_stdin().await;
}

/// A prompt answered `{messageId}` whose turn has not reported `running` yet.
async fn accepted_not_running(client: &mut Client, daemon: &mut FakeDaemon) {
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(daemon).await;
    daemon.emit(inserted(id, "m1")).await;
    let (before, response) = client.recv_response(json!(1)).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["result"], json!({"messageId":"m1"}), "{response}");
    let lines = daemon_barrier(client, daemon, "accepted").await;
    assert_eq!(kinds(&lines), ["user_message"], "{lines:?}");
}

async fn send_close(client: &mut Client) {
    client
        .send(json!({"jsonrpc":"2.0","id":"close","method":"session/close","params":{"sessionId":SID}}))
        .await;
}

#[tokio::test]
async fn close_cancels_prompt_accepted_before_running() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client, Duration::from_millis(200)).await;
    accepted_not_running(&mut client, &mut daemon).await;

    send_close(&mut client).await;
    let cancel = daemon.recv().await;
    assert_eq!(cancel["type"], "cancel", "the accepted turn is cancelled: {cancel}");
    let (before, response) = client.recv_response(json!("close")).await;
    assert_eq!(response["result"], json!({}), "{response}");
    assert_eq!(
        updates(&before),
        [&json!({"sessionUpdate":"state_update","state":"idle","stopReason":"cancelled"})],
        "the accepted turn reports its end before the close answer: {before:?}"
    );

    daemon.emit(running("r1", "m1", None)).await;
    daemon.emit(text("too late")).await;
    daemon.emit(idle("r1", "m1", "cancelled")).await;
    daemon
        .emit(json!({"type":"session_renamed","session_id":SID,"display_title":"late"}))
        .await;
    assert!(session.v2.get().expect("v2 state").is_closed());
    assert!(
        client.barrier("late").await.is_empty(),
        "nothing of the turn follows the close"
    );
    assert!(client.runtime.sessions.lock().await.is_empty());
    client.close_stdin().await;
}

#[tokio::test]
async fn close_waits_for_daemon_idle_of_prompt_accepted_before_running() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    accepted_not_running(&mut client, &mut daemon).await;

    send_close(&mut client).await;
    assert_eq!(daemon.recv().await["type"], "cancel");
    daemon.emit(idle("r1", "m1", "cancelled")).await;
    let (before, response) = client.recv_response(json!("close")).await;
    assert_eq!(response["result"], json!({}), "{response}");
    assert_eq!(
        updates(&before),
        [&json!({"sessionUpdate":"state_update","state":"idle","stopReason":"cancelled"})],
        "{before:?}"
    );
    client.close_stdin().await;
}

/// The close finds the prompt queued; the withdrawal loses to the turn, which
/// claimed it and has not reported `running` when the refusal is read.
#[tokio::test]
async fn close_cancels_turn_that_won_the_withdrawal_before_running() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, Duration::from_millis(200)).await;
    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;

    send_close(&mut client).await;
    let withdraw = daemon.recv().await;
    assert_eq!(withdraw["type"], "withdraw_message", "{withdraw}");
    daemon.emit(inserted(id, "m1")).await;
    daemon
        .emit(json!({"type":"error","id":withdraw["id"],"message":"already inserted"}))
        .await;
    let cancel = daemon.recv().await;
    assert_eq!(cancel["type"], "cancel", "the inserted turn is cancelled: {cancel}");
    let (before, response) = client.recv_response(json!("close")).await;
    assert_eq!(response["result"], json!({}), "{response}");
    let answer = before
        .iter()
        .find(|line| line["id"] == json!(1))
        .expect("the prompt is answered before the close");
    assert_eq!(answer["result"], json!({"messageId":"m1"}), "{answer}");
    assert_eq!(kinds(&before), ["user_message", "state_update:idle"], "{before:?}");
    assert_eq!(updates(&before)[1]["stopReason"], "cancelled");
    client.close_stdin().await;
}

/// IS-7: a close that gives up ends the run's open calls before the
/// synthesized idle update, so no tool call is left `in_progress`.
#[tokio::test]
async fn close_give_up_ends_open_tool_calls_before_idle() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, Duration::from_millis(200)).await;
    daemon.emit(running("r1", "peer-1", None)).await;
    daemon.emit(json!({"type":"tool_start","id":"c1","name":"bash"})).await;
    let opened = daemon_barrier(&mut client, &mut daemon, "running").await;
    assert_eq!(
        kinds(&opened),
        ["state_update:running", "tool_call_update"],
        "{opened:?}"
    );

    send_close(&mut client).await;
    assert_eq!(daemon.recv().await["type"], "cancel");
    let (before, response) = client.recv_response(json!("close")).await;
    assert_eq!(response["result"], json!({}), "{response}");
    assert_eq!(
        kinds(&before),
        ["tool_call_update", "state_update:idle"],
        "the open call ends before the synthesized idle: {before:?}"
    );
    let updates = updates(&before);
    assert_eq!(
        updates[0],
        &json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"cancelled"})
    );
    assert_eq!(updates[1]["stopReason"], "cancelled");
    client.close_stdin().await;
}

/// The `user_message` of an inserted prompt follows the prompt's turn being
/// opened: the prompt is still pending at that moment, so a close that reads
/// the session between the two operations cannot call it settled.
#[tokio::test]
async fn insertion_opens_the_turn_before_the_prompt_stops_pending() {
    let mut client = v2_client().await;
    let (session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    let v2 = Arc::clone(session.v2.get().expect("v2 state"));
    let observed = Arc::new(std::sync::Mutex::new(None));
    let recorded = Arc::clone(&observed);
    v2.set_inserted_probe(Box::new(move |_, pending| {
        *recorded.lock().expect("the probe lock is not poisoned") = Some(pending);
    }));

    client.send(prompt(json!(1), SID)).await;
    let id = next_message(&mut daemon).await;
    daemon.emit(inserted(id, "m1")).await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert_eq!(response["result"], json!({"messageId":"m1"}), "{response}");
    assert_eq!(
        *observed.lock().expect("the probe lock is not poisoned"),
        Some(true),
        "the prompt must still be pending when its turn opens"
    );
    client.close_stdin().await;
}

/// A command whose stream ends between its hand-off and its answer still gets
/// exactly one response: the `Err` of `written` is answered rather than
/// dropped, as a `Control::LocalCommand` queued behind `Control::Close` is.
#[tokio::test]
async fn command_is_answered_when_its_stream_ends_before_answering() {
    let mut client = v2_client().await;
    let (session, _daemon) = daemon_pair(SID);
    let events = session.open_v2_stream();
    let v2 = Arc::new(V2Session::new(events, PROMPT_CANCEL_BOUND));
    assert!(session.v2.set(Arc::clone(&v2)).is_ok());
    client.runtime.install_session_bridge(&session);
    offer_models(&session, 1).await;
    let session = Arc::new(session);
    client
        .runtime
        .sessions
        .lock()
        .await
        .insert(SID.to_string(), Arc::clone(&session));
    // No stream task: the test plays it and ends it after taking the command.
    let (mut control, _events) = v2.take_stream_receivers().expect("the stream was not started");

    client
        .send(json!({"jsonrpc":"2.0","id":"cmd","method":"session/prompt",
            "params":{"sessionId":SID,"prompt":[{"type":"text","text":"/model"}]}}))
        .await;
    assert!(
        matches!(control.recv().await, Some(Control::LocalCommand { .. })),
        "the command is handed to the stream"
    );
    drop(control);
    let (before, response) = client.recv_response(json!("cmd")).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
    assert_eq!(response["error"]["message"], SESSION_CLOSED, "{response}");
    client.close_stdin().await;
}

/// A prompt that arrives after the session's stream closed is answered, never
/// left waiting: `register` refuses it and the handler writes the error.
#[tokio::test]
async fn prompt_after_close_is_answered() {
    let mut client = v2_client().await;
    let (session, _daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    let v2 = Arc::clone(session.v2.get().expect("v2 state"));
    let (done, closed) = tokio::sync::oneshot::channel();
    assert!(v2.send_control(Control::Close { give_up: false, done }));
    closed.await.expect("the stream closes");

    client.send(prompt(json!(1), SID)).await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
    assert_eq!(response["error"]["message"], SESSION_CLOSED, "{response}");
    client.close_stdin().await;
}

/// IS-7: the daemon going away mid-run ends the run's open calls as failed
/// before the idle `error` update, so no tool call is left `in_progress`.
#[tokio::test]
async fn daemon_disconnect_ends_open_tool_calls_before_idle_error() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client, PROMPT_CANCEL_BOUND).await;
    daemon.emit(running("r1", "peer-1", None)).await;
    daemon.emit(json!({"type":"tool_start","id":"c1","name":"bash"})).await;
    daemon_barrier(&mut client, &mut daemon, "running").await;

    drop(daemon);
    let mut lines = Vec::new();
    loop {
        let line = client.recv().await;
        let update = &line["params"]["update"];
        let last = update["sessionUpdate"] == "state_update" && update["state"] == "idle";
        lines.push(line);
        if last {
            break;
        }
    }
    assert_eq!(kinds(&lines), ["tool_call_update", "state_update:idle"], "{lines:?}");
    let updates = updates(&lines);
    assert_eq!(updates[0]["status"], "failed", "{lines:?}");
    assert_eq!(updates[1]["stopReason"], "error", "{lines:?}");
    client.close_stdin().await;
}

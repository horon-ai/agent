//! IS-2 over a scripted daemon: `tool_call/cancel` on an advertised call, and
//! the refusals that never reach the daemon.

use super::super::tests::connection_core::*;
use super::super::*;
use super::state::V2Session;

const SID: &str = "s1";

async fn v2_client() -> Client {
    let mut client = start_client_with(None);
    client.initialize_v2().await;
    client
}

/// A v2 session on a scripted daemon, wired as `session/new` wires one: v2
/// state, side channel, a usable model, then its stream task.
async fn attach_v2(client: &Client) -> (Arc<DaemonSession>, FakeDaemon) {
    let (session, daemon) = daemon_pair(SID);
    let events = session.open_v2_stream();
    assert!(
        session
            .v2
            .set(Arc::new(V2Session::new(events, PROMPT_CANCEL_BOUND)))
            .is_ok()
    );
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

fn running(run_id: &str, start: &str) -> Value {
    json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":run_id,
        "start_message_id":start})
}

fn idle(run_id: &str, start: &str, stop_reason: &str) -> Value {
    json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":run_id,
        "start_message_id":start,"stop_reason":stop_reason})
}

/// The announced, streaming and executing phases of one call.
fn call_started(id: &str, name: &str, input: &Value) -> Vec<Value> {
    vec![
        json!({"type":"tool_start","id":id,"name":name}),
        json!({"type":"tool_input","id":id,"delta":input.to_string()}),
        json!({"type":"tool_exec","id":id,"name":name}),
    ]
}

fn cancel_request(id: Value, tool_call_id: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tool_call/cancel",
        "params":{"sessionId":SID,"toolCallId":tool_call_id}})
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

fn updates(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line["method"] == "session/update")
        .map(|line| line["params"]["update"].clone())
        .collect()
}

fn kinds(lines: &[Value]) -> Vec<String> {
    updates(lines)
        .iter()
        .map(|update| {
            let kind = update["sessionUpdate"].as_str().unwrap_or_default().to_string();
            match update["state"].as_str() {
                Some(state) => format!("{kind}:{state}"),
                None => kind,
            }
        })
        .collect()
}

/// The tool-call updates the adapter wrote for `tool_call_id`, in order.
fn call_frames<'a>(lines: &'a [Value], tool_call_id: &str) -> Vec<&'a Value> {
    lines
        .iter()
        .filter(|line| line["method"] == "session/update")
        .map(|line| &line["params"]["update"])
        .filter(|update| update["sessionUpdate"] == "tool_call_update" && update["toolCallId"] == tool_call_id)
        .collect()
}

/// The daemon's answer to a `cancel_tool_call`.
fn cancel_result(id: u64, accepted: bool) -> Value {
    json!({"type":"tool_call_cancel_result","id":id,"accepted":accepted,
        "reason":(!accepted).then_some("unknown")})
}

/// QA happy: an advertised running call is cancelled through the daemon, the
/// response is `{}`, the next tool frame is the final `cancelled` update with
/// `capabilities: null`, and the `tool_done` that follows with the result the
/// model reads adds no second final update.
#[tokio::test]
async fn accepted_tool_call_cancel_answers_empty_and_reports_cancelled_once() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(running("r1", "m-1")).await;
    for event in call_started("c1", "bash", &json!({"command":"sleep 30"})) {
        daemon.emit(event).await;
    }
    let first = daemon_barrier(&mut client, &mut daemon, "running").await;
    // The call's creation, its advertising patch, and the display terminal a
    // bash call shows once its command is known.
    assert_eq!(
        kinds(&first),
        [
            "state_update:running",
            "tool_call_update",
            "tool_call_update",
            "tool_call_content_chunk",
            "terminal_update"
        ],
        "{first:?}"
    );
    let advertised = call_frames(&first, "c1")
        .into_iter()
        .find(|frame| frame["status"] == "in_progress")
        .cloned()
        .unwrap_or_else(|| panic!("the in_progress update: {first:?}"));
    assert_eq!(
        advertised["capabilities"],
        json!({"cancel":{}}),
        "a running call advertises its cancel control: {advertised}"
    );

    client.send(cancel_request(json!("cancel"), "c1")).await;
    let request = daemon.recv_type("cancel_tool_call").await;
    let request_id = request["id"].as_u64().expect("a daemon request id");
    assert_eq!(request["call_id"], "c1", "{request}");
    daemon.emit(cancel_result(request_id, true)).await;
    let (before, response) = client.recv_response(json!("cancel")).await;
    assert_eq!(response["result"], json!({}), "{response}");
    assert!(
        before.iter().all(|line| line.get("id").is_none()),
        "the response is the only answer: {before:?}"
    );

    daemon.emit(json!({"type":"tool_cancelled","id":"c1"})).await;
    daemon
        .emit(json!({"type":"tool_done","id":"c1","name":"bash",
            "output":"[Cancelled by user]","error":"[Cancelled by user]"}))
        .await;
    daemon.emit(idle("r1", "m-1", "end_turn")).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "cancelled").await;
    let cancelled: Vec<&Value> = call_frames(&lines, "c1")
        .into_iter()
        .filter(|frame| frame["status"].as_str().is_some_and(is_final))
        .collect();
    assert_eq!(cancelled.len(), 1, "one final update, no second one: {lines:?}");
    assert_eq!(cancelled[0]["status"], "cancelled", "{:?}", cancelled[0]);
    assert!(
        cancelled[0].get("capabilities").is_some_and(Value::is_null),
        "a final call clears its capabilities: {:?}",
        cancelled[0]
    );
    // No state update lands between the response and the cancellation.
    assert_eq!(
        kinds(&lines),
        ["tool_call_update", "state_update:idle"],
        "the cancellation is the only tool frame before the idle: {lines:?}"
    );
    client.close_stdin().await;
}

fn is_final(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

/// QA failure: a call that never advertised the control, one the session does
/// not know, and one that already finished are all `-32602` and write nothing
/// to the daemon; so is a second cancel of a call already being cancelled.
#[tokio::test]
async fn unadvertised_calls_are_invalid_params_without_a_daemon_request() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(running("r1", "m-1")).await;
    // A call announced but not yet executing: its update is `pending`.
    daemon
        .emit(json!({"type":"tool_start","id":"pending","name":"bash"}))
        .await;
    for event in call_started("c1", "bash", &json!({"command":"sleep 30"})) {
        daemon.emit(event).await;
    }
    let lines = daemon_barrier(&mut client, &mut daemon, "running").await;
    let pending = call_frames(&lines, "pending")
        .into_iter()
        .next()
        .cloned()
        .unwrap_or_else(|| panic!("the pending update: {lines:?}"));
    assert_eq!(pending["status"], "pending", "{pending}");
    assert!(pending.get("capabilities").is_none(), "{pending}");

    for (id, tool_call_id) in [("p", "pending"), ("u", "never-started")] {
        client.send(cancel_request(json!(id), tool_call_id)).await;
        let (_, response) = client.recv_response(json!(id)).await;
        assert_eq!(
            response["error"]["code"], JSONRPC_INVALID_PARAMS,
            "{tool_call_id}: {response}"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), daemon.recv())
            .await
            .is_err(),
        "an unadvertised cancel writes no daemon request"
    );

    // A call that finished advertises nothing any more either.
    daemon
        .emit(json!({"type":"tool_done","id":"c1","name":"bash","output":"done"}))
        .await;
    let lines = daemon_barrier(&mut client, &mut daemon, "finished").await;
    let final_frame = call_frames(&lines, "c1")
        .into_iter()
        .find(|frame| frame["status"] == "completed")
        .cloned()
        .unwrap_or_else(|| panic!("the completed update: {lines:?}"));
    assert!(
        final_frame.get("capabilities").is_some_and(Value::is_null),
        "{final_frame}"
    );
    client.send(cancel_request(json!("done"), "c1")).await;
    let (_, response) = client.recv_response(json!("done")).await;
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), daemon.recv())
            .await
            .is_err(),
        "a finished call writes no daemon request"
    );

    // The same call twice: only the first reaches the daemon.
    for event in call_started("c2", "bash", &json!({"command":"sleep 30"})) {
        daemon.emit(event).await;
    }
    daemon_barrier(&mut client, &mut daemon, "second").await;
    client.send(cancel_request(json!("first"), "c2")).await;
    let request = daemon.recv_type("cancel_tool_call").await;
    assert_eq!(request["call_id"], "c2", "{request}");
    let request_id = request["id"].as_u64().expect("a daemon request id");
    daemon.emit(cancel_result(request_id, true)).await;
    let (_, response) = client.recv_response(json!("first")).await;
    assert_eq!(response["result"], json!({}), "{response}");

    client.send(cancel_request(json!("second"), "c2")).await;
    let (_, response) = client.recv_response(json!("second")).await;
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), daemon.recv())
            .await
            .is_err(),
        "the second cancel of one call writes no daemon request"
    );
    client.close_stdin().await;
}

/// A daemon that refuses an advertised call answers `-32602`, not an empty
/// result: the call finished between its advertisement and the request.
#[tokio::test]
async fn daemon_refusal_is_invalid_params() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(running("r1", "m-1")).await;
    for event in call_started("c1", "bash", &json!({"command":"sleep 30"})) {
        daemon.emit(event).await;
    }
    daemon_barrier(&mut client, &mut daemon, "running").await;
    client.send(cancel_request(json!("cancel"), "c1")).await;
    let request = daemon.recv_type("cancel_tool_call").await;
    let request_id = request["id"].as_u64().expect("a daemon request id");
    daemon.emit(cancel_result(request_id, false)).await;
    let (_, response) = client.recv_response(json!("cancel")).await;
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
    client.close_stdin().await;
}

/// A session this connection does not own is `-32002` naming it, and a request
/// without `toolCallId` is `-32602`; neither writes to the daemon.
#[tokio::test]
async fn unknown_session_and_missing_id_are_refused() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    client
        .send(json!({"jsonrpc":"2.0","id":"s","method":"tool_call/cancel",
            "params":{"sessionId":"nope","toolCallId":"c1"}}))
        .await;
    let (_, response) = client.recv_response(json!("s")).await;
    assert_eq!(response["error"]["code"], ACP_RESOURCE_NOT_FOUND, "{response}");
    assert_eq!(response["error"]["data"]["sessionId"], "nope", "{response}");

    client
        .send(json!({"jsonrpc":"2.0","id":"m","method":"tool_call/cancel","params":{"sessionId":SID}}))
        .await;
    let (_, response) = client.recv_response(json!("m")).await;
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), daemon.recv())
            .await
            .is_err(),
        "neither refusal writes a daemon request"
    );
    client.close_stdin().await;
}

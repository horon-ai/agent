//! IS-5, IS-6, IS-12 and IS-13 over a scripted daemon: v2 `session/new`,
//! `session/resume` with and without replay, the current-state snapshot, and
//! the `session/list` rows.

use super::super::tests::connection_core::*;
use super::super::*;
use super::tests::{assert_matches, assert_update_matches};
use crate::session::reasoning_line_markup;
use tokio::sync::mpsc;

const SID: &str = "session_t10_resume";
const CWD: &str = "/work";
const TITLE: &str = "Fix the parser";
const UPDATED_AT: &str = "2026-10-28T12:00:00.000Z";

fn catalog() -> SessionCatalog {
    Arc::new(|session_id: &str| {
        (session_id == SID).then(|| CatalogInfo {
            title: Some(TITLE.to_string()),
            updated_at: UPDATED_AT.to_string(),
        })
    })
}

/// A v2 client whose daemon connections the test scripts.
async fn v2_client() -> (Client, mpsc::UnboundedReceiver<FakeDaemon>) {
    let (connector, daemons) = scripted_daemon();
    let mut client = start_client_with_catalog(connector, catalog());
    client.initialize_v2().await;
    (client, daemons)
}

fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

fn resume(id: Value, replay_from: Option<Value>) -> Value {
    let mut params = json!({"sessionId":SID,"cwd":CWD,"mcpServers":[]});
    if let Some(cursor) = replay_from {
        params["replayFrom"] = cursor;
    }
    request(id, "session/resume", params)
}

fn history(id: &Value, messages: Value) -> Value {
    json!({"type":"history","id":id,"session_id":SID,"provider_model":"m0",
        "available_models":["m0","m1"],"messages":messages,"acp_protocol_version":2})
}

fn snapshot(running: bool, awaiting_user: bool, start: Option<&str>) -> Value {
    json!({"type":"foreground_state","session_id":SID,"running":running,
        "run_id":if running { "r1" } else { "" },"start_message_id":start,
        "awaiting_user":awaiting_user,"snapshot":true})
}

fn idle_snapshot() -> Value {
    snapshot(false, false, None)
}

/// A reasoning block as the daemon renders it into an assistant row.
fn thought(text: &str) -> String {
    format!("{}\n", reasoning_line_markup(text))
}

/// The daemon end of one v2 attach: the target's History and `Done`, then
/// `after_done` (the foreground snapshot and whatever the daemon queued around
/// it), then the approval-mode resync the attach sends.
async fn serve_attach(
    daemons: &mut mpsc::UnboundedReceiver<FakeDaemon>,
    messages: Value,
    after_done: Vec<Value>,
) -> FakeDaemon {
    let mut daemon = within("the attach connection", daemons.recv())
        .await
        .expect("the attach connection");
    let subscribe = daemon.recv_type("subscribe").await;
    assert_eq!(subscribe["target_session_id"], SID, "{subscribe}");
    daemon.emit(json!({"type":"ack","id":subscribe["id"]})).await;
    daemon.emit(history(&subscribe["id"], messages)).await;
    daemon.emit(json!({"type":"done","id":subscribe["id"]})).await;
    for event in after_done {
        daemon.emit(event).await;
    }
    let mode = daemon.recv_type("set_session_approval_mode").await;
    daemon.emit(json!({"type":"ack","id":mode["id"]})).await;
    daemon
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

/// Every update is a valid v2 update, and none is a v1 `tool_call` or a
/// `current_mode_update`.
fn assert_v2_updates(lines: &[Value]) {
    for update in updates(lines) {
        assert_update_matches(update);
        let kind = update["sessionUpdate"].as_str().unwrap_or_default();
        assert!(
            kind != "tool_call" && kind != "current_mode_update",
            "a v1-only update on v2: {update}"
        );
    }
}

/// The `messageId` of every agent message and thought update, in order.
fn agent_ids(lines: &[Value]) -> Vec<String> {
    updates(lines)
        .into_iter()
        .filter(|update| {
            update["sessionUpdate"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("agent_"))
        })
        .map(|update| update["messageId"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every line the session's stream wrote for daemon events sent before this
/// point: it writes the marker's `session_info_update` after them.
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

/// One finished turn of `m1` as the History stores it: the prompt, a round
/// with reasoning, text and a read call, the call's result, and the answer.
fn finished_turn() -> Value {
    json!([
        {"role":"user","content":"read a.rs","message_id":"m1","turn_start":true},
        {"role":"assistant","content":format!("{}I will read it.", thought("checking the file")),
            "tool_calls":["read"],"message_id":"m1-a"},
        {"role":"tool","content":"fn main() {}","message_id":"m1-r",
            "tool_data":{"id":"call_1","name":"read","input":{"file_path":"a.rs"}}},
        {"role":"assistant","content":"done","message_id":"m1-b"}
    ])
}

#[tokio::test]
async fn resume_without_cursor_does_not_replay() {
    for (id, cursor) in [(1, None), (2, Some(Value::Null))] {
        let (mut client, mut daemons) = v2_client().await;
        client.send(resume(json!(id), cursor)).await;
        let _daemon = serve_attach(&mut daemons, finished_turn(), vec![idle_snapshot()]).await;
        let (before, response) = client.recv_response(json!(id)).await;
        assert_eq!(
            kinds(&before),
            ["session_info_update", "state_update:idle"],
            "attached without replay: {before:?}"
        );
        assert_v2_updates(&before);
        assert_matches("ResumeSessionResponse", &response["result"]);
        client.close_stdin().await;
    }
}

#[tokio::test]
async fn resume_from_start_replays_upserts_then_snapshot_then_response() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), Some(json!({"type":"start"})))).await;
    let _daemon = serve_attach(&mut daemons, finished_turn(), vec![idle_snapshot()]).await;
    let (before, response) = client.recv_response(json!(1)).await;
    assert_eq!(
        kinds(&before),
        [
            "user_message",
            "agent_thought",
            "agent_message",
            "tool_call_update",
            "agent_message",
            "session_info_update",
            "state_update:idle"
        ],
        "{before:?}"
    );
    assert_v2_updates(&before);
    let updates = updates(&before);
    assert_eq!(
        updates[0],
        &json!({"sessionUpdate":"user_message","messageId":"m1",
            "content":[{"type":"text","text":"read a.rs"}]})
    );
    assert_eq!(updates[1]["messageId"], "m1:t1");
    assert_eq!(
        updates[1]["content"],
        json!([{"type":"text","text":"checking the file"}])
    );
    assert_eq!(updates[2]["messageId"], "m1:a1");
    assert_eq!(updates[2]["content"], json!([{"type":"text","text":"I will read it."}]));
    assert_eq!(
        updates[3],
        &json!({"sessionUpdate":"tool_call_update","toolCallId":"call_1","name":"read",
            "title":tool_title("read"),"kind":tool_kind("read"),"status":"completed",
            "content":[{"type":"content","content":{"type":"text","text":"fn main() {}"}}],
            "locations":[{"path":"/work/a.rs"}],"rawInput":{"file_path":"a.rs"}})
    );
    assert_eq!(updates[4]["messageId"], "m1:a2");
    assert!(
        updates[6].get("stopReason").is_none(),
        "an idle snapshot reports no completion: {}",
        updates[6]
    );
    assert_matches("ResumeSessionResponse", &response["result"]);
    client.close_stdin().await;
}

#[tokio::test]
async fn replayed_user_ids_equal_prompt_ids() {
    // A prompt on one connection is answered with the id the daemon stored.
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), None)).await;
    let mut daemon = serve_attach(&mut daemons, json!([]), vec![idle_snapshot()]).await;
    client.recv_response(json!(1)).await;
    client.send(prompt(json!(2), SID)).await;
    let message = daemon.recv_type("message").await;
    daemon
        .emit(json!({"type":"user_message_inserted","id":message["id"],"message_id":"stored-7"}))
        .await;
    let (_, response) = client.recv_response(json!(2)).await;
    let prompt_id = response["result"]["messageId"]
        .as_str()
        .expect("a messageId")
        .to_string();
    client.close_stdin().await;

    // A later connection replays that stored row under the same id.
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), Some(json!({"type":"start"})))).await;
    let rows = json!([
        {"role":"user","content":"hi","message_id":prompt_id},
        {"role":"assistant","content":"hello","message_id":"stored-8"}
    ]);
    let _daemon = serve_attach(&mut daemons, rows, vec![idle_snapshot()]).await;
    let (before, _) = client.recv_response(json!(1)).await;
    let users: Vec<&Value> = updates(&before)
        .into_iter()
        .filter(|update| update["sessionUpdate"] == "user_message")
        .collect();
    assert_eq!(users.len(), 1, "{before:?}");
    assert_eq!(users[0]["messageId"], prompt_id.as_str());
    assert_eq!(agent_ids(&before), [format!("{prompt_id}:a1")]);
    client.close_stdin().await;
}

#[tokio::test]
async fn bad_cursor_invalid_params_without_replay() {
    let (mut client, mut daemons) = v2_client().await;
    let cursors = [
        json!({"type":"_later"}),
        json!({"type":"later"}),
        json!({}),
        json!({"type":3}),
        json!("start"),
        json!(7),
    ];
    for (index, cursor) in cursors.into_iter().enumerate() {
        let id = json!(index);
        client.send(resume(id.clone(), Some(cursor.clone()))).await;
        let (before, response) = client.recv_response(id).await;
        assert!(before.is_empty(), "{cursor}: {before:?}");
        assert_eq!(
            response["error"]["code"], JSONRPC_INVALID_PARAMS,
            "{cursor}: {response}"
        );
    }
    assert!(client.barrier("cursors").await.is_empty());
    assert!(daemons.try_recv().is_err(), "a refused cursor attaches nothing");
    client.close_stdin().await;
}

/// The daemon end of one `session/new` up to its History: the Subscribe, then
/// the History of the new session `new-1`.
async fn serve_new_session(daemons: &mut mpsc::UnboundedReceiver<FakeDaemon>) -> FakeDaemon {
    let mut daemon = within("the session/new connection", daemons.recv())
        .await
        .expect("the session/new connection");
    let subscribe = daemon.recv_type("subscribe").await;
    daemon.emit(json!({"type":"done","id":subscribe["id"]})).await;
    let get_history = daemon.recv_type("get_history").await;
    daemon
        .emit(json!({"type":"history","id":get_history["id"],"session_id":"new-1",
            "provider_model":"m0","available_models":["m0","m1"],"messages":[],
            "acp_protocol_version":2}))
        .await;
    daemon
}

#[tokio::test]
async fn new_response_has_commands_and_no_modes() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(request(json!(1), "session/new", json!({"cwd":CWD}))).await;
    let mut daemon = serve_new_session(&mut daemons).await;
    let mode = daemon.recv_type("set_session_approval_mode").await;
    daemon.emit(json!({"type":"ack","id":mode["id"]})).await;
    let (before, response) = client.recv_response(json!(1)).await;
    assert!(kinds(&before).is_empty(), "{before:?}");
    let result = &response["result"];
    assert_matches("NewSessionResponse", result);
    assert_eq!(result["sessionId"], "new-1");
    assert!(result.get("modes").is_none(), "{result}");
    assert_eq!(result["configOptions"][0]["configId"], "mode", "{result}");
    assert!(
        result["configOptions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|option| option.get("id").is_none()),
        "{result}"
    );
    let commands = result["availableCommands"].as_array().expect("availableCommands");
    assert_eq!(commands.len(), 3, "{result}");
    assert!(commands.iter().all(|command| command["type"] == "command"), "{result}");
    let after = client.barrier("after-new").await;
    assert!(
        kinds(&after).is_empty(),
        "no available_commands_update follows: {after:?}"
    );
    client.close_stdin().await;
}

/// IS-1: a daemon that predates the adapter's v2 support reuses the running
/// process, ignores the Subscribe's `protocol_version` and never sends
/// `UserMessageInserted`, so a v2 session/new is refused with a message that
/// names the restart instead of leaving every prompt rejected.
#[tokio::test]
async fn session_new_against_a_daemon_without_the_v2_ack_names_the_restart() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(request(json!(1), "session/new", json!({"cwd":CWD}))).await;
    let mut daemon = within("the session/new connection", daemons.recv())
        .await
        .expect("the session/new connection");
    let subscribe = daemon.recv_type("subscribe").await;
    daemon.emit(json!({"type":"done","id":subscribe["id"]})).await;
    let get_history = daemon.recv_type("get_history").await;
    daemon
        .emit(json!({"type":"history","id":get_history["id"],"session_id":"new-1",
            "provider_model":"m0","available_models":["m0"],"messages":[]}))
        .await;
    let (before, response) = client.recv_response(json!(1)).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
    let message = response["error"]["message"].as_str().expect("a message");
    assert!(
        message.contains("older") && message.contains("Restart"),
        "the refusal names the restart: {message}"
    );
    client.close_stdin().await;
}

/// IS-1: the same for `session/resume`, whose attach reads the Subscribe's
/// History directly.
#[tokio::test]
async fn session_resume_against_a_daemon_without_the_v2_ack_names_the_restart() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), None)).await;
    let mut daemon = within("the attach connection", daemons.recv())
        .await
        .expect("the attach connection");
    let subscribe = daemon.recv_type("subscribe").await;
    daemon.emit(json!({"type":"ack","id":subscribe["id"]})).await;
    daemon
        .emit(json!({"type":"history","id":subscribe["id"],"session_id":SID,
            "provider_model":"m0","available_models":["m0"],"messages":[]}))
        .await;
    let (before, response) = client.recv_response(json!(1)).await;
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(response["error"]["code"], JSONRPC_INTERNAL_ERROR, "{response}");
    let message = response["error"]["message"].as_str().expect("a message");
    assert!(
        message.contains("older") && message.contains("Restart"),
        "the refusal names the restart: {message}"
    );
    client.close_stdin().await;
}

#[tokio::test]
async fn mcp_sse_and_missing_type_rejected_v2() {
    let (mut client, mut daemons) = v2_client().await;
    let refused = [
        json!({"type":"sse","name":"remote","url":"http://127.0.0.1:1/sse","headers":[]}),
        json!({"name":"local","command":"/bin/mcp","args":[],"env":[]}),
    ];
    let mut id = 0;
    for server in refused {
        for method in ["session/new", "session/resume"] {
            id += 1;
            let params = json!({"sessionId":SID,"cwd":CWD,"mcpServers":[server.clone()]});
            client.send(request(json!(id), method, params)).await;
            let (before, response) = client.recv_response(json!(id)).await;
            assert!(before.is_empty(), "{method} {server}: {before:?}");
            assert_eq!(
                response["error"]["code"], JSONRPC_INVALID_PARAMS,
                "{method} {server}: {response}"
            );
        }
    }
    assert!(daemons.try_recv().is_err(), "a refused server list attaches nothing");

    // A typed stdio server is accepted and handed to the daemon.
    let server = json!({"type":"stdio","name":"local","command":"/bin/mcp","args":["--stdio"],"env":[]});
    client
        .send(request(
            json!(10),
            "session/new",
            json!({"cwd":CWD,"mcpServers":[server]}),
        ))
        .await;
    let mut daemon = serve_new_session(&mut daemons).await;
    let mcp = daemon.recv_type("configure_session_mcp").await;
    assert_eq!(mcp["servers"][0]["transport"], "stdio", "{mcp}");
    assert_eq!(mcp["servers"][0]["command"], "/bin/mcp", "{mcp}");
    daemon.emit(json!({"type":"done","id":mcp["id"]})).await;
    let mode = daemon.recv_type("set_session_approval_mode").await;
    daemon.emit(json!({"type":"ack","id":mode["id"]})).await;
    let (_, response) = client.recv_response(json!(10)).await;
    assert_eq!(response["result"]["sessionId"], "new-1", "{response}");
    client.close_stdin().await;
}

#[test]
fn list_rows_v2_shape() {
    let at = |seconds: i64| chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap();
    let row = |index: i64, title: Option<&str>| ListRow {
        session_id: format!("session_{index:03}"),
        cwd: Some("/work/a".to_string()),
        title: title.map(str::to_string),
        updated_at: at(index),
        last_active_at_ms: None,
        archived: false,
    };
    let mut rows: Vec<ListRow> = (0..51).map(|index| row(index, None)).collect();
    rows.push(row(60, Some("Newest")));
    let page = session_list_page(rows.clone(), None, None, AcpVersion::V2).unwrap();
    assert_matches("ListSessionsResponse", &page);
    let sessions = page["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 50);
    assert_eq!(
        sessions[0],
        json!({"sessionId":"session_060","cwd":"/work/a","title":"Newest",
            "updatedAt":"2027-01-15T08:01:00.000Z"})
    );
    assert!(sessions[1].get("title").is_none(), "{}", sessions[1]);
    for session in sessions {
        assert_matches("SessionInfo", session);
        let updated_at = session["updatedAt"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(updated_at).is_ok(), "{session}");
    }
    let cursor = page["nextCursor"].as_str().expect("a second page");
    let next = session_list_page(rows, None, Some(cursor), AcpVersion::V2).unwrap();
    assert_matches("ListSessionsResponse", &next);
    assert_eq!(next["sessions"].as_array().unwrap().len(), 2);
    assert!(next.get("nextCursor").is_none(), "{next}");
}

#[tokio::test]
async fn snapshot_running_when_processing() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), None)).await;
    let rows = json!([{"role":"user","content":"hi","message_id":"m1"}]);
    let mut daemon = serve_attach(&mut daemons, rows, vec![snapshot(true, false, Some("m1"))]).await;
    let (before, _) = client.recv_response(json!(1)).await;
    assert_eq!(
        kinds(&before),
        ["session_info_update", "state_update:running"],
        "{before:?}"
    );
    // The run keeps streaming under its own ids and ends once.
    daemon.emit(json!({"type":"text_delta","text":"working"})).await;
    daemon
        .emit(
            json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":"r1",
            "start_message_id":"m1","stop_reason":"end_turn"}),
        )
        .await;
    let lines = daemon_barrier(&mut client, &mut daemon, "ended").await;
    assert_eq!(kinds(&lines), ["agent_message_chunk", "state_update:idle"], "{lines:?}");
    assert_eq!(updates(&lines)[0]["messageId"], "m1:a1");
    assert_eq!(updates(&lines)[1]["stopReason"], "end_turn");
    client.close_stdin().await;
}

#[tokio::test]
async fn resume_without_cursor_still_sends_snapshot() {
    for (id, snapshot, state) in [
        (1, idle_snapshot(), "state_update:idle"),
        (2, snapshot(true, false, Some("m1")), "state_update:running"),
    ] {
        let (mut client, mut daemons) = v2_client().await;
        client.send(resume(json!(id), None)).await;
        let _daemon = serve_attach(&mut daemons, finished_turn(), vec![snapshot]).await;
        let (before, response) = client.recv_response(json!(id)).await;
        assert_eq!(kinds(&before).last().map(String::as_str), Some(state), "{before:?}");
        assert_eq!(
            kinds(&before)
                .iter()
                .filter(|kind| kind.starts_with("state_update"))
                .count(),
            1,
            "exactly one state snapshot: {before:?}"
        );
        assert!(response["result"].is_object(), "{response}");
        client.close_stdin().await;
    }
}

#[tokio::test]
async fn resume_while_permission_pending_reports_requires_action() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), None)).await;
    let rows = json!([{"role":"user","content":"edit it","message_id":"m1"}]);
    let mut daemon = serve_attach(&mut daemons, rows, vec![snapshot(true, true, Some("m1"))]).await;
    let (before, _) = client.recv_response(json!(1)).await;
    assert_eq!(
        kinds(&before),
        ["session_info_update", "state_update:requires_action"],
        "{before:?}"
    );
    // Answered elsewhere: the turn's next event reports it running first.
    daemon.emit(json!({"type":"text_delta","text":"applied"})).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "answered").await;
    assert_eq!(
        kinds(&lines),
        ["state_update:running", "agent_message_chunk"],
        "{lines:?}"
    );
    client.close_stdin().await;
}

#[tokio::test]
async fn replayed_agent_ids_equal_live_ids() {
    // Live: a turn of m1 with reasoning, text, a tool round and the answer.
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), None)).await;
    let mut daemon = serve_attach(&mut daemons, json!([]), vec![idle_snapshot()]).await;
    client.recv_response(json!(1)).await;
    for event in [
        json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":"r1",
            "start_message_id":"m1","user_content":"read a.rs"}),
        json!({"type":"reasoning_delta","text":"checking the file"}),
        json!({"type":"text_delta","text":"I will read it."}),
        json!({"type":"tool_start","id":"call_1","name":"read"}),
        json!({"type":"tool_input","id":"call_1","delta":"{\"file_path\":\"a.rs\"}"}),
        json!({"type":"tool_exec","id":"call_1","name":"read"}),
        json!({"type":"tool_done","id":"call_1","name":"read","output":"fn main() {}"}),
        json!({"type":"text_delta","text":"done"}),
        json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":"r1",
            "start_message_id":"m1","stop_reason":"end_turn"}),
    ] {
        daemon.emit(event).await;
    }
    let live = daemon_barrier(&mut client, &mut daemon, "live").await;
    let live_ids = agent_ids(&live);
    assert_eq!(live_ids, ["m1:t1", "m1:a1", "m1:a2"], "{live:?}");
    client.close_stdin().await;

    // Replay of the History that turn stored reports the same ids.
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), Some(json!({"type":"start"})))).await;
    let _daemon = serve_attach(&mut daemons, finished_turn(), vec![idle_snapshot()]).await;
    let (replayed, _) = client.recv_response(json!(1)).await;
    assert_eq!(agent_ids(&replayed), live_ids, "{replayed:?}");
    client.close_stdin().await;
}

#[tokio::test]
async fn resume_sends_session_info_before_response() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), Some(json!({"type":"start"})))).await;
    let _daemon = serve_attach(&mut daemons, finished_turn(), vec![idle_snapshot()]).await;
    let (before, _) = client.recv_response(json!(1)).await;
    let kinds = kinds(&before);
    let info = kinds
        .iter()
        .position(|kind| kind == "session_info_update")
        .expect("a session_info_update");
    assert_eq!(info, kinds.len() - 2, "after the replay, before the state: {kinds:?}");
    assert_eq!(
        updates(&before)[info],
        &json!({"sessionUpdate":"session_info_update","title":TITLE,"updatedAt":UPDATED_AT})
    );
    client.close_stdin().await;
}

/// A run attached part-way continues the ids its stored rounds used, with and
/// without replay: the History holds two rounds (`m1:a1`, `m1:a2`), each
/// followed by a tool round, so the next text is `m1:a3`.
#[tokio::test]
async fn mid_run_attach_continues_replayed_ids() {
    for (id, cursor) in [(1, Some(json!({"type":"start"}))), (2, None)] {
        let (mut client, mut daemons) = v2_client().await;
        client.send(resume(json!(id), cursor.clone())).await;
        let rows = json!([
            {"role":"user","content":"read a.rs","message_id":"m1"},
            {"role":"assistant","content":"I will read it.","tool_calls":["read"],"message_id":"m1-a"},
            {"role":"tool","content":"fn main() {}","message_id":"m1-r",
                "tool_data":{"id":"call_1","name":"read","input":{"file_path":"a.rs"}}},
            {"role":"assistant","content":"And b.rs.","tool_calls":["read"],"message_id":"m1-b"},
            {"role":"tool","content":"fn b() {}","message_id":"m1-s",
                "tool_data":{"id":"call_2","name":"read","input":{"file_path":"b.rs"}}}
        ]);
        let mut daemon = serve_attach(&mut daemons, rows, vec![snapshot(true, false, Some("m1"))]).await;
        let (before, _) = client.recv_response(json!(id)).await;
        if cursor.is_some() {
            assert_eq!(agent_ids(&before), ["m1:a1", "m1:a2"], "{before:?}");
        }
        daemon.emit(json!({"type":"reasoning_delta","text":"next"})).await;
        daemon.emit(json!({"type":"text_delta","text":"done"})).await;
        let lines = daemon_barrier(&mut client, &mut daemon, "live").await;
        assert_eq!(agent_ids(&lines), ["m1:t3", "m1:a3"], "{cursor:?}: {lines:?}");
        client.close_stdin().await;
    }
}

/// Events the daemon queued between the attach's `Done` and its snapshot are
/// neither lost nor reported before it: they follow the snapshot, under the
/// run's ids, before the response.
#[tokio::test]
async fn events_before_the_snapshot_follow_it() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), None)).await;
    let rows = json!([{"role":"user","content":"hi","message_id":"m1"}]);
    let after_done = vec![
        json!({"type":"text_delta","text":"early"}),
        snapshot(true, false, Some("m1")),
    ];
    let mut daemon = serve_attach(&mut daemons, rows, after_done).await;
    let (before, _) = client.recv_response(json!(1)).await;
    assert_eq!(
        kinds(&before),
        ["session_info_update", "state_update:running", "agent_message_chunk"],
        "{before:?}"
    );
    assert_eq!(updates(&before)[2]["messageId"], "m1:a1");
    daemon.emit(json!({"type":"text_delta","text":" late"})).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "late").await;
    assert_eq!(agent_ids(&lines), ["m1:a1"], "{lines:?}");
    client.close_stdin().await;
}

/// One turn of `m1` the user interrupted after its first tool round: the
/// interrupt is stored as a user row of its own (`si1`) that started no turn.
fn interrupted_turn(with_answer: bool) -> Value {
    let mut rows = json!([
        {"role":"user","content":"read a.rs","message_id":"m1","turn_start":true},
        {"role":"assistant","content":"I will read it.","tool_calls":["read"],"message_id":"m1-a"},
        {"role":"tool","content":"fn main() {}","message_id":"m1-r",
            "tool_data":{"id":"call_1","name":"read","input":{"file_path":"a.rs"}}},
        {"role":"user","content":"also read b.rs","message_id":"si1"}
    ]);
    if with_answer {
        rows.as_array_mut()
            .unwrap()
            .push(json!({"role":"assistant","content":"Both read.","message_id":"m1-b"}));
    }
    rows
}

/// IS-6: a user message the running turn took in between its rounds starts no
/// turn, so the turn's later messages keep its ids, live and replayed alike.
/// The interrupt is replayed as the user's own message.
#[tokio::test]
async fn replayed_ids_continue_across_a_soft_interrupt() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), None)).await;
    let mut daemon = serve_attach(&mut daemons, json!([]), vec![idle_snapshot()]).await;
    client.recv_response(json!(1)).await;
    for event in [
        json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":"r1",
            "start_message_id":"m1","user_content":"read a.rs"}),
        json!({"type":"text_delta","text":"I will read it."}),
        json!({"type":"tool_start","id":"call_1","name":"read"}),
        json!({"type":"tool_exec","id":"call_1","name":"read"}),
        json!({"type":"tool_done","id":"call_1","name":"read","output":"fn main() {}"}),
        json!({"type":"soft_interrupt_injected","content":"also read b.rs","point":"D"}),
        json!({"type":"text_delta","text":"Both read."}),
        json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":"r1",
            "start_message_id":"m1","stop_reason":"end_turn"}),
    ] {
        daemon.emit(event).await;
    }
    let live = daemon_barrier(&mut client, &mut daemon, "live").await;
    let live_ids = agent_ids(&live);
    assert_eq!(live_ids, ["m1:a1", "m1:a2"], "{live:?}");
    client.close_stdin().await;

    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), Some(json!({"type":"start"})))).await;
    let _daemon = serve_attach(&mut daemons, interrupted_turn(true), vec![idle_snapshot()]).await;
    let (replayed, _) = client.recv_response(json!(1)).await;
    assert_eq!(agent_ids(&replayed), live_ids, "{replayed:?}");
    let users: Vec<&Value> = updates(&replayed)
        .into_iter()
        .filter(|update| update["sessionUpdate"] == "user_message")
        .collect();
    assert_eq!(users.len(), 2, "{replayed:?}");
    assert_eq!(users[1]["messageId"], "si1");
    assert_eq!(users[1]["content"], json!([{"type":"text","text":"also read b.rs"}]));
    client.close_stdin().await;
}

/// A run attached after a soft interrupt continues the ids of the turn that
/// is running, not of the interrupt: its next text is `m1:a2`.
#[tokio::test]
async fn mid_run_attach_after_a_soft_interrupt_continues_the_turn_ids() {
    for (id, cursor) in [(1, Some(json!({"type":"start"}))), (2, None)] {
        let (mut client, mut daemons) = v2_client().await;
        client.send(resume(json!(id), cursor.clone())).await;
        let mut daemon = serve_attach(
            &mut daemons,
            interrupted_turn(false),
            vec![snapshot(true, false, Some("m1"))],
        )
        .await;
        let (before, _) = client.recv_response(json!(id)).await;
        if cursor.is_some() {
            assert_eq!(agent_ids(&before), ["m1:a1"], "{before:?}");
        }
        daemon.emit(json!({"type":"text_delta","text":"Both read."})).await;
        let lines = daemon_barrier(&mut client, &mut daemon, "live").await;
        assert_eq!(agent_ids(&lines), ["m1:a2"], "{cursor:?}: {lines:?}");
        client.close_stdin().await;
    }
}

/// Without any `turn_start` marker (an older daemon, or turns stored before
/// the marker), every user row starts a turn, as before.
#[test]
fn replay_without_turn_start_markers_starts_at_every_user_row() {
    let mut rows = interrupted_turn(true);
    rows[0].as_object_mut().unwrap().remove("turn_start");
    let rows: Vec<crate::protocol::HistoryMessage> = serde_json::from_value(rows).unwrap();
    let ids: Vec<String> = v2::replay::replay_updates(&rows, &[], std::path::Path::new(CWD))
        .iter()
        .filter(|update| update["sessionUpdate"] == "agent_message")
        .map(|update| update["messageId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["m1:a1", "si1:a1"]);
    assert_eq!(
        v2::replay::resume_position(&rows).map(|position| position.start),
        Some("si1".to_string())
    );
}

/// A run that ended between the attach's `Done` and the snapshot is over in
/// the snapshot: its end reports no idle while the snapshot's run runs.
#[tokio::test]
async fn a_foreign_run_end_before_the_snapshot_reports_no_idle() {
    let (mut client, mut daemons) = v2_client().await;
    client.send(resume(json!(1), None)).await;
    let after_done = vec![
        json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":"r0",
            "start_message_id":"m0","stop_reason":"end_turn"}),
        snapshot(true, false, Some("m1")),
    ];
    let mut daemon = serve_attach(&mut daemons, json!([]), after_done).await;
    let (before, _) = client.recv_response(json!(1)).await;
    assert_eq!(
        kinds(&before),
        ["session_info_update", "state_update:running"],
        "{before:?}"
    );
    daemon.emit(json!({"type":"text_delta","text":"still running"})).await;
    let lines = daemon_barrier(&mut client, &mut daemon, "running").await;
    assert_eq!(kinds(&lines), ["agent_message_chunk"], "{lines:?}");
    client.close_stdin().await;
}

/// A session the catalog has no row for gets no empty `session_info_update`.
#[tokio::test]
async fn resume_without_a_catalog_row_sends_no_session_info() {
    let (connector, mut daemons) = scripted_daemon();
    let mut client = start_client_with_catalog(connector, Arc::new(|_: &str| None));
    client.initialize_v2().await;
    client.send(resume(json!(1), None)).await;
    let _daemon = serve_attach(&mut daemons, json!([]), vec![idle_snapshot()]).await;
    let (before, response) = client.recv_response(json!(1)).await;
    assert_eq!(kinds(&before), ["state_update:idle"], "{before:?}");
    assert_matches("ResumeSessionResponse", &response["result"]);
    client.close_stdin().await;
}

/// The agent ids of a replay of `rows`.
fn replayed_agent_ids(rows: &[crate::protocol::HistoryMessage]) -> Vec<String> {
    v2::replay::replay_updates(rows, &[], std::path::Path::new(CWD))
        .iter()
        .filter(|update| update["sessionUpdate"] == "agent_message")
        .map(|update| update["messageId"].as_str().unwrap().to_string())
        .collect()
}

/// A marked turn followed by a turn another path started (`run --resume`
/// stores a user row, an ambient resume a system row): each turn keys its own
/// ids by its own start, `r1:a1` and `s1:a1`.
#[test]
fn a_turn_after_a_marked_turn_keys_its_own_ids() {
    let rows: Vec<crate::protocol::HistoryMessage> = serde_json::from_value(json!([
        {"role":"user","content":"q","message_id":"m1","turn_start":true},
        {"role":"assistant","content":"A1","message_id":"m1-a"},
        {"role":"user","content":"from run --resume","message_id":"r1","turn_start":true},
        {"role":"assistant","content":"A2","message_id":"r1-a"},
        {"role":"system","content":"[auto] reminder due","message_id":"s1","turn_start":true},
        {"role":"assistant","content":"A3","message_id":"s1-a"}
    ]))
    .unwrap();
    assert_eq!(replayed_agent_ids(&rows), ["m1:a1", "r1:a1", "s1:a1"]);
    assert_eq!(
        v2::replay::resume_position(&rows).map(|position| position.start),
        Some("s1".to_string())
    );
}

/// A stored tool result that carries a verdict replays it under `_meta` on
/// the call's completed upsert; a result without one replays as it always did.
#[test]
fn a_stored_verdict_replays_under_meta() {
    let rows: Vec<crate::protocol::HistoryMessage> = serde_json::from_value(json!([
        {"role":"user","content":"read both","message_id":"m1","turn_start":true},
        {"role":"assistant","content":"reading","tool_calls":["read","read"],"message_id":"m1-a"},
        {"role":"tool","content":"boom","message_id":"m1-r1","verdict":"failed",
            "tool_data":{"id":"call_1","name":"read","input":{"file_path":"a.rs"}}},
        {"role":"tool","content":"fn main() {}","message_id":"m1-r2",
            "tool_data":{"id":"call_2","name":"read","input":{"file_path":"b.rs"}}}
    ]))
    .unwrap();
    let updates = v2::replay::replay_updates(&rows, &[], std::path::Path::new(CWD));
    let call = |id: &str| {
        updates
            .iter()
            .find(|update| update["toolCallId"] == id)
            .unwrap_or_else(|| panic!("no update for {id}: {updates:?}"))
    };
    for update in &updates {
        assert_update_matches(update);
    }
    let failed = call("call_1");
    assert_eq!(
        failed["status"], "completed",
        "history keeps no failed status: {failed}"
    );
    assert_eq!(
        failed["_meta"],
        json!({"horon-agent": {"verdict": "failed"}}),
        "{failed}"
    );
    assert!(
        call("call_2").get("_meta").is_none(),
        "no verdict, no `_meta`: {updates:?}"
    );
}

struct AnswerProvider;

#[async_trait::async_trait]
impl crate::provider::Provider for AnswerProvider {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(crate::message::StreamEvent::TextDelta("answer".into())),
            Ok(crate::message::StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ])))
    }

    fn name(&self) -> &str {
        "replay-turn-paths"
    }

    fn fork(&self) -> Arc<dyn crate::provider::Provider> {
        Arc::new(AnswerProvider)
    }
}

/// End to end through the agent: a daemon (streaming) turn, then a turn of
/// `horon-agent run --resume` (`run_once`) and one of a debug or swarm
/// message (`run_once_capture`) in the same session. The History a v2
/// connection reads replays every turn under its own start id.
#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "HORON_HOME is process-global; the guard serializes every test that sets it"
)]
async fn every_turn_path_replays_under_its_own_start_id() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", home.path());
    let provider: Arc<dyn crate::provider::Provider> = Arc::new(AnswerProvider);
    let registry = crate::tool::Registry::new(provider.clone()).await;
    let mut agent = crate::agent::Agent::new(provider, registry);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let ran = async {
        agent.run_once_streaming_mpsc("streamed", vec![], None, tx).await?;
        agent.run_once("resumed by run").await?;
        agent.run_once_capture("debug message").await?;
        anyhow::Ok(agent.get_history(true))
    }
    .await;
    match previous {
        Some(previous) => crate::env::set_var("HORON_HOME", previous),
        None => crate::env::remove_var("HORON_HOME"),
    }
    let rows = ran.unwrap();
    let starts: Vec<String> = rows
        .iter()
        .filter(|row| row.role == "user")
        .map(|row| row.message_id.clone().expect("a v2 History row carries its id"))
        .collect();
    assert_eq!(starts.len(), 3, "{rows:#?}");
    let expected: Vec<String> = starts.iter().map(|start| format!("{start}:a1")).collect();
    assert_eq!(replayed_agent_ids(&rows), expected, "{rows:#?}");
}

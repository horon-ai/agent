//! IS-3, IS-4 and IS-5 over a scripted daemon: the background list on every
//! `state_update`, the announce-before-list and finalize-before-removal order,
//! the deferred removal while idle, `terminal/cancel`, and live terminal
//! output. Every frame written for a background item is checked against the
//! alpha.13 schema.

use super::super::tests::connection_core::*;
use super::super::*;
use super::background::{BackgroundSet, Kind, keep_listed_call_open, stamp};
use super::state::{Control, Resumed, V2Session};
use super::tests::assert_update_matches;
use crate::protocol::BackgroundWorkItem;

const SID: &str = "s1";

async fn v2_client() -> Client {
    let mut client = start_client_with(None);
    client.initialize_v2().await;
    client
}

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

fn running(run_id: &str) -> Value {
    json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":run_id,"start_message_id":"m-1"})
}

fn idle(run_id: &str) -> Value {
    json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":run_id,
        "start_message_id":"m-1","stop_reason":"end_turn"})
}

fn started(call_id: &str, kind: &str) -> Value {
    json!({"type":"background_work","call_id":call_id,"task_id":format!("task-{call_id}"),"kind":kind,
        "state":"started","command":"sleep 30","cwd":"/tmp"})
}

fn ended(call_id: &str, kind: &str, state: &str) -> Value {
    json!({"type":"background_work","call_id":call_id,"task_id":format!("task-{call_id}"),"kind":kind,
        "state":state,"exit_code":0,"output":"ZG9uZQ=="})
}

/// Every line written for daemon events sent before this point.
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

fn kinds(updates: &[Value]) -> Vec<String> {
    updates
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

#[track_caller]
fn assert_all_valid(updates: &[Value]) {
    for update in updates {
        assert_update_matches(update);
    }
}

#[test]
fn the_set_lists_terminals_and_tool_calls_once_in_start_order() {
    let mut set = BackgroundSet::default();
    assert_eq!(set.snapshot(), json!([]));
    assert!(set.insert("t1", Kind::Terminal));
    assert!(set.insert("c1", Kind::ToolCall));
    assert!(!set.insert("t1", Kind::Terminal), "listed once");
    assert_eq!(
        set.snapshot(),
        json!([{"type":"terminal","terminalId":"t1"},{"type":"tool_call","toolCallId":"c1"}])
    );
    assert!(set.remove("t1"));
    assert!(!set.remove("t1"));
    assert_eq!(set.kind("c1"), Some(Kind::ToolCall));
    assert_eq!(set.kind("t1"), None);

    let mut state = json!({"sessionUpdate":"state_update","state":"idle"});
    assert!(stamp(&mut state, &set));
    assert_eq!(state["background"], json!([{"type":"tool_call","toolCallId":"c1"}]));
    assert_update_matches(&state);
    let mut other = json!({"sessionUpdate":"agent_message_chunk"});
    assert!(!stamp(&mut other, &set));
    assert!(other.get("background").is_none());
}

#[test]
fn a_replayed_final_status_of_a_listed_call_stays_open() {
    let mut set = BackgroundSet::default();
    set.insert("c1", Kind::ToolCall);
    set.insert("t1", Kind::Terminal);
    let mut listed = json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed"});
    keep_listed_call_open(&mut listed, &set);
    assert_eq!(listed["status"], "in_progress");
    // A call whose work is its terminal is final on its own; only the
    // terminal stays live.
    let mut terminal_call = json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"});
    keep_listed_call_open(&mut terminal_call, &set);
    assert_eq!(terminal_call["status"], "completed");
    let mut unlisted = json!({"sessionUpdate":"tool_call_update","toolCallId":"c9","status":"completed"});
    keep_listed_call_open(&mut unlisted, &set);
    assert_eq!(unlisted["status"], "completed");
}

/// QA happy: a background terminal is announced before the snapshot that lists
/// it, its live output arrives as chunks, its exit goes out before the
/// snapshot that removes it, and while idle that removal is a repeated idle
/// with no stop reason.
#[tokio::test]
async fn background_terminal_is_announced_streamed_finalized_then_removed() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(started("c9", "terminal")).await;
    daemon
        .emit(json!({"type":"tool_output_delta","id":"c9","data":"YQ=="}))
        .await;
    daemon.emit(ended("c9", "terminal", "completed")).await;
    let early = updates(&daemon_barrier(&mut client, &mut daemon, "ended").await);
    assert_eq!(
        kinds(&early),
        [
            "terminal_update",
            "state_update:idle",
            "terminal_output_chunk",
            "terminal_update"
        ],
        "the removal snapshot waits while idle: {early:?}"
    );
    assert_eq!(early[0]["terminalId"], "c9");
    assert_eq!(early[0]["command"], "sleep 30");
    assert_eq!(early[0]["capabilities"], json!({"cancel":{}}));
    assert_eq!(early[1]["background"], json!([{"type":"terminal","terminalId":"c9"}]));
    assert!(early[1].get("stopReason").is_none(), "a repeated idle is no completion");
    assert_eq!(
        early[2],
        json!({"sessionUpdate":"terminal_output_chunk","terminalId":"c9","data":"YQ=="})
    );
    assert_eq!(early[3]["exitStatus"]["exitCode"], 0);
    assert!(early[3]["capabilities"].is_null(), "{}", early[3]);
    assert_eq!(early[3]["output"], json!({"data":"ZG9uZQ=="}));

    let removal = client.recv().await;
    let removal = &removal["params"]["update"];
    assert_eq!(removal["sessionUpdate"], "state_update", "{removal}");
    assert_eq!(removal["state"], "idle");
    assert_eq!(removal["background"], json!([]));
    assert!(removal.get("stopReason").is_none());
    assert_all_valid(&early);
    assert_update_matches(removal);
    client.close_stdin().await;
}

/// A follow-up turn that starts within the deferral reports `running` with
/// the item already removed, never `idle` with an empty list first.
#[tokio::test]
async fn a_follow_up_turn_carries_the_removal() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(started("c9", "terminal")).await;
    daemon.emit(ended("c9", "terminal", "completed")).await;
    daemon.emit(running("r2")).await;
    daemon.emit(idle("r2")).await;
    let lines = updates(&daemon_barrier(&mut client, &mut daemon, "followed").await);
    assert_eq!(
        kinds(&lines),
        [
            "terminal_update",
            "state_update:idle",
            "terminal_update",
            "state_update:running",
            "state_update:idle"
        ],
        "{lines:?}"
    );
    assert_eq!(lines[3]["background"], json!([]), "running carries the removal");
    assert_eq!(lines[4]["stopReason"], "end_turn");
    assert!(
        tokio::time::timeout(Duration::from_millis(400), client.recv())
            .await
            .is_err(),
        "no late removal snapshot once a state carried it"
    );
    assert_all_valid(&lines);
    client.close_stdin().await;
}

/// A background tool call stays `in_progress` across its `ToolDone` and is
/// neither finalized by its run's end nor listed twice; its own end gives it
/// the final status, before the snapshot that removes it. While a turn runs
/// the list change repeats `running` at once.
#[tokio::test]
async fn background_tool_call_stays_open_until_its_work_ends() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(running("r1")).await;
    daemon
        .emit(json!({"type":"tool_start","id":"g1","name":"agentgrep"}))
        .await;
    daemon
        .emit(json!({"type":"tool_exec","id":"g1","name":"agentgrep"}))
        .await;
    daemon.emit(started("g1", "tool_call")).await;
    daemon
        .emit(json!({"type":"tool_done","id":"g1","name":"agentgrep","output":"moved to background"}))
        .await;
    daemon.emit(idle("r1")).await;
    let lines = updates(&daemon_barrier(&mut client, &mut daemon, "held").await);
    let finals: Vec<&Value> = lines
        .iter()
        .filter(|update| {
            update["toolCallId"] == "g1"
                && update["status"]
                    .as_str()
                    .is_some_and(|status| matches!(status, "completed" | "failed" | "cancelled"))
        })
        .collect();
    assert!(finals.is_empty(), "no final while the work runs: {lines:?}");
    let listed = lines
        .iter()
        .position(|update| {
            update["sessionUpdate"] == "state_update"
                && update["background"].as_array().is_some_and(|list| !list.is_empty())
        })
        .unwrap_or_else(|| panic!("a running snapshot listing g1: {lines:?}"));
    assert_eq!(lines[listed]["state"], "running");
    assert_eq!(
        lines[listed]["background"],
        json!([{"type":"tool_call","toolCallId":"g1"}])
    );
    let idle_update = lines.last().unwrap();
    assert_eq!(idle_update["stopReason"], "end_turn");
    assert_eq!(
        idle_update["background"],
        json!([{"type":"tool_call","toolCallId":"g1"}])
    );

    daemon.emit(ended("g1", "tool_call", "completed")).await;
    let ending = updates(&daemon_barrier(&mut client, &mut daemon, "done").await);
    assert_eq!(kinds(&ending), ["tool_call_update"], "{ending:?}");
    assert_eq!(ending[0]["status"], "completed");
    assert!(ending[0]["capabilities"].is_null());
    let removal = client.recv().await;
    assert_eq!(removal["params"]["update"]["background"], json!([]));
    assert_all_valid(&lines);
    assert_all_valid(&ending);
    client.close_stdin().await;
}

fn terminal_cancel(id: Value, terminal_id: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"terminal/cancel",
        "params":{"sessionId":SID,"terminalId":terminal_id}})
}

/// IS-3: a listed terminal is cancelled through the daemon and answered `{}`
/// before its exit; the exit then arrives as its final `terminal_update`.
#[tokio::test]
async fn listed_terminal_cancel_reaches_the_daemon_and_answers_empty() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(started("c9", "terminal")).await;
    daemon_barrier(&mut client, &mut daemon, "listed").await;
    client.send(terminal_cancel(json!("cancel"), "c9")).await;
    let request = daemon.recv_type("cancel_background_terminal").await;
    assert_eq!(request["call_id"], "c9", "{request}");
    let request_id = request["id"].as_u64().expect("a daemon request id");
    daemon
        .emit(json!({"type":"tool_call_cancel_result","id":request_id,"accepted":true}))
        .await;
    let (_, response) = client.recv_response(json!("cancel")).await;
    assert_eq!(response["result"], json!({}), "{response}");
    daemon.emit(ended("c9", "terminal", "cancelled")).await;
    let lines = updates(&daemon_barrier(&mut client, &mut daemon, "exited").await);
    assert_eq!(kinds(&lines), ["terminal_update"], "{lines:?}");
    assert!(lines[0]["exitStatus"].is_object());
    client.close_stdin().await;
}

/// QA failure: an unlisted terminal, one that exited, and a daemon refusal
/// are all `-32602`; the first two write nothing to the daemon. An unknown
/// session is `-32002`.
#[tokio::test]
async fn unlisted_terminal_cancel_is_invalid_params_without_a_daemon_request() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    daemon.emit(started("c9", "terminal")).await;
    daemon.emit(ended("c9", "terminal", "completed")).await;
    daemon_barrier(&mut client, &mut daemon, "exited").await;
    for (id, terminal_id) in [("exited", "c9"), ("unknown", "nope")] {
        client.send(terminal_cancel(json!(id), terminal_id)).await;
        let (_, response) = client.recv_response(json!(id)).await;
        assert_eq!(
            response["error"]["code"], JSONRPC_INVALID_PARAMS,
            "{terminal_id}: {response}"
        );
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            daemon.recv_type("cancel_background_terminal")
        )
        .await
        .is_err(),
        "no daemon request for a terminal that is not listed"
    );

    daemon.emit(started("c10", "terminal")).await;
    daemon_barrier(&mut client, &mut daemon, "listed").await;
    client.send(terminal_cancel(json!("raced"), "c10")).await;
    let request = daemon.recv_type("cancel_background_terminal").await;
    let request_id = request["id"].as_u64().expect("a daemon request id");
    daemon
        .emit(json!({"type":"tool_call_cancel_result","id":request_id,"accepted":false,"reason":"final"}))
        .await;
    let (_, response) = client.recv_response(json!("raced")).await;
    assert_eq!(response["error"]["code"], JSONRPC_INVALID_PARAMS, "{response}");

    client
        .send(json!({"jsonrpc":"2.0","id":"gone","method":"terminal/cancel",
            "params":{"sessionId":"other","terminalId":"c10"}}))
        .await;
    let (_, response) = client.recv_response(json!("gone")).await;
    assert_eq!(response["error"]["code"], ACP_RESOURCE_NOT_FOUND, "{response}");
    assert_eq!(response["error"]["data"]["sessionId"], "other");
    client.close_stdin().await;
}

/// IS-4 on resume: the daemon's list is announced before the first snapshot,
/// a listed tool call is replayed open, and the snapshot lists both.
#[tokio::test]
async fn resume_lists_running_work_after_announcing_it() {
    let mut client = v2_client().await;
    let (session, _daemon) = attach_v2(&client).await;
    let v2 = session.v2.get().cloned().expect("v2 state");
    let resumed = Resumed {
        updates: vec![json!({"sessionUpdate":"tool_call_update","toolCallId":"g1","status":"completed"})],
        snapshot: None,
        position: None,
        leftovers: Vec::new(),
        background: vec![
            BackgroundWorkItem {
                call_id: "t1".to_string(),
                task_id: "task-t1".to_string(),
                kind: "terminal".to_string(),
                command: Some("npm run dev".to_string()),
                cwd: Some("/tmp".to_string()),
                output: Some("cmVhZHk=".to_string()),
            },
            BackgroundWorkItem {
                call_id: "g1".to_string(),
                task_id: "task-g1".to_string(),
                kind: "tool_call".to_string(),
                command: None,
                cwd: None,
                output: None,
            },
        ],
    };
    let (done, written) = tokio::sync::oneshot::channel();
    assert!(v2.send_control(Control::Resumed {
        resumed: Box::new(resumed),
        done,
    }));
    written.await.expect("the resume is written");
    let mut lines = Vec::new();
    for _ in 0..4 {
        lines.push(client.recv().await["params"]["update"].clone());
    }
    assert_eq!(
        kinds(&lines),
        [
            "tool_call_update",
            "terminal_update",
            "tool_call_update",
            "state_update:idle"
        ],
        "{lines:?}"
    );
    assert_eq!(lines[0]["status"], "in_progress", "a listed call is replayed open");
    assert_eq!(lines[1]["output"], json!({"data":"cmVhZHk="}));
    assert_eq!(
        lines[3]["background"],
        json!([{"type":"terminal","terminalId":"t1"},{"type":"tool_call","toolCallId":"g1"}])
    );
    assert_all_valid(&lines);
    client.close_stdin().await;
}

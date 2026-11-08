//! IS-12 over a scripted daemon: a prompt sent while this connection's
//! previous prompt is still processing is held, not refused, and sent in
//! arrival order once the session is free; a held prompt cancelled or closed
//! before it was sent answers -32800 and never reaches the daemon.

use super::super::tests::connection_core::*;
use super::super::*;
use super::state::V2Session;

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

fn prompt(id: &str, text: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"session/prompt",
        "params":{"sessionId":SID,"prompt":[{"type":"text","text":text}]}})
}

fn running(run_id: &str, start: &str) -> Value {
    json!({"type":"foreground_state","session_id":SID,"running":true,"run_id":run_id,"start_message_id":start})
}

fn idle(run_id: &str, start: &str) -> Value {
    json!({"type":"foreground_state","session_id":SID,"running":false,"run_id":run_id,
        "start_message_id":start,"stop_reason":"end_turn"})
}

/// The daemon accepts a `message` and runs its turn to the end.
async fn run_turn(daemon: &mut FakeDaemon, message: &Value, start: &str, run_id: &str) {
    let id = message["id"].as_u64().expect("a message id");
    daemon
        .emit(json!({"type":"user_message_inserted","id":id,"message_id":start}))
        .await;
    daemon.emit(running(run_id, start)).await;
    daemon.emit(idle(run_id, start)).await;
}

/// No daemon `message` arrives within a short window.
async fn assert_no_message(daemon: &mut FakeDaemon, why: &str) {
    assert!(
        tokio::time::timeout(Duration::from_millis(150), daemon.recv_type("message"))
            .await
            .is_err(),
        "{why}"
    );
}

/// QA happy: prompts A and B sent while the first prompt's turn runs are both
/// held, reach the daemon one at a time in arrival order, and each is answered
/// only at its own insertion.
#[tokio::test]
async fn prompts_during_active_work_are_held_and_sent_in_order() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    client.send(prompt("first", "one")).await;
    let first = daemon.recv_type("message").await;
    let first_id = first["id"].as_u64().expect("an id");
    daemon
        .emit(json!({"type":"user_message_inserted","id":first_id,"message_id":"m-1"}))
        .await;
    daemon.emit(running("r1", "m-1")).await;
    let (_, answer) = client.recv_response(json!("first")).await;
    assert_eq!(answer["result"]["messageId"], "m-1", "{answer}");

    client.send(prompt("a", "two")).await;
    client.send(prompt("b", "three")).await;
    assert_no_message(&mut daemon, "a held prompt is not sent while the turn runs").await;

    daemon.emit(idle("r1", "m-1")).await;
    let a = daemon.recv_type("message").await;
    assert_eq!(a["content"], "two", "A goes first: {a}");
    assert_no_message(&mut daemon, "B waits for A").await;
    run_turn(&mut daemon, &a, "m-2", "r2").await;
    let (_, answer) = client.recv_response(json!("a")).await;
    assert_eq!(
        answer["result"]["messageId"], "m-2",
        "A answers at its insertion: {answer}"
    );

    let b = daemon.recv_type("message").await;
    assert_eq!(b["content"], "three", "then B: {b}");
    run_turn(&mut daemon, &b, "m-3", "r3").await;
    let (_, answer) = client.recv_response(json!("b")).await;
    assert_eq!(answer["result"]["messageId"], "m-3", "{answer}");
    client.close_stdin().await;
}

/// QA failure: a held prompt withdrawn with `$/cancel_request` answers -32800
/// and is never sent, while the prompt behind it still goes; `session/close`
/// answers every prompt still held -32800 before its own answer.
#[tokio::test]
async fn held_prompts_cancelled_or_closed_answer_cancelled_unsent() {
    let mut client = v2_client().await;
    let (_session, mut daemon) = attach_v2(&client).await;
    client.send(prompt("first", "one")).await;
    let first = daemon.recv_type("message").await;
    let first_id = first["id"].as_u64().expect("an id");
    daemon
        .emit(json!({"type":"user_message_inserted","id":first_id,"message_id":"m-1"}))
        .await;
    daemon.emit(running("r1", "m-1")).await;
    client.recv_response(json!("first")).await;

    client.send(prompt("b", "withdrawn")).await;
    client.send(prompt("c", "kept")).await;
    assert_no_message(&mut daemon, "held while the turn runs").await;
    client
        .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":"b"}}))
        .await;
    let (_, answer) = client.recv_response(json!("b")).await;
    assert_eq!(answer["error"]["code"], JSONRPC_REQUEST_CANCELLED, "{answer}");

    daemon.emit(idle("r1", "m-1")).await;
    let next = daemon.recv_type("message").await;
    assert_eq!(next["content"], "kept", "the withdrawn prompt is skipped: {next}");
    let next_id = next["id"].as_u64().expect("an id");
    daemon
        .emit(json!({"type":"user_message_inserted","id":next_id,"message_id":"m-2"}))
        .await;
    daemon.emit(running("r2", "m-2")).await;
    client.recv_response(json!("c")).await;

    client.send(prompt("d", "never")).await;
    assert_no_message(&mut daemon, "held behind the running turn").await;
    client
        .send(json!({"jsonrpc":"2.0","id":"close","method":"session/close","params":{"sessionId":SID}}))
        .await;
    let (_, answer) = client.recv_response(json!("d")).await;
    assert_eq!(answer["error"]["code"], JSONRPC_REQUEST_CANCELLED, "{answer}");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), daemon.recv_type("message"))
            .await
            .is_err(),
        "a held prompt is never sent after the close"
    );
    client.close_stdin().await;
}

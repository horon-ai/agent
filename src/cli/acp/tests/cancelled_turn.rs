//! Regression tests for #263: a call or a message that belongs to a turn
//! which already ended must not be left waiting for a turn that never comes.
//!
//! Both tests drive the real v1 adapter over the in-process daemon script and
//! fail without the cancelled-turn barrier in `permission.rs` and
//! `client_router.rs`.

use super::connection_core::*;
use super::*;

/// #263: a permission request of a cancelled turn that lands after the
/// cancellation pass is never asked. Before the barrier the pass had taken
/// its snapshot, so the question reached the client and the turn ended
/// without waiting for it: the client kept a prompt nobody would resolve.
#[tokio::test]
async fn permission_request_after_cancel_is_never_asked() {
    let mut client = start_client();
    initialize_with(&mut client, ALL_CLIENT_CAPABILITIES).await;
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
    let mut kinds: Vec<String> = Vec::new();
    let mut decision = Value::Null;
    for _ in 0..2 {
        let event = daemon.recv().await;
        kinds.push(event["type"].as_str().unwrap_or_default().to_string());
        if event["type"] == "permission_decision" {
            decision = event;
        }
    }
    assert!(kinds.contains(&"cancel".to_string()), "{kinds:?}");
    assert_eq!(decision["request_id"], "p1");
    assert_eq!(decision["decision"], "deny");

    // The turn is already cancelled; its next gate reaches the client now.
    daemon.emit(permission_requested("p2", "bash")).await;
    expect_decision(&mut daemon, "p2", "deny").await;
    assert!(
        client.barrier("late-permission").await.is_empty(),
        "no question is asked for a cancelled turn"
    );

    daemon
        .emit(json!({"type":"tool_permission_resolved","request_id":"p1","approved":false}))
        .await;
    daemon.emit(json!({"type":"done","id":message["id"]})).await;
    let (_, response) = client.recv_response(json!(1)).await;
    assert_eq!(response["result"]["stopReason"], "cancelled", "{response}");
    assert_eq!(session.permissions.open_len(), 0);
    client.close_stdin().await;
}

/// #263: a call that arrives after the cancellation pass still gets no
/// question. Before the cancelled-turn barrier the withdrawal had already
/// taken its snapshot, so this call reached the client, the turn ended, and
/// nothing ever cancelled it: the client was left holding a question the
/// turn no longer waits for.
#[tokio::test]
async fn cancel_refuses_calls_that_arrive_after_the_pass() {
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
    let asked = client.recv().await;
    assert_eq!(asked["method"], "fs/read_text_file");

    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}))
        .await;
    let withdrawn = client.recv().await;
    assert_eq!(withdrawn["method"], "$/cancel_request", "{withdrawn}");
    assert_eq!(withdrawn["params"]["requestId"], asked["id"]);
    assert_eq!(daemon.recv().await["type"], "cancel");

    // The turn is cancelled; a request of that turn lands afterwards.
    daemon
        .emit(client_request(
            "cr-late",
            None,
            "fs/read_text_file",
            json!({"path":"/w/late"}),
        ))
        .await;
    let mut answered_calls = HashMap::new();
    for _ in 0..2 {
        let response = daemon.recv_type("client_response").await;
        daemon.emit(json!({"type":"ack","id":response["id"]})).await;
        answered_calls.insert(response["request_id"].as_str().unwrap().to_string(), response);
    }
    for request_id in ["cr-read", "cr-late"] {
        assert_eq!(
            answered_calls[request_id]["error"]["code"], JSONRPC_REQUEST_CANCELLED,
            "a call of a cancelled turn is answered, never asked: {request_id}: {:?}",
            answered_calls[request_id]
        );
    }
    assert!(
        client.barrier("late-call").await.is_empty(),
        "the client is never asked for a cancelled turn's call"
    );

    daemon.emit(json!({"type":"done","id":message["id"]})).await;
    let (_, done) = client.recv_response(json!(1)).await;
    assert_eq!(done["result"]["stopReason"], "cancelled", "{done}");
    assert_eq!(session.client_calls.open_len(), 0);
    client.close_stdin().await;
}

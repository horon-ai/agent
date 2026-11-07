//! Daemon-side tool-call cancellation for an ACP v2 editor: the
//! `cancel_tool_call` and `cancel_turn` requests (plan row 5).
//!
//! These drive the real request loop through a socket, so the reply
//! (`tool_call_cancel_result`, or the Ack of a targeted cancel), the
//! `tool_cancelled` side channel, the tool result the model sees, and the
//! turn's idle state are all observed in the order a client receives them.

use super::acp_v2_events::{Conn, Harness, is_idle, position};
use super::*;
use serde_json::json;

const DEADLINE: Duration = Duration::from_secs(30);

/// `bash`'s built-in tool spends its whole run inside the tool call, so a
/// cancelled call is one whose work really was aborted.
const SLOW_COMMAND: &str = "sleep 30";

fn tool_call_events(events: &[ServerEvent]) -> Vec<&ServerEvent> {
    events
        .iter()
        .filter(|event| matches!(event, ServerEvent::ToolDone { .. } | ServerEvent::ToolCancelled { .. }))
        .collect()
}

/// Wait until the daemon registered `call_id` as cancellable, so the cancel
/// names a call that is genuinely in flight instead of racing the tool spawn.
async fn await_cancellable(session_id: &str, call_id: &str) -> bool {
    for _ in 0..600 {
        if crate::tool_cancel_registry::running_call_ids(session_id)
            .iter()
            .any(|id| id == call_id)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Queue a `cancel_tool_call` and read until its reply arrives, returning every
/// event seen along the way in order.
async fn cancel_tool_call(conn: &mut Conn, id: u64, call_id: &str) -> Vec<ServerEvent> {
    conn.send(json!({"type": "cancel_tool_call", "id": id, "call_id": call_id}))
        .await;
    conn.until(|event| matches!(event, ServerEvent::ToolCallCancelResult { id: reply, .. } if *reply == id))
        .await
}

fn cancel_reply(events: &[ServerEvent]) -> (bool, Option<String>) {
    events
        .iter()
        .find_map(|event| match event {
            ServerEvent::ToolCallCancelResult { accepted, reason, .. } => Some((*accepted, reason.clone())),
            _ => None,
        })
        .expect("the cancel is answered")
}

/// QA happy: a `bash sleep 30` call and the text that follows it, from one
/// turn. Cancelling the call stops it, the turn continues, and the turn ends on
/// its own with `end_turn`.
#[tokio::test]
async fn cancelled_tool_call_stops_that_call_and_the_turn_continues() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    h.provider
        .push_tool_call("call-sleep", "bash", json!({"command": SLOW_COMMAND, "intent": "qa"}));
    editor
        .send(json!({"type": "message", "id": 10, "content": "sleep for a while"}))
        .await;

    // The call is announced, then registered once the turn actually awaits it.
    editor
        .until(|event| matches!(event, ServerEvent::ToolStart { id, .. } if id == "call-sleep"))
        .await;
    assert!(
        await_cancellable(&session_id, "call-sleep").await,
        "the running call must be registered under its own id"
    );

    let mut events = cancel_tool_call(&mut editor, 11, "call-sleep").await;
    assert_eq!(
        cancel_reply(&events),
        (true, None),
        "a running call is accepted: {events:#?}"
    );

    // Within two seconds the call reports its cancellation and its result.
    let cancelled = tokio::time::timeout(
        Duration::from_secs(2),
        editor.until(|event| matches!(event, ServerEvent::ToolCancelled { id } if id == "call-sleep")),
    )
    .await
    .expect("the cancellation reaches the editor within two seconds");
    events.extend(cancelled);

    let rest = editor.until(is_idle).await;
    events.extend(rest);

    let cancelled_at = position(
        &events,
        "tool_cancelled",
        |event| matches!(event, ServerEvent::ToolCancelled { id } if id == "call-sleep"),
    );
    let done_at = position(
        &events,
        "the tool result",
        |event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-sleep"),
    );
    assert!(
        cancelled_at < done_at,
        "tool_cancelled precedes the result the model sees: {events:#?}"
    );
    let ServerEvent::ToolDone { error, output, .. } = &events[done_at] else {
        unreachable!()
    };
    assert_eq!(
        error.as_deref(),
        Some("[Cancelled by user]"),
        "the model reads a cancellation, not a failure: {events:#?}"
    );
    assert_eq!(output, "[Cancelled by user]");

    // The same turn asked the provider again after the cancelled call, and then
    // finished on its own.
    let follow_up = position(
        &events,
        "the follow-up answer",
        |event| matches!(event, ServerEvent::TextDelta { text } if text == "tool round finished"),
    );
    assert!(
        done_at < follow_up,
        "the next model request follows the cancelled call: {events:#?}"
    );

    let idle = events.last().expect("the idle state closes the turn");
    let ServerEvent::ForegroundState {
        running: false,
        stop_reason,
        ..
    } = idle
    else {
        panic!("the last event is the idle state: {idle:?}");
    };
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(
        tool_call_events(&events)
            .iter()
            .filter(|event| matches!(event, ServerEvent::ToolCancelled { .. }))
            .count(),
        1,
        "a cancelled call reports tool_cancelled exactly once"
    );

    h.close(vec![editor]).await;
}

/// QA failure, part 1: an unknown call id is refused and the turn keeps going.
#[tokio::test]
async fn unknown_call_id_is_refused_and_the_turn_keeps_running() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    h.provider
        .push_tool_call("call-real", "bash", json!({"command": "sleep 1", "intent": "qa"}));
    editor
        .send(json!({"type": "message", "id": 10, "content": "run"}))
        .await;
    editor
        .until(|event| matches!(event, ServerEvent::ToolStart { id, .. } if id == "call-real"))
        .await;
    assert!(await_cancellable(&session_id, "call-real").await);

    let events = cancel_tool_call(&mut editor, 11, "call-nobody-started").await;
    assert_eq!(cancel_reply(&events), (false, Some("unknown".to_string())));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ServerEvent::ToolCancelled { .. })),
        "a refused cancel reports nothing cancelled: {events:#?}"
    );

    // The running call is untouched: it finishes with its own result and the
    // turn ends normally.
    let rest = editor.until(is_idle).await;
    let done_at = position(
        &rest,
        "the real call's result",
        |event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-real"),
    );
    let ServerEvent::ToolDone { error, .. } = &rest[done_at] else {
        unreachable!()
    };
    assert_eq!(*error, None, "the refused cancel must not stop the call: {rest:#?}");
    let ServerEvent::ForegroundState { stop_reason, .. } = rest.last().expect("idle") else {
        panic!("the last event is the idle state: {rest:#?}");
    };
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));

    h.close(vec![editor]).await;
}

/// QA failure, part 2: a `cancel_turn` naming another run stops nothing.
#[tokio::test]
async fn cancel_turn_with_another_run_id_leaves_the_turn_running() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    h.provider
        .push_tool_call("call-live", "bash", json!({"command": "sleep 1", "intent": "qa"}));
    editor
        .send(json!({"type": "message", "id": 10, "content": "run"}))
        .await;
    // The running state carries the run id the caller would report.
    let running = editor
        .until(|event| {
            matches!(
                event,
                ServerEvent::ForegroundState {
                    running: true,
                    snapshot: false,
                    ..
                }
            )
        })
        .await;
    let (_, _, run_id) = super::acp_v2_events::running_state(&running);
    assert!(!run_id.is_empty(), "a running turn reports a run id: {running:#?}");
    assert!(await_cancellable(&session_id, "call-live").await);

    editor
        .send(json!({"type": "cancel_turn", "id": 11, "run_id": "some-other-run"}))
        .await;
    let ack = editor
        .until(|event| matches!(event, ServerEvent::Ack { id } if *id == 11))
        .await;
    assert!(
        !ack.iter().any(|event| matches!(event, ServerEvent::Interrupted)),
        "a cancel for another run interrupts nothing: {ack:#?}"
    );

    let rest = editor.until(is_idle).await;
    let done_at = position(
        &rest,
        "the running call's result",
        |event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-live"),
    );
    let ServerEvent::ToolDone { error, .. } = &rest[done_at] else {
        unreachable!()
    };
    assert_eq!(*error, None, "the turn kept running: {rest:#?}");
    let ServerEvent::ForegroundState { stop_reason, .. } = rest.last().expect("idle") else {
        panic!("the last event is the idle state: {rest:#?}");
    };
    assert_eq!(
        stop_reason.as_deref(),
        Some("end_turn"),
        "a mistargeted cancel must not mark the turn cancelled: {rest:#?}"
    );

    h.close(vec![editor]).await;
}

/// The other half of the same rule: naming the run that is actually running
/// stops it, exactly as a plain `cancel` does.
#[tokio::test]
async fn cancel_turn_with_the_reported_run_id_stops_the_turn() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    h.provider
        .push_tool_call("call-long", "bash", json!({"command": SLOW_COMMAND, "intent": "qa"}));
    editor
        .send(json!({"type": "message", "id": 10, "content": "sleep"}))
        .await;
    let running = editor
        .until(|event| {
            matches!(
                event,
                ServerEvent::ForegroundState {
                    running: true,
                    snapshot: false,
                    ..
                }
            )
        })
        .await;
    let (_, _, run_id) = super::acp_v2_events::running_state(&running);

    editor
        .send(json!({"type": "cancel_turn", "id": 11, "run_id": run_id}))
        .await;
    let events = tokio::time::timeout(DEADLINE, editor.until(is_idle))
        .await
        .expect("a targeted cancel stops the turn");
    let idle = events.last().expect("the idle state closes the turn");
    let ServerEvent::ForegroundState { stop_reason, .. } = idle else {
        panic!("the last event is the idle state: {idle:?}");
    };
    assert_eq!(
        stop_reason.as_deref(),
        Some("cancelled"),
        "the named run was the running one, so the turn is cancelled: {events:#?}"
    );

    h.close(vec![editor]).await;
}

/// The two background-work requests exist on the wire in this wave and answer
/// `unsupported`; the row that owns background lifetimes replaces them.
#[tokio::test]
async fn background_work_requests_answer_unsupported() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    for (id, request) in [
        (20u64, json!({"type": "list_background_work", "id": 20})),
        (
            21,
            json!({"type": "cancel_background_terminal", "id": 21, "call_id": "call-x"}),
        ),
    ] {
        editor.send(request).await;
        let reply = editor
            .until(|event| matches!(event, ServerEvent::Error { id: reply, .. } if *reply == id))
            .await;
        let answer = reply
            .iter()
            .find(|event| matches!(event, ServerEvent::Error { id: reply, .. } if *reply == id))
            .expect("the request is answered");
        let ServerEvent::Error { message, .. } = answer else {
            unreachable!()
        };
        assert_eq!(message, "unsupported");
    }
    h.close(vec![editor]).await;
}

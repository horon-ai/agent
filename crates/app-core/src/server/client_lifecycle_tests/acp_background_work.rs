//! Background work as an ACP v2 editor sees it (plan row 6): the lifetime
//! events, the live list, and the terminal cancel.
//!
//! Every test drives the real request loop over a socket, so the order the
//! events arrive in is the order the editor's mapper would receive them:
//! `background_work{started}` before the call's `tool_done`, live output
//! deltas while the command runs, and the outcome after them.

use super::acp_v2_events::{Conn, Harness, is_idle};
use super::*;
use crate::protocol::{BackgroundWorkEvent, BackgroundWorkItem};
use serde_json::json;

const DEADLINE: Duration = Duration::from_secs(30);

/// A command that prints, waits, and prints again: one delta can be reported
/// before the end, and the end carries the exit code.
const SPLIT_COMMAND: &str = "printf a; sleep 1; printf b";

fn background_events(events: &[ServerEvent]) -> Vec<&BackgroundWorkEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::BackgroundWork(work) => Some(work),
            _ => None,
        })
        .collect()
}

fn deltas_for<'a>(events: &'a [ServerEvent], call_id: &str) -> Vec<&'a ServerEvent> {
    events
        .iter()
        .filter(|event| matches!(event, ServerEvent::ToolOutputDelta { id, .. } if id == call_id))
        .collect()
}

fn position_of(events: &[ServerEvent], matches: impl Fn(&ServerEvent) -> bool, what: &str) -> usize {
    events
        .iter()
        .position(matches)
        .unwrap_or_else(|| panic!("{what} is missing from {events:#?}"))
}

/// Ask for the live background work and return the answer.
async fn list_background_work(conn: &mut Conn, id: u64) -> Vec<BackgroundWorkItem> {
    conn.send(json!({"type": "list_background_work", "id": id})).await;
    let events = conn
        .until(|event| matches!(event, ServerEvent::BackgroundWorkList { id: reply, .. } if *reply == id))
        .await;
    events
        .iter()
        .find_map(|event| match event {
            ServerEvent::BackgroundWorkList { items, .. } => Some(items.clone()),
            _ => None,
        })
        .expect("the list is answered")
}

/// Ask to cancel one terminal and return its answer.
async fn cancel_terminal(conn: &mut Conn, id: u64, call_id: &str) -> Vec<ServerEvent> {
    conn.send(json!({"type": "cancel_background_terminal", "id": id, "call_id": call_id}))
        .await;
    conn.until(|event| matches!(event, ServerEvent::ToolCallCancelResult { id: reply, .. } if *reply == id))
        .await
}

/// Find the cancelled transition in events already read, otherwise wait for it.
///
/// The daemon signals the process group before it answers the request, so the
/// work's own end can arrive *before* the cancel reply the caller just read.
fn find_cancelled(events: &[ServerEvent]) -> Option<BackgroundWorkEvent> {
    events.iter().find_map(|event| match event {
        ServerEvent::BackgroundWork(work) if work.state == "cancelled" => Some(work.clone()),
        _ => None,
    })
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

/// QA happy: a background shell reports its start before the call's own result,
/// forwards what it prints, and reports completing with exit code 0.
#[tokio::test]
async fn background_bash_reports_start_output_and_completion() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    h.provider.push_tool_call(
        "call-bg",
        "bash",
        json!({"command": SPLIT_COMMAND, "run_in_background": true, "notify": false, "intent": "qa"}),
    );
    editor
        .send(json!({"type": "message", "id": 10, "content": "run it in the background"}))
        .await;

    let mut events = editor.until(is_idle).await;
    // The completion is published after the turn's idle state, so it needs its
    // own wait rather than the turn's deadline.
    if !background_events(&events).iter().any(|work| work.state == "completed") {
        let completed = tokio::time::timeout(
            DEADLINE,
            editor.until(|event| matches!(event, ServerEvent::BackgroundWork(work) if work.state == "completed")),
        )
        .await
        .expect("the completion is reported");
        events.extend(completed);
    }

    let started = position_of(
        &events,
        |event| matches!(event, ServerEvent::BackgroundWork(work) if work.state == "started"),
        "the started event",
    );
    let done = position_of(
        &events,
        |event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-bg"),
        "the call's tool_done",
    );
    let completed = position_of(
        &events,
        |event| matches!(event, ServerEvent::BackgroundWork(work) if work.state == "completed"),
        "the completed event",
    );
    assert!(
        started < done,
        "the editor hears the work started before the call finished: {events:#?}"
    );
    let ServerEvent::BackgroundWork(work) = &events[started] else {
        unreachable!()
    };
    assert_eq!(work.call_id, "call-bg");
    assert_eq!(work.kind, "terminal");
    assert_eq!(work.command.as_deref(), Some(SPLIT_COMMAND));
    assert!(work.cwd.is_some(), "the terminal names the directory it ran in");

    let deltas = deltas_for(&events, "call-bg");
    assert!(
        !deltas.is_empty(),
        "the output the command printed while it ran is forwarded: {events:#?}"
    );
    for delta in &deltas {
        let ServerEvent::ToolOutputDelta { data, .. } = delta else {
            unreachable!()
        };
        assert!(!data.is_empty(), "a forwarded delta carries bytes: {events:#?}");
    }

    assert!(
        done < completed,
        "the outcome follows the call and its output: {events:#?}"
    );
    let ServerEvent::BackgroundWork(work) = &events[completed] else {
        unreachable!()
    };
    assert_eq!(work.state, "completed");
    assert_eq!(work.exit_code, Some(0));
    assert!(work.output.is_some(), "the outcome carries the output snapshot");

    h.close(vec![editor]).await;
}

/// A background shell outlives the tool call that started it.
///
/// The call returns as soon as the process is spawned, so a command killed at
/// that moment would be a product bug: the editor would see work that stops the
/// instant it is announced. This holds the task for a full second past the
/// call's result and asserts it is still live and that nothing has reported an
/// end for it.
#[tokio::test]
async fn background_shell_outlives_its_tool_call() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let session_id = h.only_session().await;
    h.provider.push_tool_call(
        "call-alive",
        "bash",
        json!({"command": "printf started; sleep 30", "run_in_background": true, "notify": false, "intent": "qa"}),
    );
    editor
        .send(json!({"type": "message", "id": 10, "content": "start a long job"}))
        .await;
    let events = editor
        .until(|event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-alive"))
        .await;

    tokio::time::sleep(Duration::from_millis(1200)).await;

    let manager = crate::background::global();
    let live = manager.live_tasks_for_session(&session_id).await;
    assert!(
        live.iter()
            .any(|task| task.tool_call_id.as_deref() == Some("call-alive")),
        "a background shell is still running a second after its call returned: {live:#?}"
    );
    assert!(
        background_events(&events).iter().all(|work| work.state == "started"),
        "nothing reported the work as ended: {events:#?}"
    );
    if let Some(task) = manager.task_for_tool_call(&session_id, "call-alive").await {
        assert!(
            task.exit_code.is_none(),
            "the process did not exit on its own: {task:#?}"
        );
        manager
            .cancel_with_grace(&task.task_id, Duration::from_millis(200))
            .await
            .expect("the test cleans up its own task");
    }
    h.close(vec![editor]).await;
}

/// QA happy: while a terminal runs, the list names the call that started it.
#[tokio::test]
async fn list_background_work_names_a_running_terminal() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    let command = "printf started; sleep 30";
    h.provider.push_tool_call(
        "call-list",
        "bash",
        json!({"command": command, "run_in_background": true, "notify": false, "intent": "qa"}),
    );
    editor
        .send(json!({"type": "message", "id": 10, "content": "start a long job"}))
        .await;
    editor
        .until(|event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-list"))
        .await;

    let items = list_background_work(&mut editor, 11).await;
    let item = items
        .iter()
        .find(|item| item.call_id == "call-list")
        .unwrap_or_else(|| panic!("the running terminal is listed: {items:#?}"));
    assert_eq!(item.kind, "terminal");
    assert_eq!(item.command.as_deref(), Some(command));

    // Leave no long-running process behind for the next test.
    let mut reply = cancel_terminal(&mut editor, 12, "call-list").await;
    assert_eq!(cancel_reply(&reply), (true, None), "the listed terminal is cancellable");
    if find_cancelled(&reply).is_none() {
        reply.extend(
            tokio::time::timeout(
                Duration::from_secs(10),
                editor.until(|event| matches!(event, ServerEvent::BackgroundWork(work) if work.state == "cancelled")),
            )
            .await
            .expect("the listed terminal reports its cancellation"),
        );
    }
    assert_eq!(
        find_cancelled(&reply).map(|work| work.call_id),
        Some("call-list".to_string())
    );
    h.close(vec![editor]).await;
}

/// QA happy: cancelling a terminal is accepted, and its completion says so.
#[tokio::test]
async fn cancel_background_terminal_stops_a_running_terminal() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    h.provider.push_tool_call(
        "call-cancel",
        "bash",
        json!({"command": "sleep 30", "run_in_background": true, "notify": false, "intent": "qa"}),
    );
    editor
        .send(json!({"type": "message", "id": 10, "content": "start a long job"}))
        .await;
    editor
        .until(|event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-cancel"))
        .await;

    let mut reply = cancel_terminal(&mut editor, 11, "call-cancel").await;
    assert_eq!(
        cancel_reply(&reply),
        (true, None),
        "a running terminal accepts its cancel: {reply:#?}"
    );

    if find_cancelled(&reply).is_none() {
        reply.extend(
            tokio::time::timeout(
                Duration::from_secs(10),
                editor.until(|event| matches!(event, ServerEvent::BackgroundWork(work) if work.state == "cancelled")),
            )
            .await
            .expect("the cancellation is confirmed by the work's own event"),
        );
    }
    let outcome = find_cancelled(&reply).expect("the cancelled event is present");
    assert_eq!(outcome.call_id, "call-cancel");
    assert_eq!(outcome.kind, "terminal");
    h.close(vec![editor]).await;
}

/// A foreground shell forwards what it prints while it still runs, so a long
/// command is not silent until it exits.
#[tokio::test]
async fn foreground_bash_forwards_output_before_the_call_ends() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    h.provider
        .push_tool_call("call-live", "bash", json!({"command": SPLIT_COMMAND, "intent": "qa"}));
    editor
        .send(json!({"type": "message", "id": 10, "content": "run it in the foreground"}))
        .await;

    let events = editor.until(is_idle).await;
    let first_delta = position_of(
        &events,
        |event| matches!(event, ServerEvent::ToolOutputDelta { id, .. } if id == "call-live"),
        "the forwarded output",
    );
    let done = position_of(
        &events,
        |event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-live"),
        "the call's tool_done",
    );
    assert!(
        first_delta < done,
        "the output arrives while the command still runs: {events:#?}"
    );
    let ServerEvent::ToolOutputDelta { data, .. } = &events[first_delta] else {
        unreachable!()
    };
    assert!(!data.is_empty(), "a forwarded delta carries bytes: {events:#?}");

    // A foreground call is not background work, so it never enters the list.
    let items = list_background_work(&mut editor, 11).await;
    assert!(
        items.iter().all(|item| item.call_id != "call-live"),
        "a foreground call is not listed as background work: {items:#?}"
    );
    h.close(vec![editor]).await;
}

/// QA failure: a session no v2 connection owns gets neither event, and the
/// command's own result is unchanged.
#[tokio::test]
async fn no_v2_connection_gets_no_background_work_events() {
    let h = Harness::new();
    let mut desktop = h.connect_other(None, false).await.0;
    h.provider.push_tool_call(
        "call-plain",
        "bash",
        json!({"command": SPLIT_COMMAND, "run_in_background": true, "notify": false, "intent": "qa"}),
    );
    desktop
        .send(json!({"type": "message", "id": 10, "content": "run it in the background"}))
        .await;
    // A plain connection is never sent the v2-only foreground state, so the
    // turn's own reply is what marks its end.
    let events = desktop
        .until(|event| matches!(event, ServerEvent::Done { id } if *id == 10))
        .await;
    // Give the tail loop, which must not exist, the time it would have needed.
    tokio::time::sleep(Duration::from_millis(700)).await;

    assert!(
        background_events(&events).is_empty(),
        "no background_work event leaves without a v2 owner: {events:#?}"
    );
    assert!(
        deltas_for(&events, "call-plain").is_empty(),
        "no output delta leaves without a v2 owner: {events:#?}"
    );
    let done = events
        .iter()
        .find_map(|event| match event {
            ServerEvent::ToolDone { id, output, .. } if id == "call-plain" => Some(output.clone()),
            _ => None,
        })
        .expect("the call still reports");
    assert!(
        done.contains("Command started in background."),
        "the command's own result is unchanged: {done}"
    );

    // The task is real; only its reporting is absent. Stop it so the test
    // leaves no process behind.
    crate::background::global()
        .cancel_with_grace(
            &crate::background::global()
                .task_for_tool_call(&h.only_session().await, "call-plain")
                .await
                .expect("the task exists")
                .task_id,
            Duration::from_millis(400),
        )
        .await
        .expect("the task is cancelled");
    h.close(vec![desktop]).await;
}

/// QA failure: a terminal that already ended answers `final` instead of being
/// cancelled again.
#[tokio::test]
async fn cancel_of_a_finished_terminal_reports_final() {
    let h = Harness::new();
    let mut editor = h.connect_v2(None, false).await;
    h.provider.push_tool_call(
        "call-done",
        "bash",
        json!({"command": "printf done", "run_in_background": true, "notify": false, "intent": "qa"}),
    );
    editor
        .send(json!({"type": "message", "id": 10, "content": "run it in the background"}))
        .await;
    editor
        .until(|event| matches!(event, ServerEvent::BackgroundWork(work) if work.state == "completed"))
        .await;

    let reply = cancel_terminal(&mut editor, 11, "call-done").await;
    assert_eq!(
        cancel_reply(&reply),
        (false, Some("final".to_string())),
        "a finished terminal is not cancellable: {reply:#?}"
    );
    h.close(vec![editor]).await;
}

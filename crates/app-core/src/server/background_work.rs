//! Background work as an ACP v2 editor sees it: the lifetime events, the
//! terminal cancel, and the live list.
//!
//! Three things live here because they answer one question — "what background
//! work does this session still have?" — and because a v2 editor is the only
//! client that asks it:
//!
//! - [`announce_started`] reports a task the moment a tool call creates it, so
//!   the editor shows the work before the call's own `tool_done` arrives.
//! - [`watch_finalized`] waits on the background manager's terminal broadcast
//!   and reports the outcome (state, exit code, last 64 KiB of output).
//! - the two request handlers answer `cancel_background_terminal` and
//!   `list_background_work`.
//!
//! Subscribing before the task is created is deliberate: the broadcast only
//! delivers events published after a subscription, and a command that exits at
//! once must still report its end. Every function here sends through
//! [`crate::tool::acp_session::send_v2`], so a session no v2 connection owns
//! produces no event at all.

use super::*;
use crate::background::{FinalizedTask, TaskStatusFile};
use crate::bus::BackgroundTaskStatus;
use crate::protocol::{BackgroundWorkEvent, BackgroundWorkItem, ServerEvent};
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::sync::broadcast::error::RecvError;

/// How often a running shell's output file is tailed for a v2 editor.
const BACKGROUND_TAIL_INTERVAL: Duration = Duration::from_millis(500);

/// The error text every cancellation path writes into a task's status file.
const CANCELLED_ERROR: &str = "Cancelled by user";

/// How much of a finished task's output the final event carries.
const OUTPUT_TAIL_BYTES: u64 = 64 * 1024;

/// Grace period between TERM and KILL when a terminal is cancelled, matching
/// `bg cancel`'s default.
const CANCEL_GRACE: Duration = Duration::from_millis(400);

/// Whether a tool's background work is a display-only terminal.
fn kind_for(tool_name: &str) -> &'static str {
    if tool_name == "bash" { "terminal" } else { "tool_call" }
}

/// Report a task that just came into existence for `call_id`, start and end.
///
/// The v2 ownership check happens here so a caller cannot announce a start
/// without arming the end (or the other way round), and so a session no v2
/// connection owns arms no listener at all.
#[expect(
    clippy::too_many_arguments,
    reason = "One call names the work, the values the daemon ran with, and its two listeners"
)]
pub(crate) fn report_started(
    session_id: &str,
    call_id: &str,
    task_id: &str,
    tool_name: &str,
    command: Option<&str>,
    cwd: Option<&Path>,
    finalized: broadcast::Receiver<FinalizedTask>,
    tail: Option<crate::tool::BackgroundTail>,
) -> bool {
    if !crate::tool::acp_session::owns_v2(session_id) {
        return false;
    }
    announce_started(session_id, call_id, task_id, tool_name, command, cwd);
    watch_finalized(finalized, session_id, call_id, task_id, tail);
    true
}

/// Report that this call's background work started.
///
/// `command` and `cwd` are the values the daemon actually ran with, so the
/// editor can name the terminal without guessing.
pub(crate) fn announce_started(
    session_id: &str,
    call_id: &str,
    task_id: &str,
    tool_name: &str,
    command: Option<&str>,
    cwd: Option<&Path>,
) {
    let event = ServerEvent::BackgroundWork(BackgroundWorkEvent {
        call_id: call_id.to_string(),
        task_id: task_id.to_string(),
        kind: kind_for(tool_name).to_string(),
        state: "started".to_string(),
        command: command.map(str::to_string),
        cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
        exit_code: None,
        signal: None,
        output: None,
    });
    if !crate::tool::acp_session::send_v2(session_id, event) {
        // The session lost its v2 connection between the ownership check and
        // here. Nothing to report to, and the tool result still says the work
        // is running.
        crate::logging::info(&format!(
            "Background work {} started with no v2 connection listening (session={})",
            task_id, session_id
        ));
    }
}

/// Report the end of this call's background work once the manager publishes it.
///
/// `receiver` must have been created before the task was spawned, or a task
/// that finishes immediately would end before this listener existed. `tail`
/// forwards a shell's growing output file while the task runs; the last drain
/// happens before the final event, so a client receives every delta of a
/// finished command ahead of its outcome.
pub(crate) fn watch_finalized(
    mut receiver: broadcast::Receiver<FinalizedTask>,
    session_id: &str,
    call_id: &str,
    task_id: &str,
    mut tail: Option<crate::tool::BackgroundTail>,
) {
    let session_id = session_id.to_string();
    let call_id = call_id.to_string();
    let task_id = task_id.to_string();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(BACKGROUND_TAIL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                received = receiver.recv() => match received {
                    Ok(task) if task.task_id == task_id => {
                        if let Some(tail) = tail.as_mut() {
                            tail.drain().await;
                        }
                        report_final(&session_id, &call_id, &task_id, &task).await;
                        return;
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(skipped)) => {
                        // The broadcast is bounded; a burst of unrelated tasks
                        // can push this one out. The status file is the source
                        // of truth, so a terminal status still gets reported.
                        crate::logging::warn(&format!(
                            "Background work listener for {} lagged by {} event(s); reading the status file",
                            task_id, skipped
                        ));
                        if let Some(status) = crate::background::global().status(&task_id).await
                            && status.status != BackgroundTaskStatus::Running
                        {
                            if let Some(tail) = tail.as_mut() {
                                tail.drain().await;
                            }
                            report_final_status(&session_id, &call_id, &status, false).await;
                            return;
                        }
                    }
                    Err(RecvError::Closed) => {
                        crate::logging::warn(&format!(
                            "Background work listener for {} closed before the task ended",
                            task_id
                        ));
                        return;
                    }
                },
                _ = ticker.tick() => {
                    if let Some(tail) = tail.as_mut() {
                        tail.drain().await;
                    }
                    // A task that left the live map has already been finalized.
                    // Reading the status file here covers a broadcast this
                    // listener missed, and keeps the listener from waiting on a
                    // task nothing will ever announce again.
                    if !crate::background::global().is_live_task(&task_id)
                        && let Some(status) = crate::background::global().status(&task_id).await
                        && status.status != BackgroundTaskStatus::Running
                    {
                        let cancelled = status.error.as_deref() == Some(CANCELLED_ERROR);
                        if let Some(tail) = tail.as_mut() {
                            tail.drain().await;
                        }
                        report_final_status(&session_id, &call_id, &status, cancelled).await;
                        return;
                    }
                }
            }
        }
    });
}

/// Send a task's terminal transition.
async fn report_final(session_id: &str, call_id: &str, task_id: &str, task: &FinalizedTask) {
    let status = crate::background::global().status(task_id).await;
    let (tool_name, command, cwd) = match status {
        Some(status) => (status.tool_name, status.command, status.cwd),
        None => (String::new(), None, None),
    };
    let state = if task.cancelled {
        "cancelled"
    } else if task.status == BackgroundTaskStatus::Completed {
        "completed"
    } else {
        "failed"
    };
    send_final(
        session_id,
        call_id,
        task_id,
        &tool_name,
        state,
        command,
        cwd,
        task.exit_code,
        task.signal.clone(),
    )
    .await;
}

/// Send a transition reconstructed from the status file alone.
async fn report_final_status(session_id: &str, call_id: &str, status: &TaskStatusFile, cancelled: bool) {
    let state = if cancelled || status.status == BackgroundTaskStatus::Superseded {
        "cancelled"
    } else if status.status == BackgroundTaskStatus::Completed {
        "completed"
    } else {
        "failed"
    };
    send_final(
        session_id,
        call_id,
        &status.task_id,
        &status.tool_name,
        state,
        status.command.clone(),
        status.cwd.clone(),
        status.exit_code,
        None,
    )
    .await;
}

#[expect(
    clippy::too_many_arguments,
    reason = "The event is a flat record of what the daemon observed"
)]
async fn send_final(
    session_id: &str,
    call_id: &str,
    task_id: &str,
    tool_name: &str,
    state: &str,
    command: Option<String>,
    cwd: Option<String>,
    exit_code: Option<i32>,
    signal: Option<String>,
) {
    let output = read_output_tail(&crate::background::global().output_path_for(task_id)).await;
    let event = ServerEvent::BackgroundWork(BackgroundWorkEvent {
        call_id: call_id.to_string(),
        task_id: task_id.to_string(),
        kind: kind_for(tool_name).to_string(),
        state: state.to_string(),
        command,
        cwd,
        exit_code,
        signal,
        output,
    });
    if !crate::tool::acp_session::send_v2(session_id, event) {
        crate::logging::info(&format!(
            "Background work {} ended with no v2 connection listening (session={})",
            task_id, session_id
        ));
    }
}

/// The last [`OUTPUT_TAIL_BYTES`] of a task's output file, as base64.
async fn read_output_tail(path: &Path) -> Option<String> {
    use tokio::io::AsyncSeekExt;
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return None,
    };
    let length = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(_) => return None,
    };
    if length == 0 {
        return None;
    }
    if file
        .seek(std::io::SeekFrom::Start(length.saturating_sub(OUTPUT_TAIL_BYTES)))
        .await
        .is_err()
    {
        return None;
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).await.is_err() || bytes.is_empty() {
        return None;
    }
    Some(crate::acp_render::base64_encode(&bytes))
}

/// Answer `cancel_background_terminal`.
///
/// `accepted: true` means a running bash task was signalled; its own
/// `background_work` completion (state `cancelled`) is the confirmation. A task
/// the manager no longer runs answers `final`, an unknown call id answers
/// `unknown`, and a non-shell background tool answers `not_cancellable`.
pub(super) async fn handle_cancel_background_terminal(
    id: u64,
    call_id: &str,
    session_id: &str,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let manager = crate::background::global();
    let answer = match manager.task_for_tool_call(session_id, call_id).await {
        None => refused(id, "unknown"),
        Some(task) if task.status != BackgroundTaskStatus::Running => refused(id, "final"),
        Some(task) if task.tool_name != "bash" => refused(id, "not_cancellable"),
        Some(task) => {
            // Marked before the signal: stopping the process group lets the
            // task future publish its own "failed" record during the grace
            // window, and the mark is what makes that earlier record report the
            // cancellation that caused it.
            manager.mark_task_cancelled(&task.task_id);
            match manager.cancel_with_grace(&task.task_id, CANCEL_GRACE).await {
                Ok(true) => ServerEvent::ToolCallCancelResult {
                    id,
                    accepted: true,
                    reason: None,
                },
                Ok(false) => refused(id, "final"),
                Err(error) => {
                    crate::logging::warn(&format!(
                        "SERVER_BACKGROUND_TERMINAL_CANCEL_FAILED id={} session={} call_id={} error={error:#}",
                        id, session_id, call_id
                    ));
                    refused(id, "final")
                }
            }
        }
    };
    let accepted = matches!(answer, ServerEvent::ToolCallCancelResult { accepted: true, .. });
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TERMINAL_CANCEL_REQUEST id={} session={} call_id={} accepted={}",
        id, session_id, call_id, accepted
    ));
    reply(client_event_tx, answer, "cancel_background_terminal");
}

fn refused(id: u64, reason: &'static str) -> ServerEvent {
    ServerEvent::ToolCallCancelResult {
        id,
        accepted: false,
        reason: Some(reason.to_string()),
    }
}

/// Answer `list_background_work` with the tasks this session is running.
///
/// Only tasks a tool call started are listed; housekeeping the daemon runs on
/// its own has no call id and no editor-side home.
pub(super) async fn handle_list_background_work(
    id: u64,
    session_id: &str,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let manager = crate::background::global();
    let mut items = Vec::new();
    for task in manager.live_tasks_for_session(session_id).await {
        let Some(call_id) = task.tool_call_id.clone() else {
            continue;
        };
        let output = read_output_tail(&manager.output_path_for(&task.task_id)).await;
        items.push(BackgroundWorkItem {
            call_id,
            task_id: task.task_id,
            kind: kind_for(&task.tool_name).to_string(),
            command: task.command,
            cwd: task.cwd,
            output,
        });
    }
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_WORK_LIST id={} session={} items={}",
        id,
        session_id,
        items.len()
    ));
    reply(
        client_event_tx,
        ServerEvent::BackgroundWorkList { id, items },
        "list_background_work",
    );
}

/// Queue one reply, reporting a connection that closed before it was read.
fn reply(client_event_tx: &mpsc::UnboundedSender<ServerEvent>, event: ServerEvent, request: &'static str) {
    if client_event_tx.send(event).is_err() {
        crate::logging::warn(&format!("SERVER_REQUEST_REPLY_DROPPED request={request}"));
    }
}

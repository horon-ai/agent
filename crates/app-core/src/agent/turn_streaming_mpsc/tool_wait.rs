//! How one tool call's wait ends: completion, a move to the background, a
//! server reload, or the user cancelling that one call.
//!
//! The sequential path of the streaming turn awaits a single tool here, and a
//! concurrent group observes the same four outcomes per member through
//! [`super::tool_round`]. Keeping the wait in its own module keeps the turn
//! file flat while all four outcomes stay one decision: a call that ends by
//! cancellation must record exactly one tool result, exactly like a call that
//! failed, so the next provider request is always a valid transcript.
//!
//! The result text of a cancelled call is [`USER_CANCELLED`]. It is the text
//! the model reads, which is why it says who stopped the work rather than
//! reporting a tool failure: the turn continues and the model can react.

use super::*;

/// Result text of a call the user cancelled. The model sees this as an error
/// result, so the wording says the work was stopped on purpose.
pub(crate) const USER_CANCELLED: &str = "[Cancelled by user]";

/// How long a reload hands a bash call to finish on its own before the tool
/// task is aborted. Bash may have already produced output the user needs, so a
/// reload that lands in its last moment should still keep the result.
const RELOAD_HANDOFF_DRAIN: Duration = Duration::from_millis(750);

/// The four ways one tool wait can end.
pub(super) enum ToolWait {
    /// The tool produced a result (including a panic, which becomes an error).
    Finished(Result<crate::tool::ToolOutput>),
    /// The user asked for this call to be moved to the background (Alt+B).
    Background,
    /// A server reload interrupted the call.
    Reloaded,
    /// The user cancelled this call; the tool task was aborted.
    Cancelled,
}

/// Await one tool call, honoring the background, reload and per-call cancel
/// signals.
///
/// `handle` is aborted only on the paths that abandon it: cancellation, and a
/// reload of a call that cannot be handed off. The tool task owns no other
/// reference, so an aborted task stops with it.
pub(super) async fn wait_for_tool(
    handle: &mut tokio::task::JoinHandle<Result<crate::tool::ToolOutput>>,
    background: &InterruptSignal,
    shutdown: &InterruptSignal,
    cancel: &InterruptSignal,
    allow_reload_handoff: bool,
) -> ToolWait {
    tokio::select! {
        biased;
        res = &mut *handle => ToolWait::Finished(unwrap_tool_join(res)),
        _ = cancel.notified() => {
            handle.abort();
            ToolWait::Cancelled
        }
        _ = async {
            tokio::select! {
                _ = background.notified() => {}
                _ = shutdown.notified() => {}
            }
        } => {
            if shutdown.is_set() {
                if allow_reload_handoff {
                    match tokio::time::timeout(RELOAD_HANDOFF_DRAIN, &mut *handle).await {
                        Ok(res) => ToolWait::Finished(unwrap_tool_join(res)),
                        Err(_) => ToolWait::Reloaded,
                    }
                } else {
                    ToolWait::Reloaded
                }
            } else {
                ToolWait::Background
            }
        }
    }
}

/// A panicking tool is that call's error, never a lost turn.
fn unwrap_tool_join(
    res: std::result::Result<Result<crate::tool::ToolOutput>, tokio::task::JoinError>,
) -> Result<crate::tool::ToolOutput> {
    match res {
        Ok(result) => result,
        Err(error) => Err(anyhow::anyhow!("Tool task panicked: {}", error)),
    }
}

/// Tell a v2 editor that this call was cancelled, then write the result the
/// model reads.
///
/// The event and the result are sent from one place so a caller cannot report
/// `tool_cancelled` without the `tool_done` that follows it, and the turn
/// continues with exactly one tool result for this call.
impl Agent {
    pub(super) fn apply_tool_cancel(
        &mut self,
        tc: &ToolCall,
        elapsed: Duration,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
    ) {
        crate::tool::acp_session::send_v2(&self.session.id, ServerEvent::ToolCancelled { id: tc.id.clone() });
        logging::info(&format!(
            "Tool '{}' cancelled by user after {:.1}s",
            tc.name,
            elapsed.as_secs_f64()
        ));
        if event_tx
            .send(ServerEvent::ToolDone {
                id: tc.id.clone(),
                name: tc.name.clone(),
                output: USER_CANCELLED.to_string(),
                error: Some(USER_CANCELLED.to_string()),
            })
            .is_err()
        {
            logging::warn(&format!(
                "the cancelled tool result for '{}' was dropped: the turn's event channel is closed",
                tc.name
            ));
        }
        self.add_message_with_duration(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: tc.id.clone(),
                content: USER_CANCELLED.to_string(),
                is_error: Some(true),
            }],
            Some(elapsed.as_millis() as u64),
        );
    }
}

/// Send the file changes a tool call made, immediately before its `ToolDone`.
///
/// The diffs come from the tool's own metadata and are read before the history
/// caps and the round budget rewrite the result. They leave only through the
/// ACP connection's sender, so a client that renders no diffs never sees them.
pub(super) fn send_tool_diff(session_id: &str, call_id: &str, output: &crate::tool::ToolOutput) {
    let Some(diffs) = output.metadata.as_ref().and_then(|metadata| metadata.get("file_diffs")) else {
        return;
    };
    let Ok(diffs) = serde_json::from_value::<Vec<crate::protocol::FileDiffWire>>(diffs.clone()) else {
        // A metadata value this daemon did not write is not a diff report. The
        // tool's own output still says what the file holds, so a client that
        // renders diffs loses nothing it would have been given anyway.
        return;
    };
    if diffs.is_empty() {
        return;
    }
    crate::tool::acp_session::send_for_version(session_id, |version| {
        let mut diffs = diffs;
        // Deletes, creates and moves are v2 fields: a v1 owner gets the diff
        // bytes it always got.
        if version != Some(2) {
            for diff in &mut diffs {
                diff.deleted = false;
                diff.created = false;
                diff.old_path = None;
            }
        }
        ServerEvent::ToolDiff {
            id: call_id.to_string(),
            diffs,
        }
    });
}

/// Send how a shell call exited, immediately before its `ToolDone`, to an ACP
/// connection that negotiated v2 (its terminal view). A call handed to a
/// background task reports `backgrounded` without an exit.
pub(super) fn send_tool_exit(session_id: &str, call_id: &str, tool_name: &str, output: &crate::tool::ToolOutput) {
    if tool_name != "bash" {
        return;
    }
    let Some(metadata) = output.metadata.as_ref() else {
        return;
    };
    let event = if let Some(exit) = metadata.get("exit_status") {
        ServerEvent::ToolExit {
            id: call_id.to_string(),
            exit_code: exit_code(exit),
            signal: exit
                .get("signal")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            backgrounded: false,
        }
    } else if metadata.get("background").and_then(serde_json::Value::as_bool) == Some(true) {
        ServerEvent::ToolExit {
            id: call_id.to_string(),
            exit_code: None,
            signal: None,
            backgrounded: true,
        }
    } else {
        return;
    };
    crate::tool::acp_session::send_v2(session_id, event);
}

/// The exit code a shell reported, when it fits the wire field.
fn exit_code(exit: &serde_json::Value) -> Option<i32> {
    let code = exit.get("exit_code")?.as_i64()?;
    let Ok(code) = i32::try_from(code) else {
        // Not a shell exit code this daemon writes: report the exit without one
        // rather than substitute a value the process never returned.
        return None;
    };
    Some(code)
}

pub(super) fn reload_interrupted_tool_result(tc: &ToolCall, elapsed_secs: f64) -> (String, bool) {
    let action = tc.input.get("action").and_then(|value| value.as_str()).unwrap_or("");
    let is_wait_like =
        (tc.name == "bg" && action == "wait") || (tc.name == "swarm" && matches!(action, "await_members" | "run_plan"));

    if is_wait_like {
        let input = serde_json::to_string(&tc.input).unwrap_or_else(|_| "{}".to_string());
        return (
            format!(
                "[Tool '{}' wait interrupted by server reload after {:.1}s. The underlying operation may still be running. Resume the wait by rerunning the same tool call with input: {}]",
                tc.name, elapsed_secs, input
            ),
            false,
        );
    }

    (
        format!(
            "[Tool '{}' interrupted by server reload after {:.1}s]",
            tc.name, elapsed_secs
        ),
        true,
    )
}

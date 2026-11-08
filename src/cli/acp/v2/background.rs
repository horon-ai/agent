//! IS-3, IS-4 and IS-5: the background work of one v2 session.
//!
//! Background work is session-owned work that keeps running after the tool
//! call that started it returned control to the model: a backgrounded shell,
//! listed as its display-only terminal, or another tool the daemon moved to
//! the background, listed as its tool call. The daemon reports each item's
//! start and end (`ServerEvent::BackgroundWork`); the session stream keeps the
//! live items in a [`BackgroundSet`] and stamps that snapshot onto every
//! `state_update` it writes, so a client always knows whether the session is
//! quiet and what closing it would stop.
//!
//! The rules the stream follows, from prompt-lifecycle "Background work":
//! an item is announced (`terminal_update` or `tool_call_update`) before the
//! first snapshot that lists it; its final update (`exitStatus` or a final
//! status) goes out before the snapshot that removes it; every list change
//! repeats the current state with the new list and no new stop reason; and a
//! removal while idle waits [`REMOVAL_DEFER`] so a follow-up turn reports
//! `running` with the removal instead of `idle` with an empty list followed by
//! `running`.
//!
//! A listed terminal advertises `capabilities.cancel`, and `terminal/cancel`
//! for it reaches the daemon as `cancel_background_terminal`. Anything else is
//! refused with `-32602` before the daemon sees it.

use super::super::*;
use super::cancel::{CANCEL_REPLY_BOUND, wait_for_cancel_result};
use super::shapes::{ToolCallPatch, cancel_capabilities, tool_call_update};
use crate::protocol::{BackgroundWorkEvent, BackgroundWorkItem, Request};
use std::path::Path;

/// How long a removal while the session is idle waits for a follow-up turn.
pub(crate) const REMOVAL_DEFER: Duration = Duration::from_millis(250);

/// How long `session/resume` waits for the daemon's background work list. A
/// daemon too old to know the request never answers it, and the resume then
/// reports an empty list rather than hanging.
const LIST_REPLY_BOUND: Duration = Duration::from_secs(3);

/// How one background item is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A process, listed as its terminal (whose id is its tool call's).
    Terminal,
    /// Work without a process of its own, listed as its tool call.
    ToolCall,
}

impl Kind {
    fn from_wire(kind: &str) -> Self {
        if kind == "terminal" {
            Self::Terminal
        } else {
            Self::ToolCall
        }
    }
}

/// The background work a v2 session currently reports, in start order.
#[derive(Debug, Default)]
pub(crate) struct BackgroundSet {
    items: Vec<(String, Kind)>,
}

impl BackgroundSet {
    /// The `background` list of a `state_update`.
    pub(crate) fn snapshot(&self) -> Value {
        Value::Array(
            self.items
                .iter()
                .map(|(id, kind)| match kind {
                    Kind::Terminal => json!({ "type": "terminal", "terminalId": id }),
                    Kind::ToolCall => json!({ "type": "tool_call", "toolCallId": id }),
                })
                .collect(),
        )
    }

    /// Lists `id`; false when it is already listed.
    pub(crate) fn insert(&mut self, id: &str, kind: Kind) -> bool {
        if self.items.iter().any(|(listed, _)| listed == id) {
            return false;
        }
        self.items.push((id.to_string(), kind));
        true
    }

    /// Stops listing `id`; false when it was not listed.
    pub(crate) fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|(listed, _)| listed != id);
        self.items.len() != before
    }

    pub(crate) fn kind(&self, id: &str) -> Option<Kind> {
        self.items
            .iter()
            .find(|(listed, _)| listed == id)
            .map(|(_, kind)| *kind)
    }
}

/// Stamps the session's list onto a `state_update`; other updates pass
/// unchanged. True when `update` is a `state_update`.
pub(crate) fn stamp(update: &mut Value, set: &BackgroundSet) -> bool {
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("state_update") {
        return false;
    }
    update["background"] = set.snapshot();
    true
}

/// A replayed `tool_call_update` for a call the session still lists as running
/// in the background goes out `in_progress`: its stored result is the moment
/// it went to the background, and a final status would be followed by a live
/// one, which a final status never is.
pub(crate) fn keep_listed_call_open(update: &mut Value, set: &BackgroundSet) {
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("tool_call_update") {
        return;
    }
    let listed = update
        .get("toolCallId")
        .and_then(Value::as_str)
        .is_some_and(|id| set.kind(id) == Some(Kind::ToolCall));
    if listed && update.get("status").is_some() {
        update["status"] = json!("in_progress");
    }
}

/// The update that announces a newly listed item.
fn announce(id: &str, kind: Kind, command: Option<&str>, cwd: Option<&str>, output: Option<&str>) -> Value {
    match kind {
        Kind::Terminal => {
            let mut update = json!({
                "sessionUpdate": "terminal_update",
                "terminalId": id,
                "capabilities": cancel_capabilities(),
            });
            if let Some(command) = command {
                update["command"] = json!(command);
            }
            if let Some(cwd) = cwd.filter(|cwd| Path::new(cwd).is_absolute()) {
                update["cwd"] = json!(cwd);
            }
            if let Some(output) = output {
                update["output"] = json!({ "data": output });
            }
            update
        }
        // The call stopped advertising `tool_call/cancel` when it left the
        // foreground; the daemon has no per-call cancel for background work.
        Kind::ToolCall => tool_call_update(
            id,
            &ToolCallPatch {
                status: Some("in_progress".to_string()),
                capabilities: Some(Value::Null),
                ..ToolCallPatch::default()
            },
        ),
    }
}

/// The final update of an item: the terminal's last output and exit status,
/// or the tool call's final status. Either way it stops advertising controls.
fn finalized(work: &BackgroundWorkEvent) -> Value {
    match Kind::from_wire(&work.kind) {
        Kind::Terminal => {
            let mut update = json!({
                "sessionUpdate": "terminal_update",
                "terminalId": work.call_id,
                "exitStatus": { "exitCode": work.exit_code, "signal": work.signal },
                "capabilities": Value::Null,
            });
            if let Some(output) = &work.output {
                update["output"] = json!({ "data": output });
            }
            update
        }
        Kind::ToolCall => {
            let status = match work.state.as_str() {
                "failed" => "failed",
                "cancelled" => "cancelled",
                _ => "completed",
            };
            tool_call_update(&work.call_id, &ToolCallPatch::final_status(status))
        }
    }
}

/// IS-5: live output of a terminal, an independently base64-encoded chunk.
pub(crate) fn output_chunk(terminal_id: &str, data: &str) -> Value {
    json!({
        "sessionUpdate": "terminal_output_chunk",
        "terminalId": terminal_id,
        "data": data,
    })
}

/// What one `BackgroundWork` transition does to the session.
pub(crate) struct Transition {
    /// The announcement or final update, written before any snapshot.
    pub(crate) updates: Vec<Value>,
    /// The list changed, so the current state is repeated with it.
    pub(crate) list_changed: bool,
    /// A tool call newly listed: the run's mapper keeps it open.
    pub(crate) hold_open: Option<String>,
}

impl BackgroundSet {
    /// Apply one daemon transition.
    pub(crate) fn apply(&mut self, work: &BackgroundWorkEvent) -> Transition {
        let kind = Kind::from_wire(&work.kind);
        if work.state == "started" {
            let listed = self.insert(&work.call_id, kind);
            return Transition {
                updates: if listed {
                    vec![announce(
                        &work.call_id,
                        kind,
                        work.command.as_deref(),
                        work.cwd.as_deref(),
                        None,
                    )]
                } else {
                    Vec::new()
                },
                list_changed: listed,
                hold_open: (listed && kind == Kind::ToolCall).then(|| work.call_id.clone()),
            };
        }
        let removed = self.remove(&work.call_id);
        Transition {
            updates: vec![finalized(work)],
            list_changed: removed,
            hold_open: None,
        }
    }

    /// IS-4 on resume: list what the daemon reports running and return the
    /// announcements, which go out before the first snapshot.
    pub(crate) fn hydrate(&mut self, items: &[BackgroundWorkItem]) -> Vec<Value> {
        items
            .iter()
            .filter_map(|item| {
                let kind = Kind::from_wire(&item.kind);
                self.insert(&item.call_id, kind).then(|| {
                    announce(
                        &item.call_id,
                        kind,
                        item.command.as_deref(),
                        item.cwd.as_deref(),
                        item.output.as_deref(),
                    )
                })
            })
            .collect()
    }
}

impl AcpRuntime {
    /// The daemon's list of the session's running background work. A refusal,
    /// a disconnect or no answer within [`LIST_REPLY_BOUND`] is an empty list.
    pub(in super::super) async fn background_work(&self, session: &DaemonSession) -> Vec<BackgroundWorkItem> {
        let request_id = session.next_id();
        let request = Request::ListBackgroundWork { id: request_id };
        let listed = async {
            let mut reply = session.request(request_id, &request).await?;
            loop {
                match reply.next().await? {
                    ServerEvent::BackgroundWorkList { id, items } if id == request_id => return Ok(items),
                    ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
                    _ => {}
                }
            }
        };
        match tokio::time::timeout(LIST_REPLY_BOUND, listed).await {
            Ok(Ok(items)) => items,
            Ok(Err(err)) => {
                crate::logging::info(&format!(
                    "ACP: no background work list for {}: {err:#}",
                    session.session_id
                ));
                Vec::new()
            }
            Err(_) => {
                crate::logging::info(&format!(
                    "ACP: background work list for {} not answered within {}s",
                    session.session_id,
                    LIST_REPLY_BOUND.as_secs()
                ));
                Vec::new()
            }
        }
    }

    /// `terminal/cancel`: stop one listed background terminal.
    ///
    /// Answers `{}` once the daemon started stopping it; the `terminal_update`
    /// with its `exitStatus` follows on the session stream. A terminal that is
    /// unknown, already exited, or not listed is `-32602` with nothing written
    /// to the daemon. The tool call that references the terminal and the
    /// foreground state are left as they are.
    pub(in super::super) async fn handle_terminal_cancel(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        let Some(terminal_id) = message
            .params
            .get("terminalId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        else {
            return self
                .write_error_value(id, JSONRPC_INVALID_PARAMS, "Missing required terminalId".to_string())
                .await;
        };
        let session = self.sessions.lock().await.get(&session_id).cloned();
        let Some((session, v2)) = session.and_then(|session| {
            let v2 = session.v2.get().cloned()?;
            Some((session, v2))
        }) else {
            return self.write_rpc_error(id, RpcError::session_not_found(&session_id)).await;
        };
        if v2.background().kind(&terminal_id) != Some(Kind::Terminal) {
            return self.refuse_terminal_cancel(id, &terminal_id).await;
        }
        let request_id = session.next_id();
        let request = Request::CancelBackgroundTerminal {
            id: request_id,
            call_id: terminal_id.clone(),
        };
        let verdict = match session.request(request_id, &request).await {
            Ok(mut reply) => {
                match tokio::time::timeout(CANCEL_REPLY_BOUND, wait_for_cancel_result(&mut reply, request_id)).await {
                    Ok(verdict) => verdict,
                    Err(_) => Err(anyhow::anyhow!("no reply within {}s", CANCEL_REPLY_BOUND.as_secs())),
                }
            }
            Err(err) => Err(err),
        };
        match verdict {
            Ok(true) => self.write_result(id, json!({})).await,
            // The process exited between its listing and this request.
            Ok(false) => self.refuse_terminal_cancel(id, &terminal_id).await,
            Err(err) => {
                self.write_rpc_error(
                    id,
                    RpcError::new(
                        JSONRPC_INTERNAL_ERROR,
                        format!("Cancel of terminal {terminal_id} failed: {err:#}"),
                    ),
                )
                .await
            }
        }
    }

    async fn refuse_terminal_cancel(&self, id: Value, terminal_id: &str) -> Result<()> {
        self.write_error_value(
            id,
            JSONRPC_INVALID_PARAMS,
            format!("Terminal {terminal_id} is not cancellable"),
        )
        .await
    }
}

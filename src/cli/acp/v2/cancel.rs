//! IS-2: `tool_call/cancel` on a v2 connection, and the set of tool calls this
//! connection currently advertises as cancellable.
//!
//! A v2 client may send `tool_call/cancel` only for a call whose last
//! `tool_call_update` advertised `capabilities.cancel`. The adapter keeps one
//! [`CancelRegistry`] per daemon connection, fed by the pump from the daemon
//! events the mapper turns into those updates (see
//! [`super::super::pump::routes`]): the call enters it at its `tool_exec`, whose
//! mapped `tool_call_update` advertises the control, and leaves it when the
//! call reaches a final status or its run ends.
//!
//! A cancel for a call the registry does not hold is refused with `-32602`
//! without writing anything to the daemon. That is exactly the set the protocol
//! names as invalid: an unknown call, one already final, and one that never
//! advertised the control.
//!
//! The accepted cancel answers `{}` as soon as the daemon confirms it started
//! the cancellation; it never waits for the final `tool_call_update`, which the
//! session stream writes like any other turn event. The call leaves the
//! registry at acceptance, so a second cancel for the same call is `-32602`
//! even before that update is written.

use super::super::*;
use crate::protocol::Request;
use std::collections::HashSet;
use std::sync::Mutex as StdMutex;

/// How long a `tool_call/cancel` waits for the daemon's verdict. The daemon
/// answers from its own request loop without waiting on the turn, so this bound
/// only keeps a hung daemon from holding the client's request open.
pub(super) const CANCEL_REPLY_BOUND: Duration = Duration::from_secs(10);

/// The calls one daemon connection advertises `capabilities.cancel` for.
///
/// The pump writes it while it routes events and the read loop reads it, so a
/// plain mutex is enough; neither side holds it across an await.
#[derive(Debug, Default)]
pub(crate) struct CancelRegistry {
    state: StdMutex<Advertised>,
}

#[derive(Debug, Default)]
struct Advertised {
    calls: HashSet<String>,
    /// The run the advertised calls belong to: the last one reported running.
    run_id: Option<String>,
}

impl CancelRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Advertised> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The call entered its execution phase: the mapper advertised
    /// `capabilities.cancel` for it.
    pub(crate) fn advertise(&self, tool_call_id: &str) {
        self.lock().calls.insert(tool_call_id.to_string());
    }

    /// The call reached a final status, or its cancel was accepted.
    pub(crate) fn withdraw(&self, tool_call_id: &str) {
        self.lock().calls.remove(tool_call_id);
    }

    /// A run reported running: the calls it advertises belong to it.
    pub(crate) fn run_started(&self, run_id: &str) {
        self.lock().run_id = Some(run_id.to_string());
    }

    /// A run reported its end. Only the run whose calls are advertised clears
    /// them: the late end of a run another run already replaced must not take
    /// back what the running one advertised.
    pub(crate) fn run_ended(&self, run_id: &str) {
        let mut state = self.lock();
        if state.run_id.as_deref().is_none_or(|current| current == run_id) {
            state.run_id = None;
            state.calls.clear();
        }
    }

    pub(crate) fn is_advertised(&self, tool_call_id: &str) -> bool {
        self.lock().calls.contains(tool_call_id)
    }
}

impl AcpRuntime {
    /// `tool_call/cancel`: cancel one running tool call of a session this
    /// connection owns.
    ///
    /// Answers `{}` once the daemon accepted the cancellation. A malformed
    /// request, an unknown session, a call that is not advertised as
    /// cancellable, and a daemon refusal all answer `-32602`; only an
    /// advertised call writes anything to the daemon.
    pub(in super::super) async fn handle_tool_call_cancel(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        let Some(tool_call_id) = message
            .params
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        else {
            return self
                .write_error_value(id, JSONRPC_INVALID_PARAMS, "Missing required toolCallId".to_string())
                .await;
        };
        let session = self.sessions.lock().await.get(&session_id).cloned();
        let Some(session) = session.filter(|session| session.v2.get().is_some()) else {
            return self.write_rpc_error(id, RpcError::session_not_found(&session_id)).await;
        };
        let cancelable = session.cancel_registry();
        if !cancelable.is_advertised(&tool_call_id) {
            return self.refuse_cancel(id, &tool_call_id).await;
        }
        let request_id = session.next_id();
        let request = Request::CancelToolCall {
            id: request_id,
            call_id: tool_call_id.clone(),
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
        // The call is no longer cancellable either way: it is stopped, or the
        // daemon reported it finished before this request reached it.
        cancelable.withdraw(&tool_call_id);
        match verdict {
            Ok(true) => self.write_result(id, json!({})).await,
            // The daemon already finished the call between its advertisement
            // and this request, which is the protocol's "already final".
            Ok(false) => self.refuse_cancel(id, &tool_call_id).await,
            Err(err) => {
                self.write_rpc_error(
                    id,
                    RpcError::new(
                        JSONRPC_INTERNAL_ERROR,
                        format!("Cancel of tool call {tool_call_id} failed: {err:#}"),
                    ),
                )
                .await
            }
        }
    }

    /// The protocol's answer for a call that is unknown, already final, or not
    /// cancellable.
    async fn refuse_cancel(&self, id: Value, tool_call_id: &str) -> Result<()> {
        self.write_error_value(
            id,
            JSONRPC_INVALID_PARAMS,
            format!("Tool call {tool_call_id} is not cancellable"),
        )
        .await
    }
}

/// The daemon's verdict on a `cancel_tool_call` or `cancel_background_terminal`.
pub(super) async fn wait_for_cancel_result(reply: &mut EventStream, request_id: u64) -> Result<bool> {
    loop {
        match reply.next().await? {
            ServerEvent::ToolCallCancelResult { id, accepted, .. } if id == request_id => return Ok(accepted),
            ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
            _ => {}
        }
    }
}

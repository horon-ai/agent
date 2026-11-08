//! IS-4: `session/prompt` on a v2 connection, plus the v2 halves of
//! `session/cancel`, `$/cancel_request` and `session/close`.
//!
//! A v2 prompt is answered `{messageId}` once the daemon reports the message
//! inserted, never before and never only when the turn ends; the session's
//! stream task (`state.rs`) writes that answer. Before insertion a prompt can
//! still be refused (a JSON-RPC error, no update) or taken back: a cancel sends
//! `withdraw_message`, never a daemon `Cancel`, and a withdrawn prompt answers
//! -32800. After insertion it is never answered with an error; how its turn
//! ends is the idle `state_update`.

use super::super::prompt::{PromptRunGuard, parse_prompt};
use super::super::*;
use super::held::Hold;
use super::state::{Control, PendingPrompt, V2Session, Withdrawal};
use tokio::sync::oneshot;

/// A parsed prompt that owns the session's `prompt_running`.
struct TakenPrompt {
    id: Value,
    cancel: Arc<CancelSignal>,
    text: String,
    images: Vec<(String, String)>,
    content: Vec<Value>,
}

/// A request the session's stream ended before answering. The stream task is
/// the only writer of a v2 prompt's or command's response, so one that resolves
/// its hand-off with an error is answered here instead of left unanswered.
pub(super) const SESSION_CLOSED: &str = "The session closed before the request was answered";

/// The answer to a v2 `session/prompt` whose `prompt` array is empty.
///
/// The alpha.13 `PromptRequest.prompt` is `minItems: 1`, and the empty
/// `user_message` an empty array would insert is not a turn, so the adapter
/// refuses it before the daemon ever sees it. v1 keeps accepting an empty
/// array: its `PromptRequest` sets no `minItems`, and its surface must not
/// change.
const EMPTY_PROMPT: &str = "session/prompt requires at least one prompt content block";

/// Whether the request's `prompt` array is present and empty. A missing or
/// non-array `prompt` never reaches this check: `prompt_from_params` refuses it
/// first.
fn is_empty_prompt(params: &Value) -> bool {
    params
        .get("prompt")
        .and_then(Value::as_array)
        .is_some_and(|prompt| prompt.is_empty())
}

impl AcpRuntime {
    pub(in super::super) async fn handle_session_prompt_v2(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        let (text, images) = match parse_prompt(&message.params, true) {
            Ok(prompt) => prompt,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        if is_empty_prompt(&message.params) {
            return self
                .write_error_value(id, JSONRPC_INVALID_PARAMS, EMPTY_PROMPT.to_string())
                .await;
        }
        let content = message
            .params
            .get("prompt")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let session = self.sessions.lock().await.get(&session_id).cloned();
        let Some((session, v2)) = session.and_then(|session| {
            let v2 = session.v2.get().cloned()?;
            Some((session, v2))
        }) else {
            return self.write_rpc_error(id, RpcError::session_not_found(&session_id)).await;
        };
        // IS-12: a prompt sent while this connection's previous one is still
        // processing waits for it instead of being refused. Cancelled or
        // closed while it waits, it never reached the daemon: -32800.
        match v2.take_prompt(&session, &message.cancel).await {
            Hold::Taken => {}
            Hold::Cancelled | Hold::Closed => return self.write_rpc_error(id, RpcError::cancelled()).await,
        }
        let taken = TakenPrompt {
            id,
            cancel: message.cancel,
            text,
            images,
            content,
        };
        let result = self.run_taken_prompt_v2(taken, &session, &v2).await;
        // Whichever way the prompt ended, the next held one re-checks whether
        // the session is free.
        v2.changed();
        result
    }

    /// The rest of a prompt once it owns the session's `prompt_running`.
    async fn run_taken_prompt_v2(
        &self,
        prompt: TakenPrompt,
        session: &Arc<DaemonSession>,
        v2: &Arc<V2Session>,
    ) -> Result<()> {
        let TakenPrompt {
            id,
            cancel,
            text,
            images,
            content,
        } = prompt;
        let mut guard = PromptRunGuard::new(session);

        if let Some(command) = parse_acp_slash_command(&text) {
            let output = match command {
                Ok(command) => self.run_session_command(session, command).await,
                Err(err) => Err(err),
            };
            let text = match output {
                Ok(text) => text,
                Err(err) => {
                    guard.release();
                    return self
                        .write_error_value(id, JSONRPC_INTERNAL_ERROR, format!("Prompt failed: {err:#}"))
                        .await;
                }
            };
            let (done, written) = oneshot::channel();
            if !v2.send_control(Control::LocalCommand {
                rpc_id: id.clone(),
                content,
                text,
                done,
            }) {
                guard.release();
                return self
                    .write_rpc_error(id, RpcError::new(JSONRPC_INTERNAL_ERROR, SESSION_CLOSED))
                    .await;
            }
            guard.hand_over();
            if written.await.is_err() {
                // The stream ended before it answered: this request still
                // needs exactly one response.
                return self
                    .write_rpc_error(id, RpcError::new(JSONRPC_INTERNAL_ERROR, SESSION_CLOSED))
                    .await;
            }
            return Ok(());
        }

        let usable = {
            let state = session.ui_state.lock().await;
            model_is_usable(state.model.as_deref(), &state.available_models)
        };
        if !usable {
            guard.release();
            return self.write_rpc_error(id, RpcError::auth_required()).await;
        }

        let daemon_id = session.next_message_id();
        let (outcome, answered) = oneshot::channel();
        let request = Request::Message {
            id: daemon_id,
            content: text,
            images,
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        };
        {
            let _gate = v2.submit_gate.lock().await;
            if cancel.is_recorded() {
                guard.release();
                return self.write_rpc_error(id, RpcError::cancelled()).await;
            }
            let pending = PendingPrompt {
                rpc_id: id.clone(),
                key: RequestKey::of(&id),
                content,
                outcome,
            };
            if v2.register(daemon_id, pending).is_err() {
                guard.release();
                return self
                    .write_rpc_error(id, RpcError::new(JSONRPC_INTERNAL_ERROR, SESSION_CLOSED))
                    .await;
            }
            // From here the stream task answers the prompt and keeps the
            // session busy until the prompt is refused or its turn reports idle.
            guard.hand_over();
            if let Err(err) = session.send(&request).await
                && let Some(prompt) = v2.take_pending(daemon_id)
            {
                session.prompt_running.store(false, Ordering::SeqCst);
                return self
                    .write_rpc_error(
                        prompt.rpc_id,
                        RpcError::new(JSONRPC_INTERNAL_ERROR, format!("Prompt failed: {err:#}")),
                    )
                    .await;
            }
        }
        if answered.await.is_err() {
            // The stream ended without answering the prompt (nothing wrote its
            // message to a turn): answer it here so the request gets exactly
            // one response.
            let _ = v2.take_pending(daemon_id);
            session.prompt_running.store(false, Ordering::SeqCst);
            return self
                .write_rpc_error(id, RpcError::new(JSONRPC_INTERNAL_ERROR, SESSION_CLOSED))
                .await;
        }
        Ok(())
    }

    async fn v2_session(&self, session_id: &str) -> Option<(Arc<DaemonSession>, Arc<V2Session>)> {
        let session = self.sessions.lock().await.get(session_id).cloned()?;
        let v2 = session.v2.get().cloned()?;
        Some((session, v2))
    }

    /// `session/cancel` on v2: withdraw the session's prompts that are not
    /// inserted yet, then cancel the turn when foreground work is reported, an
    /// inserted prompt of this editor has not reported its end, or a withdrawal
    /// found its prompt already inserted while that prompt's turn was still
    /// open in daemon order. A prompt that is still queued is only ever
    /// withdrawn. The idle `cancelled` comes from the daemon.
    ///
    /// When the tracker names the running run, the daemon gets
    /// `cancel_turn{run_id}`, which stops only that run: a peer's turn that
    /// starts after this decision is left alone. Without a run id (this
    /// editor's prompt has not reported running yet) it gets the plain
    /// `cancel`.
    ///
    /// What to cancel is settled here, before the caller reads its next
    /// message; the daemon round trips run on the returned task, so the read
    /// loop never waits on the daemon.
    pub(in super::super) async fn cancel_session_v2(&self, session_id: &str) -> Option<tokio::task::JoinHandle<()>> {
        let (session, v2) = self.v2_session(session_id).await?;
        let pending = {
            let _gate = v2.submit_gate.lock().await;
            for signal in self.inflight.prompt_signals(session_id) {
                signal.cancel();
            }
            v2.pending_ids()
        };
        let (mut cancel_turn, run_id) = {
            let tracker = v2.tracker();
            let active = tracker.is_active();
            (
                tracker.has_cancellable_turn(),
                active.then(|| tracker.run_id.clone()).flatten(),
            )
        };
        let runtime = self.clone();
        Some(tokio::spawn(async move {
            let mut untargeted = false;
            for daemon_id in pending {
                let withdrawn = runtime.withdraw_v2_prompt(&session, &v2, daemon_id).await;
                untargeted |= withdrawn == Withdrawal::AlreadyInserted { open: true };
            }
            cancel_turn |= untargeted;
            if cancel_turn {
                let cancel_id = session.next_id();
                let request = match run_id {
                    // A withdrawal raced its own insertion: that turn may not
                    // be the tracked run, so cancel whatever runs.
                    Some(run_id) if !untargeted => Request::CancelTurn { id: cancel_id, run_id },
                    _ => Request::Cancel { id: cancel_id },
                };
                let _ = session.send(&request).await;
                runtime.withdraw_session_calls(&session.session_id).await;
            }
        }))
    }

    /// `$/cancel_request` of a v2 prompt: withdraw it while it is not inserted;
    /// an inserted prompt ignores it. The withdrawal runs on its own task.
    pub(in super::super) async fn cancel_prompt_request_v2(&self, session_id: &str, request_id: &Value) {
        let Some((session, v2)) = self.v2_session(session_id).await else {
            return;
        };
        let pending = {
            let _gate = v2.submit_gate.lock().await;
            v2.pending_id_of(&RequestKey::of(request_id))
        };
        if let Some(daemon_id) = pending {
            let runtime = self.clone();
            tokio::spawn(async move {
                runtime.withdraw_v2_prompt(&session, &v2, daemon_id).await;
            });
        }
    }

    /// Ask the daemon to drop a queued `message`. `Done` means it never will
    /// be stored, so the prompt answers -32800; `already inserted` leaves the
    /// prompt to its insertion answer.
    ///
    /// The reply has no waiter: it reaches the session's stream task in daemon
    /// order, after the insertion and every state event the daemon sent before
    /// it, so the verdict says whether the prompt's turn is still open at that
    /// point. Its id comes from the v2 message range for the same reason a
    /// `message` id does: a peer's turn terminal never carries it.
    async fn withdraw_v2_prompt(&self, session: &DaemonSession, v2: &V2Session, daemon_id: u64) -> Withdrawal {
        let id = session.next_message_id();
        let verdict = v2.expect_withdraw(id, daemon_id);
        let request = Request::WithdrawMessage {
            id,
            message_request_id: daemon_id,
        };
        if let Err(err) = session.send(&request).await {
            v2.forget_withdraw(id);
            crate::logging::warn(&format!(
                "ACP: withdraw of prompt {daemon_id} in {} not sent: {err:#}",
                session.session_id
            ));
            return Withdrawal::Unsettled;
        }
        match tokio::time::timeout(PROMPT_CANCEL_BOUND, verdict).await {
            Ok(Ok(verdict)) => verdict,
            Ok(Err(_)) => Withdrawal::Unsettled,
            Err(_) => {
                v2.forget_withdraw(id);
                crate::logging::warn(&format!(
                    "ACP: withdraw of prompt {daemon_id} in {} got no reply within {}s",
                    session.session_id,
                    PROMPT_CANCEL_BOUND.as_secs()
                ));
                Withdrawal::Unsettled
            }
        }
    }

    /// `session/close` on v2: cancel, wait up to the close bound for the turn
    /// to report idle and every prompt to be answered, then end the session's
    /// stream. A turn still reported then is reported cancelled here, and
    /// whatever the daemon sends for it later is dropped. False when the
    /// session is not active on this connection.
    pub(in super::super) async fn close_active_session_v2(&self, session_id: &str) -> bool {
        let session = self.sessions.lock().await.get(session_id).cloned();
        let Some(session) = session else {
            return false;
        };
        if let Some(v2) = session.v2.get().cloned() {
            if let Some(cancelled) = self.cancel_session_v2(session_id).await {
                let _ = cancelled.await;
            }
            self.withdraw_session_calls(session_id).await;
            let deadline = tokio::time::Instant::now() + v2.close_bound;
            let settled = v2.wait_until(Some(deadline), V2Session::settled).await;
            if !settled {
                crate::logging::warn(&format!(
                    "ACP: closing {session_id} before its turn reported idle within {}s",
                    v2.close_bound.as_secs()
                ));
            }
            let (done, closed) = oneshot::channel();
            if v2.send_control(Control::Close {
                give_up: !settled,
                done,
            }) {
                let _ = closed.await;
            }
        } else {
            self.withdraw_session_calls(session_id).await;
        }
        self.sessions.lock().await.remove(session_id);
        true
    }
}

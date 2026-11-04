//! Blocking approval for turns an ACP editor drives.
//!
//! Without a bridge the approval gate refuses a call, queues a request and asks
//! the model to retry once someone answered (`Registry::approval_refusal`). An
//! ACP editor can answer while the call waits, so a connection that advertised
//! `permission_requests` installs a [`PermissionBridge`] for its session and the
//! gate blocks on it instead: the request is still queued and recorded exactly
//! as before (so `approvals resolve`, the desktop prompt and the durable record
//! keep working), and the editor additionally receives a
//! `tool_permission_requested` event through its own sender.
//!
//! The wait ends on the first of: the editor's answer, a decision recorded on
//! any other surface (observed through `SafetySystem::await_decision`, which
//! also applies the request's timeout default), or the turn's cancel signal.
//! The first resolver wins; later answers are ignored. When the decision came
//! from somewhere other than the editor, the editor is sent
//! `tool_permission_resolved` so it can withdraw the prompt it still shows.
//!
//! Identical calls (same `approval_key`) that overlap share one prompt and one
//! decision. A wait that is dropped without an answer (the turn task aborted)
//! records the request denied with `decided_via = "cancelled"` and releases its
//! key, so nothing is left pending behind a turn that no longer exists.
//!
//! As with [`super::client_bridge`], presence is not ownership: the bridge acts
//! only while the running turn was started by the installing connection.

use super::{Registry, ToolContext, acp_session};
use crate::protocol::ServerEvent;
use crate::safety::PermissionResult;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use tokio::sync::{Notify, mpsc, watch};

/// `decided_via` for a request whose turn was cancelled or aborted while it waited.
pub(crate) const CANCELLED_VIA: &str = "cancelled";
/// `decided_via` for a request whose editor connection went away while it waited.
pub(crate) const DISCONNECTED_VIA: &str = "acp_disconnected";
/// `decided_via` when no decision could be recorded at all.
const UNRECORDED_VIA: &str = "unrecorded";
/// How many settled request ids a bridge remembers to refuse repeated answers.
const DECIDED_MEMORY: usize = 256;

/// The decision a waiting call acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) approved: bool,
    pub(crate) via: String,
}

impl Outcome {
    fn denied(via: &str) -> Self {
        Self {
            approved: false,
            via: via.to_string(),
        }
    }
}

/// Who settled a request, which decides what else settling has to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The editor answered; its answer is already recorded.
    Editor,
    /// The decision store holds the decision; another surface recorded it.
    Store,
    /// The bridge decided (cancel, abort, disconnect) and records it itself.
    Local,
    /// The request never reached the queue, so there is nothing to record.
    Unqueued,
}

struct Pending {
    key: String,
    outcome: watch::Sender<Option<Outcome>>,
    waiters: usize,
    editor_answering: bool,
}

#[derive(Default)]
struct State {
    pending: HashMap<String, Pending>,
    decided: VecDeque<String>,
}

pub(crate) struct PermissionBridge {
    session_id: String,
    connection_id: String,
    tx: mpsc::UnboundedSender<ServerEvent>,
    state: Mutex<State>,
    waiters_changed: Notify,
}

struct Installed {
    token: u64,
    bridge: Arc<PermissionBridge>,
}

fn registry() -> &'static Mutex<HashMap<String, Installed>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Installed>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> MutexGuard<'static, HashMap<String, Installed>> {
    registry().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Offer `connection_id` as the approver for turns it starts in `session_id`.
///
/// Replaces any previous installation and returns the token a later [`clear`]
/// must present.
pub(crate) fn install(session_id: &str, connection_id: &str, tx: mpsc::UnboundedSender<ServerEvent>) -> u64 {
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let bridge = Arc::new(PermissionBridge {
        session_id: session_id.to_string(),
        connection_id: connection_id.to_string(),
        tx,
        state: Mutex::new(State::default()),
        waiters_changed: Notify::new(),
    });
    let replaced = lock().insert(session_id.to_string(), Installed { token, bridge });
    if let Some(previous) = replaced {
        previous.bridge.close();
    }
    token
}

/// Withdraw an installation and deny everything still waiting on it.
///
/// Only the token's own installation is withdrawn, so an older connection
/// finishing its disconnect cannot take away a newer one. A waiting call is
/// denied rather than left parked on an editor that is gone.
pub(crate) fn clear(session_id: &str, token: u64) {
    let removed = {
        let mut installed = lock();
        match installed.get(session_id) {
            Some(entry) if entry.token == token => installed.remove(session_id),
            _ => None,
        }
    };
    if let Some(installed) = removed {
        installed.bridge.close();
    }
}

/// The bridge for `session_id`, or `None` when the running turn belongs to
/// another connection (or none is running).
pub(crate) fn for_session(session_id: &str) -> Option<Arc<PermissionBridge>> {
    let bridge = lock().get(session_id).map(|entry| Arc::clone(&entry.bridge))?;
    acp_session::turn_owned_by(session_id, &bridge.connection_id).then_some(bridge)
}

/// Whether a tool call in `session_id` is waiting for the editor's answer.
pub(crate) fn awaiting_answer(session_id: &str) -> bool {
    let bridge = lock().get(session_id).map(|entry| Arc::clone(&entry.bridge));
    bridge.is_some_and(|bridge| !bridge.state().pending.is_empty())
}

/// The bridge for `session_id`, but only for the connection that installed it.
///
/// Answer routing uses this: an answer belongs to a request this connection
/// was sent, whether or not the turn that asked is still the running one.
pub(crate) fn bridge_for_connection(session_id: &str, connection_id: &str) -> Option<Arc<PermissionBridge>> {
    let bridge = lock().get(session_id).map(|entry| Arc::clone(&entry.bridge))?;
    (bridge.connection_id == connection_id).then_some(bridge)
}

/// What the bridge knows about a request the editor is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorAnswer {
    /// A call is waiting on it; the answer is being applied.
    Pending,
    /// It was already settled; the answer must not change anything.
    AlreadyDecided,
    /// This bridge never asked it.
    Unknown,
}

impl PermissionBridge {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.state().pending.len()
    }

    #[cfg(test)]
    pub(crate) fn waiters(&self, request_id: &str) -> usize {
        self.state().pending.get(request_id).map_or(0, |entry| entry.waiters)
    }

    #[cfg(test)]
    pub(crate) async fn waiters_reach(&self, request_id: &str, count: usize) {
        loop {
            let changed = self.waiters_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.waiters(request_id) >= count {
                return;
            }
            changed.await;
        }
    }

    /// Mark the editor's answer to `request_id` as in flight.
    ///
    /// Called before the answer is recorded, so a waiter that sees the recorded
    /// decision first does not send the editor a withdrawal for its own answer.
    pub(crate) fn begin_editor_answer(&self, request_id: &str) -> EditorAnswer {
        let mut state = self.state();
        if let Some(entry) = state.pending.get_mut(request_id) {
            entry.editor_answering = true;
            return EditorAnswer::Pending;
        }
        if state.decided.iter().any(|id| id == request_id) {
            EditorAnswer::AlreadyDecided
        } else {
            EditorAnswer::Unknown
        }
    }

    /// The editor's answer could not be recorded; it is not in flight any more.
    pub(crate) fn abandon_editor_answer(&self, request_id: &str) {
        if let Some(entry) = self.state().pending.get_mut(request_id) {
            entry.editor_answering = false;
        }
    }

    /// Release the calls waiting on `request_id` with the editor's recorded answer.
    ///
    /// Returns whether this answer settled the request; `false` means another
    /// surface decided first and that decision stands.
    pub(crate) fn finish_editor_answer(&self, request_id: &str, approved: bool, via: &str) -> bool {
        self.settle(
            request_id,
            Outcome {
                approved,
                via: via.to_string(),
            },
            Source::Editor,
        )
    }

    /// Settle `request_id` once. Returns whether this call was the first.
    fn settle(&self, request_id: &str, outcome: Outcome, source: Source) -> bool {
        let removed = {
            let mut state = self.state();
            let Some(entry) = state.pending.remove(request_id) else {
                return false;
            };
            // Published under the lock, so a later settler that finds the entry
            // gone always reads the winning outcome.
            entry.outcome.send_replace(Some(outcome.clone()));
            state.decided.push_back(request_id.to_string());
            while state.decided.len() > DECIDED_MEMORY {
                state.decided.pop_front();
            }
            entry
        };

        if source == Source::Local {
            let message = format!("The waiting tool call ended without an answer ({}).", outcome.via);
            if let Err(error) = crate::tool::ambient::shared_safety_system().record_decision(
                request_id,
                outcome.approved,
                &outcome.via,
                Some(message),
            ) {
                crate::logging::warn(&format!(
                    "permission request {request_id} ended ({}) but the denial could not be recorded: {error}",
                    outcome.via
                ));
            }
        }

        {
            let mut open = Registry::open_approval_requests()
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if open.get(&removed.key).is_some_and(|id| id == request_id) {
                open.remove(&removed.key);
            }
        }

        if source != Source::Unqueued {
            crate::tool::ambient::emit_approval_resolved(
                &self.session_id,
                request_id,
                outcome.approved,
                &outcome.via,
                "approval-gate",
            );
        }
        if source != Source::Editor && !removed.editor_answering {
            let _ = self.tx.send(ServerEvent::ToolPermissionResolved {
                request_id: request_id.to_string(),
                approved: outcome.approved,
            });
        }
        true
    }

    /// Deny every request still waiting; the editor that would answer is gone.
    fn close(&self) {
        let ids: Vec<String> = self.state().pending.keys().cloned().collect();
        for request_id in ids {
            self.settle(&request_id, Outcome::denied(DISCONNECTED_VIA), Source::Local);
        }
    }

    /// Join the open request for `key`, or open a new one.
    ///
    /// The open-request map lock is taken before the bridge's own, the only
    /// order either is ever nested in, so an identical call arriving at the same
    /// moment either finds this request or creates the one this call finds.
    fn claim(&self, key: &str) -> Claim {
        let mut open = Registry::open_approval_requests()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut state = self.state();
        if let Some(request_id) = open.get(key)
            && let Some(entry) = state.pending.get_mut(request_id)
        {
            entry.waiters += 1;
            let claim = Claim {
                request_id: request_id.clone(),
                opened: false,
                outcome: entry.outcome.subscribe(),
            };
            drop(state);
            drop(open);
            self.waiters_changed.notify_waiters();
            return claim;
        }
        let request_id = crate::safety::new_request_id();
        let (outcome, receiver) = watch::channel(None);
        state.pending.insert(
            request_id.clone(),
            Pending {
                key: key.to_string(),
                outcome,
                waiters: 1,
                editor_answering: false,
            },
        );
        open.insert(key.to_string(), request_id.clone());
        drop(state);
        drop(open);
        self.waiters_changed.notify_waiters();
        Claim {
            request_id,
            opened: true,
            outcome: receiver,
        }
    }

    /// Queue the request, record it, and ask the editor.
    ///
    /// On failure the request is already settled and the refusal text returned.
    fn open_request(
        &self,
        request_id: &str,
        resolved_name: &str,
        reason: &str,
        is_dual_confirm: bool,
        input: &Value,
        ctx: &ToolContext,
    ) -> Result<(), String> {
        let request = Registry::gate_permission_request(
            request_id.to_string(),
            resolved_name,
            reason,
            is_dual_confirm,
            input,
            ctx,
            true,
        );
        let description = request.description.clone();
        let queued = crate::tool::ambient::shared_safety_system().request_permission(request);
        if !matches!(&queued, PermissionResult::Queued { request_id: queued_id } if queued_id == request_id) {
            self.settle(request_id, Outcome::denied(UNRECORDED_VIA), Source::Unqueued);
            return Err(Registry::unqueued_refusal(resolved_name, reason, &queued));
        }
        Registry::record_blocked_approval(&ctx.session_id, request_id);
        crate::tool::ambient::emit_approval_pending(
            &ctx.session_id,
            request_id,
            resolved_name,
            chrono::Utc::now()
                + chrono::Duration::from_std(crate::safety::DEFAULT_APPROVAL_TIMEOUT)
                    .unwrap_or_else(|_| chrono::Duration::seconds(1800)),
        );
        crate::safety::dispatch_permission_notification(resolved_name, &description, request_id);
        let asked = self.tx.send(ServerEvent::ToolPermissionRequested {
            request_id: request_id.to_string(),
            tool_call_id: ctx.tool_call_id.clone(),
            tool_name: resolved_name.to_string(),
            title: format!("{resolved_name}: {reason}"),
            input: input.clone(),
        });
        if asked.is_err() {
            self.settle(request_id, Outcome::denied(DISCONNECTED_VIA), Source::Local);
            return Err(denial_text(resolved_name, request_id, DISCONNECTED_VIA));
        }
        Ok(())
    }

    async fn wait(
        &self,
        request_id: &str,
        mut outcome: watch::Receiver<Option<Outcome>>,
        ctx: &ToolContext,
    ) -> Outcome {
        let system = crate::tool::ambient::shared_safety_system();
        let decided = system.await_decision(request_id, crate::safety::DEFAULT_APPROVAL_TIMEOUT);
        tokio::pin!(decided);
        let cancelled = turn_cancelled(ctx);
        tokio::pin!(cancelled);
        loop {
            if let Some(settled) = outcome.borrow_and_update().clone() {
                return settled;
            }
            let candidate = tokio::select! {
                changed = outcome.changed() => match changed {
                    Ok(()) => continue,
                    Err(_) => (Outcome::denied(DISCONNECTED_VIA), Source::Local),
                },
                result = &mut decided => (store_outcome(&system, request_id, &result), Source::Store),
                () = &mut cancelled => (Outcome::denied(CANCELLED_VIA), Source::Local),
            };
            let (candidate, source) = candidate;
            self.settle(request_id, candidate.clone(), source);
            return outcome.borrow().clone().unwrap_or(candidate);
        }
    }
}

struct Claim {
    request_id: String,
    opened: bool,
    outcome: watch::Receiver<Option<Outcome>>,
}

/// Withdraws one waiting call; the last call to leave unanswered denies the
/// request as cancelled.
struct Waiter<'a> {
    bridge: &'a PermissionBridge,
    request_id: String,
    finished: bool,
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let last = {
            let mut state = self.bridge.state();
            match state.pending.get_mut(&self.request_id) {
                Some(entry) => {
                    entry.waiters = entry.waiters.saturating_sub(1);
                    entry.waiters == 0
                }
                None => false,
            }
        };
        if last {
            self.bridge
                .settle(&self.request_id, Outcome::denied(CANCELLED_VIA), Source::Local);
        }
    }
}

fn store_outcome(system: &crate::safety::SafetySystem, request_id: &str, result: &PermissionResult) -> Outcome {
    let via = system
        .decision_for(request_id)
        .map(|decision| decision.decided_via)
        .unwrap_or_else(|| UNRECORDED_VIA.to_string());
    match result {
        PermissionResult::Approved { .. } => Outcome { approved: true, via },
        PermissionResult::Denied { .. } => Outcome { approved: false, via },
        PermissionResult::Queued { .. } | PermissionResult::Timeout => Outcome::denied(UNRECORDED_VIA),
    }
}

/// Resolves when the running turn is cancelled or the daemon reloads.
///
/// `SessionControlHandle::request_cancel` fires every signal
/// `turn_cancel_registry::active_turn_signals` holds for the session, and the
/// turn registers its own `graceful_shutdown` signal there; that same signal is
/// what `ToolContext::graceful_shutdown_signal` carries and what a reload fires.
async fn turn_cancelled(ctx: &ToolContext) {
    let mut signals = crate::turn_cancel_registry::active_turn_signals(&ctx.session_id);
    if let Some(own) = &ctx.graceful_shutdown_signal
        && !signals.iter().any(|signal| signal.same_instance(own))
    {
        signals.push(own.clone());
    }
    if signals.is_empty() {
        return std::future::pending().await;
    }
    futures::future::select_all(signals.iter().map(|signal| Box::pin(signal.notified()))).await;
}

fn denial_text(resolved_name: &str, request_id: &str, via: &str) -> String {
    let how = match via {
        CANCELLED_VIA => "was cancelled before it was answered",
        DISCONNECTED_VIA => "lost its editor connection before it was answered",
        crate::safety::TIMEOUT_DEFAULT_VIA => "was not answered before its wait elapsed",
        UNRECORDED_VIA => "could not be decided",
        _ => "was answered no",
    };
    format!("Tool '{resolved_name}' was denied: request {request_id} {how}. Nothing was executed.")
}

/// Ask the editor and wait for a decision on one call the gate held.
///
/// `None` lets the call run; `Some` is the refusal the call returns.
pub(super) async fn await_permission(
    bridge: &PermissionBridge,
    resolved_name: &str,
    reason: &str,
    is_dual_confirm: bool,
    input: &Value,
    ctx: &ToolContext,
) -> Option<String> {
    let key = Registry::approval_key(&ctx.session_id, resolved_name, input, ctx);
    let claim = bridge.claim(&key);
    let mut waiter = Waiter {
        bridge,
        request_id: claim.request_id.clone(),
        finished: false,
    };
    if claim.opened
        && let Err(refusal) = bridge.open_request(&claim.request_id, resolved_name, reason, is_dual_confirm, input, ctx)
    {
        waiter.finished = true;
        return Some(refusal);
    }
    let outcome = bridge.wait(&claim.request_id, claim.outcome, ctx).await;
    waiter.finished = true;
    if outcome.approved {
        let mut fields = Registry::tool_lifecycle_fields("approved", resolved_name, resolved_name, input, ctx);
        fields.push(("block_reason".to_string(), "approval".to_string()));
        fields.push(("approval_request_id".to_string(), claim.request_id));
        crate::logging::event_info("TOOL_LIFECYCLE", fields);
        return None;
    }
    Some(denial_text(resolved_name, &claim.request_id, &outcome.via))
}

#[cfg(test)]
#[path = "permission_bridge_tests.rs"]
mod tests;

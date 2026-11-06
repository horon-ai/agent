use super::{
    ClientConnectionInfo, FileTouchService, SessionInterruptQueues, SwarmEvent, SwarmEventType, SwarmMember,
    VersionedPlan, record_swarm_event, remove_background_tool_signal, remove_session_channel_subscriptions,
    remove_session_from_swarm, remove_session_interrupt_queue, unregister_session_event_sender, update_member_status,
};
use crate::agent::Agent;
use anyhow::Result;
use horon_agent_agent_runtime::InterruptSignal;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;
type ChannelSubscriptions = Arc<RwLock<HashMap<String, HashMap<String, HashSet<String>>>>>;

const RELOAD_DISCONNECT_MARKER_MAX_AGE: Duration = Duration::from_secs(30);
pub(super) const IDLE_RECONNECT_GRACE: Duration = Duration::from_secs(30);

/// How long a close awaits the turn task it aborted before giving up on it.
const ABORTED_TURN_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-session close barriers.
///
/// A lifecycle that actually removed a session from `sessions` publishes one
/// locked gate here inside the same critical section that decided the removal.
/// A cold resume of that id waits for the published gate before it takes its
/// provisional agent lock or loads state, so it never loads a transcript an old
/// owner is still writing. Live and busy attaches never read this map: they keep
/// their existing fast path, and an idle session that is retained for reconnect
/// grace publishes nothing because nothing was removed.
static SESSION_CLOSE_GATES: LazyLock<std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn close_gates() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Mutex<()>>>> {
    // The map is plain registry data, so a panic while it is held cannot leave
    // a half-applied invariant. Recovering from the poison keeps one broken
    // connection from disabling every later close.
    SESSION_CLOSE_GATES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Owns the locked gate of a session that is being closed.
///
/// The guard outlives every operation that belongs to the removed owner: the
/// terminal save, the removed `Agent` reference (which owns the session writer
/// and its thread), and the identity cleanup. Awaiters are therefore never
/// released while an old writer can still conflict with the successor that
/// resumes the session.
pub(super) struct SessionCloseGuard {
    session_id: String,
    gate: Arc<Mutex<()>>,
    held: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl SessionCloseGuard {
    /// Publish the barrier for `session_id` and take its lock.
    ///
    /// The caller runs this inside the same critical section that decided the
    /// removal. A close owner never waits on another close gate, and this gate
    /// is fresh, so the lock is taken without contending with anyone.
    pub(super) async fn register(session_id: &str) -> Self {
        let gate = Arc::new(Mutex::new(()));
        let held = Arc::clone(&gate).lock_owned().await;
        close_gates().insert(session_id.to_string(), Arc::clone(&gate));
        #[cfg(test)]
        close_barrier_probe::note_published();
        Self {
            session_id: session_id.to_string(),
            gate,
            held: Some(held),
        }
    }
}

impl Drop for SessionCloseGuard {
    fn drop(&mut self) {
        {
            let mut gates = close_gates();
            // A later close of the same id may already have published a fresh
            // gate. Removing by id alone would delete that owner's entry and
            // release waiters it still owns.
            let replaced = gates
                .get(&self.session_id)
                .is_some_and(|current| !Arc::ptr_eq(current, &self.gate));
            if !replaced {
                gates.remove(&self.session_id);
            }
        }
        drop(self.held.take());
        #[cfg(test)]
        close_barrier_probe::note_released(&self.session_id);
    }
}

/// Wait until no close is pending for `session_id`, restarting whenever a newer
/// close replaces the one being waited on. Returns true when it waited at all.
pub(super) async fn await_pending_close(session_id: &str) -> bool {
    let mut waited = false;
    loop {
        let gate = close_gates().get(session_id).cloned();
        let Some(gate) = gate else {
            return waited;
        };
        #[cfg(test)]
        close_barrier_probe::note_wait(session_id);
        let _released = gate.lock().await;
        waited = true;
    }
}

/// Per-session cold-resume claims.
///
/// A cold resume that may load, rewrite and republish a session holds the claim
/// for that id, so two connections cannot both restore the same transcript. The
/// registry only holds weak entries: an entry owns the id and the async mutex,
/// every holder and waiter keeps an `Arc` of that entry, and the last one out
/// removes the key only while the map still points at that same entry. A queued
/// waiter that is cancelled therefore cannot leave a stale key, and a late
/// acquirer can never be handed a second lock for one id.
static COLD_RESUME_CLAIMS: LazyLock<std::sync::Mutex<HashMap<String, Weak<ColdResumeClaim>>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn cold_resume_claims() -> std::sync::MutexGuard<'static, HashMap<String, Weak<ColdResumeClaim>>> {
    COLD_RESUME_CLAIMS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) struct ColdResumeClaim {
    session_id: String,
    gate: Arc<Mutex<()>>,
}

impl Drop for ColdResumeClaim {
    fn drop(&mut self) {
        // This runs on the last strong reference, after the decrement that ended
        // it. Removing the registry key here cannot race a guard's own count
        // check, and a cancelled waiter that never got a guard is covered too.
        let this = self as *const ColdResumeClaim;
        let mut claims = cold_resume_claims();
        let still_current = claims.get(&self.session_id).is_some_and(|entry| entry.as_ptr() == this);
        if still_current {
            claims.remove(&self.session_id);
        }
    }
}

/// An acquired cold-resume claim: the entry plus its owned async guard.
pub(super) struct ColdResumeClaimGuard {
    _claim: Arc<ColdResumeClaim>,
    held: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for ColdResumeClaimGuard {
    fn drop(&mut self) {
        // The async guard is released first so a waiter can proceed; the entry
        // field then drops after this body, and only its own Drop touches the
        // registry, so the registry lock is never held while an entry dies.
        drop(self.held.take());
    }
}

/// Take the cold-resume claim for `session_id`, waiting for any current holder.
pub(super) async fn claim_cold_resume(session_id: &str) -> ColdResumeClaimGuard {
    let claim = {
        let mut claims = cold_resume_claims();
        match claims.get(session_id).and_then(Weak::upgrade) {
            Some(claim) => claim,
            None => {
                let claim = Arc::new(ColdResumeClaim {
                    session_id: session_id.to_string(),
                    gate: Arc::new(Mutex::new(())),
                });
                claims.insert(session_id.to_string(), Arc::downgrade(&claim));
                claim
            }
        }
    };
    let held = Arc::clone(&claim.gate).lock_owned();
    #[cfg(test)]
    close_barrier_probe::note_claim_wait(session_id);
    let held = held.await;
    ColdResumeClaimGuard {
        _claim: claim,
        held: Some(held),
    }
}

/// Run synchronous session persistence without occupying a Tokio worker.
///
/// `block_in_place` is only legal on a multi-threaded runtime; on a current-thread
/// runtime (the common test shape) it panics, and there the blocking work cannot
/// starve anything else because nothing else runs on that thread. Off a runtime,
/// or with no terminal state to persist, the work runs directly.
pub(super) fn blocking_session_io<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Wait for an aborted owner task to drop so the `Agent` reference it cloned is
/// gone before the close barrier opens.
///
/// The deadline is diagnostic only: a task that outlives it still holds the
/// owner's reference, so the wait continues instead of detaching it and opening
/// the barrier early.
async fn settle_aborted_task(mut handle: tokio::task::JoinHandle<()>, session_id: &str) {
    match tokio::time::timeout(ABORTED_TURN_SETTLE_TIMEOUT, &mut handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) if error.is_cancelled() => {}
        Ok(Err(error)) => {
            crate::logging::warn(&format!(
                "Aborted turn for session {session_id} ended abnormally: {error}"
            ));
        }
        Err(_) => {
            #[cfg(test)]
            close_barrier_probe::note_settle_deadline(session_id);
            crate::logging::warn(&format!(
                "Session {session_id} close is still waiting for its aborted turn after {}s; the barrier stays closed until it settles",
                ABORTED_TURN_SETTLE_TIMEOUT.as_secs()
            ));
            match handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    crate::logging::warn(&format!(
                        "Aborted turn for session {session_id} ended abnormally: {error}"
                    ));
                }
            }
        }
    }
}

// The last registered event sender remains on the member after it detaches.
// It is therefore also an ownership witness: an old grace timer must not
// remove a successor's session even if that successor has disconnected again.
//
// A connection can also disconnect before its subscribe registered a swarm
// member at all (e.g. the window closed while subscribe was in flight). In
// that case an absent member is the original state, not evidence of a
// successor. Treating it as replaced made the grace loop return early and
// skip destructive cleanup, leaking the live Agent for the server's lifetime.
async fn attachment_was_replaced(
    members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    session_id: &str,
    original: &mpsc::UnboundedSender<crate::protocol::ServerEvent>,
    originally_registered: bool,
) -> bool {
    match members.read().await.get(session_id) {
        Some(member) => !member.event_tx.same_channel(original),
        None => originally_registered,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisconnectDisposition {
    Closed,
    Crashed,
    Reloading,
}

fn disconnect_disposition(disconnected_while_processing: bool) -> DisconnectDisposition {
    // Losing the UI is only a session crash when it interrupts unfinished work.
    // In particular, force-quitting Desktop after Done is an ordinary close.
    if !disconnected_while_processing {
        return DisconnectDisposition::Closed;
    }

    if crate::server::reload_marker_active(RELOAD_DISCONNECT_MARKER_MAX_AGE) {
        DisconnectDisposition::Reloading
    } else {
        DisconnectDisposition::Crashed
    }
}

fn disconnected_while_processing(
    client_is_processing: bool,
    processing_task: Option<&tokio::task::JoinHandle<()>>,
) -> bool {
    // Socket EOF is prioritized over processing_done_rx. A finished task is
    // authoritative even if the client's cached processing flag is still set.
    processing_task
        .map(|handle| !handle.is_finished())
        .unwrap_or(client_is_processing)
}

/// Release transport-owned state without changing the live session or turn.
pub(super) async fn detach_client_attachment(
    session_id: &str,
    connection_id: &str,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) {
    client_connections.write().await.remove(connection_id);
    unregister_session_event_sender(swarm_members, session_id, connection_id).await;
}

#[expect(
    clippy::too_many_arguments,
    reason = "disconnect cleanup updates sessions, swarms, files, channels, debug state, and shutdown signals together"
)]
/// Returns the event task when a successor needs this lifecycle to finish its turn.
pub(super) async fn cleanup_client_connection(
    sessions: &SessionAgents,
    client_session_id: &str,
    client_is_processing: bool,
    processing_task: &mut Option<tokio::task::JoinHandle<()>>,
    event_handle: tokio::task::JoinHandle<()>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    file_touch: &FileTouchService,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    client_connection_id: &str,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
    client_event_tx: &mpsc::UnboundedSender<crate::protocol::ServerEvent>,
    idle_reconnect_grace: Duration,
) -> Result<Option<tokio::task::JoinHandle<()>>> {
    let disposition = disconnect_disposition(disconnected_while_processing(
        client_is_processing,
        processing_task.as_ref(),
    ));
    // An idle session stays live for a bounded reconnect grace: a saved one so a
    // quick reattach finds it live instead of queueing behind its close save and
    // a cold reload, and an empty unsaved one so it can be reclaimed at all.
    let saved_session = crate::session::session_exists(client_session_id);
    let allow_reconnect = disposition == DisconnectDisposition::Closed
        && match sessions.read().await.get(client_session_id).cloned() {
            None => false,
            Some(_) if saved_session => true,
            Some(agent) => agent
                .try_lock()
                .is_ok_and(|agent| agent.visible_conversation_message_count() == 0),
        };
    let originally_registered = swarm_members.read().await.contains_key(client_session_id);

    detach_client_attachment(
        client_session_id,
        client_connection_id,
        client_connections,
        swarm_members,
    )
    .await;

    if allow_reconnect {
        // Empty roots intentionally have no snapshot, and saved sessions already
        // have theirs on disk. A replacement UI/SDK connection gets a bounded
        // opportunity to reclaim the live Agent; a saved session that nobody
        // reclaims closes (save, `session_end`, memory extraction) when the grace
        // expires. No registry or agent lock is held across the wait.
        // Processing/crash cleanup never enters this path.
        event_handle.abort();
        crate::logging::info(&format!(
            "Retaining idle {} session {} for reconnect grace",
            if saved_session { "saved" } else { "unsaved" },
            client_session_id
        ));
        let deadline = tokio::time::Instant::now() + idle_reconnect_grace;
        loop {
            if attachment_was_replaced(swarm_members, client_session_id, client_event_tx, originally_registered).await
                || client_connections
                    .read()
                    .await
                    .values()
                    .any(|info| info.session_id == client_session_id)
            {
                return Ok(None);
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep_until(std::cmp::min(
                deadline,
                tokio::time::Instant::now() + Duration::from_millis(25),
            ))
            .await;
        }
    }

    // Release stale live ownership before slower cleanup so a reconnecting TUI can
    // reclaim the same session without tripping duplicate-attach guards.
    tokio::task::yield_now().await;

    // Resume claims use this same lock before accessing the sessions map.
    // Keep it through the removal decision so a successor cannot be claimed
    // between the check and session/status/control-handle removal.
    let connections = client_connections.write().await;
    let successor_connected = connections.values().any(|info| info.session_id == client_session_id);
    if successor_connected
        || (allow_reconnect
            && attachment_was_replaced(swarm_members, client_session_id, client_event_tx, originally_registered).await)
    {
        crate::logging::info(&format!(
            "Skipping destructive disconnect cleanup for {} because another client is still attached",
            client_session_id
        ));
        // The lifecycle, not the socket, owns completion bookkeeping. Return
        // its writer handle so it can retain and await the task using the same
        // continuation path as an explicitly detached remote turn. Merely
        // dropping the JoinHandle here would lose completion/status handling.
        if processing_task.is_some() {
            return Ok(Some(event_handle));
        }
        event_handle.abort();
        return Ok(None);
    }

    // A live processing task owns the agent mutex. Abort it before trying to
    // persist the disconnect disposition; otherwise cleanup waits two seconds,
    // times out, and leaves the durable session `Active` precisely when an
    // interrupted desktop turn must become `Crashed`. The join is awaited below,
    // outside the connection region: the task clones the `Agent`, and the close
    // barrier must not open while that reference (and its writer) still lives.
    let mut aborted_turn = processing_task.take().inspect(|handle| handle.abort());

    let mut connections = Some(connections);
    let removed_agent = super::remove_session_entry(sessions, client_session_id).await;
    // Publish the close barrier inside the same critical section that decided
    // the removal, so a cold resume of this id either sees the live entry (fast
    // attach) or waits for everything below. A `None` removal publishes nothing
    // and keeps the rest of the cleanup inside the connection region, exactly as
    // before, so it cannot erase a successor's state.
    let close_guard = if removed_agent.is_some() {
        Some(SessionCloseGuard::register(client_session_id).await)
    } else {
        None
    };
    if close_guard.is_some() {
        // Slow saves, lifecycle hooks and identity cleanup belong to the
        // removed owner, not to the server-wide attach path.
        connections = None;
        // The barrier this close owns may only open after the aborted turn's
        // Agent clone is gone, so the wait happens inside that ownership.
        if let Some(handle) = aborted_turn.take() {
            settle_aborted_task(handle, client_session_id).await;
        }
    }

    {
        if let Some(agent_arc) = removed_agent {
            // The removal helper already dropped the event sink and the
            // per-session registries. A plain client disconnect returns above
            // without removing the session, so other attachments keep theirs.
            // An owned lock keeps the guard free of any borrow of `agent_arc`,
            // so the reference below can be the one that releases the writer.
            let lock_result =
                tokio::time::timeout(std::time::Duration::from_secs(2), Arc::clone(&agent_arc).lock_owned()).await;

            match lock_result {
                Ok(mut agent) => {
                    // Terminal state persistence writes and fsyncs on the
                    // calling thread; hand the worker core to the blocking pool
                    // instead of stalling the attach path of other sessions.
                    blocking_session_io(|| match disposition {
                        DisconnectDisposition::Closed => {
                            agent.mark_closed();
                        }
                        DisconnectDisposition::Reloading => {
                            agent.mark_crashed(Some("Server reload interrupted processing".to_string()));
                        }
                        DisconnectDisposition::Crashed => {
                            agent.mark_crashed(Some("Client disconnected while processing".to_string()));
                        }
                    });

                    let memory_enabled = agent.memory_enabled();
                    let transcript = if memory_enabled {
                        Some(agent.build_transcript_for_extraction())
                    } else {
                        None
                    };
                    let sid = client_session_id.to_string();
                    let working_dir = agent.working_dir().map(|dir| dir.to_string());
                    drop(agent);
                    let event = match disposition {
                        DisconnectDisposition::Closed => crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                            "session_closed",
                            "client_disconnected",
                        ),
                        DisconnectDisposition::Crashed => crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                            "session_crashed",
                            "client_disconnected_while_processing",
                        ),
                        DisconnectDisposition::Reloading => crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                            "session_reloading",
                            "server_reload_disconnect",
                        ),
                    }
                    .with_session_id(sid.clone())
                    .force_attribution();
                    crate::runtime_memory_log::emit_event(event);
                    if let Some(transcript) = transcript {
                        crate::memory_agent::trigger_final_extraction_with_dir(transcript, sid, working_dir);
                    }
                }
                Err(_) => {
                    crate::logging::warn(&format!(
                        "Session {} cleanup timed out waiting for agent lock (stuck task); skipping graceful shutdown",
                        client_session_id
                    ));
                }
            }
            // Releasing the removed owner's own reference drops the session and
            // its writer arc, whose `Drop` joins the writer thread. Run that in
            // blocking work so the join never occupies a runtime worker.
            blocking_session_io(move || drop(agent_arc));
        }
    }

    {
        let (status, detail) = match disposition {
            DisconnectDisposition::Closed => ("stopped", Some("disconnected".to_string())),
            DisconnectDisposition::Crashed => ("crashed", Some("disconnect while running".to_string())),
            DisconnectDisposition::Reloading => ("stopped", Some("server reload in progress".to_string())),
        };
        update_member_status(
            client_session_id,
            status,
            detail,
            swarm_members,
            swarms_by_id,
            Some(event_history),
            Some(event_counter),
            Some(swarm_event_tx),
        )
        .await;

        let (swarm_id, removed_name) = {
            let mut members = swarm_members.write().await;
            if let Some(member) = members.remove(client_session_id) {
                (member.swarm_id, member.friendly_name)
            } else {
                (None, None)
            }
        };
        crate::session_metrics::forget(client_session_id);
        crate::session_effort::forget_session_effort(client_session_id);

        if let Some(ref swarm_id) = swarm_id {
            record_swarm_event(
                event_history,
                event_counter,
                swarm_event_tx,
                client_session_id.to_string(),
                removed_name.clone(),
                Some(swarm_id.clone()),
                SwarmEventType::MemberChange {
                    action: "left".to_string(),
                },
            )
            .await;
            remove_session_from_swarm(
                client_session_id,
                swarm_id,
                swarm_members,
                swarms_by_id,
                swarm_coordinators,
                swarm_plans,
            )
            .await;
        }
        remove_session_channel_subscriptions(
            client_session_id,
            channel_subscriptions,
            channel_subscriptions_by_session,
        )
        .await;
        file_touch.clear_session(client_session_id).await;
    }

    {
        let mut signals = shutdown_signals.write().await;
        signals.remove(client_session_id);
    }
    remove_background_tool_signal(client_session_id);
    remove_session_interrupt_queue(soft_interrupt_queues, client_session_id).await;

    drop(connections);
    // Nothing was removed, so no barrier owns this turn task: settle it here,
    // outside the connection region, so an untimed wait never blocks attach.
    if let Some(handle) = aborted_turn {
        settle_aborted_task(handle, client_session_id).await;
    }
    // The barrier opens only now: the terminal save, the removed reference and
    // every identity cleanup above belong to the removed owner.
    drop(close_guard);
    event_handle.abort();
    Ok(None)
}

#[cfg(test)]
#[path = "client_close_barrier_tests.rs"]
mod close_barrier_tests;

/// Test-only observations for the close barrier.
///
/// A wait is recorded before the await so a test can prove the resume reached
/// the barrier instead of merely having started, and a release observer runs
/// inside `SessionCloseGuard::drop`, which is the only place that can witness
/// what was still alive when the barrier opened.
#[cfg(test)]
pub(crate) mod close_barrier_probe {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    /// One-shot callbacks run when the gate of a session is released.
    type ReleaseObserver = Box<dyn FnOnce() + Send>;
    type ReleaseObservers = HashMap<String, Vec<ReleaseObserver>>;

    static WAITS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
    static OBSERVATIONS: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
    static RELEASE_OBSERVERS: LazyLock<Mutex<ReleaseObservers>> = LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(super) fn note_wait(session_id: &str) {
        if let Ok(mut waits) = WAITS.lock() {
            waits.push(session_id.to_string());
        }
        OBSERVATIONS.notify_waiters();
    }

    pub(super) fn note_published() {
        OBSERVATIONS.notify_waiters();
    }

    /// Subscribe to publication and wait/deadline notes before triggering work.
    pub(crate) fn observe(predicate: impl Fn() -> bool) -> impl std::future::Future<Output = ()> {
        let mut notified = Box::pin(OBSERVATIONS.notified());
        let _ = notified.as_mut().enable();
        async move {
            loop {
                if predicate() {
                    return;
                }
                notified.as_mut().await;
                notified.as_mut().set(OBSERVATIONS.notified());
                let _ = notified.as_mut().enable();
            }
        }
    }

    static SETTLE_DEADLINES: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

    pub(super) fn note_settle_deadline(session_id: &str) {
        if let Ok(mut deadlines) = SETTLE_DEADLINES.lock() {
            deadlines.push(session_id.to_string());
        }
        OBSERVATIONS.notify_waiters();
    }

    /// How many times a close reached the aborted-turn settle deadline.
    pub(crate) fn settle_deadlines(session_id: &str) -> usize {
        SETTLE_DEADLINES
            .lock()
            .map(|deadlines| deadlines.iter().filter(|deadline| *deadline == session_id).count())
            .unwrap_or_default()
    }

    /// How many times a cold resume reached the barrier wait for this session.
    pub(crate) fn waits(session_id: &str) -> usize {
        WAITS
            .lock()
            .map(|waits| waits.iter().filter(|wait| *wait == session_id).count())
            .unwrap_or_default()
    }

    /// Run `observer` when the close guard of this session is dropped.
    pub(crate) fn on_release(session_id: &str, observer: impl FnOnce() + Send + 'static) {
        if let Ok(mut observers) = RELEASE_OBSERVERS.lock() {
            observers
                .entry(session_id.to_string())
                .or_default()
                .push(Box::new(observer));
        }
    }

    pub(super) fn note_released(session_id: &str) {
        let observers = RELEASE_OBSERVERS
            .lock()
            .ok()
            .and_then(|mut observers| observers.remove(session_id))
            .unwrap_or_default();
        for observer in observers {
            observer();
        }
    }

    /// Number of sessions that currently have a published close barrier.
    pub(crate) fn published_gates() -> usize {
        super::close_gates().len()
    }

    /// Number of registered entries, including any stale weak references.
    pub(crate) fn cold_resume_claim_entries() -> usize {
        super::cold_resume_claims().len()
    }

    /// Identity of the registered entry for this session, when one is live.
    pub(crate) fn cold_resume_claim_ptr(session_id: &str) -> Option<usize> {
        super::cold_resume_claims()
            .get(session_id)
            .filter(|entry| entry.strong_count() > 0)
            .map(|entry| entry.as_ptr() as usize)
    }

    static CLAIM_WAITS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

    pub(super) fn note_claim_wait(session_id: &str) {
        if let Ok(mut waits) = CLAIM_WAITS.lock() {
            waits.push(session_id.to_string());
        }
        OBSERVATIONS.notify_waiters();
    }

    /// How many cold-resume attempts reached the claim for this session.
    pub(crate) fn claim_waits(session_id: &str) -> usize {
        CLAIM_WAITS
            .lock()
            .map(|waits| waits.iter().filter(|wait| *wait == session_id).count())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "client_disconnect_grace_tests.rs"]
mod grace_tests;

#[cfg(test)]
mod tests {
    use super::{DisconnectDisposition, disconnect_disposition, disconnected_while_processing};

    #[test]
    fn idle_disconnect_is_closed() {
        assert_eq!(disconnect_disposition(false), DisconnectDisposition::Closed);
    }

    #[tokio::test]
    async fn completed_task_overrides_stale_processing_flag() {
        let task = tokio::spawn(async {});
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            disconnect_disposition(disconnected_while_processing(true, Some(&task))),
            DisconnectDisposition::Closed
        );
    }

    #[tokio::test]
    async fn unfinished_task_is_processing_even_without_cached_flag() {
        let task = tokio::spawn(std::future::pending::<()>());
        assert!(disconnected_while_processing(false, Some(&task)));
        task.abort();
        assert!(disconnected_while_processing(true, None));
        assert!(!disconnected_while_processing(false, None));
    }

    #[test]
    fn running_disconnect_without_reload_is_crash() {
        let _guard = crate::storage::lock_test_env();
        crate::server::clear_reload_marker();
        assert_eq!(disconnect_disposition(true), DisconnectDisposition::Crashed);
    }

    #[test]
    fn running_disconnect_during_reload_is_expected() {
        let _guard = crate::storage::lock_test_env();
        let runtime = tempfile::TempDir::new().expect("create runtime dir");
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        crate::server::clear_reload_marker();
        crate::server::write_reload_state("test-request", "test-hash", crate::server::ReloadPhase::Starting, None);
        assert_eq!(disconnect_disposition(true), DisconnectDisposition::Reloading);
        assert_eq!(disconnect_disposition(false), DisconnectDisposition::Closed);
        crate::server::clear_reload_marker();
        crate::env::remove_var("HORON_AGENT_RUNTIME_DIR");
    }

    #[test]
    fn running_disconnect_during_recent_socket_ready_reload_is_expected() {
        let _guard = crate::storage::lock_test_env();
        let runtime = tempfile::TempDir::new().expect("create runtime dir");
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        crate::server::clear_reload_marker();
        crate::server::write_reload_state(
            "test-request",
            "test-hash",
            crate::server::ReloadPhase::SocketReady,
            None,
        );
        assert_eq!(disconnect_disposition(true), DisconnectDisposition::Reloading);
        crate::server::clear_reload_marker();
        crate::env::remove_var("HORON_AGENT_RUNTIME_DIR");
    }
}

//! Foreground turn state for connections that negotiated ACP protocol v2.
//!
//! A v2 editor is told when a session starts and stops running a turn,
//! whichever surface started it: its own prompt, another attached client, or
//! the daemon itself. Every streaming turn path wraps its turn in a
//! [`ForegroundGuard`] and hands the agent the guard's [`TurnReport`]:
//!
//! - The agent fills the report once the turn holds the agent lock and has
//!   stored its starting message (`TurnReport::inserted`, then
//!   `TurnReport::started`), and `started` queues `ForegroundState { running:
//!   true }` on the turn's own fanout sender, so it is ordered with the turn's
//!   stream events.
//! - Dropping the guard queues `ForegroundState { running: false }` on the same
//!   sender. The guard lives in the task that owns the turn and is dropped after
//!   that task's terminal `Done`/`Error`, so the idle state is the turn's last
//!   event; because it is a drop, an aborted or panicking turn reports it too. A
//!   turn that never stored its message (a busy, withdrawn or rejected start)
//!   reports nothing at all.
//!
//! The stop reason is settled by the fanout task that forwards the turn's
//! events ([`StopObserver`]): it is the only place that has seen every
//! `TurnStopped`, `ProviderGuardrail` and `Interrupted` of the turn, in order,
//! when the idle event passes through. It applies the same table the v1
//! adapter uses for its `stopReason`.
//!
//! The agent passes to the next queued turn only after the idle state reached
//! the session's attachments: every turn has its own fanout task, so an idle
//! state merely queued on one turn's sender could still be forwarded after the
//! next turn's `running`. A turn owner wraps its agent lock in
//! [`ForegroundGuard::keep_until_forwarded`]; the lock is released only once
//! the fanout task reported the turn's idle state through [`note_forwarded`].
//! [`ForegroundGuard::close`] waits for that in the owning task. When a cancel
//! holds the idle state ([`DeferredIdle`]) the owner cannot wait (the cancel
//! waits for the owner), and an aborted owner cannot wait at all: then the
//! lock is parked in the turn's slot and released after the held idle state
//! went out and was forwarded, so the next turn still starts after it.
//!
//! Every connection receives these events through the shared session fanout;
//! the per-connection forwarder drops them for every connection that did not
//! negotiate v2, so desktop, web, harness and v1 ACP clients see exactly the
//! stream they had before.

use crate::protocol::{ServerEvent, TurnStopReason};
use std::any::Any;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Called once with the stored message id, right after the turn stored its
/// starting message and before the session is persisted.
pub(crate) type InsertionCallback = Box<dyn FnOnce(&str) + Send>;

const STOP_END_TURN: &str = "end_turn";
const STOP_CANCELLED: &str = "cancelled";
const STOP_ERROR: &str = "error";
const STOP_REFUSAL: &str = "refusal";
const STOP_MAX_TOKENS: &str = "max_tokens";
const STOP_MAX_TURN_REQUESTS: &str = "max_turn_requests";

/// `provider_stop_reason` values of a `LimitReached` stop that mean the output
/// token limit.
///
/// Matched ASCII case-insensitively: a wire reports its own spelling, and Gemini
/// says `MAX_TOKENS` where the OpenAI wires say `max_tokens`.
pub const OUTPUT_LIMIT_STOPS: [&str; 4] = ["max_tokens", "max_output_tokens", "length", "incomplete"];

/// Raw provider stop reasons on `ProviderGuardrail` that are a refusal.
///
/// Matched ASCII case-insensitively, for the same reason as
/// [`OUTPUT_LIMIT_STOPS`].
pub const REFUSAL_STOPS: [&str; 2] = ["refusal", "content_filter"];

/// Whether `reason` names one of `stops`, ignoring ASCII case.
pub fn is_stop_reason(reason: &str, stops: &[&str]) -> bool {
    stops.iter().any(|stop| reason.eq_ignore_ascii_case(stop))
}

#[derive(Default)]
struct TurnSlot {
    session_id: Option<String>,
    run_id: Option<String>,
    start_message_id: Option<String>,
    inserted: bool,
    started: bool,
    cancelled: bool,
    registration: Option<u64>,
    /// The turn's ordered fanout sender; the idle state goes here.
    tx: Option<mpsc::UnboundedSender<ServerEvent>>,
    /// How the owning task saw the turn end; `None` for an aborted task.
    end: Option<TurnEnd>,
    guard_dropped: bool,
    /// Cancel paths that hold the idle report until their own reply is queued.
    deferrals: u32,
    idle_sent: bool,
    /// The owner keeps a lock until the idle state was forwarded
    /// ([`ForegroundGuard::keep_until_forwarded`]): the idle send registers a
    /// forward waiter first.
    tracks_forward: bool,
    /// Resolves (as `true`) once a fanout task forwarded the idle state; kept
    /// cloneable so both the owner waiting in `close` and the fallback release
    /// of an aborted owner can wait on it.
    forwarded: Option<watch::Receiver<bool>>,
    /// The owner's lock, parked here when the owner left before its idle state
    /// went out (a cancel holds it, or the owner was aborted).
    parked: Option<Box<dyn Any + Send>>,
}

/// A parked lock whose release waits for the idle state to be forwarded.
struct Release {
    keep: Box<dyn Any + Send>,
    run_id: Option<String>,
    forwarded: Option<watch::Receiver<bool>>,
}

impl Release {
    /// Drop the lock once the idle state was forwarded (bounded), off the
    /// caller's path: the caller may be the cancel the idle state waited for.
    fn run(self) {
        let Release {
            keep,
            run_id,
            forwarded,
        } = self;
        let (Some(run_id), Some(forwarded)) = (run_id, forwarded) else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            await_forward(&run_id, forwarded).await;
            drop(keep);
        });
    }
}

impl TurnSlot {
    /// Queue the idle state once, when the turn is over and no cancel path
    /// holds it back. A turn that never stored its message reports nothing.
    ///
    /// Returns the release of a lock its owner parked here, to run once the
    /// slot is unlocked.
    #[must_use = "a parked lock is released only by running the release"]
    fn send_idle_if_due(&mut self) -> Option<Release> {
        if self.idle_sent || !self.guard_dropped || self.deferrals > 0 || !self.reports_idle() {
            return None;
        }
        self.idle_sent = true;
        let session_id = self.session_id.clone().unwrap_or_default();
        if self.tracks_forward
            && let Some(run_id) = self.run_id.as_ref()
        {
            // Registered before the send, so the forward cannot be missed.
            let (waiter, forwarded) = watch::channel(false);
            forward_waiters().insert(run_id.clone(), waiter);
            // The turn ended but its lock is held until the fanout forwarded
            // this idle state: publish that window so a wake waits for it
            // instead of reading the held lock as a busy session (#263).
            let (handover, _) = watch::channel(false);
            lock_handovers().insert(
                session_id.clone(),
                Handover {
                    run_id: run_id.clone(),
                    sender: handover,
                },
            );
            self.forwarded = Some(forwarded);
        }
        let hint = match (self.cancelled, self.end) {
            (true, _) | (false, None) => STOP_CANCELLED,
            (false, Some(TurnEnd::Done)) => STOP_END_TURN,
            (false, Some(TurnEnd::Error)) => STOP_ERROR,
        };
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(ServerEvent::ForegroundState {
                session_id,
                running: false,
                run_id: self.run_id.clone().unwrap_or_default(),
                start_message_id: self.start_message_id.clone(),
                user_content: None,
                awaiting_user: false,
                stop_reason: Some(hint.to_string()),
                snapshot: false,
            });
        }
        self.parked.take().map(|keep| Release {
            keep,
            run_id: self.run_id.clone(),
            forwarded: self.forwarded.clone(),
        })
    }

    /// A turn that stored its message or started reports an idle state.
    fn reports_idle(&self) -> bool {
        self.inserted || self.started
    }
}

/// Wait (bounded) until a fanout task forwarded the idle state of `run_id`.
async fn await_forward(run_id: &str, mut forwarded: watch::Receiver<bool>) {
    // `wait_for` returns at once when the idle state was already forwarded.
    let wait = async move {
        let _ = forwarded.wait_for(|forwarded| *forwarded).await;
    };
    if tokio::time::timeout(IDLE_FORWARD_BOUND, wait).await.is_err() {
        forward_waiters().remove(run_id);
        // The handover window is over either way: a wake must not wait on a
        // forward that already gave up.
        end_handover(run_id);
        crate::logging::warn(&format!(
            "foreground: the idle state of run {run_id} was not forwarded within {IDLE_FORWARD_BOUND:?}"
        ));
    }
}

fn lock_slot(slot: &Mutex<TurnSlot>) -> MutexGuard<'_, TurnSlot> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

struct ActiveTurn {
    token: u64,
    slot: Arc<Mutex<TurnSlot>>,
}

/// Session id -> the turn running in it now. Read by the attach snapshot and by
/// cancellation; written only by [`TurnReport::started`] and the guard's drop.
static ACTIVE: LazyLock<Mutex<HashMap<String, ActiveTurn>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn active() -> MutexGuard<'static, HashMap<String, ActiveTurn>> {
    ACTIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run id -> the turn owner waiting for that run's idle state to be forwarded.
static FORWARD_WAITERS: LazyLock<Mutex<HashMap<String, watch::Sender<bool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn forward_waiters() -> MutexGuard<'static, HashMap<String, watch::Sender<bool>>> {
    FORWARD_WAITERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long a turn owner keeps the agent waiting for its idle state to be
/// forwarded. Only a safety bound: the fanout task forwards in order and is
/// alive while the owner still holds the turn's sender.
const IDLE_FORWARD_BOUND: Duration = Duration::from_secs(5);

/// The run a session fanout task reports through [`note_forwarded`] once it
/// forwarded `event`: the run of an idle state, `None` for every other event.
pub(crate) fn idle_run_id(event: &ServerEvent) -> Option<String> {
    match event {
        ServerEvent::ForegroundState {
            running: false,
            snapshot: false,
            run_id,
            ..
        } => Some(run_id.clone()),
        _ => None,
    }
}

/// Called by a session fanout task after it forwarded the idle state of
/// `run_id`: releases the turn owner waiting in [`ForegroundGuard::close`], and
/// ends the session's handover window when that run owned it.
pub(crate) fn note_forwarded(run_id: &str) {
    if let Some(waiter) = forward_waiters().remove(run_id) {
        let _ = waiter.send(true);
    }
    end_handover(run_id);
}

/// Drop the handover window of `run_id`, if one is still open. Dropping the
/// `watch::Sender` closes the channel, which is what wakes a waiting wake.
fn end_handover(run_id: &str) {
    let mut handovers = lock_handovers();
    let session_id = handovers
        .iter()
        .find(|(_, handover)| handover.run_id == run_id)
        .map(|(session_id, _)| session_id.clone());
    if let Some(session_id) = session_id {
        handovers.remove(&session_id);
    }
}

struct Handover {
    run_id: String,
    /// Never sent on: the window ends when the entry is dropped, which closes
    /// the channel and releases whoever waits in [`await_handover`].
    sender: watch::Sender<bool>,
}

/// Session id -> its finished turn's idle state still being forwarded.
///
/// A finished turn holds its agent lock until every attachment has its idle
/// state, so for that window `try_lock` fails although no turn runs. A wake
/// that finds the lock taken needs to tell that window from a busy session:
/// only a session listed here has ended its turn, and [`await_handover`] waits
/// for its entry to go. A lock held by anything else (a connection processing a
/// request, a test holding the guard) is not a handover and never delays a
/// wake. Keyed by session because the turn's registration is gone by the time
/// its lock is released, so the run id alone no longer leads back here.
static HANDOVERS: LazyLock<Mutex<HashMap<String, Handover>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_handovers() -> MutexGuard<'static, HashMap<String, Handover>> {
    HANDOVERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Test seam: publish a handover window for `session_id` exactly as a finished
/// turn does, so [`await_handover`] can be exercised without driving a whole
/// turn through the fanout task.
#[cfg(test)]
pub(crate) fn register_handover_for_test(session_id: &str, run_id: &str) {
    let (sender, _) = watch::channel(false);
    lock_handovers().insert(
        session_id.to_string(),
        Handover {
            run_id: run_id.to_string(),
            sender,
        },
    );
}

/// Wait (bounded) for the finished turn of `session_id` to hand over, so a
/// wake can take the lock instead of reading it as a busy session. False when
/// no handover is in progress or it outlived `bound`.
pub(crate) async fn await_handover(session_id: &str, bound: Duration) -> bool {
    let Some(mut forwarded) = lock_handovers()
        .get(session_id)
        .map(|handover| handover.sender.subscribe())
    else {
        return false;
    };
    // The channel closes when the handover entry is dropped: no value is ever
    // sent, so the loop ends on the error, not on `done`.
    let wait = async move { while forwarded.changed().await.is_ok() {} };
    tokio::time::timeout(bound, wait).await.is_ok()
}

/// What the agent fills in for one turn. Handed to
/// `Agent::run_once_streaming_mpsc_with_display_role`.
pub(crate) struct TurnReport {
    slot: Arc<Mutex<TurnSlot>>,
    on_inserted: Option<InsertionCallback>,
}

impl TurnReport {
    /// The turn stored its starting message as `message_id`.
    ///
    /// Fires the insertion callback exactly once. From here on the turn counts
    /// as started for the guard: a failure after this point is reported as an
    /// idle state with stop reason `error`, never as silence.
    pub(crate) fn inserted(&mut self, session_id: &str, message_id: &str) {
        {
            let mut slot = lock_slot(&self.slot);
            slot.session_id = Some(session_id.to_string());
            slot.start_message_id = Some(message_id.to_string());
            slot.inserted = true;
        }
        if let Some(callback) = self.on_inserted.take() {
            callback(message_id);
        }
    }

    /// The turn began run `run_id`: queue `running: true` on the turn's sender.
    ///
    /// `user_content` is the stored text of a message stored without a display
    /// role, which is what makes it a user message; system and `[auto]` rows
    /// pass `None`.
    pub(crate) fn started(
        &self,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        session_id: &str,
        run_id: &str,
        user_content: Option<String>,
    ) {
        static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let start_message_id = {
            let mut slot = lock_slot(&self.slot);
            slot.session_id = Some(session_id.to_string());
            slot.run_id = Some(run_id.to_string());
            slot.started = true;
            slot.registration = Some(token);
            slot.start_message_id.clone()
        };
        active().insert(
            session_id.to_string(),
            ActiveTurn {
                token,
                slot: Arc::clone(&self.slot),
            },
        );
        let _ = event_tx.send(ServerEvent::ForegroundState {
            session_id: session_id.to_string(),
            running: true,
            run_id: run_id.to_string(),
            start_message_id,
            user_content,
            awaiting_user: false,
            stop_reason: None,
            snapshot: false,
        });
    }

    /// The turn's own stop cause, recorded before the turn returns. A cancel or
    /// shutdown it observed makes the idle state `cancelled` whatever the result.
    pub(crate) fn stopped(&self, provenance: horon_agent_agent_runtime::StopProvenance) {
        use horon_agent_agent_runtime::StopProvenance;
        if matches!(provenance, StopProvenance::UserCancelled | StopProvenance::Shutdown) {
            lock_slot(&self.slot).cancelled = true;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnEnd {
    Done,
    Error,
}

/// Owns the idle report of one turn. See the module documentation.
#[must_use = "the idle state is reported when the guard drops"]
pub(crate) struct ForegroundGuard {
    slot: Arc<Mutex<TurnSlot>>,
}

impl ForegroundGuard {
    pub(crate) fn new(tx: &mpsc::UnboundedSender<ServerEvent>) -> Self {
        Self {
            slot: Arc::new(Mutex::new(TurnSlot {
                tx: Some(tx.clone()),
                ..TurnSlot::default()
            })),
        }
    }

    pub(crate) fn report(&self, on_inserted: Option<InsertionCallback>) -> TurnReport {
        TurnReport {
            slot: Arc::clone(&self.slot),
            on_inserted,
        }
    }

    /// A handle a cancel path can use to hold this turn's idle report, even
    /// before the turn started or registered as running.
    pub(crate) fn idle_handle(&self) -> IdleHandle {
        IdleHandle {
            slot: Arc::clone(&self.slot),
        }
    }

    /// Record how the owning task saw the turn end. A guard dropped without this
    /// (an aborted task) reports `cancelled`.
    pub(crate) fn finish(&mut self, ok: bool) {
        lock_slot(&self.slot).end = Some(if ok { TurnEnd::Done } else { TurnEnd::Error });
    }

    /// Keep `lock` (the turn's agent lock) until this turn's idle state was
    /// forwarded to the session, however the owner leaves: through
    /// [`Self::close`], or by being aborted.
    ///
    /// Use it only when the guard's sender is a session fanout sender
    /// (`session_event_fanout_sender*`), which reports forwarding.
    pub(crate) fn keep_until_forwarded<T: Send + 'static>(&self, lock: T) -> KeptUntilForwarded<T> {
        lock_slot(&self.slot).tracks_forward = true;
        KeptUntilForwarded {
            lock: Some(lock),
            slot: Arc::clone(&self.slot),
        }
    }

    /// Drop the guard, then release `kept` once a session fanout task
    /// forwarded the idle state the drop reported, so the next turn finds
    /// every attachment already holding this turn's end.
    ///
    /// When a cancel holds the idle state this returns at once and parks the
    /// lock in the turn's slot: that cancel waits for this task, so the task
    /// cannot wait for it; the lock is released after the held idle state was
    /// sent and forwarded. An owner aborted while waiting here parks the lock
    /// the same way, instead of releasing it with the task.
    pub(crate) async fn close<T: Send + 'static>(self, mut kept: KeptUntilForwarded<T>) {
        let slot = Arc::clone(&self.slot);
        drop(self);
        let forward = {
            let slot = lock_slot(&slot);
            slot.idle_sent.then(|| (slot.run_id.clone(), slot.forwarded.clone()))
        };
        // Not sent: the drop of `kept` parks the lock (or releases it when the
        // turn never started and reports nothing).
        let Some((run_id, forwarded)) = forward else {
            return;
        };
        if let (Some(run_id), Some(forwarded)) = (run_id, forwarded) {
            // Waited for while `kept` still owns the lock: an owner aborted
            // during this wait parks the lock through `kept`'s drop and
            // releases it only once the idle state was forwarded, instead of
            // releasing it with the aborted task.
            await_forward(&run_id, forwarded).await;
        }
        if let Some(lock) = kept.lock.take() {
            drop(lock);
        }
    }
}

/// A turn owner's lock, released only after the turn's idle state was
/// forwarded. See [`ForegroundGuard::keep_until_forwarded`].
pub(crate) struct KeptUntilForwarded<T: Send + 'static> {
    lock: Option<T>,
    slot: Arc<Mutex<TurnSlot>>,
}

impl<T: Send + 'static> Deref for KeptUntilForwarded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.lock.as_ref().expect("the lock is held until the owner leaves")
    }
}

impl<T: Send + 'static> DerefMut for KeptUntilForwarded<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.lock.as_mut().expect("the lock is held until the owner leaves")
    }
}

impl<T: Send + 'static> Drop for KeptUntilForwarded<T> {
    /// The owner left without [`ForegroundGuard::close`] waiting for it (a
    /// cancel holds the idle state, or the owner was aborted): park the lock
    /// until the idle state is out and forwarded.
    fn drop(&mut self) {
        let Some(lock) = self.lock.take() else {
            return;
        };
        let release = {
            let mut slot = lock_slot(&self.slot);
            if !slot.reports_idle() {
                None
            } else if !slot.idle_sent {
                slot.parked = Some(Box::new(lock));
                return;
            } else {
                Some(Release {
                    keep: Box::new(lock),
                    run_id: slot.run_id.clone(),
                    forwarded: slot.forwarded.clone(),
                })
            }
        };
        if let Some(release) = release {
            release.run();
        }
    }
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        let (session_id, registration, release) = {
            let mut slot = lock_slot(&self.slot);
            slot.guard_dropped = true;
            let release = slot.send_idle_if_due();
            (slot.session_id.clone().unwrap_or_default(), slot.registration, release)
        };
        if let Some(release) = release {
            release.run();
        }
        if let Some(token) = registration {
            let mut turns = active();
            if turns.get(&session_id).is_some_and(|turn| turn.token == token) {
                turns.remove(&session_id);
            }
        }
    }
}

/// A cancel path's hold on the idle report of the turn running in a session.
///
/// The cancel reply (`Interrupted`, `Done`) goes straight to the cancelling
/// connection while the turn's own events travel its fanout queue, so a turn
/// that ends during the cancel would otherwise report idle before that reply.
/// While this is held the guard records the end but does not report it; on
/// drop, after the reply was queued, the idle state goes out on the turn's own
/// ordered sender, so it still follows every event of the turn.
#[must_use = "the idle state is held back only while this is alive"]
pub(crate) struct DeferredIdle {
    slot: Arc<Mutex<TurnSlot>>,
}

impl DeferredIdle {
    /// The cancel holding this report is stopping the turn: its idle state says
    /// `cancelled` however the turn ends. A cancel that returns early without
    /// stopping anything only releases the hold.
    pub(crate) fn mark_cancelled(&self) {
        lock_slot(&self.slot).cancelled = true;
    }
}

impl Drop for DeferredIdle {
    /// Sends the held idle state; a lock its turn's owner parked meanwhile is
    /// released once that idle state was forwarded.
    fn drop(&mut self) {
        let release = {
            let mut slot = lock_slot(&self.slot);
            slot.deferrals = slot.deferrals.saturating_sub(1);
            slot.send_idle_if_due()
        };
        if let Some(release) = release {
            release.run();
        }
    }
}

fn hold(slot: Arc<Mutex<TurnSlot>>) -> Option<DeferredIdle> {
    {
        let mut guard = lock_slot(&slot);
        if guard.idle_sent {
            return None;
        }
        guard.deferrals += 1;
    }
    Some(DeferredIdle { slot })
}

/// Hold the idle report of the turn running in `session_id` until the returned
/// hold drops. `None` when no turn is running or its idle state is already out.
pub(crate) fn defer_idle_for_cancel(session_id: &str) -> Option<DeferredIdle> {
    let slot = active().get(session_id).map(|turn| Arc::clone(&turn.slot))?;
    hold(slot)
}

/// One turn's idle report, reachable before that turn registers as running.
#[derive(Clone)]
pub(crate) struct IdleHandle {
    slot: Arc<Mutex<TurnSlot>>,
}

impl IdleHandle {
    /// [`defer_idle_for_cancel`] for this turn.
    pub(crate) fn defer(&self) -> Option<DeferredIdle> {
        hold(Arc::clone(&self.slot))
    }
}

/// Whether a turn currently holds `session_id` in the foreground. A caller that
/// is about to report the session idle must not, while this is true.
pub(crate) fn is_running(session_id: &str) -> bool {
    active().contains_key(session_id)
}

/// The run id the session is executing right now, or `None` when it is idle.
///
/// A targeted cancel compares its own `run_id` with this: equal means the turn
/// the caller saw is still the one running, so stopping it cannot hit a turn
/// that replaced it.
pub(crate) fn current_run_id(session_id: &str) -> Option<String> {
    let slot = active().get(session_id).map(|turn| Arc::clone(&turn.slot))?;
    let run_id = lock_slot(&slot).run_id.clone();
    run_id.filter(|run_id| !run_id.is_empty())
}

/// The current foreground state of `session_id`, for a v2 connection that just
/// attached. An idle session has no run, so its `run_id` is empty.
pub(crate) fn snapshot(session_id: &str) -> ServerEvent {
    let slot = active().get(session_id).map(|turn| Arc::clone(&turn.slot));
    let (running, run_id, start_message_id) = match slot {
        Some(slot) => {
            let slot = lock_slot(&slot);
            (
                true,
                slot.run_id.clone().unwrap_or_default(),
                slot.start_message_id.clone(),
            )
        }
        None => (false, String::new(), None),
    };
    let awaiting_user = running
        && (crate::tool::permission_bridge::awaiting_answer(session_id)
            || crate::tool::ask_bridge::awaiting_answer(session_id));
    ServerEvent::ForegroundState {
        session_id: session_id.to_string(),
        running,
        run_id,
        start_message_id,
        user_content: None,
        awaiting_user,
        stop_reason: None,
        snapshot: true,
    }
}

/// Settles the stop reason of the idle event a turn's fanout forwards.
///
/// Fed every event of one turn in order. The guard's hint says how the owning
/// task saw the end (`end_turn`, `error`, `cancelled`); a cancel always wins,
/// then the first cause the turn named, then a failure the turn reported, then
/// the hint.
#[derive(Default)]
pub(crate) struct StopObserver {
    stop: Option<&'static str>,
    failed: bool,
}

impl StopObserver {
    pub(crate) fn apply(&mut self, event: &mut ServerEvent) {
        match event {
            ServerEvent::Interrupted => self.record(STOP_CANCELLED),
            ServerEvent::TurnStopped {
                reason,
                provider_stop_reason,
                ..
            } => match reason {
                TurnStopReason::Interrupted => self.record(STOP_CANCELLED),
                TurnStopReason::ProviderGuardrail => self.record(STOP_REFUSAL),
                TurnStopReason::LimitReached => {
                    let output_limit = provider_stop_reason
                        .as_deref()
                        .is_some_and(|reason| is_stop_reason(reason, &OUTPUT_LIMIT_STOPS));
                    self.record(if output_limit {
                        STOP_MAX_TOKENS
                    } else {
                        STOP_MAX_TURN_REQUESTS
                    });
                }
                TurnStopReason::Failure | TurnStopReason::Crash | TurnStopReason::Unknown => self.failed = true,
            },
            ServerEvent::ProviderGuardrail {
                stop_reason: Some(stop_reason),
                ..
            } if is_stop_reason(stop_reason, &REFUSAL_STOPS) => self.record(STOP_REFUSAL),
            ServerEvent::ForegroundState {
                running: true,
                snapshot: false,
                ..
            } => *self = Self::default(),
            ServerEvent::ForegroundState {
                running: false,
                snapshot: false,
                stop_reason,
                ..
            } => {
                let hint = stop_reason.as_deref().unwrap_or(STOP_END_TURN);
                let settled = if hint == STOP_CANCELLED {
                    STOP_CANCELLED
                } else if let Some(stop) = self.stop {
                    stop
                } else if self.failed || hint == STOP_ERROR {
                    STOP_ERROR
                } else {
                    STOP_END_TURN
                };
                *stop_reason = Some(settled.to_string());
                *self = Self::default();
            }
            _ => {}
        }
    }

    fn record(&mut self, reason: &'static str) {
        if self.stop.is_none() || reason == STOP_CANCELLED {
            self.stop = Some(reason);
        }
    }
}

/// Which id-bearing behaviour a History reply uses.
///
/// Two independent questions: whether a row names the stored message it came
/// from, and whether the connection negotiated ACP v2, which the reply echoes
/// so a v2 adapter knows the daemon serves its protocol. A client that only
/// wants the ids (the TUI's bridge) sets `rows` alone and is never told it
/// speaks v2.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HistoryIds {
    /// Rows name the stored message they came from.
    pub(crate) rows: bool,
    /// The connection negotiated ACP v2.
    pub(crate) acp_v2: bool,
}

impl HistoryIds {
    /// The ids a connection with these two flags reads.
    pub(crate) fn for_connection(acp_v2: bool, history_message_ids: bool) -> Self {
        Self {
            rows: acp_v2 || history_message_ids,
            acp_v2,
        }
    }
}

/// One History row. The stored message id, and whether that message started a
/// turn, ride along only for a connection that asked for them
/// (`include_message_ids`); every other connection gets the row it always got.
/// A tool row also reports the verdict stored on its result, to every
/// connection, and only when one was stored.
///
/// The id comes from the row itself: the renderer copied it from the message it
/// was building the row from, so it names the exact original even when the row
/// indexes a full-history snapshot rather than the resident context.
pub(crate) fn history_message(
    msg: crate::session::RenderedMessage,
    stored: &[crate::session::StoredMessage],
    include_message_ids: bool,
) -> crate::protocol::HistoryMessage {
    let verdict = stored_verdict(&msg, stored);
    // The row carries its own id, so moving it out costs no copy, and a
    // connection that did not ask for ids allocates nothing here.
    let message_id = if include_message_ids { msg.stored_id } else { None };
    let turn_start = message_id
        .as_deref()
        .and_then(|id| stored.iter().find(|message| message.id == id))
        .is_some_and(|message| message.turn_start);
    crate::protocol::HistoryMessage {
        message_id,
        turn_start,
        response_stats: msg.response_stats,
        role: msg.role,
        content: msg.content,
        tool_calls: if msg.tool_calls.is_empty() {
            None
        } else {
            Some(msg.tool_calls)
        },
        tool_data: msg.tool_data,
        verdict,
    }
}

/// The verdict on the stored result block a tool row was rendered from.
fn stored_verdict(
    msg: &crate::session::RenderedMessage,
    stored: &[crate::session::StoredMessage],
) -> Option<crate::tool::ToolVerdict> {
    let call = msg.tool_data.as_ref().filter(|_| msg.role == "tool")?;
    let blocks = &stored.get(msg.stored_index?)?.content;
    blocks.iter().find_map(|block| match block {
        crate::message::ContentBlock::ToolResult {
            tool_use_id, verdict, ..
        } if *tool_use_id == call.id => *verdict,
        _ => None,
    })
}

pub(crate) fn is_v2_only(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::ForegroundState { .. } | ServerEvent::UserMessageInserted { .. } | ServerEvent::ToolExit { .. }
    )
}

#[cfg(test)]
#[path = "foreground_state_tests.rs"]
mod tests;

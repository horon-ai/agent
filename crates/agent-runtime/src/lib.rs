use std::sync::{Arc, LockResult, Mutex, MutexGuard, PoisonError};

mod turn_state;
pub use turn_state::{
    RunIdentity, RunLedger, RunStop, StopDisposition, StopProvenance, TurnOutcome, TurnState, TurnStateError,
    begin_run, conversation_saved, current_run, current_stop, note_conversation_save, observe_cause, observed_cause,
    outcome_for_stop_reason, provider_failure_provenance, record_stop, stop_history, with_run_ledger,
};

#[cfg(test)]
#[path = "turn_state_tests.rs"]
mod turn_state_tests;

#[cfg(test)]
#[path = "queue_tests.rs"]
mod queue_tests;

pub mod goal_continuation;
pub mod turn_recovery;

/// A soft interrupt message queued for injection at the next safe point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftInterruptMessage {
    pub content: String,
    pub images: Vec<(String, String)>,
    /// If true, can skip remaining tools when injected at point C.
    pub urgent: bool,
    pub source: SoftInterruptSource,
    /// Opaque caller-supplied id. Echoed in the injection event and used to
    /// cancel this one message while it is still queued.
    pub client_id: Option<String>,
    /// Stable identity this message carries in the durable runtime journal.
    ///
    /// The id is minted once, at acceptance, and then travels with the message:
    /// every later transition (`started`, `committed`, `cancelled`, `returned`,
    /// `in_doubt`) names this id instead of repeating the content, and a restart
    /// recognizes an already-accepted message by it, so restoring twice cannot
    /// queue the same input twice.
    ///
    /// It is deliberately separate from [`client_id`](Self::client_id): the
    /// caller's id is theirs to choose, replay, and cancel, while this one is
    /// the journal's own handle and is never reused for a different input.
    pub input_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftInterruptSource {
    User,
    System,
    BackgroundTask,
}

/// Thread-safe soft interrupt queue that can be accessed without holding the agent lock.
///
/// One mutex owns both collections, so a message is never in two places at
/// once and a transition between them is never observable half-done:
///
/// * `pending` holds messages that have not reached the transcript yet. It is
///   the queue's public face: [`lock`](Self::lock) hands out a guard over it so
///   existing callers keep their `Vec` operations.
/// * `in_flight` holds messages that were injected into this turn's transcript
///   but whose turn has not finished. A successful turn commits them away; a
///   failed, cancelled, or panicking turn restores them, so a crash between
///   the disk overwrite and the memory commit cannot lose the message.
///
/// The type is a handle, not the state: cloning it shares the same queue.
#[derive(Clone, Debug, Default)]
pub struct SoftInterruptQueue {
    inner: Arc<Mutex<QueueInner>>,
}

#[derive(Debug, Default)]
struct QueueInner {
    pending: Vec<SoftInterruptMessage>,
    in_flight: Vec<SoftInterruptMessage>,
}

/// Guard over the still-queued (not yet injected) messages.
///
/// Dereferences to the pending vector, so `push`, `drain`, `retain`, `iter`
/// and the rest keep working unchanged. The in-flight collection is reachable
/// only through [`SoftInterruptQueue`]'s own methods, which take the lock once
/// and update both collections together.
#[derive(Debug)]
pub struct QueueGuard<'a> {
    inner: MutexGuard<'a, QueueInner>,
}

impl std::ops::Deref for QueueGuard<'_> {
    type Target = Vec<SoftInterruptMessage>;

    fn deref(&self) -> &Self::Target {
        &self.inner.pending
    }
}

impl std::ops::DerefMut for QueueGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner.pending
    }
}

impl SoftInterruptQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(QueueInner::default())),
        }
    }

    /// Lock the queue and operate on the pending messages.
    ///
    /// A poisoned lock is still returned as an error, matching what callers
    /// have always done, but the guard carries the recovered state so a caller
    /// that chooses to proceed can.
    pub fn lock(&self) -> LockResult<QueueGuard<'_>> {
        match self.inner.lock() {
            Ok(inner) => Ok(QueueGuard { inner }),
            Err(poisoned) => Err(PoisonError::new(QueueGuard {
                inner: poisoned.into_inner(),
            })),
        }
    }

    fn lock_inner(&self) -> MutexGuard<'_, QueueInner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Move every pending message to `in_flight`, returning them in queue order.
    ///
    /// Both vectors change under the same lock, so no observer can see a
    /// message in neither collection.
    pub fn transfer_to_in_flight(&self) -> Vec<SoftInterruptMessage> {
        let mut inner = self.lock_inner();
        let moved = std::mem::take(&mut inner.pending);
        inner.in_flight.extend(moved.iter().cloned());
        moved
    }

    /// Drop the in-flight messages: the turn that injected them finished.
    ///
    /// Returns how many were cleared. Memory only - the caller clears the disk
    /// copy first and calls this only after that write succeeded.
    pub fn commit(&self) -> usize {
        let mut inner = self.lock_inner();
        let cleared = inner.in_flight.len();
        inner.in_flight.clear();
        cleared
    }

    /// Put the in-flight messages back in front of the pending queue.
    ///
    /// Returns how many were restored. Memory only: the disk copy is left as
    /// it is, so a retry or a restart can inject them again exactly once.
    pub fn restore(&self) -> usize {
        let mut inner = self.lock_inner();
        if inner.in_flight.is_empty() {
            return 0;
        }
        let restored = std::mem::take(&mut inner.in_flight);
        let count = restored.len();
        let pending = std::mem::take(&mut inner.pending);
        inner.pending.extend(restored);
        inner.pending.extend(pending);
        count
    }

    /// Snapshot of the pending messages, for the disk store.
    pub fn pending_snapshot(&self) -> Vec<SoftInterruptMessage> {
        self.lock_inner().pending.clone()
    }

    /// Snapshot of the messages this turn injected but has not settled yet.
    ///
    /// The durable layer reads their ids here so a commit or a restore names
    /// exactly the inputs this turn carried.
    pub fn in_flight_snapshot(&self) -> Vec<SoftInterruptMessage> {
        self.lock_inner().in_flight.clone()
    }

    /// Drop one pending message by its journal id, returning whether it was there.
    pub fn remove_pending_by_input_id(&self, input_id: &str) -> bool {
        let mut inner = self.lock_inner();
        let before = inner.pending.len();
        inner
            .pending
            .retain(|message| message.input_id.as_deref() != Some(input_id));
        before != inner.pending.len()
    }

    pub fn pending_len(&self) -> usize {
        self.lock_inner().pending.len()
    }

    pub fn in_flight_len(&self) -> usize {
        self.lock_inner().in_flight.len()
    }

    /// Messages the queue is holding in memory: pending plus in-flight, since a
    /// message injected into a turn that has not finished is still retained.
    pub fn depth(&self) -> usize {
        let inner = self.lock_inner();
        inner.pending.len() + inner.in_flight.len()
    }

    /// Payload bytes the queue is holding: message text plus attached image data.
    pub fn buffered_bytes(&self) -> usize {
        let inner = self.lock_inner();
        inner
            .pending
            .iter()
            .chain(inner.in_flight.iter())
            .map(|message| message.content.len() + message.images.iter().map(|(_, data)| data.len()).sum::<usize>())
            .sum()
    }

    /// Record depth and buffered bytes into the process-wide counters.
    ///
    /// Called after every mutation that can grow the queue, so the high-water
    /// marks describe the deepest the queue ever got.
    pub fn observe(&self) {
        if !horon_agent_perf_counters::enabled() {
            return;
        }
        horon_agent_perf_counters::observe_queue(self.depth() as u64, self.buffered_bytes() as u64);
    }

    /// True when `other` is a clone of this queue (shares the same state).
    /// Used where a caller must prove it holds a *different* queue, such as a
    /// conversation reset replacing the session's queue.
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

/// Signal to move the currently executing tool to background.
/// Uses std::sync so it can be set without async from outside the agent lock.
pub type BackgroundToolSignal = Arc<std::sync::atomic::AtomicBool>;

/// Signal to gracefully stop generation.
pub type GracefulShutdownSignal = Arc<std::sync::atomic::AtomicBool>;

/// Async-aware interrupt signal that combines AtomicBool (sync read) with
/// tokio::Notify (async wake). Eliminates spin-loops during tool execution.
#[derive(Clone)]
pub struct InterruptSignal {
    flag: Arc<std::sync::atomic::AtomicBool>,
    /// Monotonic fire counter. Lets owners of a timed/deferred reset detect
    /// that a *newer* fire landed in the meantime and skip the reset instead
    /// of erasing a cancel the target has not observed yet (issue #428).
    epoch: Arc<std::sync::atomic::AtomicU64>,
    notify: Arc<tokio::sync::Notify>,
}

impl InterruptSignal {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn fire(&self) {
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_set(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn reset(&self) {
        self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Current fire epoch. Capture this right after a [`fire`](Self::fire) to
    /// later reset only that specific fire via
    /// [`reset_if_epoch`](Self::reset_if_epoch).
    pub fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Reset the signal only if no newer [`fire`](Self::fire) happened since
    /// `epoch` was captured. Returns `true` when the reset was applied.
    ///
    /// If a racing fire lands between the epoch check and the reset, the
    /// fire is restored (flag re-set and waiters re-notified) so no cancel
    /// is ever silently erased.
    pub fn reset_if_epoch(&self, epoch: u64) -> bool {
        if self.epoch.load(std::sync::atomic::Ordering::SeqCst) != epoch {
            return false;
        }
        self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
        if self.epoch.load(std::sync::atomic::Ordering::SeqCst) != epoch {
            // A newer fire raced with the reset; restore it.
            self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
            self.notify.notify_waiters();
            return false;
        }
        true
    }

    pub async fn notified(&self) {
        let mut notified = std::pin::pin!(self.notify.notified());
        // Explicitly register this waiter with the Notify before checking the
        // flag. `notify_waiters()` (used by `fire()`) wakes only registered
        // waiters; current tokio registers a `notified()` future at creation,
        // but `enable()` makes the registration explicit rather than relying
        // on that version-specific guarantee, since a lost wakeup here parks
        // the cancel path (agent stream loop, tool-wait select) until an
        // unrelated event arrives (issue #428).
        notified.as_mut().enable();
        if self.is_set() {
            return;
        }
        notified.await;
    }

    pub fn as_atomic(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.flag)
    }

    /// True when `other` is a clone of this signal (shares the same state).
    /// Used by cancel fan-out to avoid double-firing the same signal and by
    /// diagnostics that need to detect stale signal instances (issue #428).
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.flag, &other.flag)
    }
}

impl Default for InterruptSignal {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StreamError {
    pub message: String,
    pub retry_after_secs: Option<u64>,
}

impl StreamError {
    pub fn new(message: String, retry_after_secs: Option<u64>) -> Self {
        Self {
            message,
            retry_after_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Documents the tokio semantics `InterruptSignal::notified()` relies on:
    /// current tokio guarantees a `notified()` future receives wakeups from
    /// `notify_waiters()` from the moment it is *created*, even before its
    /// first poll. The explicit `enable()` in `notified()` makes that
    /// registration explicit instead of relying on the version-specific
    /// creation-time guarantee (hardening for issue #428).
    #[tokio::test]
    async fn notified_future_receives_notify_waiters_from_creation() {
        let notify = tokio::sync::Notify::new();

        // Created before the notification, not yet polled: must be woken.
        let created_before = notify.notified();
        notify.notify_waiters();
        tokio::time::timeout(Duration::from_millis(100), created_before)
            .await
            .expect("a notified() future created before notify_waiters() must be woken");

        // Created after the notification: must NOT be woken (notify_waiters
        // stores no permit). This is why fire() also sets the atomic flag.
        let created_after = notify.notified();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), created_after)
                .await
                .is_err(),
            "notify_waiters() must not store a permit for future waiters"
        );
    }

    /// Probabilistic race hammer for issue #428: `fire()` must never be lost
    /// regardless of where the waiter is between creating the `notified()`
    /// future and its first poll. The agent stream loop recreates this future
    /// per stream event, so under fast token streams the pre-fix race made
    /// Esc/Ctrl+C cancels appear to be ignored.
    #[test]
    fn fire_never_loses_wakeup_while_notified_races() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("runtime");
        rt.block_on(async {
            for i in 0..2000 {
                let signal = InterruptSignal::new();
                let waiter = {
                    let signal = signal.clone();
                    tokio::spawn(async move { signal.notified().await })
                };
                // Fire concurrently: the waiter may be anywhere between
                // future creation and first poll.
                signal.fire();
                tokio::time::timeout(Duration::from_secs(2), waiter)
                    .await
                    .unwrap_or_else(|_| panic!("lost wakeup on iteration {i}: notified() missed fire() (issue #428)"))
                    .expect("waiter task must not panic");
            }
        });
    }

    /// A fire() that happened before notified() is observed immediately.
    #[tokio::test]
    async fn notified_returns_immediately_when_already_fired() {
        let signal = InterruptSignal::new();
        signal.fire();
        tokio::time::timeout(Duration::from_millis(100), signal.notified())
            .await
            .expect("pre-fired signal must resolve notified() immediately");
    }

    /// reset() clears the flag so subsequent notified() calls wait again.
    #[tokio::test]
    async fn reset_clears_fired_state() {
        let signal = InterruptSignal::new();
        signal.fire();
        assert!(signal.is_set());
        signal.reset();
        assert!(!signal.is_set());
        let waited = tokio::time::timeout(Duration::from_millis(50), signal.notified()).await;
        assert!(waited.is_err(), "reset signal must park notified() again");
    }

    /// reset_if_epoch() clears the flag only for the fire that captured the
    /// epoch. A deferred reset (e.g. the server's 500ms timer for detached
    /// turns) must not erase a newer cancel fired in the meantime, otherwise
    /// rapid repeated Esc presses cancel each other out (issue #428).
    #[test]
    fn reset_if_epoch_skips_when_newer_fire_landed() {
        let signal = InterruptSignal::new();
        signal.fire();
        let first_epoch = signal.epoch();

        // A second cancel (repeated Esc) fires before the deferred reset runs.
        signal.fire();
        assert!(
            !signal.reset_if_epoch(first_epoch),
            "stale deferred reset must be skipped"
        );
        assert!(signal.is_set(), "newer cancel must survive the stale deferred reset");

        // The reset scheduled for the latest fire still works.
        let second_epoch = signal.epoch();
        assert!(signal.reset_if_epoch(second_epoch));
        assert!(!signal.is_set());

        // And a reset for an already-consumed epoch stays a no-op.
        assert!(!signal.reset_if_epoch(first_epoch));
    }

    /// A fire() racing the flag-clear inside reset_if_epoch() is restored
    /// rather than silently erased.
    #[test]
    fn reset_if_epoch_never_erases_concurrent_fire() {
        for _ in 0..2000 {
            let signal = InterruptSignal::new();
            signal.fire();
            let epoch = signal.epoch();

            let firer = {
                let signal = signal.clone();
                std::thread::spawn(move || signal.fire())
            };
            let _ = signal.reset_if_epoch(epoch);
            firer.join().expect("firer thread");

            assert!(
                signal.is_set(),
                "a concurrent fire() must never be erased by reset_if_epoch()"
            );
        }
    }
}

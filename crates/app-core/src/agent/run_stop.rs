//! The run rows that bound one turn, and the close path that settles them.
//!
//! A turn writes `run.started` before it executes and `run.stopped` after it has
//! saved the conversation, so a replay can name what ran and why it ended. Both
//! rows are written on a dedicated OS thread and joined synchronously at the
//! turn's tail: the tail must not yield, and the write itself blocks on an fsync.
//!
//! # Deferred stop join
//!
//! `horon-agent run` makes the run stop and the close binding its last two
//! durable writes. [`Agent::arm_deferred_run_stop_join`] lets the stop row start
//! on its thread and postpones only its join: [`Agent::settle_run_stop`] joins it
//! after the close binding is on disk and before the `session_end` hook, so the
//! binding write and the journal write overlap instead of running one after the
//! other.
//!
//! The ledger record is written after the join, so a `record_stop` reader never
//! sees a stop whose row is still in flight, and a journal write the store
//! refuses leaves `persisted == false` without touching the turn's result. A
//! turn that never reaches the close path is covered by the CLI's drop guard.
//!
//! Every other caller — the daemon, ambient turns, swarm workers — leaves the
//! flag off and keeps the immediate join.

use super::Agent;
use horon_agent_agent_runtime::{
    RunIdentity, RunStop, StopDisposition, StopProvenance, conversation_saved, record_stop,
};
use horon_agent_base::durable_runtime::{
    DurableRuntimeStore, Payload, RunStarted, RunStopped, StopCause as JournalStopCause,
};

/// The session journals this process already replayed.
///
/// Reopening a journal replays it, and a run boundary is once per turn, so
/// reopening per turn would turn every turn into a full journal replay. The
/// store is kept instead and reused: it already catches up on rows other writers
/// appended before every append, so a cached handle never serves stale state.
static RUN_JOURNALS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, DurableRuntimeStore>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Dispatch one run row to `session_id`'s journal on a dedicated OS thread.
///
/// The handle is returned rather than awaited, because this entry point must
/// reach its turn and publish its terminal event without yielding: an await in
/// the turn's tail lets the caller's own timers run before that event lands.
/// The write still starts before the turn does and is joined before the stop is
/// recorded, so the journal keeps start-before-stop order, and the join blocks a
/// few milliseconds instead of yielding the worker.
fn dispatch_run_row(session_id: &str, run_id: &str, payload: Payload) -> std::thread::JoinHandle<bool> {
    let session_id = session_id.to_string();
    let run_id = run_id.to_string();
    std::thread::spawn(move || append_run_row(&session_id, &run_id, payload))
}

/// `run.started` writes dispatched but not yet joined, keyed by session.
///
/// The handle is held here rather than on `Agent` because the entry point must
/// reach its turn without yielding first, and this registry lets the write start
/// before the turn while its join waits until the turn ends.
static PENDING_RUN_STARTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::thread::JoinHandle<bool>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// `run.stopped` writes a deferred session holds until its close path.
///
/// Only a session armed through [`RunStopSettler`] ever has an entry here, and
/// it holds at most one: a new run settles the previous stop before it starts.
static PENDING_RUN_STOPS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, DeferredRunStop>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// One dispatched stop row with the stop its write describes.
struct DeferredRunStop {
    /// Captured when the turn ended, so `at` is the turn's own end and not the
    /// later moment the close path happened to join the write.
    stop: RunStop,
    handle: std::thread::JoinHandle<bool>,
}

/// Append one run row to `session_id`'s journal, on the calling (blocking) thread.
fn append_run_row(session_id: &str, run_id: &str, payload: Payload) -> bool {
    let Ok(root) = horon_agent_base::storage::horon_agent_dir() else {
        crate::logging::warn(&format!("run row not recorded for {session_id}: no agent home"));
        return false;
    };
    let key = root
        .join(horon_agent_base::durable_runtime::JOURNAL_DIR_NAME)
        .join(session_id);
    let mut journals = RUN_JOURNALS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let store = match journals.entry(key.clone()) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => match DurableRuntimeStore::open(session_id) {
            Ok(store) => entry.insert(store),
            Err(error) => {
                crate::logging::warn(&format!("run row not recorded for {session_id}: {error}"));
                return false;
            }
        },
    };
    match store.append(run_id, payload) {
        Ok(_) => true,
        Err(error) => {
            crate::logging::warn(&format!("run row not recorded for {session_id}: {error}"));
            // A corrupt or gapped journal must not be retried as transient, so
            // the handle is dropped and the next attempt replays it afresh.
            journals.remove(&key);
            false
        }
    }
}

/// The journal's spelling of a stop cause.
fn journal_cause(provenance: StopProvenance) -> JournalStopCause {
    match provenance {
        StopProvenance::Completed => JournalStopCause::Completed,
        StopProvenance::UserCancelled => JournalStopCause::UserCancelled,
        StopProvenance::Shutdown => JournalStopCause::Shutdown,
        StopProvenance::ProviderError => JournalStopCause::ProviderError,
        StopProvenance::Timeout => JournalStopCause::Timeout,
        StopProvenance::LoopGuard => JournalStopCause::LoopGuard,
        StopProvenance::Limit => JournalStopCause::Limit,
        StopProvenance::InterruptedUnknown => JournalStopCause::InterruptedUnknown,
    }
}

/// Record one finished run's stop with the journal's own answer.
///
/// The one place a stop reaches the run ledger: the immediate path calls it at
/// the turn's tail, the deferred path at settle, so both record the same fields
/// and only the moment differs.
fn record_finished_stop(session_id: &str, mut stop: RunStop, persisted: bool) -> StopDisposition {
    if persisted {
        stop.mark_persisted();
    }
    let run_id = stop.run_id().to_string();
    let cause = stop.provenance().as_str();
    let conversation_saved = conversation_saved(session_id);
    let disposition = record_stop(session_id, stop);
    crate::logging::info(&format!(
        "run.stopped session={session_id} run={run_id} cause={cause} persisted={persisted} conversation_saved={conversation_saved:?} disposition={disposition:?}"
    ));
    disposition
}

/// Hold one `run.stopped` write for its session until the close path joins it.
fn defer_run_stop(session_id: &str, stop: RunStop, handle: std::thread::JoinHandle<bool>) {
    // A session holds one deferred stop, so anything still pending belongs to an
    // older run and has to settle first: its row precedes the next start row.
    settle_deferred_run_stop(session_id);
    PENDING_RUN_STOPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(session_id.to_string(), DeferredRunStop { stop, handle });
}

/// Join a session's deferred `run.stopped` write and record the stop it carried.
///
/// Idempotent: a session with nothing pending is left alone, so the close path
/// and the CLI's drop guard can both reach it in any order. Blocking on purpose —
/// both callers are at the process's own end, where a yield would delay the join
/// rather than overlap it with anything.
pub(crate) fn settle_deferred_run_stop(session_id: &str) {
    let pending = PENDING_RUN_STOPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
    let Some(pending) = pending else {
        return;
    };
    let persisted = pending.handle.join().unwrap_or(false);
    record_finished_stop(session_id, pending.stop, persisted);
}

/// Settles a session's deferred run stop when the CLI's `run` returns.
///
/// `horon-agent run` has return paths that never reach the close path — an error
/// propagated with `?`, an early return, an unwind — and those must still leave
/// the stop row on disk. [`Agent::mark_closed`] settles as well; both calls are
/// idempotent, so whichever runs first joins the write and the other is a no-op.
pub struct RunStopSettler {
    session_id: String,
}

impl RunStopSettler {
    fn new(session_id: String) -> Self {
        Self { session_id }
    }
}

impl Drop for RunStopSettler {
    fn drop(&mut self) {
        settle_deferred_run_stop(&self.session_id);
    }
}

impl Agent {
    /// Start the run this turn executes, recording `run.started` when it begins.
    ///
    /// The journal write is dispatched to a blocking thread and never performed
    /// while an async lock is held. A failed write leaves the run unrecorded and
    /// the turn still runs: an audit row is worth less than the answer the user
    /// asked for.
    pub(super) fn begin_run(&mut self) -> RunIdentity {
        // A stop the previous run deferred is joined before this run's start row
        // is dispatched, so the journal still reads start-before-stop per run.
        self.settle_run_stop();
        let identity = RunIdentity::new(Self::mint_run_id());
        // Dispatched, not awaited: the streaming entry point must reach its turn
        // without yielding first. The write still starts before the turn runs and
        // is joined before the stop is recorded, so start precedes stop on disk.
        let handle = dispatch_run_row(
            &self.session.id,
            identity.run_id(),
            Payload::RunStarted(RunStarted { input_id: None }),
        );
        PENDING_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(self.session.id.clone(), handle);
        identity
    }

    /// Await this turn's `run.started` write and adopt its result.
    ///
    /// The join is synchronous on purpose: this runs at the turn's end, where a
    /// yield would let a caller's timers fire before the terminal event is
    /// published. It blocks for the few milliseconds the write takes.
    pub(super) async fn settle_run_start(&mut self, identity: &RunIdentity) {
        let handle = PENDING_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.session.id);
        let Some(handle) = handle else {
            return;
        };
        let persisted = handle.join().unwrap_or(false);
        horon_agent_agent_runtime::begin_run(&self.session.id, identity.clone(), persisted);
    }

    /// End the run captured at turn start, after the conversation save.
    ///
    /// The caller passes the run id and cause captured when the turn ended, so
    /// this writes what happened rather than what is current now. The in-memory
    /// stop is recorded and kept even when the journal write fails; only a
    /// successful write marks it persisted.
    ///
    /// Returns where the stop landed, so a caller can tell a late stop recorded
    /// as history from the current run's own end. A session that armed deferred
    /// joining returns [`StopDisposition::Deferred`] instead: its row is already
    /// being written, and the ledger record follows at
    /// [`Agent::settle_run_stop`].
    pub(super) async fn finish_run(&self, run_id: &str, provenance: StopProvenance) -> StopDisposition {
        let session_id = self.session.id.clone();
        let stop = RunStop::capture(run_id, provenance);
        // Dispatched here, after the turn's conversation save, so the stop row
        // never precedes the transcript it describes.
        let handle = dispatch_run_row(
            &self.session.id,
            run_id,
            Payload::RunStopped(RunStopped {
                cause: journal_cause(provenance),
                input_id: None,
            }),
        );
        if self.defer_run_stop_join {
            defer_run_stop(&session_id, stop, handle);
            return StopDisposition::Deferred;
        }
        // Synchronous join, for the same reason as the start: the turn's tail
        // must not yield, and the write itself already runs off this thread.
        let persisted = handle.join().unwrap_or(false);
        record_finished_stop(&session_id, stop, persisted)
    }

    /// Hand this session's run-stop join to the close path.
    ///
    /// Only `horon-agent run` arms this: its process ends right after the turn, so
    /// the stop row and the close binding can overlap. The returned guard settles
    /// the stop on paths that never reach the close, and every long-lived caller
    /// keeps the immediate join.
    pub fn arm_deferred_run_stop_join(&mut self) -> RunStopSettler {
        self.defer_run_stop_join = true;
        RunStopSettler::new(self.session.id.clone())
    }

    /// Join this session's deferred run stop, if any.
    pub(super) fn settle_run_stop(&self) {
        settle_deferred_run_stop(&self.session.id);
    }
}

impl Agent {
    /// Mark this agent session as closed and persist it.
    ///
    /// A deferred `run.stopped` is joined between the binding write and the
    /// `session_end` hook: the two writes overlap, and an observer still sees a
    /// run that has settled.
    pub fn mark_closed(&mut self) {
        // Closing releases ownership and persists the whole session; it is
        // never the resume commit, so it must not announce `session_start`.
        let persisted = !self.session.active_context().is_empty();
        self.close_session_binding();
        if persisted {
            self.persist_session_best_effort("session close state");
        }
        self.settle_run_stop();
        self.fire_session_lifecycle_hook("session_end", "close");
    }
}

#[cfg(test)]
#[path = "run_stop_overlap_tests.rs"]
mod run_stop_overlap_tests;

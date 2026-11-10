//! Run rows: the journal's record of a turn's start and end.
//!
//! A row states the session binding as it stood when the row was written, so a
//! restart reads the execution it belonged to from the journal even when the
//! mirror never reached the disk. The session takes that binding back through
//! `note_journal_binding`, which lets the next publish of the same value land by
//! rename instead of a flush.

use super::*;
use horon_agent_agent_runtime::{
    RunIdentity, RunStop, StopDisposition, StopProvenance, begin_run as note_run_start, conversation_saved, record_stop,
};
use horon_agent_base::durable_runtime::{
    Binding, DurableRuntimeStore, Payload, RunStarted, RunStopped, StopCause as JournalStopCause,
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

/// A `run.started` write still running, and the binding it confirms if it lands.
struct PendingStart {
    handle: std::thread::JoinHandle<bool>,
    binding: Option<Binding>,
}

/// `run.started` writes dispatched but not yet joined, keyed by session.
///
/// The handle is held here rather than on `Agent` because the entry point must
/// reach its turn without yielding first, and this registry lets the write start
/// before the turn while its join waits until the turn ends.
static PENDING_RUN_STARTS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, PendingStart>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

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

/// Append one run row to `session_id`'s journal, on the calling (blocking) thread.
fn append_run_row(session_id: &str, run_id: &str, payload: Payload) -> bool {
    let Ok(root) = horon_agent_base::storage::horon_agent_dir() else {
        logging::warn(&format!("run row not recorded for {session_id}: no agent home"));
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
                logging::warn(&format!("run row not recorded for {session_id}: {error}"));
                return false;
            }
        },
    };
    match store.append(run_id, payload) {
        Ok(_) => true,
        Err(error) => {
            logging::warn(&format!("run row not recorded for {session_id}: {error}"));
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

impl Agent {
    /// Start the run this turn executes, recording `run.started` when it begins.
    ///
    /// The row states the session binding as it stands at dispatch, numbered at
    /// the version after the newest one either copy holds. `settle_run_start`
    /// hands that binding back to the session once the row lands, so the next
    /// publish of the same value is a rename rather than a flush.
    ///
    /// A binding that cannot be read leaves the row without one, and the session
    /// stays on the flushing path: an audit row is worth less than the turn.
    pub(super) fn begin_run(&mut self) -> RunIdentity {
        let identity = RunIdentity::new(Self::mint_run_id());
        let binding = match self.session.next_journal_binding() {
            Ok(binding) => Some(binding),
            Err(error) => {
                logging::warn(&format!("run.started without a session binding: {error}"));
                None
            }
        };
        let payload = Payload::RunStarted(RunStarted::with_binding(None, binding.clone()));
        // Dispatched, not awaited: the streaming entry point must reach its turn
        // without yielding first. The write still starts before the turn runs and
        // is joined before the stop is recorded, so start precedes stop on disk.
        let handle = dispatch_run_row(&self.session.id, identity.run_id(), payload);
        PENDING_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(self.session.id.clone(), PendingStart { handle, binding });
        identity
    }

    /// Await this turn's `run.started` write and adopt its result.
    ///
    /// The join is synchronous on purpose: this runs at the turn's end, where a
    /// yield would let a caller's timers fire before the terminal event is
    /// published. It blocks for the few milliseconds the write takes.
    ///
    /// Only a write that landed confirms its binding. A failed one leaves the
    /// session on the flushing path, because no journal row answers the value.
    pub(super) async fn settle_run_start(&mut self, identity: &RunIdentity) {
        let pending = PENDING_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.session.id);
        let Some(PendingStart { handle, binding }) = pending else {
            return;
        };
        let persisted = handle.join().unwrap_or(false);
        note_run_start(&self.session.id, identity.clone(), persisted);
        if persisted && let Some(binding) = binding {
            self.session.note_journal_binding(binding);
        }
    }

    /// End the run captured at turn start, after the conversation save.
    ///
    /// The caller passes the run id and cause captured when the turn ended, so
    /// this writes what happened rather than what is current now. The in-memory
    /// stop is recorded and kept even when the journal write fails; only a
    /// successful write marks it persisted.
    ///
    /// A session this process closes as soon as the turn returns states the
    /// binding that close publishes, so the close itself lands by rename. A
    /// daemon session stays open, so its stop row states nothing and the close
    /// keeps its flush.
    ///
    /// Returns where the stop landed, so a caller can tell a late stop recorded
    /// as history from the current run's own end.
    pub(super) async fn finish_run(&mut self, run_id: &str, provenance: StopProvenance) -> StopDisposition {
        let mut stop = RunStop::capture(run_id, provenance);
        let session_id = self.session.id.clone();
        let cause = journal_cause(provenance);
        let binding = if self.closes_after_run {
            match self.session.closing_journal_binding() {
                Ok(binding) => Some(binding),
                Err(error) => {
                    logging::warn(&format!("run.stopped without a session binding: {error}"));
                    None
                }
            }
        } else {
            None
        };
        let payload = Payload::RunStopped(RunStopped::with_binding(cause, None, binding.clone()));
        let handle = dispatch_run_row(&session_id, run_id, payload);
        // Synchronous join, for the same reason as the start: the turn's tail
        // must not yield, and the write itself already runs off this thread.
        let persisted = handle.join().unwrap_or(false);
        if persisted {
            stop.mark_persisted();
            if let Some(binding) = binding {
                self.session.note_journal_binding(binding);
            }
        }
        let conversation_saved = conversation_saved(&session_id);
        let disposition = record_stop(&session_id, stop);
        logging::info(&format!(
            "run.stopped session={session_id} run={run_id} cause={} persisted={persisted} conversation_saved={conversation_saved:?} disposition={disposition:?}",
            provenance.as_str()
        ));
        disposition
    }
}

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

/// Starts this process began for a turn before its first commit, by session.
///
/// The turn adopts the identity here through `begin_run` instead of writing a
/// second start row, so one run keeps one start. A start whose row did not land
/// is adopted too: the turn still needs its identity, and the binding it would
/// have confirmed is simply not confirmed.
static PRESTARTED_RUN_STARTS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, RunIdentity>>> =
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

/// The hidden instruction a continuation carries.
///
/// Not user prose: the transcript already holds the prompt this continues, and a
/// visible second copy would read as the user asking twice. The reminder-only
/// turn prefixes it with `[auto] ` when it stores the message.
pub(crate) const CONTINUATION_INSTRUCTION: &str = "The previous model request failed with a transient error and \
this run was resumed automatically. Continue the task from where it left off. Do not repeat tool calls or edits that \
already completed, and do not ask the user what to do next.";

/// Whether `request` asks to continue a failed run.
///
/// Such a request is acknowledged by the turn that decides it — `ok` once a new
/// run exists, an error when the run may not be continued — so the connection's
/// generic ack must not claim it first.
pub(crate) fn is_continuation_request(request: &crate::protocol::Request) -> bool {
    matches!(
        request,
        crate::protocol::Request::Message {
            expected_run_id: Some(_),
            ..
        }
    )
}

/// How the connection that asked for a continuation learns its answer.
///
/// An ordinary message is acknowledged when the daemon receives it, because
/// receiving is all it promises. A continuation promises something narrower and
/// later: a new run exists for the run it named. So the acknowledgement is held
/// until [`Agent::continue_run`] has begun that run, and a refusal — which never
/// begins one — is answered by the rejection path instead.
pub(crate) struct ContinuationDecision {
    sender: mpsc::UnboundedSender<ServerEvent>,
    id: u64,
}

impl ContinuationDecision {
    pub(crate) fn new(sender: mpsc::UnboundedSender<ServerEvent>, id: u64) -> Self {
        Self { sender, id }
    }

    /// Tell the asking connection that its continuation began a run.
    fn accept(self) {
        // A closed channel means the connection went away; the turn it asked
        // for still runs, and nobody is left to answer.
        if self.sender.send(ServerEvent::Ack { id: self.id }).is_err() {
            logging::debug("Retry started after its requesting connection closed");
        }
    }
}

/// What one continuation request did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationOutcome {
    /// The run was continued; its turn ran to its own end.
    Continued,
    /// Nothing ran, and this is the sentence a client shows.
    Refused(String),
}

impl Agent {
    /// Continue the run `expected_run_id` if it may still be continued.
    ///
    /// The decision and the new run happen here, under the agent this caller
    /// already holds, so two clients cannot both act on the same failed run: the
    /// first begins a run, and the second finds a different current run and is
    /// refused. The turn itself is the ordinary reminder-only turn, whose
    /// message carries the system display role, which is what keeps the visible
    /// history at the one user prompt the failed turn already stored.
    pub(crate) async fn continue_run(
        &mut self,
        expected_run_id: &str,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
        report: Option<crate::server::foreground_state::TurnReport>,
        decision: ContinuationDecision,
    ) -> ContinuationOutcome {
        use horon_agent_agent_runtime::{ContinuationVerdict, continuation_verdict};
        let refusal = |reason: String| ContinuationOutcome::Refused(reason);
        match continuation_verdict(&self.session.id, expected_run_id) {
            ContinuationVerdict::Eligible => {}
            ContinuationVerdict::Stale { current_run_id } => {
                return refusal(match current_run_id {
                    Some(current) => format!(
                        "the run named is no longer current (`{current}` is), so that work has moved on; nothing was started"
                    ),
                    None => "the run named is not current in this session; nothing was started".to_string(),
                });
            }
            ContinuationVerdict::InFlight => {
                return refusal("that run has not stopped, so there is nothing to retry".to_string());
            }
            ContinuationVerdict::Ineligible { cause } => {
                return refusal(format!(
                    "that run ended as `{}`, which retrying would not change; nothing was started",
                    cause.as_str()
                ));
            }
        }
        // Beginning the run here, before the turn, is what closes the race: the
        // failed run stops being current now, so a second request naming it is
        // refused even while this turn is still starting.
        let identity = self.begin_run();
        note_run_start(&self.session.id, identity, false);
        // The request is answered only now that a new run exists, because that is
        // what the caller asked about. Sending it any earlier would confirm a
        // continuation whose run never began; the refusal paths above return
        // before this point and are answered with an error instead.
        decision.accept();
        let result = self
            .run_once_streaming_mpsc_with_display_role(
                "",
                Vec::new(),
                Some(CONTINUATION_INSTRUCTION.to_string()),
                event_tx,
                None,
                report,
            )
            .await;
        if let Err(error) = result {
            // The turn ran and failed again: that is the turn's own outcome,
            // reported through its events, not a refusal of the continuation.
            crate::logging::warn(&format!(
                "continuation of run {expected_run_id} in session {} failed: {error}",
                self.session.id
            ));
        }
        ContinuationOutcome::Continued
    }
}

#[cfg(test)]
#[path = "run_binding_tests.rs"]
mod run_binding_tests;

impl Agent {
    /// Mark this agent as one whose process closes the session when it exits.
    ///
    /// The `run` command has no later quit path, so it closes the session as
    /// soon as the turn returns. Saying so lets the run's stop row state the
    /// binding that close publishes, and the close then writes the mirror by
    /// rename instead of flushing a value no journal row answers.
    pub fn closing_after_run(mut self) -> Self {
        self.closes_after_run = true;
        self
    }

    /// Progress and exit state of the current (or most recent) turn.
    pub(crate) fn turn_state(&self) -> &TurnState {
        &self.turn_state
    }

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
        // A one-shot turn confirms its start before the first commit, so the
        // commit's binding publish lands by rename. Adopting that row here keeps
        // the run to one start instead of writing a second one.
        if let Some(run) = PRESTARTED_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.session.id)
        {
            return run;
        }
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
        self.confirm_run_start(identity);
    }

    /// Join this run's `run.started` write and adopt its result, on this thread.
    ///
    /// Returns whether the row landed. Only a landed row confirms its binding;
    /// a failed one leaves the session on the flushing path, because no journal
    /// row answers the value.
    fn confirm_run_start(&mut self, identity: &RunIdentity) -> bool {
        let pending = PENDING_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.session.id);
        let Some(PendingStart { handle, binding }) = pending else {
            return false;
        };
        let persisted = handle.join().unwrap_or(false);
        note_run_start(&self.session.id, identity.clone(), persisted);
        if persisted && let Some(binding) = binding {
            self.session.note_journal_binding(binding);
        }
        persisted
    }

    /// Save the turn's first commit, confirming the run's start before it.
    ///
    /// A process that closes the session after this run states the binding its
    /// first commit publishes in the `run.started` row before the commit writes
    /// it, so that publish lands by rename: a crash that loses the file still
    /// reads the row. The same turn adopts the start through [`Self::begin_run`],
    /// so the run keeps one start row. A row that does not land confirms
    /// nothing, and the commit takes the flushing path exactly as before.
    ///
    /// A failed commit closes a start that did land, so the journal is not left
    /// holding a live run for a turn that never began.
    pub(super) fn save_first_commit(&mut self) -> Result<()> {
        if !self.closes_after_run {
            return self.save_session();
        }
        let run = self.begin_run();
        let persisted = self.confirm_run_start(&run);
        let session_id = self.session.id.clone();
        PRESTARTED_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id.clone(), run.clone());
        match self.save_session() {
            Ok(()) => Ok(()),
            Err(error) => {
                PRESTARTED_RUN_STARTS
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&session_id);
                if persisted {
                    self.close_unrun_start(&run);
                }
                Err(error)
            }
        }
    }

    /// Close the start of a run whose first commit failed, so no live run is
    /// left behind for a turn that never began.
    fn close_unrun_start(&self, run: &RunIdentity) {
        let payload = Payload::RunStopped(RunStopped::with_binding(
            JournalStopCause::InterruptedUnknown,
            None,
            None,
        ));
        let persisted = dispatch_run_row(&self.session.id, run.run_id(), payload)
            .join()
            .unwrap_or(false);
        let mut stop = RunStop::capture(run.run_id(), StopProvenance::InterruptedUnknown);
        if persisted {
            stop.mark_persisted();
        }
        record_stop(&self.session.id, stop);
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

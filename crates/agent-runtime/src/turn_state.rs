//! Turn progress and exit state, owned outside the agent loop.
//!
//! Like `agent/tool_round.rs` in app-core, this module is deliberately free of
//! `Agent` state: it holds no session, emits no client event, writes nothing to
//! disk, and reports rather than swallows. Both turn loops create one
//! [`TurnState`] per turn and advance it through the recovery helpers, so the
//! same turn no longer has two different shapes and a surface without a TUI can
//! read what the loop is doing.
//!
//! # Asymmetric counters
//!
//! The blocking loop nudges the model toward the `batch` tool after several
//! one-call rounds; the streaming loop has no such nudge. Those two fields
//! ([`TurnState::sequential_single_tool_rounds`] and
//! [`TurnState::batch_nudge_pending`]) still live here so the two loops share
//! one type, and the streaming path simply never reads or writes them.
//!
//! # Automatic continuations
//!
//! [`TurnState::record_auto_continuation`] spends the bounded budget that
//! `turn_recovery` hands out, so a loop never keeps that count in a local of its
//! own: it starts at zero on every turn and stops at
//! [`MAX_AUTO_CONTINUATIONS`].
//!
//! # Outcomes
//!
//! A turn ends for one of four reasons, and [`TurnState::finish`] records
//! exactly one. [`TurnOutcome::Length`] comes from a stop reason that still
//! names truncation after every continuation ran out; [`TurnOutcome::Aborted`]
//! is a cancel or shutdown the loop observed; [`TurnOutcome::Error`] is a failed
//! turn, and it is the one outcome that cannot carry final text.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::SystemTime;

use crate::turn_recovery::MAX_AUTO_CONTINUATIONS;

/// Most late stops one turn keeps as history.
///
/// History exists for stops that arrived after their run stopped being current,
/// so it is bounded rather than growing for the life of the process. 64 records
/// is far more than one agent can legitimately produce per turn.
pub const MAX_RUN_STOP_HISTORY: usize = 64;

/// Identity of one externally triggered run.
///
/// A run starts when new external input begins executing and ends when the turn
/// that executed it finishes. Provider retries, failover switches, and automatic
/// continuations stay inside that run, so they keep its id: only a new external
/// input that actually begins executing issues a new one.
///
/// The value is I/O-free on purpose. The caller mints the UUID v4, because this
/// crate owns no id generator and no journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIdentity {
    run_id: String,
}

impl RunIdentity {
    /// Wrap an already minted run id.
    pub fn new(run_id: impl Into<String>) -> Self {
        Self { run_id: run_id.into() }
    }

    /// The run id, unique per externally triggered run.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

/// Why a run stopped, limited to causes a caller actually observed.
///
/// The set is closed and spelled exactly as the durable journal spells it. A
/// caller that did not observe a cause records none: a run left without a stop
/// is reported as [`StopProvenance::InterruptedUnknown`] by recovery, never as
/// a guess and never as completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopProvenance {
    /// The turn reached its normal end.
    Completed,
    /// A cancel was observed before the turn finished.
    UserCancelled,
    /// A process reload or shutdown was observed.
    Shutdown,
    /// A provider request failed.
    ProviderError,
    /// A provider request failed with a timeout.
    Timeout,
    /// The loop guard ended the turn.
    LoopGuard,
    /// A token, context, or plan limit ended the turn.
    Limit,
    /// A run was found started with no stop: the process died or was killed.
    InterruptedUnknown,
}

impl StopProvenance {
    /// Every cause, in the journal's spelling order.
    pub const ALL: [StopProvenance; 8] = [
        StopProvenance::Completed,
        StopProvenance::UserCancelled,
        StopProvenance::Shutdown,
        StopProvenance::ProviderError,
        StopProvenance::Timeout,
        StopProvenance::LoopGuard,
        StopProvenance::Limit,
        StopProvenance::InterruptedUnknown,
    ];

    /// The stable label, identical to the journal's `snake_case` spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::UserCancelled => "user_cancelled",
            Self::Shutdown => "shutdown",
            Self::ProviderError => "provider_error",
            Self::Timeout => "timeout",
            Self::LoopGuard => "loop_guard",
            Self::Limit => "limit",
            Self::InterruptedUnknown => "interrupted_unknown",
        }
    }

    /// Parse a label written by [`StopProvenance::as_str`].
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cause| cause.as_str() == raw)
    }
}

/// The cause a provider failure names.
///
/// A failure whose own message names a timeout is a timeout the transport
/// reported; every other failure is the provider error the loop received. This
/// reads the message it was handed and never guesses from its absence.
pub fn provider_failure_provenance(message: &str) -> StopProvenance {
    let lower = message.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        StopProvenance::Timeout
    } else {
        StopProvenance::ProviderError
    }
}

/// The stop one run recorded, with the journal's answer for it.
///
/// `persisted` is the write's own outcome, never an assumption: a stop whose
/// journal write failed stays in memory with `persisted == false`, so a reader
/// can tell a durable record from one that exists only in this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStop {
    run_id: String,
    provenance: StopProvenance,
    at: SystemTime,
    persisted: bool,
}

impl RunStop {
    /// Capture a stop for `run_id`, before its journal write is attempted.
    pub fn capture(run_id: impl Into<String>, provenance: StopProvenance) -> Self {
        Self {
            run_id: run_id.into(),
            provenance,
            at: SystemTime::now(),
            persisted: false,
        }
    }

    /// The stop a replayed journal implies for a run that only ever started.
    ///
    /// A started row with no stop row is a process that ended without recording
    /// an end, so the cause was never observed: it is
    /// [`StopProvenance::InterruptedUnknown`] and never completion or a panic.
    /// Nothing was written for it, so it is not persisted.
    pub fn recovered_interrupted(run_id: impl Into<String>) -> Self {
        Self::capture(run_id, StopProvenance::InterruptedUnknown)
    }

    /// The run this stop belongs to.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Why the run stopped, as observed.
    pub fn provenance(&self) -> StopProvenance {
        self.provenance
    }

    /// When the stop was captured.
    pub fn at(&self) -> SystemTime {
        self.at
    }

    /// Whether the journal accepted the stop row.
    pub fn persisted(&self) -> bool {
        self.persisted
    }

    /// Record that the journal accepted this stop.
    pub fn mark_persisted(&mut self) {
        self.persisted = true;
    }
}

/// Where a recorded stop landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopDisposition {
    /// It became the current run's stop.
    Current,
    /// It names a run that is no longer current, so it went to history only and
    /// the current run's state was left untouched.
    History,
}

/// The current run, its stop, and the stops that arrived too late to be current.
///
/// The rule this type enforces is ordering: a stop becomes current only when it
/// names the run that is current now. A stop for an older run is kept as history,
/// so a turn that ends after a newer one started can never relabel the newer
/// run's outcome.
#[derive(Debug, Default)]
pub struct RunLedger {
    current: Option<RunIdentity>,
    current_stop: Option<RunStop>,
    history: Vec<RunStop>,
    start_persisted: bool,
    observed_cause: Option<StopProvenance>,
    conversation_saved: Option<bool>,
}

impl RunLedger {
    /// Begin a new run, clearing the current stop.
    ///
    /// The stop being cleared moves to history: it still happened, it is just no
    /// longer the current run's state.
    pub fn begin_run(&mut self, identity: RunIdentity, start_persisted: bool) {
        if let Some(previous) = self.current_stop.take() {
            self.push_history(previous);
        }
        self.current = Some(identity);
        self.start_persisted = start_persisted;
        self.observed_cause = None;
        self.conversation_saved = None;
    }

    /// Carry an earlier ledger's stops forward without inheriting its current run.
    ///
    /// A turn that starts replaces the previous turn's state, but the stops that
    /// already happened belong to the session and not to the turn that recorded
    /// them, so they survive the reset. The previous current run's stop is
    /// included, because it is history the moment the new run begins.
    pub fn carry_over(&mut self, previous: &Self) {
        for stop in previous.history().iter().chain(previous.current_stop()) {
            self.push_history(stop.clone());
        }
    }

    /// The run that is executing now, if any.
    pub fn current(&self) -> Option<&RunIdentity> {
        self.current.as_ref()
    }

    /// The current run's stop, if it ended.
    pub fn current_stop(&self) -> Option<&RunStop> {
        self.current_stop.as_ref()
    }

    /// Stops that arrived after their run stopped being current, oldest first.
    pub fn history(&self) -> &[RunStop] {
        &self.history
    }

    /// Whether the current run's `run.started` row reached the journal.
    pub fn start_persisted(&self) -> bool {
        self.start_persisted
    }

    /// Record a cause the loop observed at a point that ends the turn.
    ///
    /// Only a terminal observation belongs here: a retryable failure the turn
    /// recovered from is not a cause. The first terminal observation wins, so a
    /// later one cannot rewrite why the run ended.
    pub fn observe_cause(&mut self, cause: StopProvenance) {
        if self.observed_cause.is_none() {
            self.observed_cause = Some(cause);
        }
    }

    /// The cause observed for this run, if the loop named one.
    pub fn observed_cause(&self) -> Option<StopProvenance> {
        self.observed_cause
    }

    /// Record whether the turn's conversation save succeeded.
    pub fn note_conversation_save(&mut self, saved: bool) {
        self.conversation_saved = Some(saved);
    }

    /// Whether the turn's conversation save succeeded, if one was attempted.
    pub fn conversation_saved(&self) -> Option<bool> {
        self.conversation_saved
    }

    /// Record a stop, routing it by the run it names.
    ///
    /// A stop that names the current run and is the first one recorded for it
    /// becomes the current stop. Anything else - a stop for an older run, or a
    /// second stop for the current one - goes to history only.
    pub fn record_stop(&mut self, stop: RunStop) -> StopDisposition {
        let names_current = self
            .current
            .as_ref()
            .is_some_and(|current| current.run_id() == stop.run_id());
        if names_current && self.current_stop.is_none() {
            self.current_stop = Some(stop);
            return StopDisposition::Current;
        }
        self.push_history(stop);
        StopDisposition::History
    }

    /// Keep one record per run, so a repeated late stop does not accumulate.
    fn push_history(&mut self, stop: RunStop) {
        if let Some(existing) = self
            .history
            .iter_mut()
            .find(|existing| existing.run_id() == stop.run_id())
        {
            *existing = stop;
            return;
        }
        if self.history.len() == MAX_RUN_STOP_HISTORY {
            self.history.remove(0);
        }
        self.history.push(stop);
    }
}

/// Soft cap on the sessions whose run ledger this process keeps.
///
/// A ledger is small, and a live run is never dropped to honour the cap: the
/// number of live runs is already bounded by the server's own session map, so
/// the cap only evicts sessions whose run has already stopped.
const MAX_TRACKED_SESSIONS: usize = 4_096;

static LEDGERS: LazyLock<Mutex<HashMap<String, RunLedger>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn ledgers() -> MutexGuard<'static, HashMap<String, RunLedger>> {
    LEDGERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run one operation against a session's ledger, creating it if needed.
pub fn with_run_ledger<T>(session_id: &str, body: impl FnOnce(&mut RunLedger) -> T) -> T {
    let mut map = ledgers();
    if !map.contains_key(session_id) && map.len() >= MAX_TRACKED_SESSIONS {
        let oldest_stopped = map
            .iter()
            .filter(|(_, ledger)| ledger.current().is_none() || ledger.current_stop().is_some())
            .min_by_key(|(_, ledger)| {
                ledger
                    .current_stop()
                    .map(|stop| stop.at())
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            })
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest_stopped {
            map.remove(&id);
        }
    }
    body(map.entry(session_id.to_string()).or_default())
}

/// Begin a run for `session_id`, carrying the session's earlier stops forward.
pub fn begin_run(session_id: &str, identity: RunIdentity, start_persisted: bool) {
    with_run_ledger(session_id, |ledger| {
        let mut fresh = RunLedger::default();
        fresh.carry_over(ledger);
        fresh.begin_run(identity, start_persisted);
        *ledger = fresh;
    });
}

/// Record a stop for `session_id`, routing it by the run it names.
pub fn record_stop(session_id: &str, stop: RunStop) -> StopDisposition {
    with_run_ledger(session_id, |ledger| ledger.record_stop(stop))
}

/// Record a cause the loop observed for `session_id`'s current run.
pub fn observe_cause(session_id: &str, cause: StopProvenance) {
    with_run_ledger(session_id, |ledger| ledger.observe_cause(cause));
}

/// The cause observed for `session_id`'s current run, if the loop named one.
pub fn observed_cause(session_id: &str) -> Option<StopProvenance> {
    with_run_ledger(session_id, |ledger| ledger.observed_cause())
}

/// The run executing now for `session_id`, if any.
pub fn current_run(session_id: &str) -> Option<RunIdentity> {
    with_run_ledger(session_id, |ledger| ledger.current().cloned())
}

/// The current run's stop for `session_id`, if it ended.
pub fn current_stop(session_id: &str) -> Option<RunStop> {
    with_run_ledger(session_id, |ledger| ledger.current_stop().cloned())
}

/// The stops `session_id` kept because their run was no longer current.
pub fn stop_history(session_id: &str) -> Vec<RunStop> {
    with_run_ledger(session_id, |ledger| ledger.history().to_vec())
}

/// Record whether `session_id`'s turn conversation save succeeded.
pub fn note_conversation_save(session_id: &str, saved: bool) {
    with_run_ledger(session_id, |ledger| ledger.note_conversation_save(saved));
}

/// Whether `session_id`'s turn conversation save succeeded, if attempted.
pub fn conversation_saved(session_id: &str) -> Option<bool> {
    with_run_ledger(session_id, |ledger| ledger.conversation_saved())
}

/// How one turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The provider finished the turn.
    Stop,
    /// The response was truncated and no continuation recovered it.
    Length,
    /// The turn failed and the loop returns the error to its caller.
    Error,
    /// A cancel or shutdown was observed before the turn finished.
    Aborted,
}

/// A rejected transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TurnStateError {
    /// A turn records its outcome once; a second record is a programming error.
    #[error("turn outcome {0:?} was already recorded")]
    AlreadyFinished(TurnOutcome),
    /// Only a turn that produced an answer may carry final text.
    #[error("turn outcome {0:?} cannot carry final text")]
    FinalTextNotAllowed(TurnOutcome),
}

/// The outcome a provider stop reason implies once every recovery has run.
///
/// A stop reason that still names truncation at the loop's terminal path means
/// the continuation budget is spent, so the turn ends [`TurnOutcome::Length`].
/// Everything else, including `end_turn`, `stop`, and `tool_use` with no calls,
/// ends [`TurnOutcome::Stop`].
pub fn outcome_for_stop_reason(stop_reason: Option<&str>) -> TurnOutcome {
    let Some(reason) = stop_reason.map(str::trim).filter(|reason| !reason.is_empty()) else {
        return TurnOutcome::Stop;
    };
    let reason = reason.to_ascii_lowercase();
    let truncated = reason == "length"
        || reason.contains("max_tokens")
        || reason.contains("max_output_tokens")
        || reason.contains("trunc");
    if truncated {
        TurnOutcome::Length
    } else {
        TurnOutcome::Stop
    }
}

/// Progress and exit state for one turn.
#[derive(Debug, Default)]
pub struct TurnState {
    context_limit_retries: u32,
    rate_limit_retries: u32,
    incomplete_continuations: u32,
    empty_post_tool_continuations: u32,
    fable_guardrail_reconsiderations: u32,
    sequential_single_tool_rounds: u32,
    batch_nudge_pending: bool,
    consecutive_malformed_tool_rounds: u32,
    unparsed_tool_markup_nudges: u32,
    auto_continuations: u8,
    final_text: String,
    outcome: Option<TurnOutcome>,
}

impl TurnState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn context_limit_retries(&self) -> u32 {
        self.context_limit_retries
    }

    pub fn record_context_limit_retry(&mut self) -> u32 {
        self.context_limit_retries = self.context_limit_retries.saturating_add(1);
        self.context_limit_retries
    }

    pub fn reset_context_limit_retries(&mut self) {
        self.context_limit_retries = 0;
    }

    pub fn rate_limit_retries(&self) -> u32 {
        self.rate_limit_retries
    }

    pub fn record_rate_limit_retry(&mut self) -> u32 {
        self.rate_limit_retries = self.rate_limit_retries.saturating_add(1);
        self.rate_limit_retries
    }

    pub fn incomplete_continuations(&self) -> u32 {
        self.incomplete_continuations
    }

    pub fn record_incomplete_continuation(&mut self) -> u32 {
        self.incomplete_continuations = self.incomplete_continuations.saturating_add(1);
        self.incomplete_continuations
    }

    pub fn empty_post_tool_continuations(&self) -> u32 {
        self.empty_post_tool_continuations
    }

    pub fn record_empty_post_tool_continuation(&mut self) -> u32 {
        self.empty_post_tool_continuations = self.empty_post_tool_continuations.saturating_add(1);
        self.empty_post_tool_continuations
    }

    pub fn fable_guardrail_reconsiderations(&self) -> u32 {
        self.fable_guardrail_reconsiderations
    }

    pub fn record_fable_guardrail_reconsideration(&mut self) -> u32 {
        self.fable_guardrail_reconsiderations = self.fable_guardrail_reconsiderations.saturating_add(1);
        self.fable_guardrail_reconsiderations
    }

    pub fn sequential_single_tool_rounds(&self) -> u32 {
        self.sequential_single_tool_rounds
    }

    pub fn batch_nudge_pending(&self) -> bool {
        self.batch_nudge_pending
    }

    /// Fold one completed tool round into the batch-nudge counters.
    ///
    /// A round of exactly one call that was not `batch` extends the streak and
    /// anything else clears it. Reaching `nudge_threshold` owes a nudge on the
    /// next request that has the `batch` tool available.
    pub fn record_tool_round(&mut self, tool_count: usize, used_batch: bool, nudge_threshold: u32) {
        if tool_count == 1 && !used_batch {
            self.sequential_single_tool_rounds = self.sequential_single_tool_rounds.saturating_add(1);
        } else {
            self.sequential_single_tool_rounds = 0;
        }
        if self.sequential_single_tool_rounds >= nudge_threshold {
            self.batch_nudge_pending = true;
        }
    }

    /// Take the owed nudge when the `batch` tool is available.
    ///
    /// Returns false and leaves the nudge owed when `batch` is not registered,
    /// so the reminder still reaches the model once it is.
    pub fn take_batch_nudge(&mut self, batch_available: bool) -> bool {
        if !(self.batch_nudge_pending && batch_available) {
            return false;
        }
        self.batch_nudge_pending = false;
        self.sequential_single_tool_rounds = 0;
        true
    }

    pub fn consecutive_malformed_tool_rounds(&self) -> u32 {
        self.consecutive_malformed_tool_rounds
    }

    pub fn record_malformed_tool_round(&mut self) -> u32 {
        self.consecutive_malformed_tool_rounds = self.consecutive_malformed_tool_rounds.saturating_add(1);
        self.consecutive_malformed_tool_rounds
    }

    pub fn reset_consecutive_malformed_tool_rounds(&mut self) {
        self.consecutive_malformed_tool_rounds = 0;
    }

    pub fn unparsed_tool_markup_nudges(&self) -> u32 {
        self.unparsed_tool_markup_nudges
    }

    pub fn record_unparsed_tool_markup_nudge(&mut self) -> u32 {
        self.unparsed_tool_markup_nudges = self.unparsed_tool_markup_nudges.saturating_add(1);
        self.unparsed_tool_markup_nudges
    }

    pub fn auto_continuations(&self) -> u8 {
        self.auto_continuations
    }

    /// Spend one automatic continuation, never past the shared cap.
    pub fn record_auto_continuation(&mut self) -> u8 {
        self.auto_continuations = self.auto_continuations.saturating_add(1).min(MAX_AUTO_CONTINUATIONS);
        self.auto_continuations
    }

    pub fn final_text(&self) -> &str {
        &self.final_text
    }

    pub fn set_final_text(&mut self, text: &str) {
        self.final_text.clear();
        self.final_text.push_str(text);
    }

    pub fn outcome(&self) -> Option<TurnOutcome> {
        self.outcome
    }

    /// Record how the turn ended: once, and never as `Error` with final text.
    pub fn finish(&mut self, outcome: TurnOutcome) -> Result<(), TurnStateError> {
        if let Some(previous) = self.outcome {
            return Err(TurnStateError::AlreadyFinished(previous));
        }
        if outcome == TurnOutcome::Error && !self.final_text.is_empty() {
            return Err(TurnStateError::FinalTextNotAllowed(outcome));
        }
        self.outcome = Some(outcome);
        Ok(())
    }
}

//! Bounded control requests, each answered by a durable receipt.
//!
//! A client that loses the reply to a control request has to be able to resend
//! it. That is only safe when the request carries an identity the server can
//! remember, so this module owns exactly two actions, both bounded to work the
//! session already holds:
//!
//! * `abort_run { expected_run_id }` stops the run the caller names. When the
//!   session is already executing a different run, the answer is `stale_run`
//!   and nothing is cancelled: a late abort must never kill a newer run.
//! * `cancel_inputs { input_ids }` drops queued soft interrupts by caller id.
//!
//! There is no model switch, file, mail, shell, or generic runtime command
//! here. An operation a client may resend must be one whose duplicate is
//! harmless, and that set is deliberately this small.
//!
//! # The receipt
//!
//! One operation is one id in the session's durable runtime journal
//! (`durable_runtime`), recorded twice:
//!
//! 1. `control.applying` is appended and fsynced *before* the action runs, so a
//!    crash after this point is detectable instead of silent.
//! 2. `control.terminal` is appended and fsynced with the result, after the
//!    action ran.
//!
//! The fingerprint is computed on the server from the action itself (canonical
//! JSON, SHA-256), never trusted from the client:
//!
//! * same operation id, same fingerprint, terminal row present: the stored
//!   result is replayed and the action does not run again;
//! * same operation id, different fingerprint: `conflict`, nothing runs;
//! * same operation id while this process is still applying it: `pending`,
//!   and the second copy never becomes a second application;
//! * an `applying` row with no terminal row, found by a process that did not
//!   write it: `indeterminate`. The action is sealed and never re-applied.
//!
//! The journal file lock is held only for each append. It is released while the
//! action itself runs, and the operation state (not the lock) is what prevents
//! a duplicate application.
//!
//! # What this does not promise
//!
//! This is at-most-once *application* with a durable record of the decision. It
//! does **not** guarantee exactly-once external side effects: an action that
//! ran and then failed to record its terminal row is reported as indeterminate,
//! and the effect it had on the outside world is neither repeated nor undone.
//! Callers must treat `indeterminate` as "the effect may have happened".
//!
//! # Retention
//!
//! The newest [`TERMINAL_CAP`] (1,024) terminal receipts are kept, matching the
//! journal's own terminal-control bound, and every nonterminal operation is kept
//! regardless of age. At most [`NONTERMINAL_CAP`] (256) operations may be
//! applying at once; beyond that a new request is refused with `busy` so an
//! unfinished operation can never be pushed out of the record.
//!
//! An id the journal has already pruned answers `unknown` for as long as the
//! ledger remembers evicting it ([`EVICTION_MEMORY`] ids, four times the
//! terminal bound). Past that, an id is indistinguishable from a new request, so
//! a client that receives `unknown` must stop resending that id rather than
//! expect the answer to stay `unknown` forever.
//!
//! Receipts are per session: the journal is one file per session and the record
//! id is derived from the session id and the operation id together, so the same
//! operation id in two sessions is two unrelated operations.

use crate::durable_runtime::{ControlApplying, ControlTerminal, DurableRuntimeStore, Payload, RuntimeState};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

/// Operations that may be applying at once in one session.
pub(crate) const NONTERMINAL_CAP: usize = 256;

/// Terminal receipts one session keeps, matching the journal's terminal bound.
pub(crate) const TERMINAL_CAP: usize = 1_024;

/// Evicted operation ids the ledger remembers, so a pruned id answers `unknown`
/// instead of being applied a second time.
pub(crate) const EVICTION_MEMORY: usize = 4 * TERMINAL_CAP;

/// Longest operation id accepted. Long enough for a UUID or a caller token.
const MAX_OPERATION_ID_LEN: usize = 128;

/// The two supported control actions.
///
/// `cancel_inputs` normalizes its ids (sorted, deduplicated) when it is built, so
/// the same set resent in a different order is the same fingerprint and replays
/// instead of conflicting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlAction {
    /// Stop the run the caller expects to be executing.
    AbortRun { expected_run_id: String },
    /// Drop queued soft interrupts whose caller id is listed.
    CancelInputs { input_ids: Vec<String> },
}

impl ControlAction {
    pub fn abort_run(expected_run_id: impl Into<String>) -> Self {
        Self::AbortRun {
            expected_run_id: expected_run_id.into(),
        }
    }

    pub fn cancel_inputs(input_ids: &[String]) -> Self {
        let mut normalized = input_ids.to_vec();
        normalized.sort();
        normalized.dedup();
        Self::CancelInputs { input_ids: normalized }
    }

    /// The stable action name, as recorded in the journal row.
    pub fn label(&self) -> &'static str {
        match self {
            Self::AbortRun { .. } => "abort_run",
            Self::CancelInputs { .. } => "cancel_inputs",
        }
    }

    /// The canonical form the fingerprint is taken over.
    fn canonical(&self) -> Value {
        match self {
            Self::AbortRun { expected_run_id } => json!({
                "action": self.label(),
                "expected_run_id": expected_run_id,
            }),
            Self::CancelInputs { input_ids } => json!({
                "action": self.label(),
                "input_ids": input_ids,
            }),
        }
    }

    /// The server-computed fingerprint: canonical JSON, then SHA-256.
    pub fn digest(&self) -> Result<String, String> {
        let canonical = horon_agent_session_store::schema::canonical_json(&self.canonical())
            .map_err(|error| format!("the action could not be canonicalized: {error}"))?;
        Ok(horon_agent_session_store::schema::sha256_hex(canonical.as_bytes()))
    }
}

/// What one control request did, or why it did nothing.
#[derive(Clone, Debug)]
pub enum ControlOutcome {
    /// This request ran the action and recorded its terminal receipt.
    Applied { result: Value },
    /// The same operation was applied earlier; its stored result is replayed.
    Replayed { result: Value },
    /// A duplicate arrived while this process is still applying the operation.
    Pending { reason: String },
    /// The operation id was used for a different action.
    Conflict { reason: String },
    /// An `applying` receipt has no terminal row: the effect may have happened.
    Indeterminate { reason: String },
    /// The run the caller expected is no longer the current run; nothing ran.
    StaleRun {
        expected_run_id: String,
        current_run_id: Option<String>,
    },
    /// The action ran but reported a failure; the failure is recorded.
    Failed { reason: String },
    /// The id was pruned by retention, so it is not this ledger's operation.
    Unknown { reason: String },
    /// The request itself was not acceptable; nothing was recorded.
    Invalid { reason: String },
    /// Too many operations are applying at once; nothing was recorded.
    Busy { reason: String },
    /// The journal could not be read or written; nothing was applied.
    JournalFailed { reason: String },
}

impl ControlOutcome {
    /// The stable label for logs and tests.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Applied { .. } => "applied",
            Self::Replayed { .. } => "replayed",
            Self::Pending { .. } => "pending",
            Self::Conflict { .. } => "conflict",
            Self::Indeterminate { .. } => "indeterminate",
            Self::StaleRun { .. } => "stale_run",
            Self::Failed { .. } => "failed",
            Self::Unknown { .. } => "unknown",
            Self::Invalid { .. } => "invalid",
            Self::Busy { .. } => "busy",
            Self::JournalFailed { .. } => "journal_failed",
        }
    }

    /// The stored result, when this answer carries one.
    pub fn result(&self) -> Option<&Value> {
        match self {
            Self::Applied { result } | Self::Replayed { result } => Some(result),
            _ => None,
        }
    }
}

/// The session-side work one control action performs.
///
/// The effect runs on a blocking thread with no journal lock held, so an action
/// that has to take a queue lock or fire a signal never holds the writer lock.
/// It is called at most once per operation id.
pub trait ControlEffect: Send + Sync {
    /// Stop the run `run_id`, or the current turn when no run was recorded.
    fn abort_run(&self, run_id: Option<&str>) -> Result<Value, String>;
    /// Drop the queued soft interrupts named by `input_ids`.
    fn cancel_inputs(&self, input_ids: &[String]) -> Result<Value, String>;
}

#[derive(Clone, Debug)]
enum Receipt {
    /// Reserved in this process and being applied right now.
    Claiming { digest: String },
    /// Applied, with the result a retry replays.
    Terminal { digest: String, result: Value },
    /// An `applying` row with no terminal row: sealed, never re-applied.
    Doubtful { digest: String },
}

#[derive(Default)]
struct LedgerState {
    entries: HashMap<String, Receipt>,
    /// Terminal ids in the order they were completed, for the retention bound.
    terminal_order: VecDeque<String>,
    /// Ids retention removed, so a pruned id answers `unknown`.
    evicted: HashSet<String>,
    evicted_order: VecDeque<String>,
}

/// One session's control receipts.
pub(crate) struct ControlLedger {
    session_id: String,
    /// Journal root; `None` means the agent home (production).
    root: Option<PathBuf>,
    terminal_cap: usize,
    nonterminal_cap: usize,
    state: Mutex<LedgerState>,
    /// Test-only fault injection: the terminal append reports a failure without
    /// touching the journal, which is how the "applied but not recorded" branch
    /// is exercised.
    #[cfg(test)]
    fail_terminal: std::sync::atomic::AtomicBool,
}

static LEDGERS: LazyLock<Mutex<HashMap<String, Arc<ControlLedger>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn ledgers() -> MutexGuard<'static, HashMap<String, Arc<ControlLedger>>> {
    LEDGERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Drop one session's ledger. A closed session's receipts stay in the journal,
/// so nothing durable is lost; the in-memory cache is what this releases.
pub(crate) fn forget_session(session_id: &str) {
    ledgers().remove(session_id);
}

/// The process-wide ledger for one session.
pub(crate) fn ledger_for(session_id: &str) -> Arc<ControlLedger> {
    let mut all = ledgers();
    if let Some(existing) = all.get(session_id) {
        return Arc::clone(existing);
    }
    let created = Arc::new(ControlLedger::new(None, session_id, TERMINAL_CAP, NONTERMINAL_CAP));
    all.insert(session_id.to_string(), Arc::clone(&created));
    created
}

/// Apply one control action under its durable receipt.
///
/// `operation_id` is the client's identity for the request: the same id with the
/// same action is answered from the receipt, and a different action under the
/// same id is a conflict. Callers that pass no operation id keep using the plain
/// cancel paths and never reach this function.
pub(crate) async fn apply_control(
    session_id: &str,
    operation_id: &str,
    action: &ControlAction,
    effect: Arc<dyn ControlEffect>,
) -> ControlOutcome {
    ledger_for(session_id).apply(operation_id, action, effect).await
}

/// The journal record id one operation always maps to.
///
/// Derived, so a restarted process finds the previous receipt instead of
/// creating a second one. The session id is part of the derivation and the
/// journal is one file per session, so two sessions never share a receipt.
fn record_id(session_id: &str, operation_id: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("horon-control/{session_id}/{operation_id}").as_bytes(),
    )
    .to_string()
}

/// An operation id is a caller-chosen token: reject anything a log line or a
/// derived id could not carry unambiguously.
fn validate_operation_id(operation_id: &str) -> Result<(), String> {
    if operation_id.is_empty() {
        return Err("the operation id is empty".to_string());
    }
    if operation_id.len() > MAX_OPERATION_ID_LEN {
        return Err(format!(
            "the operation id is {} bytes; the limit is {MAX_OPERATION_ID_LEN}",
            operation_id.len()
        ));
    }
    if operation_id
        .chars()
        .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.'))
    {
        return Err("the operation id may only contain ASCII letters, digits, dot, dash and underscore".to_string());
    }
    Ok(())
}

/// The run a session is executing, from the journal: the newest live run.
fn live_run_id(state: &RuntimeState) -> Option<String> {
    state
        .live_runs()
        .max_by_key(|(_, run)| run.seq)
        .map(|(id, _)| id.clone())
}

struct RecordedControl {
    digest: String,
    terminal: Option<Value>,
}

struct JournalStep {
    /// The receipt already in the journal, if the id was used before.
    recorded: Option<RecordedControl>,
    /// The newest live run, from the same snapshot.
    current_run_id: Option<String>,
    /// Whether the journal already recorded the run the caller named as ended.
    expected_run_stopped: bool,
}

impl ControlLedger {
    pub(crate) fn new(root: Option<PathBuf>, session_id: &str, terminal_cap: usize, nonterminal_cap: usize) -> Self {
        Self {
            session_id: session_id.to_string(),
            root,
            terminal_cap,
            nonterminal_cap,
            state: Mutex::new(LedgerState::default()),
            #[cfg(test)]
            fail_terminal: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, LedgerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Apply `action` under `operation_id`, or answer from its receipt.
    pub(crate) async fn apply(
        &self,
        operation_id: &str,
        action: &ControlAction,
        effect: Arc<dyn ControlEffect>,
    ) -> ControlOutcome {
        if let Err(reason) = validate_operation_id(operation_id) {
            return ControlOutcome::Invalid { reason };
        }
        let digest = match action.digest() {
            Ok(digest) => digest,
            Err(reason) => return ControlOutcome::Invalid { reason },
        };
        let record_id = record_id(&self.session_id, operation_id);

        if let Some(outcome) = self.decide_from_memory(operation_id, &digest) {
            return outcome;
        }

        // Reserve before the journal write, so a second copy arriving while the
        // write is in flight sees an in-flight claim instead of claiming too.
        {
            let mut state = self.lock();
            if let Some(outcome) = decide_locked(&state, operation_id, &digest) {
                return outcome;
            }
            let applying = state
                .entries
                .values()
                .filter(|receipt| matches!(receipt, Receipt::Claiming { .. }))
                .count();
            if applying >= self.nonterminal_cap {
                return ControlOutcome::Busy {
                    reason: format!(
                        "{} operations are already applying; the limit is {}",
                        applying, self.nonterminal_cap
                    ),
                };
            }
            state
                .entries
                .insert(operation_id.to_string(), Receipt::Claiming { digest: digest.clone() });
        }

        let step = match self
            .claim_in_journal(&record_id, action.label(), &digest, expected_run_id(action))
            .await
        {
            Ok(step) => step,
            Err(reason) => {
                self.release(operation_id, &digest);
                return ControlOutcome::JournalFailed { reason };
            }
        };

        if let Some(recorded) = step.recorded {
            return self.adopt(operation_id, &digest, recorded);
        }

        // Staleness is decided before anything runs, and only from what the
        // journal positively contradicts: the named run already stopped, or a
        // different run is executing now. A late abort must never stop the new
        // run, and a run the journal never saw is not evidence either way, so it
        // still cancels.
        if let ControlAction::AbortRun { expected_run_id } = action {
            let replaced = step
                .current_run_id
                .as_deref()
                .is_some_and(|current| current != expected_run_id);
            if replaced || step.expected_run_stopped {
                let current = step.current_run_id.clone();
                let result = json!({
                    "status": "stale_run",
                    "action": action.label(),
                    "expected_run_id": expected_run_id,
                    "current_run_id": current,
                });
                return match self.record_terminal(&record_id, result.clone()).await {
                    Ok(()) => {
                        self.remember_terminal(operation_id, &digest, result);
                        ControlOutcome::StaleRun {
                            expected_run_id: expected_run_id.clone(),
                            current_run_id: current,
                        }
                    }
                    Err(reason) => {
                        self.seal_doubtful(operation_id, &digest);
                        ControlOutcome::Indeterminate {
                            reason: format!(
                                "the abort was decided stale but its receipt could not be recorded: {reason}"
                            ),
                        }
                    }
                };
            }
        }

        let run_id = step.current_run_id;
        let effect_result = run_effect(effect, action.clone(), run_id).await;
        let (result, outcome) = match effect_result {
            Ok(detail) => {
                let result = json!({
                    "status": "applied",
                    "action": action.label(),
                    "detail": detail,
                });
                (result.clone(), ControlOutcome::Applied { result })
            }
            Err(reason) => {
                let result = json!({
                    "status": "failed",
                    "action": action.label(),
                    "reason": reason,
                });
                (result, ControlOutcome::Failed { reason })
            }
        };
        match self.record_terminal(&record_id, result.clone()).await {
            Ok(()) => {
                self.remember_terminal(operation_id, &digest, result);
                outcome
            }
            Err(reason) => {
                // The action already ran. Recording that it might not have is
                // the honest answer: a retry must not apply it again.
                self.seal_doubtful(operation_id, &digest);
                ControlOutcome::Indeterminate {
                    reason: format!(
                        "the action ran but its receipt could not be recorded, so its outcome is indeterminate: {reason}"
                    ),
                }
            }
        }
    }

    /// Adopt a receipt that was already in the journal.
    fn adopt(&self, operation_id: &str, digest: &str, recorded: RecordedControl) -> ControlOutcome {
        match recorded.terminal {
            Some(result) => {
                let outcome = if recorded.digest == digest {
                    ControlOutcome::Replayed { result: result.clone() }
                } else {
                    ControlOutcome::Conflict {
                        reason: format!(
                            "operation `{operation_id}` was already applied with a different action; resending a different action under the same id cannot change it"
                        ),
                    }
                };
                if matches!(outcome, ControlOutcome::Replayed { .. }) {
                    self.remember_terminal(operation_id, digest, result);
                } else {
                    self.remember_terminal(operation_id, &recorded.digest, result);
                }
                outcome
            }
            None => {
                // An `applying` row this process did not write: the owner died
                // or never reached its terminal row. Seal it.
                self.seal_doubtful(operation_id, &recorded.digest);
                if recorded.digest != digest {
                    return ControlOutcome::Conflict {
                        reason: format!("operation `{operation_id}` is already applying a different action"),
                    };
                }
                ControlOutcome::Indeterminate {
                    reason: format!(
                        "operation `{operation_id}` was claimed before this process started and its outcome was never recorded, so it is indeterminate and is not applied again"
                    ),
                }
            }
        }
    }

    fn decide_from_memory(&self, operation_id: &str, digest: &str) -> Option<ControlOutcome> {
        let state = self.lock();
        decide_locked(&state, operation_id, digest)
    }

    fn release(&self, operation_id: &str, digest: &str) {
        let mut state = self.lock();
        let is_our_claim = matches!(
            state.entries.get(operation_id),
            Some(Receipt::Claiming { digest: held }) if held == digest
        );
        if is_our_claim {
            state.entries.remove(operation_id);
        }
    }

    fn seal_doubtful(&self, operation_id: &str, digest: &str) {
        let mut state = self.lock();
        state.entries.insert(
            operation_id.to_string(),
            Receipt::Doubtful {
                digest: digest.to_string(),
            },
        );
    }

    fn remember_terminal(&self, operation_id: &str, digest: &str, result: Value) {
        let mut state = self.lock();
        let cap = self.terminal_cap;
        remember_terminal_locked(&mut state, cap, operation_id, digest, result);
    }

    /// Read the receipt for this operation, or record the applying row when the
    /// id is new, in one journal open. The store is closed before the effect
    /// runs, so the writer lock is not held across the action.
    async fn claim_in_journal(
        &self,
        record_id: &str,
        action_label: &str,
        digest: &str,
        expected_run_id: Option<&str>,
    ) -> Result<JournalStep, String> {
        let root = self.root.clone();
        let session_id = self.session_id.clone();
        let record_id = record_id.to_string();
        let action_label = action_label.to_string();
        let digest = digest.to_string();
        let expected_run_id = expected_run_id.map(str::to_string);
        blocking(move || {
            let mut store = open_store(root.as_deref(), &session_id)?;
            let state = store
                .snapshot()
                .map_err(|error| format!("the control journal could not be replayed: {error}"))?;
            let current_run_id = live_run_id(&state);
            let expected_run_stopped = expected_run_id
                .as_deref()
                .and_then(|run_id| state.runs.get(run_id))
                .is_some_and(|run| run.stop.is_some());
            if let Some(entry) = state.controls.get(&record_id) {
                return Ok(JournalStep {
                    recorded: Some(RecordedControl {
                        digest: entry.digest.clone(),
                        terminal: entry.result.clone(),
                    }),
                    current_run_id,
                    expected_run_stopped,
                });
            }
            store
                .append(
                    &record_id,
                    Payload::ControlApplying(ControlApplying {
                        action: action_label,
                        digest,
                    }),
                )
                .map_err(|error| format!("the control claim could not be recorded: {error}"))?;
            Ok(JournalStep {
                recorded: None,
                current_run_id,
                expected_run_stopped,
            })
        })
        .await
    }

    async fn record_terminal(&self, record_id: &str, result: Value) -> Result<(), String> {
        #[cfg(test)]
        if self.fail_terminal.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("injected terminal write failure".to_string());
        }
        let root = self.root.clone();
        let session_id = self.session_id.clone();
        let record_id = record_id.to_string();
        blocking(move || {
            let mut store = open_store(root.as_deref(), &session_id)?;
            store
                .append(&record_id, Payload::ControlTerminal(ControlTerminal { result }))
                .map_err(|error| format!("the control receipt could not be recorded: {error}"))?;
            Ok(())
        })
        .await
    }

    /// Test-only: make the terminal append fail without touching the journal.
    #[cfg(test)]
    fn fail_terminal_writes(&self) {
        self.fail_terminal.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The run an `abort_run` names, when the action is an abort.
fn expected_run_id(action: &ControlAction) -> Option<&str> {
    match action {
        ControlAction::AbortRun { expected_run_id } => Some(expected_run_id),
        ControlAction::CancelInputs { .. } => None,
    }
}

fn decide_locked(state: &LedgerState, operation_id: &str, digest: &str) -> Option<ControlOutcome> {
    if state.evicted.contains(operation_id) {
        return Some(ControlOutcome::Unknown {
            reason: format!(
                "operation `{operation_id}` is outside the retained receipts, so it is not this ledger's operation"
            ),
        });
    }
    match state.entries.get(operation_id)? {
        Receipt::Terminal { digest: held, result } => Some(if held == digest {
            ControlOutcome::Replayed { result: result.clone() }
        } else {
            ControlOutcome::Conflict {
                reason: format!(
                    "operation `{operation_id}` was already applied with a different action; resending a different action under the same id cannot change it"
                ),
            }
        }),
        Receipt::Claiming { digest: held } => Some(if held == digest {
            ControlOutcome::Pending {
                reason: format!("operation `{operation_id}` is already applying"),
            }
        } else {
            ControlOutcome::Conflict {
                reason: format!("operation `{operation_id}` is already applying a different action"),
            }
        }),
        Receipt::Doubtful { digest: held } => Some(if held == digest {
            ControlOutcome::Indeterminate {
                reason: format!(
                    "operation `{operation_id}` is indeterminate: its effect may have happened and it is never applied again"
                ),
            }
        } else {
            ControlOutcome::Conflict {
                reason: format!("operation `{operation_id}` was claimed by a different action"),
            }
        }),
    }
}

fn remember_terminal_locked(state: &mut LedgerState, cap: usize, operation_id: &str, digest: &str, result: Value) {
    state.entries.insert(
        operation_id.to_string(),
        Receipt::Terminal {
            digest: digest.to_string(),
            result,
        },
    );
    state.terminal_order.push_back(operation_id.to_string());
    while state.terminal_order.len() > cap {
        let Some(oldest) = state.terminal_order.pop_front() else {
            break;
        };
        state.entries.remove(&oldest);
        state.evicted.insert(oldest.clone());
        state.evicted_order.push_back(oldest);
    }
    while state.evicted_order.len() > EVICTION_MEMORY {
        if let Some(oldest) = state.evicted_order.pop_front() {
            state.evicted.remove(&oldest);
        }
    }
}

fn open_store(root: Option<&std::path::Path>, session_id: &str) -> Result<DurableRuntimeStore, String> {
    let opened = match root {
        Some(root) => DurableRuntimeStore::open_in(root, session_id),
        None => DurableRuntimeStore::open(session_id),
    };
    opened.map_err(|error| format!("the control journal could not be opened: {error}"))
}

async fn blocking<T, F>(body: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(body)
        .await
        .map_err(|error| format!("the control journal task did not finish: {error}"))?
}

async fn run_effect(
    effect: Arc<dyn ControlEffect>,
    action: ControlAction,
    run_id: Option<String>,
) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || match &action {
        ControlAction::AbortRun { .. } => effect.abort_run(run_id.as_deref()),
        ControlAction::CancelInputs { input_ids } => effect.cancel_inputs(input_ids),
    })
    .await
    .map_err(|error| format!("the control effect did not finish: {error}"))?
}

#[cfg(test)]
#[path = "control_receipts_tests.rs"]
mod tests;

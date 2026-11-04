//! Delivery channel from the `ask` tool to a client that can answer it, and the
//! durable receipt that makes one answer resolve a question exactly once.
//!
//! The tool runs inside the daemon and holds no handle on a connection: the
//! client that renders the questions may attach, detach, or never exist. A
//! connection that advertised `supports_ask_requests` therefore installs a
//! sender here for the session it attached, and the tool looks it up by
//! session id.
//!
//! Presence *is* the capability signal. No sender means no attached client
//! claims it can answer, so the tool keeps its non-blocking policy and never
//! waits for a person who is not there. A sender that disappears mid-wait
//! (disconnect, or a newer connection replacing this one) drops the answer
//! channel, which ends the wait instead of leaving the turn parked.
//!
//! The registry is process-global because `ToolContext` is constructed in more
//! than sixty places and threading a per-session handle through all of them
//! would dwarf the feature. Keys are session ids, which are unique per
//! session, and installs are token-guarded so a disconnecting connection
//! cannot clear a newer one's channel.
//!
//! # Answer receipts
//!
//! A client that negotiated `ask_receipts_v1` sends an `answer_id` with its
//! answer. [`AnswerLedger`] then owns the whole decision for one question: the
//! pending check, the digest comparison, and the claim happen in one serialized
//! step, so two copies of one answer cannot both be delivered and a retry after
//! a lost reply is a replay rather than a second resolution.
//!
//! The lifecycle is recorded in the session's durable runtime journal as
//! `control.applying` (claimed) and `control.terminal` (delivered). A claim
//! whose terminal row never landed (a crash between delivery and the record, or
//! a failed write after delivery) is *indeterminate*: the answer is never
//! re-delivered, and no later process may treat the question as open again.
//! The record id is derived from the session and question id, so a restarted
//! process finds the claim instead of re-asking.

use crate::durable_runtime::{DurableRuntimeStore, JournalError, Payload};
use crate::protocol::AskSelection;
use horon_agent_tool_types::{AskAnswer, AskQuestion};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};

/// One question set waiting for a client answer.
pub(crate) struct AskPrompt {
    pub ask_id: String,
    pub questions: Vec<AskQuestion>,
    pub response_tx: tokio::sync::oneshot::Sender<Vec<AskAnswer>>,
}

struct Installed {
    token: u64,
    tx: tokio::sync::mpsc::UnboundedSender<AskPrompt>,
}

fn registry() -> &'static Mutex<HashMap<String, Installed>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Installed>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Installed>> {
    registry().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Offer this connection as an answerer for `session_id`.
///
/// Replaces any previous installation: the newest connection is the one whose
/// client the user is looking at. Returns the token a later [`clear`] must
/// present.
pub(crate) fn install(session_id: &str, tx: tokio::sync::mpsc::UnboundedSender<AskPrompt>) -> u64 {
    static NEXT_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let token = NEXT_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    lock().insert(session_id.to_string(), Installed { token, tx });
    token
}

/// Withdraw an installation, but only the one this token created: an older
/// transport finishing its disconnect must not take away a newer channel.
pub(crate) fn clear(session_id: &str, token: u64) {
    let mut installed = lock();
    if installed.get(session_id).is_some_and(|entry| entry.token == token) {
        installed.remove(session_id);
    }
}

pub(crate) fn sender_for(session_id: &str) -> Option<tokio::sync::mpsc::UnboundedSender<AskPrompt>> {
    lock().get(session_id).map(|entry| entry.tx.clone())
}

/// Questions per session that are waiting for a client answer right now.
static AWAITING: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn awaiting() -> MutexGuard<'static, HashMap<String, usize>> {
    AWAITING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counts one question as waiting for its client until dropped.
pub(crate) struct AwaitingAnswer {
    session_id: String,
}

pub(crate) fn begin_awaiting_answer(session_id: &str) -> AwaitingAnswer {
    *awaiting().entry(session_id.to_string()).or_default() += 1;
    AwaitingAnswer {
        session_id: session_id.to_string(),
    }
}

impl Drop for AwaitingAnswer {
    fn drop(&mut self) {
        let mut waiting = awaiting();
        if let Some(count) = waiting.get_mut(&self.session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                waiting.remove(&self.session_id);
            }
        }
    }
}

/// Whether a question in `session_id` is waiting for its client's answer.
pub(crate) fn awaiting_answer(session_id: &str) -> bool {
    awaiting().contains_key(session_id)
}

#[cfg(test)]
pub(crate) fn clear_all() {
    lock().clear();
    ledgers().clear();
}

/// Completed receipts a session keeps in memory. The journal bounds terminal
/// controls to the same number, so a receipt pruned here is still answerable
/// from disk until compaction drops it.
pub(crate) const COMPLETED_RECEIPT_CAP: usize = 1_024;

#[derive(Clone, Debug)]
enum Receipt {
    /// Claimed but not yet terminal: the answer is never delivered again.
    Claiming { digest: String, answer_id: String },
    Terminal {
        digest: String,
        answer_id: String,
        result: Value,
    },
}

#[derive(Default)]
struct LedgerState {
    entries: HashMap<String, Receipt>,
    completed: VecDeque<String>,
}

/// What the ledger decided about one incoming answer.
#[derive(Clone, Debug)]
pub(crate) enum AnswerDecision {
    /// This caller owns the answer: deliver it, then record the terminal row.
    Claimed,
    Replay {
        result: Value,
    },
    Conflict {
        reason: String,
    },
    /// The answer may already have been delivered and cannot be re-delivered.
    Indeterminate {
        reason: String,
    },
    Unknown,
    /// The journal could not be written, so nothing was claimed.
    JournalFailed {
        reason: String,
    },
}

pub(crate) struct AnswerLedger {
    session_id: String,
    /// Completed receipts this ledger keeps. Production uses
    /// [`COMPLETED_RECEIPT_CAP`]; tests inject a small value so the eviction path
    /// is exercised without one fsync per receipt.
    completed_cap: usize,
    state: Mutex<LedgerState>,
}

static LEDGERS: LazyLock<Mutex<HashMap<String, Arc<AnswerLedger>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Drop one session's answer ledger. Terminal receipts live in the journal, so
/// this releases the in-memory cache and nothing durable.
pub(crate) fn forget_session(session_id: &str) {
    LEDGERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id);
}

fn ledgers() -> MutexGuard<'static, HashMap<String, Arc<AnswerLedger>>> {
    LEDGERS.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn ledger_for(session_id: &str) -> Arc<AnswerLedger> {
    let mut all = ledgers();
    if let Some(existing) = all.get(session_id) {
        return Arc::clone(existing);
    }
    let created = Arc::new(AnswerLedger {
        session_id: session_id.to_string(),
        completed_cap: COMPLETED_RECEIPT_CAP,
        state: Mutex::new(LedgerState::default()),
    });
    all.insert(session_id.to_string(), Arc::clone(&created));
    created
}

/// The digest that decides whether two answers are the same answer: the
/// `answer_id` is part of it, so the same choices resent under a different
/// identity are a conflict rather than a replay.
pub(crate) fn answer_digest(ask_id: &str, answer_id: &str, answers: &[AskSelection]) -> String {
    let selections: Vec<Value> = answers
        .iter()
        .map(|answer| {
            let mut selected = answer.selected.clone();
            selected.sort();
            serde_json::json!({"question_id": answer.question_id, "selected": selected})
        })
        .collect();
    let canonical = serde_json::json!({
        "ask_id": ask_id,
        "answer_id": answer_id,
        "answers": selections,
    });
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    format!("{:x}", Sha256::digest(&bytes))
}

/// The journal record id one question always maps to: derived, so a restarted
/// process finds the previous claim instead of creating a second one.
fn receipt_record_id(session_id: &str, ask_id: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("horon-answer/{session_id}/{ask_id}").as_bytes(),
    )
    .to_string()
}

struct RecordedAnswer {
    digest: String,
    terminal: Option<Value>,
}

fn journal_join_error(error: tokio::task::JoinError) -> JournalError {
    JournalError::from(std::io::Error::other(format!(
        "the answer journal task did not finish: {error}"
    )))
}

impl AnswerLedger {
    fn lock(&self) -> MutexGuard<'_, LedgerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read the journal for this question, off the async worker and with no
    /// ledger lock held.
    async fn recorded(&self, ask_id: &str) -> Result<Option<RecordedAnswer>, JournalError> {
        let session_id = self.session_id.clone();
        let record_id = receipt_record_id(&self.session_id, ask_id);
        tokio::task::spawn_blocking(move || {
            let store = DurableRuntimeStore::open(&session_id)?;
            Ok(store.state().controls.get(&record_id).map(|entry| RecordedAnswer {
                digest: entry.digest.clone(),
                terminal: entry.result.clone(),
            }))
        })
        .await
        .map_err(journal_join_error)?
    }

    /// Claim this answer, or report why it cannot be claimed.
    ///
    /// `claim_allowed` is false when no client waits for the question any more:
    /// the ledger then answers from its records and never mints a claim, because
    /// a claim on a question nobody holds would block a later real answer.
    pub(crate) async fn begin(
        &self,
        ask_id: &str,
        answer_id: &str,
        digest: &str,
        claim_allowed: bool,
    ) -> AnswerDecision {
        if let Some(decision) = self.decide_from_memory(ask_id, answer_id, digest) {
            return decision;
        }
        match self.recorded(ask_id).await {
            Ok(Some(recorded)) => return self.adopt_recorded(ask_id, answer_id, digest, recorded),
            Ok(None) => {}
            Err(error) => {
                return AnswerDecision::JournalFailed {
                    reason: format!("the answer journal could not be read: {error}"),
                };
            }
        }
        if !claim_allowed {
            return AnswerDecision::Unknown;
        }
        // Reserved before the journal write, so a second copy arriving while the
        // write is in flight sees an in-flight claim rather than claiming too.
        {
            let mut state = self.lock();
            if let Some(decision) = decide_locked(&state, ask_id, answer_id, digest) {
                return decision;
            }
            state.entries.insert(
                ask_id.to_string(),
                Receipt::Claiming {
                    digest: digest.to_string(),
                    answer_id: answer_id.to_string(),
                },
            );
        }
        match self.append_applying(ask_id, digest).await {
            Ok(()) => AnswerDecision::Claimed,
            Err(error) => {
                self.release(ask_id, digest);
                AnswerDecision::JournalFailed {
                    reason: format!("the answer claim could not be recorded: {error}"),
                }
            }
        }
    }

    /// Record the terminal row after the answer reached the waiting tool. A
    /// failure here leaves the receipt in doubt on purpose: the answer was
    /// delivered, so replaying it would resolve the question twice.
    pub(crate) async fn complete(&self, ask_id: &str, digest: &str, result: Value) -> Result<(), String> {
        let answer_id = match self.lock().entries.get(ask_id) {
            Some(Receipt::Claiming { answer_id, .. }) => answer_id.clone(),
            _ => String::new(),
        };
        self.append_terminal(ask_id, result.clone())
            .await
            .map_err(|error| format!("the answer result could not be recorded: {error}"))?;
        let mut state = self.lock();
        record_completed_locked(
            &mut state,
            self.completed_cap,
            ask_id,
            Receipt::Terminal {
                digest: digest.to_string(),
                answer_id,
                result,
            },
        );
        Ok(())
    }

    /// Drop an in-memory reservation whose journal row never landed.
    fn release(&self, ask_id: &str, digest: &str) {
        let mut state = self.lock();
        let matches_reservation = matches!(
            state.entries.get(ask_id),
            Some(Receipt::Claiming { digest: held, .. }) if held == digest
        );
        if matches_reservation {
            state.entries.remove(ask_id);
        }
    }

    fn decide_from_memory(&self, ask_id: &str, answer_id: &str, digest: &str) -> Option<AnswerDecision> {
        let state = self.lock();
        decide_locked(&state, ask_id, answer_id, digest)
    }

    fn adopt_recorded(&self, ask_id: &str, answer_id: &str, digest: &str, recorded: RecordedAnswer) -> AnswerDecision {
        match recorded.terminal {
            Some(result) => {
                let decision = if recorded.digest == digest {
                    AnswerDecision::Replay { result: result.clone() }
                } else {
                    AnswerDecision::Conflict {
                        reason: format!(
                            "ask request `{ask_id}` was already answered by a different answer; resending different choices cannot change it"
                        ),
                    }
                };
                if matches!(decision, AnswerDecision::Replay { .. }) {
                    let mut state = self.lock();
                    record_completed_locked(
                        &mut state,
                        self.completed_cap,
                        ask_id,
                        Receipt::Terminal {
                            digest: digest.to_string(),
                            answer_id: answer_id.to_string(),
                            result,
                        },
                    );
                }
                decision
            }
            None => {
                let mut state = self.lock();
                state.entries.insert(
                    ask_id.to_string(),
                    Receipt::Claiming {
                        digest: recorded.digest.clone(),
                        answer_id: answer_id.to_string(),
                    },
                );
                AnswerDecision::Indeterminate {
                    reason: format!(
                        "ask request `{ask_id}` was claimed before this process started and its outcome was never recorded, so it is indeterminate and the answer is not delivered again"
                    ),
                }
            }
        }
    }

    async fn append_applying(&self, ask_id: &str, digest: &str) -> Result<(), JournalError> {
        let session_id = self.session_id.clone();
        let record_id = receipt_record_id(&self.session_id, ask_id);
        let digest = digest.to_string();
        tokio::task::spawn_blocking(move || {
            let mut store = DurableRuntimeStore::open(&session_id)?;
            store.append(
                &record_id,
                Payload::ControlApplying(crate::durable_runtime::ControlApplying {
                    action: "ask_answer".to_string(),
                    digest,
                }),
            )?;
            Ok(())
        })
        .await
        .map_err(journal_join_error)?
    }

    async fn append_terminal(&self, ask_id: &str, result: Value) -> Result<(), JournalError> {
        let session_id = self.session_id.clone();
        let record_id = receipt_record_id(&self.session_id, ask_id);
        tokio::task::spawn_blocking(move || {
            let mut store = DurableRuntimeStore::open(&session_id)?;
            store.append(
                &record_id,
                Payload::ControlTerminal(crate::durable_runtime::ControlTerminal { result }),
            )?;
            Ok(())
        })
        .await
        .map_err(journal_join_error)?
    }
}

fn record_completed_locked(state: &mut LedgerState, cap: usize, ask_id: &str, receipt: Receipt) {
    state.entries.insert(ask_id.to_string(), receipt);
    state.completed.push_back(ask_id.to_string());
    while state.completed.len() > cap {
        if let Some(oldest) = state.completed.pop_front() {
            state.entries.remove(&oldest);
        }
    }
}

fn decide_locked(state: &LedgerState, ask_id: &str, answer_id: &str, digest: &str) -> Option<AnswerDecision> {
    match state.entries.get(ask_id)? {
        Receipt::Terminal {
            digest: held_digest,
            answer_id: held_id,
            result,
        } => Some(if held_digest == digest {
            AnswerDecision::Replay { result: result.clone() }
        } else {
            AnswerDecision::Conflict {
                reason: if held_id == answer_id {
                    format!("ask request `{ask_id}` was already answered with different choices")
                } else {
                    format!("ask request `{ask_id}` was already answered under a different answer id")
                },
            }
        }),
        Receipt::Claiming {
            digest: held_digest, ..
        } => Some(if held_digest == digest {
            AnswerDecision::Indeterminate {
                reason: format!(
                    "ask request `{ask_id}` is already claimed by this answer and its outcome is not recorded yet; it will not be delivered twice"
                ),
            }
        } else {
            AnswerDecision::Conflict {
                reason: format!("ask request `{ask_id}` is already claimed by a different answer"),
            }
        }),
    }
}

#[cfg(test)]
fn forget_ledger_for_test(session_id: &str) {
    ledgers().remove(session_id);
}

#[cfg(test)]
pub(crate) fn ask_receipt_ledger_for_test(session_id: &str) -> Arc<AnswerLedger> {
    ask_receipt_ledger_with_cap_for_test(session_id, COMPLETED_RECEIPT_CAP)
}

#[cfg(test)]
pub(crate) fn ask_receipt_ledger_with_cap_for_test(session_id: &str, completed_cap: usize) -> Arc<AnswerLedger> {
    forget_ledger_for_test(session_id);
    let ledger = Arc::new(AnswerLedger {
        session_id: session_id.to_string(),
        completed_cap,
        state: Mutex::new(LedgerState::default()),
    });
    ledgers().insert(session_id.to_string(), Arc::clone(&ledger));
    ledger
}

#[cfg(test)]
#[path = "ask_receipt_tests.rs"]
mod receipt_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> (
        tokio::sync::mpsc::UnboundedSender<AskPrompt>,
        tokio::sync::mpsc::UnboundedReceiver<AskPrompt>,
    ) {
        tokio::sync::mpsc::unbounded_channel()
    }

    #[test]
    fn presence_is_the_capability_signal() {
        // `clear_all` empties the process-global registry, so every test that
        // touches it holds the shared test lock that the ask client tests and
        // the receipt-ledger tests take as well.
        let _bridge = crate::storage::lock_test_env();
        clear_all();
        assert!(sender_for("ask-bridge-none").is_none());
        let (tx, _rx) = channel();
        let token = install("ask-bridge-one", tx);
        assert!(sender_for("ask-bridge-one").is_some());
        clear("ask-bridge-one", token);
        assert!(sender_for("ask-bridge-one").is_none());
        clear_all();
    }

    #[test]
    fn an_older_connection_cannot_clear_a_newer_install() {
        let _bridge = crate::storage::lock_test_env();
        clear_all();
        let (old_tx, _old_rx) = channel();
        let old = install("ask-bridge-replace", old_tx);
        let (new_tx, _new_rx) = channel();
        let new = install("ask-bridge-replace", new_tx);
        clear("ask-bridge-replace", old);
        assert!(
            sender_for("ask-bridge-replace").is_some(),
            "the older token must not withdraw the newer connection"
        );
        clear("ask-bridge-replace", new);
        assert!(sender_for("ask-bridge-replace").is_none());
        clear_all();
    }
}

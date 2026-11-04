//! Client answers that resolve a pending daemon request: an approval parked by
//! the safety system, and an `ask` question set waiting on the tool's channel.
//! Both refuse an id that is no longer pending, so a late reply cannot change a
//! newer request or a turn that already moved on.
//!
//! An answer that carries an `answer_id` additionally passes through the
//! session's answer ledger, so one question resolves exactly once even when the
//! client retries a reply it never saw acknowledged.

use super::truncate_detail;
use crate::protocol::{AskSelection, PermissionDecision, ServerEvent};
use crate::tool::ask_bridge::{self, AnswerDecision};
use crate::tool::permission_bridge::{EditorAnswer, PermissionBridge};
use horon_agent_tool_types::{AskAnswer, AskQuestion};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

/// `decided_via` for an answer a client sent as `permission_decision`.
const PERMISSION_DECISION_VIA: &str = "api_client";

pub(super) type PendingAsks = Arc<Mutex<HashMap<String, PendingAsk>>>;

pub(super) struct PendingAsk {
    pub(super) questions: Vec<AskQuestion>,
    pub(super) response_tx: tokio::sync::oneshot::Sender<Vec<AskAnswer>>,
}

/// The receipt identity of one answer.
pub(super) struct AnswerReceipt<'a> {
    pub session_id: &'a str,
    pub answer_id: &'a str,
}

/// Send one reply to the client, naming a dropped transport in the log.
///
/// A failed send means this connection is already gone, so there is nobody to
/// answer and nothing to retry; the log is what keeps that visible instead of
/// silent.
fn reply(client_event_tx: &mpsc::UnboundedSender<ServerEvent>, event: ServerEvent) {
    if client_event_tx.send(event).is_err() {
        crate::logging::warn("a client answer reply could not be delivered: the connection closed first");
    }
}

/// Start the per-connection pump that turns the `ask` tool's prompts into
/// `ask_requested` events and keeps their answer channels.
///
/// The entry is registered before the event is sent: a client may answer the
/// instant it reads the question. Entries whose tool already stopped waiting
/// are dropped on each new prompt, so a long-lived connection does not keep
/// questions nobody will answer.
pub(super) fn spawn_ask_forwarder(
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
) -> (
    PendingAsks,
    tokio::sync::mpsc::UnboundedSender<crate::tool::ask_bridge::AskPrompt>,
    tokio::task::JoinHandle<()>,
) {
    let pending: PendingAsks = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::tool::ask_bridge::AskPrompt>();
    let shared = pending.clone();
    let handle = tokio::spawn(async move {
        while let Some(prompt) = rx.recv().await {
            let ask_id = prompt.ask_id.clone();
            {
                let mut entries = shared.lock().await;
                entries.retain(|_, ask| !ask.response_tx.is_closed());
                entries.insert(
                    ask_id.clone(),
                    PendingAsk {
                        questions: prompt.questions.clone(),
                        response_tx: prompt.response_tx,
                    },
                );
            }
            reply(
                &client_event_tx,
                ServerEvent::AskRequested {
                    request_id: ask_id,
                    questions: prompt.questions,
                },
            );
        }
    });
    (pending, tx, handle)
}

pub(super) async fn handle_ask_response(
    id: u64,
    request_id: String,
    answers: Vec<AskSelection>,
    pending: &PendingAsks,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    answer_ask(id, request_id, answers, None, pending, client_event_tx).await;
}

/// Resolve one `ask_requested` through the session's answer ledger.
///
/// This is the `ask_receipts_v1` path: the client chose an `answer_id`, so a
/// retry of the same `request_id`+`answer_id` with the same answers replays the
/// recorded resolution instead of resolving the question twice.
pub(super) async fn handle_ask_response_with_receipt(
    id: u64,
    request_id: String,
    answers: Vec<AskSelection>,
    answer_id: String,
    session_id: &str,
    pending: &PendingAsks,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let receipt = AnswerReceipt {
        session_id,
        answer_id: &answer_id,
    };
    answer_ask(id, request_id, answers, Some(receipt), pending, client_event_tx).await;
}

/// Resolve one `ask_requested`, honouring an answer receipt when one is given.
pub(super) async fn answer_ask(
    id: u64,
    request_id: String,
    answers: Vec<AskSelection>,
    receipt: Option<AnswerReceipt<'_>>,
    pending: &PendingAsks,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let Some(receipt) = receipt else {
        answer_without_receipt(id, request_id, answers, pending, client_event_tx).await;
        return;
    };
    let digest = ask_bridge::answer_digest(&request_id, receipt.answer_id, &answers);
    let ledger = ask_bridge::ledger_for(receipt.session_id);
    // Whether a client still holds the question. A claim is only minted while
    // one does, so a timed-out question is not left claimed and unable to be
    // answered by a later, real client.
    let claim_allowed = {
        let entries = pending.lock().await;
        entries.get(&request_id).is_some_and(|ask| !ask.response_tx.is_closed())
    };
    match ledger
        .begin(&request_id, receipt.answer_id, &digest, claim_allowed)
        .await
    {
        AnswerDecision::Claimed => {
            deliver_claimed(id, request_id, answers, &ledger, digest, pending, client_event_tx).await;
        }
        AnswerDecision::Replay { result } => {
            crate::logging::event_info(
                "server_ask_answer_replay",
                vec![
                    ("id", id.to_string()),
                    ("request_id", request_id.clone()),
                    ("answer_id", receipt.answer_id.to_string()),
                    ("result", result.to_string()),
                ],
            );
            reply(client_event_tx, ServerEvent::Ack { id });
        }
        AnswerDecision::Conflict { reason } => {
            reply(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: reason,
                    retry_after_secs: None,
                },
            );
        }
        AnswerDecision::Indeterminate { reason } => {
            reply(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: reason,
                    retry_after_secs: None,
                },
            );
        }
        AnswerDecision::Unknown => {
            // The question resolved itself (a timeout or a disconnect) before this
            // answer arrived. It is retired here exactly as the legacy path
            // retires it, and nothing is recorded: the question resolved without
            // a person, so no answer receipt may claim it.
            let mut entries = pending.lock().await;
            let stopped_waiting = entries.get(&request_id).is_some_and(|ask| ask.response_tx.is_closed());
            entries.remove(&request_id);
            drop(entries);
            let message = if stopped_waiting {
                format!("ask request `{request_id}` is no longer pending; the tool stopped waiting for it")
            } else {
                format!(
                    "ask request `{request_id}` is no longer pending; it already timed out, was answered, or its client disconnected"
                )
            };
            reply(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message,
                    retry_after_secs: None,
                },
            );
        }
        AnswerDecision::JournalFailed { reason } => {
            reply(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: format!("ask request `{request_id}` was not answered: {reason}"),
                    retry_after_secs: None,
                },
            );
        }
    }
}

async fn deliver_claimed(
    id: u64,
    request_id: String,
    answers: Vec<AskSelection>,
    ledger: &Arc<ask_bridge::AnswerLedger>,
    digest: String,
    pending: &PendingAsks,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let Some(pending_ask) = pending.lock().await.remove(&request_id) else {
        // Claimed but the entry vanished between the pending check and the
        // claim: the question is gone, so the answer cannot be delivered and
        // must not be claimed again by a later retry.
        let result = json!({"request_id": request_id, "resolved": false, "reason": "question no longer pending"});
        if let Err(error) = ledger.complete(&request_id, &digest, result).await {
            crate::logging::warn(&error);
        }
        reply(
            client_event_tx,
            ServerEvent::Error {
                id,
                message: format!("ask request `{request_id}` is no longer pending; the answer was not delivered"),
                retry_after_secs: None,
            },
        );
        return;
    };
    if pending_ask.response_tx.is_closed() {
        let result = json!({"request_id": request_id, "resolved": false, "reason": "the tool stopped waiting"});
        if let Err(error) = ledger.complete(&request_id, &digest, result).await {
            crate::logging::warn(&error);
        }
        reply(
            client_event_tx,
            ServerEvent::Error {
                id,
                message: format!("ask request `{request_id}` is no longer pending; the tool stopped waiting for it"),
                retry_after_secs: None,
            },
        );
        return;
    }
    let resolved = resolved_answers(pending_ask.questions, &answers);
    let delivered = pending_ask.response_tx.send(resolved.clone()).is_ok();
    let result = json!({
        "request_id": request_id,
        "resolved": delivered,
        "answers": resolved
            .iter()
            .map(|answer| json!({"question_id": answer.question_id, "selected": answer.selected}))
            .collect::<Vec<Value>>(),
    });
    if let Err(error) = ledger.complete(&request_id, &digest, result).await {
        // The answer reached the tool, so it counts; only its receipt is in
        // doubt, and a retry must not deliver it a second time.
        crate::logging::warn(&error);
    }
    if !delivered {
        crate::logging::warn("the ask tool stopped waiting before its answer arrived");
    }
    reply(client_event_tx, ServerEvent::Ack { id });
}

fn resolved_answers(questions: Vec<AskQuestion>, answers: &[AskSelection]) -> Vec<AskAnswer> {
    questions
        .into_iter()
        .map(|question| {
            let question_id = question.id.clone().unwrap_or_else(|| "q1".to_string());
            let selected: Vec<String> = answers
                .iter()
                .filter(|answer| answer.question_id == question_id)
                .flat_map(|answer| answer.selected.iter().cloned())
                .collect();
            AskAnswer {
                question_id,
                question: question.question,
                selected,
                auto_resolved: false,
            }
        })
        .collect()
}

async fn answer_without_receipt(
    id: u64,
    request_id: String,
    answers: Vec<AskSelection>,
    pending: &PendingAsks,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let Some(pending_ask) = pending.lock().await.remove(&request_id) else {
        reply(
            client_event_tx,
            ServerEvent::Error {
                id,
                message: format!(
                    "ask request `{request_id}` is no longer pending; it already timed out, was answered, or its client disconnected"
                ),
                retry_after_secs: None,
            },
        );
        return;
    };
    // The tool drops its receiver when it stops waiting (its deadline passed or
    // the turn ended), so a late answer must be refused rather than acked: an
    // ack would tell the client its choice counted when nothing received it.
    if pending_ask.response_tx.is_closed() {
        reply(
            client_event_tx,
            ServerEvent::Error {
                id,
                message: format!("ask request `{request_id}` is no longer pending; the tool stopped waiting for it"),
                retry_after_secs: None,
            },
        );
        return;
    }
    let resolved = resolved_answers(pending_ask.questions, &answers);
    if pending_ask.response_tx.send(resolved).is_err() {
        crate::logging::warn("the ask tool stopped waiting before its answer arrived");
    }
    reply(client_event_tx, ServerEvent::Ack { id });
}

/// Record a client's answer to a queued permission request.
///
/// `bridge` is this connection's ACP permission bridge, if it installed one.
/// An answer to a request the bridge is waiting on also releases the waiting
/// call at once, and an answer to one it already settled is refused, so a
/// repeated answer can never change what the tool did.
pub(super) fn handle_permission_decision(
    id: u64,
    request_id: String,
    decision: PermissionDecision,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    bridge: Option<&PermissionBridge>,
) {
    if request_id.trim().is_empty() {
        reply(
            client_event_tx,
            ServerEvent::Error {
                id,
                message: "request_id must not be empty".to_string(),
                retry_after_secs: None,
            },
        );
        return;
    }
    // `allow_always` is an approval: this daemon has no persistent always-allow
    // store, and inventing one here would claim a rule the user never got.
    let approved = !matches!(decision, PermissionDecision::Deny);
    let bridge_waits = match bridge.map(|bridge| bridge.begin_editor_answer(&request_id)) {
        Some(EditorAnswer::AlreadyDecided) => {
            reply(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: format!("permission request `{request_id}` was already decided"),
                    retry_after_secs: None,
                },
            );
            return;
        }
        Some(EditorAnswer::Pending) => bridge,
        Some(EditorAnswer::Unknown) | None => None,
    };
    match crate::safety::record_permission_via_file(&request_id, approved, PERMISSION_DECISION_VIA, None) {
        Ok(()) => {
            if let Some(bridge) = bridge_waits {
                bridge.finish_editor_answer(&request_id, approved, PERMISSION_DECISION_VIA);
            }
            crate::logging::event_info(
                "server_permission_decision",
                vec![
                    ("id", id.to_string()),
                    ("request_id", request_id),
                    ("approved", approved.to_string()),
                ],
            );
            reply(client_event_tx, ServerEvent::Ack { id });
        }
        Err(error) => {
            if let Some(bridge) = bridge_waits {
                bridge.abandon_editor_answer(&request_id);
            }
            reply(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: format!(
                        "permission decision could not be recorded: {}",
                        truncate_detail(&error.to_string(), 200)
                    ),
                    retry_after_secs: None,
                },
            );
        }
    }
}

#[cfg(test)]
#[path = "client_ask_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "client_ask_receipt_tests.rs"]
mod receipt_tests;

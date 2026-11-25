use anyhow::Result;

use super::super::args::ApprovalVerdictArg;

/// Record a human verdict on the blocked approval `request_id`.
///
/// Three facts are durable: the request-level `outcome` (`human_retry` or
/// `human_skip`), which tells the resume runner the request is no longer
/// waiting, `tool_verdicts[tool_call_id]`, which names the exact call the
/// verdict applies to, and the `ApprovalResolved` event a client reads. One
/// request can cover several calls, so the per-call entry is what keeps the
/// other calls unaffected. The note is echoed back to the operator and written
/// to the log with the verdict.
///
/// The verdict is also written to the decision history, because the two readers
/// of a human verdict are different files. The resume runner reads the
/// blocked-approval record; a turn blocked inside
/// `request_permission(wait = true)` polls `history.json` through
/// [`crate::safety::SafetySystem::decision_for`] and never sees that record.
/// Writing only one of them leaves the live turn to apply its deny default while
/// the human verdict sits on disk.
pub(super) fn resolve_blocked_approval(
    store: &crate::ambient::BlockedApprovalStore,
    request_id: &str,
    tool_call_id: &str,
    verdict: ApprovalVerdictArg,
    note: Option<&str>,
) -> Result<()> {
    let outcome = match verdict {
        ApprovalVerdictArg::Retry => "human_retry",
        ApprovalVerdictArg::Skip => "human_skip",
    };
    // A retry approves the gated action; a skip refuses it. The live waiter
    // renders the decision as "Permission approved" or "Permission denied", so
    // an approved skip would tell the model to go ahead with the very call the
    // operator told it not to re-run.
    let approved = matches!(verdict, ApprovalVerdictArg::Retry);
    let tool_call_id_owned = tool_call_id.to_string();
    let event = crate::protocol::ServerEvent::ApprovalResolved {
        request_id: request_id.to_string(),
        approved,
        via: outcome.to_string(),
        resolved_by: "human".to_string(),
    };
    let pending = serde_json::to_value(&event)?;
    let recorded = store.update_with(request_id, move |record| {
        // The first verdict closes the request; later verdicts only add their
        // own call entry. A `needs-human` stop is the runner asking for this
        // verdict, not a decision, so the first verdict replaces it. A decision
        // that already closed the request (`resumed`, `denied`, ...) stays.
        match record.outcome.as_deref() {
            None | Some("needs-human") => record.outcome = Some(outcome.to_string()),
            Some(_) => {}
        }
        record.tool_verdicts.insert(tool_call_id_owned, outcome.to_string());
        // This CLI runs in its own process, with no client sink to hand the
        // event to, so it records the event the runner replays and a client
        // acknowledges. It replaces an un-acked earlier event for the same
        // request: clients dedupe on `request_id`, and the verdict is the
        // later and more specific word on how that request was resolved.
        record.pending_event = Some(pending);
    })?;
    if !recorded {
        anyhow::bail!("no blocked approval record for request id '{request_id}'; nothing was recorded");
    }

    // The record is written first and the decision second. A waiter that
    // observes the decision can then rely on the durable verdict already
    // existing, and a crash between the two writes leaves "record says verdict,
    // history has nothing": the runner resumes the session with the exact
    // per-call verdicts, and re-running this command completes the decision.
    // The other order would leave "history says approved, record says waiting",
    // which a runner pass can close as a terminal denial (a skip) and lose the
    // verdict for good.
    let system = crate::safety::SafetySystem::new();
    let message = human_verdict_message(verdict, tool_call_id, note);
    match system.record_decision(request_id, approved, outcome, Some(message)) {
        Ok(crate::safety::RecordDecisionOutcome::Recorded(decision)) => {
            // A corrupt history records a denial whatever was asked for, so the waiter
            // must be told what it will actually receive rather than what was asked.
            if decision.approved != approved {
                crate::logging::warn(&format!(
                    "human verdict for request {request_id} was recorded as a denial in the decision history ({})",
                    decision.decided_via
                ));
                println!(
                    "Note: the decision history recorded a denial for request {request_id} ({}); the approval record still carries {outcome}.",
                    decision.decided_via
                );
            }
        }
        Ok(crate::safety::RecordDecisionOutcome::AlreadyDecided { existing }) => {
            // A timeout or another approver got there first. The per-call
            // verdict is still the more specific word, and the runner reads it
            // before the request-level decision, so the record stands.
            crate::logging::warn(&format!(
                "human verdict for request {request_id} is in the approval record, but a decision already existed ({}); that decision stands in the history",
                existing.decided_via
            ));
            println!(
                "Note: a decision for request {request_id} was already recorded ({}); the approval record still carries this verdict.",
                existing.decided_via
            );
        }
        Err(error) => anyhow::bail!(
            "{outcome} is recorded in the approval record for request {request_id}, but the decision history could not be written, so a turn blocked on it will not wake: {error}. Re-run this command to complete the decision."
        ),
    }

    let note_log = match note {
        Some(note) => format!(" note={note}"),
        None => String::new(),
    };
    let note_out = match note {
        Some(note) => format!("\nNote: {note}"),
        None => String::new(),
    };
    crate::logging::info(&format!(
        "human verdict recorded: request={request_id} tool_call={tool_call_id} verdict={outcome}{note_log}",
    ));
    println!("Recorded {outcome} for request {request_id} (tool call {tool_call_id}).{note_out}",);
    Ok(())
}

/// The text the live waiter hands the model as its tool result.
///
/// `RequestPermissionTool` renders this as `Permission approved: ...` or
/// `Permission denied: ...`, so the wording has to carry the operator's
/// instruction: a retry approves the action, and a skip refuses it while the
/// turn continues after it.
fn human_verdict_message(verdict: ApprovalVerdictArg, tool_call_id: &str, note: Option<&str>) -> String {
    let (word, instruction) = match verdict {
        ApprovalVerdictArg::Retry => ("retry", "run exactly this call again, then continue"),
        ApprovalVerdictArg::Skip => ("skip", "do not run this call again, continue after it"),
    };
    match note {
        Some(note) => format!("human {word} for tool call {tool_call_id}: {instruction} (note: {note})"),
        None => format!("human {word} for tool call {tool_call_id}: {instruction}"),
    }
}

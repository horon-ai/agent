//! Blocked-approval resume: deciding, creating and running the one turn that
//! continues a run parked on an approval.
//!
//! The session's own event log is the only record of "this request was already
//! resumed"; the claim files here bound how many times the runner may try, so a
//! crash can leave no turn (and a retry) or one turn (and one execution), never
//! two turns.

use super::persistence::{
    BlockedApproval, BlockedApprovalStore, ExecClaimOutcome, ResumeClaim, ResumeClaimState, ResumeClaimStore,
    ResumeExecClaimStore, reconcile_orphan_requests,
};
use super::runner::AmbientRunnerHandle;
use crate::agent::Agent;
use crate::logging;
use crate::message::{ContentBlock, Role};
use crate::provider::Provider;
use crate::safety::Decision;
use crate::session::{Session, SessionStatus};
use crate::tool;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::Arc;

/// How many times a resume turn may be created for one request.
pub(super) const RESUME_ATTEMPT_CAP: u32 = 2;
/// How many times a resume turn that exists may be executed.
pub(super) const RESUME_EXEC_ATTEMPT_CAP: u32 = 2;
/// How long one process owns the execution of a created resume turn.
///
/// Overridable so a crash test can reclaim a dead owner's lease without waiting
/// out the production window.
pub(super) fn resume_exec_lease() -> chrono::Duration {
    std::env::var("HORON_AGENT_RESUME_EXEC_LEASE_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .map(chrono::Duration::seconds)
        .unwrap_or_else(|| chrono::Duration::minutes(30))
}
/// Line a resumed turn uses to report that it could not tell whether an
/// interrupted tool call took effect.
pub(super) const RESUME_UNVERIFIABLE_SENTINEL: &str = "[resume-unverifiable]";

impl AmbientRunnerHandle {
    /// Run one headless turn on an existing session, restoring it first.
    ///
    /// This is the single resume primitive: a scheduled reminder and a blocked
    /// approval both continue an existing transcript through it.
    pub(super) async fn resume_session_with_message(
        &self,
        provider: &Arc<dyn Provider>,
        session_id: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        let session = Session::load(session_id)?;
        let cycle_provider = provider.fork();
        let registry = tool::Registry::new(cycle_provider.clone()).await;

        let mut agent = Agent::new(cycle_provider, registry);
        agent.set_debug(session.is_debug);
        agent.restore_session(session_id)?;

        let _ = agent
            .run_once_capture_with_display_role(message, Some(crate::session::StoredDisplayRole::System))
            .await?;
        agent.mark_closed();
        Ok(())
    }

    /// Rebuild wait records a corrupt-store recovery may have dropped, and
    /// re-emit events a client still owes an acknowledgement for.
    ///
    /// Runs once per process, before the loop starts serving work.
    pub(super) async fn reconcile_blocked_approvals(&self) {
        let store = match BlockedApprovalStore::open_default() {
            Ok(store) => store,
            Err(error) => {
                logging::warn(&format!("Ambient runner: cannot open the approval records: {error:#}"));
                return;
            }
        };

        match reconcile_orphan_requests(&store, self.safety()) {
            Ok(report) if !report.is_empty() => logging::info(&format!(
                "Ambient runner: rebuilt {} blocked approval record(s) and expired {} orphaned request(s)",
                report.created.len(),
                report.expired.len()
            )),
            Ok(_) => {}
            Err(error) => logging::warn(&format!(
                "Ambient runner: failed to reconcile blocked approvals: {error:#}"
            )),
        }

        match store.list() {
            Ok(records) => {
                for record in records {
                    self.reemit_pending_event(&record);
                }
            }
            Err(error) => logging::warn(&format!("Ambient runner: cannot read the approval records: {error:#}")),
        }
    }

    /// Re-emit an event that was recorded durably but never acknowledged.
    ///
    /// `pending_event` is cleared only when a client acknowledges it, so a
    /// restart replays it here; consumers dedupe on `request_id`.
    fn reemit_pending_event(&self, record: &BlockedApproval) {
        let Some(pending) = record.pending_event.clone() else {
            return;
        };
        let event = match serde_json::from_value::<crate::protocol::ServerEvent>(pending) {
            Ok(event) => event,
            Err(error) => {
                logging::warn(&format!(
                    "Ambient runner: dropping an unreadable pending event for request {}: {error}",
                    record.request_id
                ));
                return;
            }
        };
        let delivered = matches!(
            crate::server::emit_session_event(&record.session_id, event),
            crate::server::EmitOutcome::Delivered
        );
        logging::info(&format!(
            "Ambient runner: re-emitted the pending event for request {} (delivered to a sink: {delivered})",
            record.request_id
        ));
    }

    /// Pick up blocked runs whose approval landed, at most once per request.
    pub(super) async fn resume_blocked_approvals(&self, provider: &Arc<dyn Provider>) {
        let store = match BlockedApprovalStore::open_default() {
            Ok(store) => store,
            Err(error) => {
                logging::warn(&format!("Ambient runner: cannot open the approval records: {error:#}"));
                return;
            }
        };
        let records = match store.list() {
            Ok(records) => records,
            Err(error) => {
                logging::warn(&format!("Ambient runner: cannot read the approval records: {error:#}"));
                return;
            }
        };
        if records.is_empty() {
            return;
        }

        let now = Utc::now();
        let pending: HashSet<String> = self
            .safety()
            .pending_requests()
            .into_iter()
            .map(|request| request.id)
            .collect();

        for record in records {
            if let Err(error) = self
                .resume_blocked_approval(provider, &store, &record, now, &pending)
                .await
            {
                logging::warn(&format!(
                    "Ambient runner: blocked approval {} could not be resumed: {error:#}",
                    record.request_id
                ));
            }
        }
    }

    /// Act on one wait record: leave it alone, close it, or resume it once.
    async fn resume_blocked_approval(
        &self,
        provider: &Arc<dyn Provider>,
        store: &BlockedApprovalStore,
        record: &BlockedApproval,
        now: DateTime<Utc>,
        pending: &HashSet<String>,
    ) -> anyhow::Result<()> {
        // `decision_for` also reads a decision another process wrote to disk.
        let decision = self.safety().decision_for(&record.request_id);
        let session = Session::load(&record.session_id).ok();
        let verdict = classify_blocked_approval(
            record,
            decision.as_ref(),
            pending.contains(&record.request_id),
            session.as_ref().map(|session| &session.status),
            now,
        );

        match verdict {
            ResumeVerdict::Idle => Ok(()),
            ResumeVerdict::Expire { reason } => {
                logging::info(&format!(
                    "Ambient runner: closing blocked approval {} without resuming: {reason}",
                    record.request_id
                ));
                // The safety sweep records the denial that closes the queue entry.
                self.safety().expire_dead_session_requests("ambient_runner_resume")?;
                store.close(&record.request_id, "expired")?;
                Ok(())
            }
            ResumeVerdict::Denied { via } => {
                // A denial is final: resuming it would re-run work a person refused.
                store.update_with(&record.request_id, |stored| {
                    stored.decided_at.get_or_insert(now);
                    stored.outcome = Some("denied".to_string());
                    stored.waiter_pid = None;
                    stored.waiter_lease_expires_at = None;
                })?;
                self.queue_approval_resolved(store, record, false, &via)?;
                logging::info(&format!(
                    "Ambient runner: request {} was denied; the run stays stopped",
                    record.request_id
                ));
                Ok(())
            }
            ResumeVerdict::Resume { origin } => {
                self.run_blocked_resume(provider, store, record, origin, decision.as_ref())
                    .await
            }
        }
    }

    /// Record an `ApprovalResolved` event durably, then emit it once.
    ///
    /// The record keeps the event until a client acknowledges it, so a crash
    /// between the write and the delivery replays it instead of losing it.
    fn queue_approval_resolved(
        &self,
        store: &BlockedApprovalStore,
        record: &BlockedApproval,
        approved: bool,
        via: &str,
    ) -> anyhow::Result<()> {
        let event = crate::protocol::ServerEvent::ApprovalResolved {
            request_id: record.request_id.clone(),
            approved,
            via: via.to_string(),
            resolved_by: "runner".to_string(),
        };
        let pending = serde_json::to_value(&event)?;
        store.update_with(&record.request_id, move |stored| {
            if stored.pending_event.is_none() {
                stored.pending_event = Some(pending);
            }
        })?;
        let _ = crate::server::emit_session_event(&record.session_id, event);
        Ok(())
    }

    /// Emit the `ApprovalResolved` a human verdict already recorded.
    ///
    /// The verdict is written by the CLI in its own process, which has no
    /// session sink, so it leaves the event in `pending_event`. On a
    /// `HumanVerdict` resume the record still carries the phase-1 file approval
    /// in `decision`, and building an event from that would tell a live client
    /// that a skip was an approval. Replaying the recorded event keeps the live
    /// delivery and the restart replay identical.
    fn emit_recorded_approval_resolved(
        &self,
        store: &BlockedApprovalStore,
        record: &BlockedApproval,
    ) -> anyhow::Result<()> {
        // Read again under the record lock: a client may have acknowledged the
        // event since this pass took its snapshot, and a second verdict may
        // have replaced it.
        let current = store.get(&record.request_id)?.unwrap_or_else(|| record.clone());
        // Only a human verdict event is replayable here. Anything else in the
        // slot (the phase-1 file approval an earlier pass recorded, or an
        // unreadable value) would say the wrong thing about this resume.
        let recorded = current
            .pending_event
            .clone()
            .and_then(|pending| serde_json::from_value::<crate::protocol::ServerEvent>(pending).ok())
            .filter(|event| {
                matches!(
                    event,
                    crate::protocol::ServerEvent::ApprovalResolved { via, resolved_by, .. }
                        if resolved_by == "human" && via.starts_with("human_")
                )
            });
        let event = recorded.unwrap_or_else(|| human_verdict_event(&current));
        let _ = crate::server::emit_session_event(&current.session_id, event);
        Ok(())
    }

    /// Create the resume turn for `record` if it does not exist, then run it.
    ///
    /// The recorded marker is what says whether the turn exists; the claims only
    /// bound how many times this process may try, so a crash anywhere in here
    /// leaves either no turn (and a retry) or one turn (and at most one
    /// execution).
    async fn run_blocked_resume(
        &self,
        provider: &Arc<dyn Provider>,
        store: &BlockedApprovalStore,
        record: &BlockedApproval,
        origin: ResumeOrigin,
        decision: Option<&Decision>,
    ) -> anyhow::Result<()> {
        let request_id = record.request_id.clone();
        let session_id = record.session_id.clone();
        // The recorded marker is what says "this request already produced a
        // resume turn". A run that stopped for a person already spent that turn,
        // so a human verdict's turn needs its own key; reusing the request id
        // would make the record report the verdict turn as already run.
        let resume_key = match origin {
            ResumeOrigin::HumanVerdict => format!("{request_id}:human-verdict"),
            _ => request_id.clone(),
        };

        let claims = ResumeClaimStore::open_default()?;
        let exec_claims = ResumeExecClaimStore::open_default()?;
        let mut claim = claims.acquire(&request_id)?;
        if matches!(claim.state, ResumeClaimState::Done) {
            return Ok(());
        }
        if matches!(claim.state, ResumeClaimState::NeedsHuman) {
            // The escalation stopped the automatic attempts and asked for a
            // person. A recorded verdict is that answer, so the claim becomes
            // usable again; without one the stop still holds.
            if record.tool_verdicts.is_empty() {
                return Ok(());
            }
            claim.state = ResumeClaimState::Claimed;
            claims.save(&claim)?;
        }
        #[cfg(test)]
        resume_failpoint("claimed");

        let mut recorded_at = Session::resume_key_recorded_at(&session_id, &resume_key).unwrap_or(None);
        if recorded_at.is_none() {
            if matches!(origin, ResumeOrigin::InlineApproved) {
                // The decision was applied inline and that turn stopped before
                // continuing; this is where the recovery picks the run up.
                #[cfg(test)]
                resume_failpoint("inline");
            }
            if claim.attempts >= RESUME_ATTEMPT_CAP {
                self.escalate_blocked_resume(
                    store,
                    &exec_claims,
                    record,
                    "the resume turn could not be created within the attempt budget",
                    claim.attempts,
                )?;
                claim.state = ResumeClaimState::NeedsHuman;
                claims.save(&claim)?;
                return Ok(());
            }

            claim.attempts += 1;
            claim.state = ResumeClaimState::Resuming;
            claim.claimed_by_pid = Some(std::process::id());
            claim.claimed_at = Utc::now();
            claims.save(&claim)?;
            #[cfg(test)]
            resume_failpoint("resuming");

            // Creating the turn and recording its key are one step: the append
            // returns only once the key is durable, so there is no state with a
            // turn that has no key, or a key that has no turn.
            Session::append_resume_key(&session_id, &resume_key)?;
            recorded_at = Session::resume_key_recorded_at(&session_id, &resume_key)?;
            #[cfg(test)]
            resume_failpoint("append");
        }

        let Some(recorded_at) = recorded_at else {
            anyhow::bail!("the resume key {resume_key} did not reach the session's event log");
        };

        let session = Session::load(&session_id)?;
        if session.status == SessionStatus::Active {
            // A live turn is already running this session; a second one would
            // duplicate the work.
            return Ok(());
        }
        if session.updated_at > recorded_at {
            // The turn was created and the session moved on afterwards, so it
            // ran. Only the bookkeeping is missing.
            return self.finish_blocked_resume(store, &claims, &exec_claims, claim, record);
        }

        match exec_claims.try_acquire(&request_id, Utc::now(), resume_exec_lease(), RESUME_EXEC_ATTEMPT_CAP)? {
            ExecClaimOutcome::Acquired(_) => {}
            ExecClaimOutcome::Busy(_) | ExecClaimOutcome::Done(_) => return Ok(()),
            ExecClaimOutcome::Exhausted(_) => {
                self.escalate_blocked_resume(
                    store,
                    &exec_claims,
                    record,
                    "the resume turn exists but could not be executed within the attempt budget",
                    claim.attempts,
                )?;
                claim.state = ResumeClaimState::NeedsHuman;
                claims.save(&claim)?;
                return Ok(());
            }
        }
        #[cfg(test)]
        resume_failpoint("exec-claimed");

        match origin {
            // A human verdict is the later and more specific word on how the
            // request was resolved, so it must reach a live client as it was
            // recorded. The decision here is the phase-1 file approval that let
            // the run reach `unverifiable`; it is not this resume's verdict.
            ResumeOrigin::HumanVerdict => self.emit_recorded_approval_resolved(store, record)?,
            _ => {
                if let Some(decision) = decision {
                    self.queue_approval_resolved(store, record, decision.approved, &decision.decided_via)?;
                } else if matches!(origin, ResumeOrigin::InlineApproved) {
                    self.queue_approval_resolved(store, record, true, "inline")?;
                }
            }
        }

        let prompt = build_resume_prompt(&session, record, &origin, decision);
        if let Err(error) = self.resume_session_with_message(provider, &session_id, &prompt).await {
            // The turn ran and this resume is spent: retrying would create a
            // second turn for one request. Free the execution lease so nothing
            // waits on an owner that is already gone.
            exec_claims.release(&request_id)?;
            logging::error(&format!(
                "Ambient runner: the resume turn for {request_id} failed: {error:#}"
            ));
            return self.finish_blocked_resume(store, &claims, &exec_claims, claim, record);
        }

        // A resumed turn that could not tell whether an interrupted side effect
        // happened must not advance: it ends here and waits for a person.
        let resumed = Session::load(&session_id)?;
        if let Some(report) = unverifiable_report(&resumed) {
            self.escalate_blocked_resume(store, &exec_claims, record, &report, claim.attempts)?;
            claim.state = ResumeClaimState::NeedsHuman;
            claims.save(&claim)?;
            return Ok(());
        }

        self.finish_blocked_resume(store, &claims, &exec_claims, claim, record)
    }

    /// Close a wait record as resumed once its turn exists and has run.
    fn finish_blocked_resume(
        &self,
        store: &BlockedApprovalStore,
        claims: &ResumeClaimStore,
        exec_claims: &ResumeExecClaimStore,
        mut claim: ResumeClaim,
        record: &BlockedApproval,
    ) -> anyhow::Result<()> {
        store.update_with(&record.request_id, |stored| {
            stored.resumed_at.get_or_insert_with(Utc::now);
            stored.outcome = Some("resumed".to_string());
            stored.waiter_pid = None;
            stored.waiter_lease_expires_at = None;
        })?;
        claim.state = ResumeClaimState::Done;
        claims.save(&claim)?;
        exec_claims.mark_done(&record.request_id)?;
        logging::info(&format!(
            "Ambient runner: resumed session {} once for request {}",
            record.session_id, record.request_id
        ));
        Ok(())
    }

    /// Hand a run that cannot be resumed automatically to a person.
    fn escalate_blocked_resume(
        &self,
        store: &BlockedApprovalStore,
        exec_claims: &ResumeExecClaimStore,
        record: &BlockedApproval,
        reason: &str,
        attempts: u32,
    ) -> anyhow::Result<()> {
        store.update_with(&record.request_id, |stored| {
            stored.outcome = Some("needs-human".to_string());
            stored.waiter_pid = None;
            stored.waiter_lease_expires_at = None;
            // The verdicts this stop ends have been acted on: the turn was
            // created, ran, and stopped. They must not survive, because a
            // non-empty `tool_verdicts` is the one thing that overrides
            // `needs-human` in `classify_blocked_approval`, so a spent verdict
            // would reopen this stop on every later pass. Only the verdicts the
            // stopped turn was built from (this snapshot) are removed: a
            // verdict written while that turn ran has a different value or a
            // new key, so it survives and still resumes the run. A verdict
            // recorded after this write lands in the emptied map and resumes
            // it too. This is the only path that clears them.
            stored
                .tool_verdicts
                .retain(|tool_call_id, verdict| record.tool_verdicts.get(tool_call_id) != Some(verdict));
        })?;
        exec_claims.mark_needs_human(&record.request_id)?;
        let event = crate::protocol::ServerEvent::RunNeedsHumanResume {
            // A resume stop always names the permission request it belongs to:
            // this path runs only for a `BlockedApproval` record, so the id is
            // real and not a placeholder.
            request_id: Some(record.request_id.clone()),
            session_id: record.session_id.clone(),
            reason: reason.to_string(),
            attempts,
        };
        let _ = crate::server::emit_session_event(&record.session_id, event);
        // Fire-and-forget through the configured channels; a failed notice never
        // blocks the runner.
        crate::notifications::notify_needs_human_resume(&record.request_id, &record.session_id, reason, attempts);
        logging::warn(&format!(
            "Ambient runner: request {} needs a human resume ({reason}, attempts {attempts})",
            record.request_id
        ));
        Ok(())
    }
}

/// The `ApprovalResolved` a human verdict produces, rebuilt when the recorded
/// copy is gone because a client already acknowledged it.
fn human_verdict_event(record: &BlockedApproval) -> crate::protocol::ServerEvent {
    let via = match record.outcome.as_deref() {
        Some(outcome @ ("human_retry" | "human_skip")) => outcome.to_string(),
        // The request-level outcome is written by the first verdict only; a
        // later verdict replaces the recorded event but not this field.
        _ => {
            if record.tool_verdicts.values().any(|verdict| verdict == "human_retry") {
                "human_retry".to_string()
            } else {
                "human_skip".to_string()
            }
        }
    };
    crate::protocol::ServerEvent::ApprovalResolved {
        request_id: record.request_id.clone(),
        approved: via == "human_retry",
        via,
        resolved_by: "human".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Blocked-approval resume decisions
// ---------------------------------------------------------------------------

/// How a blocked approval is being resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeOrigin {
    /// A recorded approval decision.
    Approved,
    /// An approval the waiting turn applied before it stopped.
    InlineApproved,
    /// A human verdict on the interrupted tool calls (`retry` / `skip`).
    HumanVerdict,
}

/// What the runner should do with one wait record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeVerdict {
    /// Nothing to do on this pass.
    Idle,
    /// Nothing can decide or resume it any more; close it without resuming.
    Expire { reason: String },
    /// The decision was a denial: close it and resume zero times.
    Denied { via: String },
    /// Resume the session exactly once.
    Resume { origin: ResumeOrigin },
}

/// Decide what one wait record needs, from the facts on disk alone.
///
/// `request_pending` is whether the safety queue still holds the request, and
/// `session_status` is `None` when the session no longer exists.
pub(crate) fn classify_blocked_approval(
    record: &BlockedApproval,
    decision: Option<&Decision>,
    request_pending: bool,
    session_status: Option<&SessionStatus>,
    now: DateTime<Utc>,
) -> ResumeVerdict {
    // A record that already reached a terminal outcome is finished, whatever
    // else changed since. A `needs-human` stop is not in this list: it is the
    // runner asking a person for a decision, and the verdict branch below is
    // that decision arriving.
    if let Some(outcome) = record.outcome.as_deref()
        && matches!(outcome, "resumed" | "denied" | "timeout_denied" | "expired")
    {
        return ResumeVerdict::Idle;
    }

    // A waiter whose lease is still live owns the record: stepping in now would
    // run the same work twice, and the lease holder would be superseded.
    if record.waiter_pid.is_some()
        && record
            .waiter_lease_expires_at
            .is_some_and(|expires_at| expires_at > now)
    {
        return ResumeVerdict::Idle;
    }

    // A human verdict on specific calls: `retry` and `skip` both continue the
    // session once, and the verdicts themselves say which calls to re-run.
    if !record.tool_verdicts.is_empty() {
        // The waiting turn consumed the verdict when the decision history
        // carries it and the record shows a decision applied inline: the tool
        // returned it as that call's result, and the turn went on. A session
        // that is still running continues with the verdict, and one that then
        // closed normally finished with it in hand; either way a resume turn
        // would run the named calls a second time. A crashed or errored session
        // is different: the turn stopped after the result and before it could
        // continue, so the resume is the recovery. Without the verdict in the
        // history the wait elapsed and applied its own default, so the turn
        // never saw it; without `inline_at` no turn was waiting at all. Both of
        // those still need this resume to deliver the verdict.
        let delivered_to_the_waiting_turn = record.inline_at.is_some()
            && decision.is_some_and(|decision| matches!(decision.decided_via.as_str(), "human_retry" | "human_skip"));
        if delivered_to_the_waiting_turn
            && matches!(
                session_status,
                Some(SessionStatus::Active) | Some(SessionStatus::Closed)
            )
        {
            return ResumeVerdict::Idle;
        }
        return ResumeVerdict::Resume {
            origin: ResumeOrigin::HumanVerdict,
        };
    }

    // A `needs-human` stop with no verdict still holds: the escalation asked for
    // a person, and a decision recorded before the stop or a request still in
    // the queue is not that answer.
    if record.outcome.as_deref() == Some("needs-human") {
        return ResumeVerdict::Idle;
    }

    if let Some(decision) = decision {
        return if decision.approved {
            ResumeVerdict::Resume {
                origin: ResumeOrigin::Approved,
            }
        } else {
            ResumeVerdict::Denied {
                via: decision.decided_via.clone(),
            }
        };
    }

    match record.inline_approved {
        // A denial applied inline is final. Resuming it would re-run work a
        // person refused.
        Some(false) => ResumeVerdict::Denied {
            via: "inline".to_string(),
        },
        // The decision was applied in the waiting turn. Only a turn that is no
        // longer running leaves the run to be picked up again.
        Some(true) => match session_status {
            Some(SessionStatus::Active) => ResumeVerdict::Idle,
            Some(_) => ResumeVerdict::Resume {
                origin: ResumeOrigin::InlineApproved,
            },
            None => ResumeVerdict::Expire {
                reason: "the session is gone".to_string(),
            },
        },
        None => {
            if !request_pending {
                // Nothing holds the request any more and no decision exists, so
                // no decision can ever arrive for it.
                ResumeVerdict::Expire {
                    reason: "the request is no longer pending and has no decision".to_string(),
                }
            } else if session_status.is_none() {
                ResumeVerdict::Expire {
                    reason: "the session is gone".to_string(),
                }
            } else {
                ResumeVerdict::Idle
            }
        }
    }
}

/// A tool call that started and never recorded a result.
struct UnresolvedToolCall {
    id: String,
    name: String,
    input: serde_json::Value,
}

/// Tool calls with no matching result in the transcript.
///
/// `ToolStarted` without `ToolFinished` means "this side effect may or may not
/// have happened", so these are exactly the calls the resumed turn must check
/// against the real world instead of repeating.
fn unresolved_tool_calls(session: &Session) -> Vec<UnresolvedToolCall> {
    let mut calls: Vec<UnresolvedToolCall> = Vec::new();
    let mut finished: HashSet<String> = HashSet::new();
    for message in session.active_context() {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, name, input, .. } => calls.push(UnresolvedToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    finished.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }
    calls.retain(|call| !finished.contains(&call.id));
    calls
}

/// The turn that resumes a blocked run, with everything the model needs to avoid
/// repeating work that may already have happened.
pub(super) fn build_resume_prompt(
    session: &Session,
    record: &BlockedApproval,
    origin: &ResumeOrigin,
    decision: Option<&Decision>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("[Resume after approval]\n");
    prompt.push_str(&format!(
        "Session {} stopped because a tool call needed a human decision (request {}).\n",
        record.session_id, record.request_id
    ));
    match origin {
        ResumeOrigin::Approved => {
            let via = decision.map(|decision| decision.decided_via.as_str()).unwrap_or("approval");
            prompt.push_str(&format!(
                "The decision is final: the work is approved (via {via}). Continue from where it stopped.\n"
            ));
        }
        ResumeOrigin::InlineApproved => prompt.push_str(
            "The decision was applied while the waiting turn was still running, and that turn stopped before continuing. Continue from where it stopped.\n",
        ),
        ResumeOrigin::HumanVerdict => prompt.push_str(
            "A person reviewed the interrupted work and left a verdict for each tool call listed below. Continue from where it stopped.\n",
        ),
    }
    prompt.push_str("\nRules for this turn:\n");
    prompt.push_str("- Read the transcript above before acting, and do not repeat steps that already completed.\n");

    let unresolved = unresolved_tool_calls(session);
    if unresolved.is_empty() {
        prompt.push_str("- No tool call is left in an unknown state.\n");
    } else {
        prompt.push_str(
            "- These calls started but never recorded a result, so whether their side effect happened is unknown:\n",
        );
        for call in &unresolved {
            prompt.push_str(&format!(
                "  - {}: {}({})\n",
                call.id,
                call.name,
                crate::util::truncate_str(&call.input.to_string(), 400)
            ));
        }
        prompt.push_str("- Do not run any of them again. Check the real world first (does the file exist, is the commit hash there, does the send log show it). A read-only call may be re-run.\n");
        prompt.push_str(&format!(
            "- If you cannot verify whether a listed call took effect, stop the turn and reply with a single line starting `{RESUME_UNVERIFIABLE_SENTINEL} <tool_call_id> <reason>`. Do not continue past that call.\n"
        ));
    }

    if !record.tool_verdicts.is_empty() {
        prompt.push_str("- Operator verdicts name exact calls (one request can cover several):\n");
        for (tool_call_id, verdict) in &record.tool_verdicts {
            let instruction = match verdict.as_str() {
                "human_retry" => "run exactly this call again, then continue",
                "human_skip" => "do not run this call again; continue after it",
                other => other,
            };
            prompt.push_str(&format!("  - {}: {} - {}\n", tool_call_id, verdict, instruction));
        }
    }

    prompt
}

/// What a resumed turn reported when it could not verify an interrupted call.
pub(super) fn unverifiable_report(session: &Session) -> Option<String> {
    let last = session
        .active_context()
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)?;
    for block in &last.content {
        if let ContentBlock::Text { text, .. } = block
            && let Some((_, rest)) = text.split_once(RESUME_UNVERIFIABLE_SENTINEL)
        {
            return Some(format!(
                "the resumed turn could not verify an interrupted tool call: {}",
                rest.trim()
            ));
        }
    }
    None
}

/// Crash-injection hook for the resume path.
///
/// Test-only: `cfg(test)` keeps the hook and every reference to it out of
/// non-test builds, so no shipped binary can be made to kill itself through the
/// environment. The crash tests set the variable in a child process running this
/// crate's own test binary and kill that process here, which leaves exactly the
/// on-disk state a `kill -9` would leave.
#[cfg(test)]
pub(super) fn resume_failpoint(point: &str) {
    if std::env::var("HORON_AGENT_RESUME_FAILPOINT").ok().as_deref() != Some(point) {
        return;
    }
    let pid = std::process::id().to_string();
    let _ = std::process::Command::new("kill").args(["-9", &pid]).status();
    std::process::exit(137);
}

// ---------------------------------------------------------------------------
// The live delivery of a human verdict, and a spent verdict
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, StreamEvent, ToolDefinition};
    use crate::provider::{EventStream, Provider};
    use crate::safety::SafetySystem;
    use crate::session::{Session, SessionStatus};
    use async_trait::async_trait;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};

    /// Isolates `~/.horon-agent` for one test and serializes env mutation.
    struct TestHome {
        previous: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TestHome {
        fn new() -> Self {
            let lock = crate::storage::lock_test_env();
            let dir = tempfile::tempdir().expect("temp home");
            let previous = std::env::var_os("HORON_AGENT_HOME");
            crate::env::set_var("HORON_AGENT_HOME", dir.path());
            crate::config::Config::invalidate_cache();
            Self {
                previous,
                _dir: dir,
                _lock: lock,
            }
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
                None => crate::env::remove_var("HORON_AGENT_HOME"),
            }
            crate::config::Config::invalidate_cache();
        }
    }

    /// Answers every turn with one fixed line.
    #[derive(Clone)]
    struct ReplyProvider {
        reply: Arc<StdMutex<String>>,
    }

    impl ReplyProvider {
        fn replying(text: &str) -> Self {
            Self {
                reply: Arc::new(StdMutex::new(text.to_string())),
            }
        }
    }

    #[async_trait]
    impl Provider for ReplyProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            let reply = self.reply.lock().expect("reply lock").clone();
            let stream = async_stream::stream! {
                yield Ok(StreamEvent::TextDelta(reply));
                yield Ok(StreamEvent::MessageEnd { stop_reason: None });
            };
            Ok(Box::pin(stream))
        }

        fn name(&self) -> &str {
            "reply"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(self.clone())
        }
    }

    fn closed_session(id: &str) -> Session {
        let mut session = Session::create_with_id(id.to_string(), None, Some("Blocked run".to_string()));
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "Do the risky job".to_string(),
                cache_control: None,
            }],
        );
        // A call that started and never recorded a result: the resume prompt
        // names it and the turn is allowed to report it unverifiable.
        session.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "call_risky_write".to_string(),
                name: "write".to_string(),
                input: json!({"path": "notes/out.md", "content": "x"}),
                thought_signature: None,
            }],
        );
        session.status = SessionStatus::Closed;
        session.save().expect("save session");
        session
    }

    fn record(session_id: &str, request_id: &str) -> BlockedApproval {
        BlockedApproval {
            session_id: session_id.to_string(),
            request_id: request_id.to_string(),
            created_at: Utc::now(),
            ..BlockedApproval::default()
        }
    }

    fn store() -> BlockedApprovalStore {
        BlockedApprovalStore::open_default().expect("approval store")
    }

    fn runner() -> AmbientRunnerHandle {
        AmbientRunnerHandle::new(Arc::new(SafetySystem::new()))
    }

    /// Register the sink a connected client's fanout uses for this session.
    fn event_sink(session_id: &str) -> tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ServerEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        crate::server::register_session_event_sink(session_id, tx);
        rx
    }

    fn drain(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ServerEvent>,
    ) -> Vec<crate::protocol::ServerEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn resume_turns(session_id: &str) -> usize {
        Session::load(session_id)
            .expect("load session")
            .active_context()
            .iter()
            .filter(|message| {
                message.content.iter().any(
                    |block| matches!(block, ContentBlock::Text { text, .. } if text.starts_with("[Resume after approval]")),
                )
            })
            .count()
    }

    /// The CLI writes this event in its own process, with no sink of its own.
    fn cli_human_event(request_id: &str, verdict: &str) -> crate::protocol::ServerEvent {
        crate::protocol::ServerEvent::ApprovalResolved {
            request_id: request_id.to_string(),
            approved: verdict == "human_retry",
            via: verdict.to_string(),
            resolved_by: "human".to_string(),
        }
    }

    /// A skip must reach a connected client as a skip.
    ///
    /// The resume's `decision` is the phase-1 file approval that let the run
    /// reach `unverifiable`, not this resume's verdict; building the live event
    /// from it told every attached client that a skip was an approval while the
    /// durable record kept the human event. Only the live delivery was wrong,
    /// which is why a scenario reading `pending_event` from disk cannot see it.
    #[tokio::test]
    async fn a_human_skip_reaches_a_live_client_as_a_skip() {
        let _home = TestHome::new();
        let session = closed_session("sess_live_human_skip");
        let request_id = "req_live_human_skip";

        let mut record = record(&session.id, request_id);
        record.outcome = Some("human_skip".to_string());
        record
            .tool_verdicts
            .insert("call_risky_write".to_string(), "human_skip".to_string());
        record.pending_event = Some(
            serde_json::to_value(cli_human_event(request_id, "human_skip")).expect("serialize the recorded event"),
        );
        store().add(record).expect("add record");

        let runner = runner();
        // The phase-1 approval is still on disk, so the decision the resume
        // sees is present and would win over the verdict if it were used.
        runner
            .safety()
            .record_decision(request_id, true, "file", None)
            .expect("record the phase-1 approval");
        assert!(
            runner.safety().decision_for(request_id).is_some(),
            "precondition: the phase-1 file approval is readable"
        );

        let mut events = event_sink(&session.id);
        let provider: Arc<dyn Provider> = Arc::new(ReplyProvider::replying("skipped and continued"));
        runner.resume_blocked_approvals(&provider).await;

        assert_eq!(resume_turns(&session.id), 1, "the verdict continues the run once");
        let emitted: Vec<_> = drain(&mut events)
            .into_iter()
            .filter(|event| matches!(event, crate::protocol::ServerEvent::ApprovalResolved { .. }))
            .collect();
        assert_eq!(
            emitted.len(),
            1,
            "exactly one ApprovalResolved reaches the client: {emitted:?}"
        );
        match &emitted[0] {
            crate::protocol::ServerEvent::ApprovalResolved {
                approved,
                via,
                resolved_by,
                ..
            } => {
                assert!(!*approved, "a skip is not an approval");
                assert_eq!(
                    via, "human_skip",
                    "the live event carries the human verdict, not the phase-1 file approval"
                );
                assert_eq!(resolved_by, "human");
            }
            other => panic!("expected an ApprovalResolved, got {other:?}"),
        }

        // The durable record still holds the same human event, so a restart
        // replay and the live delivery now agree.
        let stored = store().get(request_id).expect("read record").expect("record present");
        assert_eq!(
            stored.pending_event.as_ref().and_then(|event| event.get("via")),
            Some(&json!("human_skip")),
            "the durable record keeps the human event"
        );
        assert_eq!(stored.outcome.as_deref(), Some("resumed"));
    }

    /// A verdict that already drove a turn cannot reopen the stop it ended.
    ///
    /// The escalation writes `needs-human`, and a non-empty `tool_verdicts` is
    /// the one thing that overrides that in the classifier, so the spent verdict
    /// would resume the run again on every later pass. A verdict recorded after
    /// the escalation must still be acted on.
    #[tokio::test]
    async fn an_escalation_spends_the_verdict_that_drove_it() {
        let _home = TestHome::new();
        let session = closed_session("sess_spent_verdict");
        let request_id = "req_spent_verdict";

        let mut record = record(&session.id, request_id);
        record.outcome = Some("human_skip".to_string());
        record
            .tool_verdicts
            .insert("call_risky_write".to_string(), "human_skip".to_string());
        store().add(record).expect("add record");

        let runner = runner();
        let provider: Arc<dyn Provider> = Arc::new(ReplyProvider::replying(
            "[resume-unverifiable] call_risky_write no trace on disk",
        ));
        runner.resume_blocked_approvals(&provider).await;

        assert_eq!(resume_turns(&session.id), 1, "the verdict turn runs once, then stops");
        let stopped = store().get(request_id).expect("read record").expect("record present");
        assert_eq!(stopped.outcome.as_deref(), Some("needs-human"));
        assert!(
            stopped.tool_verdicts.is_empty(),
            "the verdict the stopped turn acted on is spent: {:?}",
            stopped.tool_verdicts
        );

        let verdict = classify_blocked_approval(&stopped, None, false, Some(&SessionStatus::Closed), Utc::now());
        assert_eq!(verdict, ResumeVerdict::Idle, "a spent verdict must not reopen the stop");

        // The clear must not swallow a verdict recorded afterwards: a new one
        // still makes the record resumable.
        store()
            .update_with(request_id, |stored| {
                stored.outcome = Some("human_retry".to_string());
                stored
                    .tool_verdicts
                    .insert("call_risky_write".to_string(), "human_retry".to_string());
            })
            .expect("record a new verdict");
        let after = store().get(request_id).expect("read record").expect("record present");
        assert_eq!(
            classify_blocked_approval(&after, None, false, Some(&SessionStatus::Closed), Utc::now()),
            ResumeVerdict::Resume {
                origin: ResumeOrigin::HumanVerdict
            },
            "a verdict recorded after the escalation must still resume the run"
        );
    }
}

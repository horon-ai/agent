//! Plan mode: a session-scoped read-only state.
//!
//! While a session is planning, only calls whose declared effects carry no
//! barrier bit may run. The state lives in a process-global map keyed by
//! session id, not on `Registry`: `Registry::Clone` builds a fresh
//! `CompactionManager` per clone, so a registry field would be copied into every
//! per-call and per-subagent clone instead of following the session.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::tool::ToolEffects;

/// Where a session is in the plan-mode state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanState {
    /// Plan mode is not in use: every call runs as it does today.
    Off,
    /// Read-only: the model is working out a plan.
    Planning,
    /// Read-only: a plan was submitted and the user has not decided yet.
    PendingApproval,
    /// The plan was approved, so effects are allowed again.
    Approved,
}

impl PlanState {
    /// Stable name used in session replay events.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Planning => "planning",
            Self::PendingApproval => "pending_approval",
            Self::Approved => "approved",
        }
    }

    /// Parse a name written by [`Self::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "off" => Some(Self::Off),
            "planning" => Some(Self::Planning),
            "pending_approval" => Some(Self::PendingApproval),
            "approved" => Some(Self::Approved),
            _ => None,
        }
    }

    /// Whether this state refuses calls that carry a barrier effect.
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Planning | Self::PendingApproval)
    }
}

/// A plan the model submitted and the user has not answered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedPlan {
    /// Permission request that carries the approval.
    pub request_id: String,
    /// The plan text, verbatim.
    pub text: String,
    /// Files the plan intends to touch.
    pub files: Vec<String>,
}

static PLAN_STATES: OnceLock<Mutex<HashMap<String, PlanState>>> = OnceLock::new();
static PENDING_PLANS: OnceLock<Mutex<HashMap<String, SubmittedPlan>>> = OnceLock::new();
static APPROVED_PLANS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn states() -> &'static Mutex<HashMap<String, PlanState>> {
    PLAN_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_plans() -> &'static Mutex<HashMap<String, SubmittedPlan>> {
    PENDING_PLANS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn approved_plans() -> &'static Mutex<HashMap<String, String>> {
    APPROVED_PLANS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The session's current state; a session with no entry is [`PlanState::Off`].
pub fn state_for(session_id: &str) -> PlanState {
    states()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .copied()
        .unwrap_or(PlanState::Off)
}

/// Set the session's state. [`PlanState::Off`] removes the entry rather than
/// storing it, so the map only holds sessions that are actually planning.
pub fn set_state(session_id: &str, state: PlanState) {
    let mut states = states().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if state == PlanState::Off {
        states.remove(session_id);
    } else {
        states.insert(session_id.to_string(), state);
    }
}

/// Forget everything recorded for `session_id`: the state, a plan awaiting an
/// answer, and the plan text pinned by an earlier approval.
pub fn clear_state(session_id: &str) {
    states()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
    pending_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
    approved_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
}

/// Record a submitted plan that is waiting for the user's answer.
pub fn record_submission(session_id: &str, plan: SubmittedPlan) {
    pending_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(session_id.to_string(), plan);
}

/// Drop the pending plan without touching the state.
///
/// Used to roll back a submission whose approval request could not be queued: the
/// plan was recorded before the enqueue so that a parked session always has one,
/// and that record has to go if the enqueue never happened.
pub fn clear_submission(session_id: &str) {
    pending_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
}

/// The plan this session submitted and the user has not answered yet.
pub fn pending_plan(session_id: &str) -> Option<SubmittedPlan> {
    pending_plans().lock().ok()?.get(session_id).cloned()
}

/// Approve the pending plan: pin its text for the rest of the session and
/// reopen writes.
pub fn approve_pending_plan(session_id: &str) -> Option<SubmittedPlan> {
    let plan = pending_plans().lock().ok()?.remove(session_id)?;
    approved_plans()
        .lock()
        .ok()?
        .insert(session_id.to_string(), plan.text.clone());
    set_state(session_id, PlanState::Approved);
    Some(plan)
}

/// Reject the pending plan: drop it and go back to planning, still read-only,
/// so the model revises instead of proceeding.
pub fn reject_pending_plan(session_id: &str) -> Option<SubmittedPlan> {
    let plan = pending_plans().lock().ok()?.remove(session_id)?;
    set_state(session_id, PlanState::Planning);
    Some(plan)
}

/// The approved plan pinned for this session, if any.
pub fn approved_plan(session_id: &str) -> Option<String> {
    approved_plans().lock().ok()?.get(session_id).cloned()
}

/// Apply an approval decision that arrived through the permission queue rather
/// than through `/plan`.
///
/// The decision names only a request id, so the owning session is found by
/// looking for the submission that request carries. Returns the session whose
/// state moved.
pub fn observe_decision(request_id: &str, approved: bool) -> Option<String> {
    let session_id = pending_plans()
        .lock()
        .ok()?
        .iter()
        .find(|(_, plan)| plan.request_id == request_id)
        .map(|(session, _)| session.clone())?;
    if approved {
        approve_pending_plan(&session_id).map(|_| session_id)
    } else {
        reject_pending_plan(&session_id).map(|_| session_id)
    }
}

/// Whether a call declaring `effects` may run in this session's current state.
///
/// The read-only states allow exactly what [`ToolEffects::parallel_safe`]
/// allows. A tool that declares nothing is refused, because the trait default
/// is a write barrier.
pub fn allows_effects(session_id: &str, effects: ToolEffects) -> bool {
    match state_for(session_id) {
        PlanState::Off | PlanState::Approved => true,
        PlanState::Planning | PlanState::PendingApproval => effects.parallel_safe(),
    }
}

/// The refusal shown to the model when plan mode blocks a call: what was
/// refused, why, and the two ways out.
pub fn block_message(tool: &str) -> String {
    format!(
        "Tool '{tool}' was refused: plan mode is read-only right now, so no call may write a \
         file, append to one, or start a process.\n\n\
         This call did not run and nothing changed. Read-only calls (file reads, searches, \
         network fetches) still work.\n\n\
         To proceed, either submit the plan for the user's approval, or leave plan mode. \
         Retrying the same call changes nothing."
    )
}

/// Move a session to `state` and record the transition on its replay timeline.
pub fn transition(session: &mut crate::session::Session, state: PlanState) {
    set_state(&session.id, state);
    session.record_plan_mode_event(state.as_str().to_string());
}

/// Shown when a command needs plan mode but `[tools] plan_mode` is off.
const PLAN_MODE_DISABLED: &str =
    "Plan mode is off for this runtime. Set `plan_mode = true` under `[tools]` in config.toml first.";

/// `/plan` and its arguments, executed through the existing slash-command channel.
pub fn command(session_id: &str, args: &[String]) -> anyhow::Result<(serde_json::Value, String)> {
    use anyhow::bail;

    if args.len() > 1 {
        bail!("Usage: /plan [on|off|status|approve|reject]");
    }
    let enabled = crate::config::config().tools.plan_mode;
    match args.first().map(|arg| arg.trim().to_ascii_lowercase()).as_deref() {
        None | Some("status") | Some("show") => Ok((snapshot(session_id, enabled), report(session_id, enabled))),
        Some("on") | Some("enable") | Some("enabled") => {
            if !enabled {
                bail!(PLAN_MODE_DISABLED);
            }
            set_state(session_id, PlanState::Planning);
            Ok((snapshot(session_id, enabled), report(session_id, enabled)))
        }
        Some("off") | Some("disable") | Some("disabled") => {
            clear_state(session_id);
            Ok((snapshot(session_id, enabled), report(session_id, enabled)))
        }
        Some("approve") => {
            if !enabled {
                bail!(PLAN_MODE_DISABLED);
            }
            let Some(plan) = approve_pending_plan(session_id) else {
                bail!("No plan is waiting for approval in this session. Call submit_plan first, then approve.");
            };
            // Best effort: the queued request is answered so it does not linger
            // until its wait window closes. The plan itself is already approved.
            let _ = crate::tool::ambient::shared_safety_system().record_decision(
                &plan.request_id,
                true,
                "plan_command",
                Some("Approved with /plan approve".to_string()),
            );
            Ok((
                snapshot(session_id, enabled),
                format!(
                    "Approved the plan. {} characters are now pinned as this session's execution context.",
                    plan.text.chars().count()
                ),
            ))
        }
        Some("reject") => {
            let Some(plan) = reject_pending_plan(session_id) else {
                bail!("No plan is waiting for approval in this session. Nothing was rejected.");
            };
            let _ = crate::tool::ambient::shared_safety_system().record_decision(
                &plan.request_id,
                false,
                "plan_command",
                Some("Rejected with /plan reject".to_string()),
            );
            Ok((
                snapshot(session_id, enabled),
                "Rejected the plan. The session is still read-only, so the next plan can be submitted.".to_string(),
            ))
        }
        Some(other) => bail!("Unknown /plan action '{other}'. Expected on, off, status, approve, or reject."),
    }
}

fn snapshot(session_id: &str, enabled: bool) -> serde_json::Value {
    let state = state_for(session_id);
    let pending = pending_plan(session_id);
    let approved = approved_plan(session_id);
    serde_json::json!({
        "enabled": enabled,
        "state": state.as_str(),
        "read_only": state.is_read_only(),
        "pending_request_id": pending.as_ref().map(|plan| plan.request_id.clone()),
        "pending_plan_chars": pending.as_ref().map(|plan| plan.text.chars().count()),
        "pending_files": pending.as_ref().map(|plan| plan.files.clone()),
        "approved_plan_chars": approved.as_ref().map(|text| text.chars().count()),
    })
}

fn report(session_id: &str, enabled: bool) -> String {
    let state = state_for(session_id);
    let pending = pending_plan(session_id);
    let approved = approved_plan(session_id);
    let mut report = format!(
        "Plan mode is {}. This session is '{}'{}.",
        if enabled { "on" } else { "off" },
        state.as_str(),
        if state.is_read_only() {
            " and read-only: writes and process launches are refused"
        } else {
            ""
        }
    );
    if let Some(plan) = pending {
        report.push_str(&format!(
            "\n\nWaiting for an answer: request {} carries {} characters of plan text for {} file(s). Use `/plan approve` or `/plan reject`.",
            plan.request_id,
            plan.text.chars().count(),
            plan.files.len()
        ));
    }
    if let Some(text) = approved {
        report.push_str(&format!(
            "\n\nAn approved plan of {} characters is pinned as this session's execution context.",
            text.chars().count()
        ));
    }
    report
}

#[cfg(test)]
#[path = "plan_mode_tests.rs"]
mod tests;

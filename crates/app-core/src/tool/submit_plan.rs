//! `submit_plan`: hand a plan to the user for approval while plan mode is on.
//!
//! The tool writes no file. It moves the session to
//! [`crate::plan_mode::PlanState::PendingApproval`], queues one
//! [`PermissionRequest`] on the existing approval queue, and alerts the user.
//! Effects are [`ToolEffects::read`] on purpose: plan mode refuses every barrier
//! call, and a submission has to run while the session is still read-only.

use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use crate::plan_mode::{self, PlanState, SubmittedPlan};
use crate::safety::{self, ApprovalDefault, PermissionRequest, PermissionResult, Urgency};
use anyhow::{Result, bail};
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

pub const SUBMIT_PLAN_TOOL: &str = crate::config::PLAN_TOOL;

pub const MAX_PLAN_BYTES: usize = 256 * 1024;
pub const MAX_PLAN_FILES: usize = 128;
pub const MAX_PLAN_FILE_BYTES: usize = 1024;

const PLAN_ACTION: &str = "submit_plan";

pub struct SubmitPlanTool;

impl SubmitPlanTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SubmitPlanTool {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn enabled() -> bool {
    crate::config::config().tools.plan_mode
}

#[derive(Deserialize)]
struct SubmitPlanInput {
    plan: String,
    #[serde(default)]
    files: Vec<String>,
}

impl SubmitPlanInput {
    fn validate(self) -> Result<Self> {
        if self.plan.trim().is_empty() {
            bail!("submit_plan needs a plan: write the plan out in the `plan` argument.");
        }
        if self.plan.len() > MAX_PLAN_BYTES {
            bail!(
                "The plan is {} bytes, which is over the {} KiB limit. Shorten it, or name the \
                 files it lives in and keep the plan in the conversation.",
                self.plan.len(),
                MAX_PLAN_BYTES / 1024
            );
        }
        if self.files.len() > MAX_PLAN_FILES {
            bail!(
                "The plan names {} files, which is over the {} file limit.",
                self.files.len(),
                MAX_PLAN_FILES
            );
        }
        if let Some(long) = self.files.iter().find(|file| file.len() > MAX_PLAN_FILE_BYTES) {
            bail!(
                "A plan file path is {} bytes, which is over the {} byte limit.",
                long.len(),
                MAX_PLAN_FILE_BYTES
            );
        }
        Ok(self)
    }
}

fn request_description(input: &SubmitPlanInput) -> String {
    let files = match input.files.len() {
        0 => String::new(),
        1 => format!(" and 1 file ({})", input.files[0]),
        count => format!(" and {count} files"),
    };
    format!(
        "Approve a plan of {} characters{} before any file changes",
        input.plan.chars().count(),
        files
    )
}

#[async_trait]
impl Tool for SubmitPlanTool {
    fn name(&self) -> &str {
        SUBMIT_PLAN_TOOL
    }

    fn description(&self) -> &str {
        "Submit a plan for the user's approval. Plan mode is read-only until the plan is approved, \
         and an approved plan is pinned as this session's execution context."
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["plan"],
            "properties": {
                "intent": super::intent_schema_property(),
                "plan": {
                    "type": "string",
                    "description": "The plan, in full: what changes, in which order, and how it will be verified. At most 256 KiB."
                },
                "files": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Files the plan intends to touch. At most 128 entries, each at most 1024 bytes."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        if !enabled() {
            bail!(
                "submit_plan is unavailable: plan mode is off for this runtime. Turn it on with \
                 `plan_mode = true` under `[tools]` in config.toml."
            );
        }

        match plan_mode::state_for(&ctx.session_id) {
            PlanState::Off => {
                bail!("This session is not in plan mode. Start it with `/plan on`, then submit the plan.")
            }
            PlanState::PendingApproval => bail!(
                "A plan is already waiting for an answer in this session. Wait for `/plan approve` \
                 or `/plan reject` instead of submitting a second one."
            ),
            PlanState::Planning | PlanState::Approved => {}
        }

        let parsed: SubmitPlanInput = serde_json::from_value(input)?;
        let input = parsed.validate()?;
        let description = request_description(&input);
        let now = Utc::now();
        let request = PermissionRequest {
            id: safety::new_request_id(),
            action: PLAN_ACTION.to_string(),
            description: description.clone(),
            rationale: "Plan mode keeps the working tree read-only until the user approves the \
                        plan, so the approval is the point at which work may start."
                .to_string(),
            urgency: Urgency::Normal,
            // `wait: false` is deliberate: the answer arrives through the
            // approval surface or `/plan approve` after this call returns, so a
            // blocking submission would hold the turn open for it.
            wait: false,
            created_at: now,
            context: Some(json!({
                "session_id": ctx.session_id,
                "message_id": ctx.message_id,
                "tool_call_id": ctx.tool_call_id,
                "working_dir": ctx.working_dir.as_ref().map(|dir| dir.display().to_string()),
                "requested_at": now.to_rfc3339(),
                "files": input.files,
                "plan_chars": input.plan.chars().count(),
            })),
            run_id: None,
            expires_at: None,
            default_on_timeout: ApprovalDefault::Deny,
        };

        // Plan text first, then state, then queue, then alert. The plan has to
        // exist before the session parks in `PendingApproval`: if the enqueue
        // fails, a session left in that state with no pending plan can never be
        // released by `/plan approve`, and an approver who answers the alert at
        // once must still find the plan by request id.
        plan_mode::record_submission(
            &ctx.session_id,
            SubmittedPlan {
                request_id: request.id.clone(),
                text: input.plan.clone(),
                files: input.files.clone(),
            },
        );
        let previous_state = plan_mode::state_for(&ctx.session_id);
        plan_mode::set_state(&ctx.session_id, PlanState::PendingApproval);
        let request_id = match crate::tool::ambient::shared_safety_system().request_permission(request) {
            PermissionResult::Queued { request_id } => request_id,
            // Defensive: `request_permission` reports `Queued` unconditionally
            // today and has no failure return, so this arm is unreachable. It
            // stays so a future failure return cannot leave the session parked in
            // `PendingApproval` with nothing to approve - the rollback is the
            // invariant, not a tested path.
            other => {
                plan_mode::clear_submission(&ctx.session_id);
                // Restore the state the session actually came from, not a guess:
                // a session that was already `Approved` must not be demoted to
                // read-only by a queue failure it never caused.
                plan_mode::set_state(&ctx.session_id, previous_state);
                bail!("The plan could not be queued for approval: {other:?}");
            }
        };
        safety::dispatch_permission_notification(PLAN_ACTION, &description, &request_id);
        // The queued id is authoritative; the pre-queue placeholder is replaced.
        plan_mode::record_submission(
            &ctx.session_id,
            SubmittedPlan {
                request_id: request_id.clone(),
                text: input.plan.clone(),
                files: input.files.clone(),
            },
        );

        let metadata = json!({
            "request_id": request_id,
            "state": PlanState::PendingApproval.as_str(),
            "plan_chars": input.plan.chars().count(),
            "files": input.files,
        });
        Ok(ToolOutput::new(format!(
            "The plan is queued for approval as request {request_id}. This session stays \
             read-only until the user answers: `/plan approve` pins the plan and reopens writes, \
             `/plan reject` sends it back for revision. Nothing has changed on disk."
        ))
        .with_title("Plan submitted for approval")
        .with_metadata(metadata))
    }
}

#[cfg(test)]
#[path = "submit_plan_tests.rs"]
mod tests;

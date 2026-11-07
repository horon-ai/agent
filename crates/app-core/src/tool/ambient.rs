use super::{Tool, ToolContext, ToolOutput};
use crate::ambient::{
    AmbientCycleResult, AmbientManager, AmbientState, BlockedApproval, BlockedApprovalStore, CycleStatus, Priority,
    ScheduleRequest, ScheduleTarget, ScheduledItem,
};
use crate::ambient_runner::AmbientRunnerHandle;
use crate::config::AmbientConfig;
use crate::protocol::ServerEvent;
use crate::safety::{self, ApprovalDefault, Decision, PermissionRequest, PermissionResult, SafetySystem, Urgency};
use crate::server::{EmitOutcome, emit_session_event};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Global state for ambient tools
// ---------------------------------------------------------------------------

/// Global ambient cycle result, set by EndAmbientCycleTool for the ambient
/// runner to collect after the cycle completes.
static AMBIENT_CYCLE_RESULT: OnceLock<Mutex<Option<AmbientCycleResult>>> = OnceLock::new();

fn cycle_result_slot() -> &'static Mutex<Option<AmbientCycleResult>> {
    AMBIENT_CYCLE_RESULT.get_or_init(|| Mutex::new(None))
}

/// Store a cycle result for the ambient runner to pick up.
pub fn store_cycle_result(result: AmbientCycleResult) {
    if let Ok(mut slot) = cycle_result_slot().lock() {
        *slot = Some(result);
    }
}

/// Take the stored cycle result (returns None if not set or already taken).
pub fn take_cycle_result() -> Option<AmbientCycleResult> {
    cycle_result_slot().lock().ok().and_then(|mut slot| slot.take())
}

/// Global SafetySystem instance shared with ambient tools.
static SAFETY_SYSTEM: OnceLock<Arc<SafetySystem>> = OnceLock::new();
static SHARED_SAFETY_SYSTEM: OnceLock<Arc<SafetySystem>> = OnceLock::new();
/// Shared schedule/ambient runner handle used to wake the background loop after
/// queue changes.
static SCHEDULE_RUNNER: OnceLock<Mutex<Option<AmbientRunnerHandle>>> = OnceLock::new();
/// Session IDs currently allowed to use ambient-only permission workflows.
static AMBIENT_SESSION_IDS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub fn init_safety_system(system: Arc<SafetySystem>) {
    let _ = SAFETY_SYSTEM.set(system);
}

pub fn init_schedule_runner(handle: AmbientRunnerHandle) {
    if let Ok(mut slot) = SCHEDULE_RUNNER.get_or_init(|| Mutex::new(None)).lock() {
        *slot = Some(handle);
    }
}

fn get_safety_system() -> Arc<SafetySystem> {
    SAFETY_SYSTEM
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(SafetySystem::new()))
}

/// The process-wide safety system, for callers outside this module that queue
/// or answer a permission request on the same queue the ambient tools use.
///
/// Unlike the ambient accessor this never mints a fresh instance per call: a
/// request queued through one call has to be answerable through the next.
pub fn shared_safety_system() -> Arc<SafetySystem> {
    if let Some(system) = SAFETY_SYSTEM.get() {
        return system.clone();
    }
    SHARED_SAFETY_SYSTEM
        .get_or_init(|| Arc::new(SafetySystem::new()))
        .clone()
}

fn ambient_session_ids() -> &'static Mutex<HashSet<String>> {
    AMBIENT_SESSION_IDS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mark a session ID as ambient-enabled for ambient-only tooling.
pub fn register_ambient_session(session_id: impl Into<String>) {
    if let Ok(mut ids) = ambient_session_ids().lock() {
        ids.insert(session_id.into());
    }
}

/// Remove a session ID from the ambient-enabled set.
pub fn unregister_ambient_session(session_id: &str) {
    if let Ok(mut ids) = ambient_session_ids().lock() {
        ids.remove(session_id);
    }
}

fn is_ambient_session_registered(session_id: &str) -> bool {
    ambient_session_ids()
        .lock()
        .map(|ids| ids.contains(session_id))
        .unwrap_or(false)
}

/// Ambient-only gate for the permission workflow.
///
/// `allow_clientless` is `[ambient] approval_for_clientless_runs`: an unattended
/// run with no attached client never gets registered as an ambient session, so
/// the operator can opt it in. With the flag off the error stays byte-identical
/// to the original message.
fn ensure_ambient_session(ctx: &ToolContext, allow_clientless: bool) -> Result<()> {
    if allow_clientless || is_ambient_session_registered(&ctx.session_id) {
        Ok(())
    } else {
        anyhow::bail!(
            "request_permission is only available to ambient sessions (session '{}')",
            ctx.session_id
        )
    }
}

// ===========================================================================
// EndAmbientCycleTool
// ===========================================================================

pub struct EndAmbientCycleTool;

impl Default for EndAmbientCycleTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EndAmbientCycleTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct EndCycleInput {
    summary: String,
    #[serde(deserialize_with = "super::serde_coerce::u32_from_string_or_number")]
    memories_modified: u32,
    #[serde(deserialize_with = "super::serde_coerce::u32_from_string_or_number")]
    compactions: u32,
    #[serde(default)]
    proactive_work: Option<String>,
    #[serde(default)]
    next_schedule: Option<NextScheduleInput>,
}

#[derive(Deserialize)]
struct NextScheduleInput {
    #[serde(default, deserialize_with = "super::serde_coerce::opt_u32_from_string_or_number")]
    wake_in_minutes: Option<u32>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    priority: Option<String>,
}

#[async_trait]
impl Tool for EndAmbientCycleTool {
    fn name(&self) -> &str {
        "end_ambient_cycle"
    }

    fn description(&self) -> &str {
        "End the current ambient cycle."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["summary", "memories_modified", "compactions"],
            "properties": {
                "intent": super::intent_schema_property(),
                "summary": {
                    "type": "string",
                    "description": "Human-readable summary of what was done this cycle"
                },
                "memories_modified": {
                    "type": "integer",
                    "description": "Count of memories created, merged, pruned, or updated"
                },
                "compactions": {
                    "type": "integer",
                    "description": "Number of context compactions during this cycle"
                },
                "proactive_work": {
                    "type": "string",
                    "description": "Description of proactive code changes, if any"
                },
                "next_schedule": {
                    "type": "object",
                    "description": "When to wake next and what to do",
                    "properties": {
                        "wake_in_minutes": {
                            "type": "integer",
                            "description": "Minutes until next wake"
                        },
                        "context": {
                            "type": "string",
                            "description": "What to do next cycle"
                        },
                        "priority": {
                            "type": "string",
                            "enum": ["low", "normal", "high"],
                            "description": "Priority for next cycle"
                        }
                    }
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: EndCycleInput = serde_json::from_value(input)?;

        let next_schedule = params.next_schedule.map(|ns| ScheduleRequest {
            wake_in_minutes: ns.wake_in_minutes,
            wake_at: None,
            context: ns.context.unwrap_or_default(),
            priority: parse_priority(ns.priority.as_deref()),
            target: ScheduleTarget::Ambient,
            created_by_session: ctx.session_id.clone(),
            working_dir: None,
            task_description: None,
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
        });

        let now = Utc::now();
        let result = AmbientCycleResult {
            summary: params.summary.clone(),
            memories_modified: params.memories_modified,
            compactions: params.compactions,
            proactive_work: params.proactive_work,
            next_schedule: next_schedule.clone(),
            started_at: now, // approximate; the runner will override if it tracks start time
            ended_at: now,
            status: CycleStatus::Complete,
            conversation: None, // populated by the runner after cycle completes
        };

        // Store for the ambient runner to pick up
        store_cycle_result(result);

        // Also persist state immediately so a crash after this tool but before
        // the runner collects won't lose the cycle.
        if let Ok(mut state) = AmbientState::load() {
            let next_desc = if let Some(ref sched) = next_schedule {
                let mins = sched.wake_in_minutes.unwrap_or(30);
                format!("~{}", crate::ambient::format_minutes_human(mins))
            } else {
                "system default".to_string()
            };

            state.last_run = Some(now);
            state.last_summary = Some(params.summary.clone());
            state.last_compactions = Some(params.compactions);
            state.last_memories_modified = Some(params.memories_modified);
            state.total_cycles += 1;
            let _ = state.save();

            Ok(ToolOutput::new(format!(
                "Ambient cycle ended. Memories modified: {}, compactions: {}. Next wake: {}",
                params.memories_modified, params.compactions, next_desc
            ))
            .with_title("ambient cycle ended".to_string()))
        } else {
            Ok(ToolOutput::new(format!(
                "Ambient cycle ended (state save failed). Summary: {}",
                params.summary
            ))
            .with_title("ambient cycle ended".to_string()))
        }
    }
}

// ===========================================================================
// ScheduleAmbientTool
// ===========================================================================

pub struct ScheduleAmbientTool;

impl Default for ScheduleAmbientTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleAmbientTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ScheduleInput {
    #[serde(default, deserialize_with = "super::serde_coerce::opt_u32_from_string_or_number")]
    wake_in_minutes: Option<u32>,
    #[serde(default)]
    wake_at: Option<String>,
    context: String,
    #[serde(default)]
    priority: Option<String>,
}

#[async_trait]
impl Tool for ScheduleAmbientTool {
    fn name(&self) -> &str {
        "schedule_ambient"
    }

    fn description(&self) -> &str {
        "Schedule an ambient task."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["context"],
            "properties": {
                "intent": super::intent_schema_property(),
                "wake_in_minutes": {
                    "type": "integer",
                    "description": "Minutes from now to wake"
                },
                "wake_at": {
                    "type": "string",
                    "description": "ISO 8601 timestamp for when to wake (alternative to wake_in_minutes)"
                },
                "context": {
                    "type": "string",
                    "description": "What to do when waking — stored in the scheduled queue"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "normal", "high"],
                    "description": "Priority for this scheduled task (default: normal)"
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ScheduleInput = serde_json::from_value(input)?;

        let wake_at = if let Some(ref ts) = params.wake_at {
            Some(
                ts.parse::<chrono::DateTime<Utc>>()
                    .map_err(|e| anyhow::anyhow!("Invalid wake_at timestamp: {}", e))?,
            )
        } else {
            None
        };

        let request = ScheduleRequest {
            wake_in_minutes: params.wake_in_minutes,
            wake_at,
            context: params.context.clone(),
            priority: parse_priority(params.priority.as_deref()),
            target: ScheduleTarget::Ambient,
            created_by_session: ctx.session_id,
            working_dir: None,
            task_description: None,
            relevant_files: Vec::new(),
            git_branch: None,
            additional_context: None,
        };

        let mut manager = AmbientManager::new()?;
        let id = manager.schedule(request)?;
        nudge_schedule_runner();

        let when = if let Some(ref ts) = params.wake_at {
            ts.clone()
        } else if let Some(mins) = params.wake_in_minutes {
            format!("in {}", crate::ambient::format_minutes_human(mins))
        } else {
            "in 30m (default)".to_string()
        };

        Ok(ToolOutput::new(format!("Scheduled ambient task {} for {}", id, when))
            .with_title(format!("scheduled: {}", params.context)))
    }
}

// ===========================================================================
// RequestPermissionTool
// ===========================================================================

pub struct RequestPermissionTool;

impl Default for RequestPermissionTool {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestPermissionTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct RequestPermissionInput {
    action: String,
    description: String,
    rationale: String,
    #[serde(default)]
    urgency: Option<String>,
    #[serde(
        default = "default_false",
        deserialize_with = "super::serde_coerce::bool_from_string_or_bool"
    )]
    wait: bool,
    #[serde(default)]
    context: Option<Value>,
}

fn default_false() -> bool {
    false
}

fn extract_context_string(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            value.as_str().and_then(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
        })
    })
}

fn extract_context_list(map: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    for key in keys {
        let Some(value) = map.get(*key) else {
            continue;
        };

        if let Some(items) = value.as_array() {
            let list: Vec<String> = items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect();
            if !list.is_empty() {
                return list;
            }
        } else if let Some(single) = value.as_str() {
            let trimmed = single.trim();
            if !trimmed.is_empty() {
                return vec![trimmed.to_string()];
            }
        }
    }
    Vec::new()
}

fn build_permission_review_context(action: &str, description: &str, rationale: &str, context: Option<&Value>) -> Value {
    let context_obj = context.and_then(Value::as_object);

    let summary = context_obj
        .and_then(|m| extract_context_string(m, &["summary", "what", "activity_summary"]))
        .unwrap_or_else(|| description.to_string());

    let why_permission_needed = context_obj
        .and_then(|m| {
            extract_context_string(
                m,
                &["why_permission_needed", "why", "reason", "rationale", "justification"],
            )
        })
        .unwrap_or_else(|| rationale.to_string());

    let mut review = Map::new();
    review.insert("summary".to_string(), Value::String(summary));
    review.insert(
        "why_permission_needed".to_string(),
        Value::String(why_permission_needed),
    );
    review.insert("requested_action".to_string(), Value::String(action.to_string()));

    let string_fields: [(&str, &[&str]); 4] = [
        (
            "current_activity",
            &["current_activity", "activity", "task", "current_task"],
        ),
        (
            "expected_outcome",
            &["expected_outcome", "outcome", "success_criteria", "success"],
        ),
        ("impact", &["impact", "user_impact"]),
        ("rollback_plan", &["rollback_plan", "rollback"]),
    ];

    if let Some(map) = context_obj {
        for (field_name, keys) in string_fields {
            if let Some(value) = extract_context_string(map, keys) {
                review.insert(field_name.to_string(), Value::String(value));
            }
        }

        let list_fields: [(&str, &[&str]); 4] = [
            ("planned_steps", &["planned_steps", "steps", "plan", "checklist"]),
            ("files", &["files", "file_paths", "planned_files"]),
            ("commands", &["commands", "planned_commands"]),
            ("risks", &["risks", "risk", "safety_risks"]),
        ];

        for (field_name, keys) in list_fields {
            let items = extract_context_list(map, keys);
            if !items.is_empty() {
                review.insert(
                    field_name.to_string(),
                    Value::Array(items.into_iter().map(Value::String).collect()),
                );
            }
        }
    }

    if let Some(raw) = context
        && !raw.is_object()
    {
        review.insert("notes".to_string(), raw.clone());
    }

    Value::Object(review)
}

#[async_trait]
impl Tool for RequestPermissionTool {
    fn name(&self) -> &str {
        "request_permission"
    }

    fn description(&self) -> &str {
        "Request user permission."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action", "description", "rationale"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "description": "The action requiring permission (e.g., 'create_pull_request', 'push', 'edit')"
                },
                "description": {
                    "type": "string",
                    "description": "What the action will do"
                },
                "rationale": {
                    "type": "string",
                    "description": "Why this action is beneficial"
                },
                "urgency": {
                    "type": "string",
                    "enum": ["low", "normal", "high"],
                    "description": "How urgent the permission request is (default: normal)"
                },
                "wait": {
                    "type": "boolean",
                    "description": "If true, block until user decides (with timeout). If false, queue and continue."
                },
                "context": {
                    "type": "object",
                    "description": "Structured reviewer context. Include summary of current work and why permission is needed.",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "One-paragraph summary of what you are currently doing"
                        },
                        "why_permission_needed": {
                            "type": "string",
                            "description": "Why this action needs user approval right now"
                        },
                        "current_activity": {
                            "type": "string",
                            "description": "Current task or ambient objective"
                        },
                        "planned_steps": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Short ordered plan of intended steps"
                        },
                        "files": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Files expected to be created/modified"
                        },
                        "commands": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Commands expected to be executed"
                        },
                        "risks": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Known risks or side effects"
                        },
                        "rollback_plan": {
                            "type": "string",
                            "description": "How to back out changes if needed"
                        },
                        "expected_outcome": {
                            "type": "string",
                            "description": "What successful completion should look like"
                        }
                    },
                    "additionalProperties": true
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let settings = ApprovalSettings::from_config(&crate::config::config().ambient);
        execute_request_permission(input, ctx, get_safety_system(), settings).await
    }
}

// ---------------------------------------------------------------------------
// Clientless approval wait
// ---------------------------------------------------------------------------

/// How long a waiting tool owns its [`BlockedApproval`] record before the
/// ambient runner may treat the waiter as gone.
const WAITER_LEASE_DURATION: Duration = Duration::from_secs(90);
/// Lease renewal cadence: three renewals fit inside one lease window.
const WAITER_LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(30);
/// Record outcome for a decision the waiting turn applied itself.
const INLINE_OUTCOME: &str = "inline";

/// Ambient settings the permission tool acts on.
///
/// Production resolves these from `[ambient]`; tests build one directly so they
/// do not have to mutate the process-global config.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ApprovalSettings {
    /// How long a `wait = true` call blocks before the timeout default applies.
    pub timeout: Duration,
    /// Result to apply when that wait elapses with no decision.
    pub default_on_timeout: ApprovalDefault,
    /// Whether a session that is not registered as ambient may call the tool.
    pub allow_clientless: bool,
    lease_duration: Duration,
    lease_renew_interval: Duration,
}

impl ApprovalSettings {
    fn from_config(ambient: &AmbientConfig) -> Self {
        Self {
            timeout: Duration::from_secs(ambient.approval_timeout_secs),
            default_on_timeout: ApprovalDefault::from_config_str(&ambient.approval_default_on_timeout),
            allow_clientless: ambient.approval_for_clientless_runs,
            lease_duration: WAITER_LEASE_DURATION,
            lease_renew_interval: WAITER_LEASE_RENEW_INTERVAL,
        }
    }

    #[cfg(test)]
    fn for_test(timeout: Duration, default_on_timeout: ApprovalDefault, allow_clientless: bool) -> Self {
        Self {
            timeout,
            default_on_timeout,
            allow_clientless,
            lease_duration: WAITER_LEASE_DURATION,
            lease_renew_interval: WAITER_LEASE_RENEW_INTERVAL,
        }
    }
}

/// What one clientless wait ended with.
enum WaitOutcome {
    /// A decision was reached, or the history write failed and the request stays
    /// queued for the runner.
    Decided {
        result: PermissionResult,
        decision: Option<Decision>,
    },
    /// The waiter lease changed hands; the runner owns the record now.
    Superseded { decision: Option<Decision> },
}

async fn execute_request_permission(
    input: Value,
    ctx: ToolContext,
    system: Arc<SafetySystem>,
    settings: ApprovalSettings,
) -> Result<ToolOutput> {
    ensure_ambient_session(&ctx, settings.allow_clientless)?;

    let params: RequestPermissionInput = serde_json::from_value(input)?;

    let urgency = match params.urgency.as_deref() {
        Some("low") => Urgency::Low,
        Some("high") => Urgency::High,
        _ => Urgency::Normal,
    };

    let request_id = safety::new_request_id();
    let now = Utc::now();
    let timeout = settings.timeout;
    let expires_at = now
        + chrono::Duration::from_std(timeout)
            .unwrap_or_else(|_| chrono::Duration::seconds(safety::DEFAULT_APPROVAL_TIMEOUT.as_secs() as i64));
    let review = build_permission_review_context(
        &params.action,
        &params.description,
        &params.rationale,
        params.context.as_ref(),
    );
    let mut request_context = json!({
        "session_id": ctx.session_id.clone(),
        "message_id": ctx.message_id.clone(),
        "tool_call_id": ctx.tool_call_id.clone(),
        "working_dir": ctx.working_dir.as_ref().map(|p| p.display().to_string()),
        "requested_at": now.to_rfc3339(),
    });
    if let Some(obj) = request_context.as_object_mut() {
        obj.insert("review".to_string(), review);
        if let Some(user_context) = params.context {
            obj.insert("details".to_string(), user_context);
        }
    }

    let request = PermissionRequest {
        id: request_id,
        action: params.action.clone(),
        description: params.description.clone(),
        rationale: params.rationale.clone(),
        urgency,
        wait: params.wait,
        created_at: now,
        context: Some(request_context),
        // A clientless run has no ambient cycle id; the wait record is what the
        // runner resumes from.
        run_id: None,
        expires_at: Some(expires_at),
        default_on_timeout: settings.default_on_timeout,
    };

    let request_id = match system.request_permission_with_timeout(request, timeout) {
        PermissionResult::Queued { request_id } => request_id,
        other => return Ok(render_approval_result(&other, None, &params.action, settings)),
    };

    // The record is created before the wait starts, so a crash while blocked
    // still leaves the runner something to pick up. A `wait = false` call gets a
    // record too, with no waiter, so the runner can inherit it later.
    let store = BlockedApprovalStore::open_default().ok();
    let waiter_pid = params.wait.then(std::process::id);
    // Whether an approver who answers the alert will find a record to resolve.
    // `add` returning `Ok(false)` means the record already existed, which
    // satisfies the same condition.
    let recorded = match store.as_ref() {
        Some(store) => {
            let record = BlockedApproval {
                session_id: ctx.session_id.clone(),
                request_id: request_id.clone(),
                created_at: now,
                waiter_pid,
                waiter_started_at: waiter_pid.map(|_| now),
                waiter_lease_expires_at: waiter_pid.map(|_| now + lease_delta(settings.lease_duration)),
                ..BlockedApproval::default()
            };
            match store.add(record) {
                Ok(_) => true,
                Err(error) => {
                    crate::logging::warn(&format!(
                        "approval record for request {} could not be created: {error}",
                        request_id
                    ));
                    false
                }
            }
        }
        None => {
            crate::logging::warn(
                "the approval record store is unavailable; the ambient runner cannot resume this wait",
            );
            false
        }
    };

    // Alert only once the durable record exists: an approver who answers the
    // alert immediately resolves the record it names, so a notification that
    // outran `store.add` made that answer fail with "no blocked approval
    // record". `request_permission_with_timeout` deliberately does not notify.
    //
    // When the record could not be created the alert is withheld: an alert
    // naming a request nobody can resolve costs the operator a round trip to
    // learn what the warning above already says, and silence is the smaller
    // harm. The request stays queued either way, so a later record makes it
    // resolvable.
    if recorded {
        safety::dispatch_permission_notification(&params.action, &params.description, &request_id);
    }

    if !params.wait {
        return Ok(render_approval_result(
            &PermissionResult::Queued {
                request_id: request_id.clone(),
            },
            None,
            &params.action,
            settings,
        ));
    }

    emit_approval_pending(&ctx.session_id, &request_id, &params.action, expires_at);

    let outcome = wait_for_decision(&system, store.as_ref(), &request_id, std::process::id(), settings).await;
    let now = Utc::now();

    match outcome {
        WaitOutcome::Superseded { decision } => {
            // The runner owns the record now. Returning a tool result would let
            // this turn keep working alongside the resumed turn, so the turn is
            // stopped instead. The event carries the decision that actually
            // landed, never an assumed denial.
            if let Some(decision) = decision.as_ref() {
                // The event is recorded before it is emitted here too, so a
                // crash between the takeover and the delivery replays it. The
                // runner owns the record, so the event names it as the resolver.
                match store.as_ref() {
                    Some(store) => queue_approval_resolved(
                        store,
                        &ctx.session_id,
                        &request_id,
                        decision.approved,
                        "superseded",
                        "runner",
                    ),
                    None => {
                        emit_approval_resolved(&ctx.session_id, &request_id, decision.approved, "superseded", "runner")
                    }
                }
            }
            Err(stop_turn_for_takeover(&ctx.session_id, &request_id))
        }
        WaitOutcome::Decided { result, decision } => {
            let decided = matches!(
                &result,
                PermissionResult::Approved { .. } | PermissionResult::Denied { .. }
            );
            let approved = matches!(&result, PermissionResult::Approved { .. });

            if let Some(store) = &store {
                let update = store.update_with(&request_id, |stored| {
                    if decided {
                        // The decision fields are filled once; a concurrent
                        // writer that got there first is not overwritten.
                        stored.decided_at.get_or_insert(now);
                        stored.inline_at.get_or_insert(now);
                        stored.inline_approved.get_or_insert(approved);
                        stored.outcome.get_or_insert_with(|| INLINE_OUTCOME.to_string());
                    }
                    stored.waiter_pid = None;
                    stored.waiter_lease_expires_at = None;
                });
                if let Err(error) = update {
                    crate::logging::warn(&format!(
                        "approval record for request {} could not be updated: {error}",
                        request_id
                    ));
                }
            }

            if decided {
                let via = decision
                    .as_ref()
                    .map(|decision| decision.decided_via.clone())
                    .unwrap_or_else(|| INLINE_OUTCOME.to_string());
                // Plan C4: the record carries the event before it is emitted,
                // so a crash between the two replays it instead of losing it.
                // The record keeps the event until a client sends
                // `Request::ApprovalAck`, which the server handles in
                // `handle_approval_ack`.
                match store.as_ref() {
                    Some(store) => queue_approval_resolved(store, &ctx.session_id, &request_id, approved, &via, "tool"),
                    None => emit_approval_resolved(&ctx.session_id, &request_id, approved, &via, "tool"),
                }
            }

            Ok(render_approval_result(
                &result,
                decision.as_ref(),
                &params.action,
                settings,
            ))
        }
    }
}

async fn wait_for_decision(
    system: &SafetySystem,
    store: Option<&BlockedApprovalStore>,
    request_id: &str,
    waiter_pid: u32,
    settings: ApprovalSettings,
) -> WaitOutcome {
    let renew = renew_waiter_lease(store, request_id, waiter_pid, settings);
    tokio::pin!(renew);
    let wait = system.await_decision(request_id, settings.timeout);
    tokio::pin!(wait);

    let result = tokio::select! {
        result = &mut wait => Some(result),
        () = &mut renew => None,
    };

    // The check runs even when the wait finished first: a takeover that landed
    // in the same instant must still win over this turn's result.
    if waiter_was_superseded(store, request_id, waiter_pid) {
        return WaitOutcome::Superseded {
            decision: system.decision_for(request_id),
        };
    }

    match result {
        Some(result) => WaitOutcome::Decided {
            result,
            decision: system.decision_for(request_id),
        },
        // The renewal future only finishes when the lease changed hands.
        None => WaitOutcome::Superseded {
            decision: system.decision_for(request_id),
        },
    }
}

/// Keep this waiter's lease alive, and finish when ownership moved elsewhere.
async fn renew_waiter_lease(
    store: Option<&BlockedApprovalStore>,
    request_id: &str,
    waiter_pid: u32,
    settings: ApprovalSettings,
) {
    let Some(store) = store else {
        // Without a record nobody can take the wait over; the tool owns it until
        // the decision or the timeout.
        std::future::pending::<()>().await;
        return;
    };

    let mut ticker = tokio::time::interval(settings.lease_renew_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // A tokio interval yields immediately on its first tick; consume it so the
    // first renewal happens one interval after the wait started.
    ticker.tick().await;

    loop {
        ticker.tick().await;
        let mut lost = false;
        let update = store.update_with(request_id, |stored| {
            if stored.owner_epoch != 0 || stored.waiter_pid != Some(waiter_pid) {
                lost = true;
                return;
            }
            stored.waiter_lease_expires_at = Some(Utc::now() + lease_delta(settings.lease_duration));
        });
        if let Err(error) = update {
            crate::logging::warn(&format!(
                "approval lease for request {request_id} could not be renewed: {error}"
            ));
        }
        if lost {
            return;
        }
    }
}

fn waiter_was_superseded(store: Option<&BlockedApprovalStore>, request_id: &str, waiter_pid: u32) -> bool {
    let Some(store) = store else {
        return false;
    };
    match store.get(request_id) {
        Ok(Some(record)) => record.owner_epoch != 0 || record.waiter_pid != Some(waiter_pid),
        Ok(None) => false,
        Err(error) => {
            crate::logging::warn(&format!(
                "approval record for request {request_id} could not be read: {error}"
            ));
            false
        }
    }
}

fn lease_delta(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration)
        .unwrap_or_else(|_| chrono::Duration::seconds(WAITER_LEASE_DURATION.as_secs() as i64))
}

fn render_approval_result(
    result: &PermissionResult,
    decision: Option<&Decision>,
    action: &str,
    settings: ApprovalSettings,
) -> ToolOutput {
    let via = decision
        .map(|decision| decision.decided_via.as_str())
        .unwrap_or_default();
    let output = match result {
        PermissionResult::Approved { message } => {
            if via == safety::TIMEOUT_DEFAULT_VIA {
                timeout_default_text(true, settings.timeout)
            } else {
                let message = message.as_deref().unwrap_or("no message");
                format!("Permission approved: {}", message)
            }
        }
        PermissionResult::Denied { reason } => {
            if via == safety::TIMEOUT_DEFAULT_VIA {
                timeout_default_text(false, settings.timeout)
            } else {
                let reason = reason.as_deref().unwrap_or("no reason given");
                format!("Permission denied: {}", reason)
            }
        }
        PermissionResult::Queued { request_id } => format!(
            "Permission request queued (id: {}). Action '{}' is pending user review.",
            request_id, action
        ),
        PermissionResult::Timeout => {
            "Permission request timed out before a decision could be recorded; the run stays parked for the ambient runner."
                .to_string()
        }
    };
    ToolOutput::new(output).with_title(format!("permission: {}", action))
}

fn timeout_default_text(approved: bool, timeout: Duration) -> String {
    format!(
        "Permission request timed out after {}s with no response; the configured default ({}) was applied. This was not a human decision.",
        timeout.as_secs(),
        if approved { "approve" } else { "deny" }
    )
}

/// Tell a connected client that a request is waiting, so it can render the
/// prompt and answer through the existing `permission_decision` frame.
///
/// `pub(crate)` because the tool approval gate in `tool::mod` queues requests on
/// the same system and needs the same event: without it the request exists but a
/// plain `serve` session has no way to show it.
pub(crate) fn emit_approval_pending(
    session_id: &str,
    request_id: &str,
    action: &str,
    expires_at: chrono::DateTime<Utc>,
) {
    let event = ServerEvent::ApprovalPending {
        request_id: request_id.to_string(),
        action: action.to_string(),
        expires_at: Some(expires_at.to_rfc3339()),
    };
    report_emit(
        session_id,
        request_id,
        "ApprovalPending",
        emit_session_event(session_id, event),
    );
}

/// Tell a connected client that a request it was shown has been answered, so it
/// can clear the prompt. Emitting only the pending event leaves a client waiting
/// on a request that is already decided.
///
/// `resolved_by` names who applied the verdict: `"approval-gate"` from the tool
/// approval gate, `"runner"` from the ambient runner, and `"tool"` from the
/// ambient `request_permission` tool.
pub(crate) fn emit_approval_resolved(session_id: &str, request_id: &str, approved: bool, via: &str, resolved_by: &str) {
    let event = ServerEvent::ApprovalResolved {
        request_id: request_id.to_string(),
        approved,
        via: via.to_string(),
        resolved_by: resolved_by.to_string(),
    };
    report_emit(
        session_id,
        request_id,
        "ApprovalResolved",
        emit_session_event(session_id, event),
    );
}

/// Record the resolved event on the wait record, then emit it once.
///
/// Mirrors the runner's `queue_approval_resolved`: the record keeps the event
/// so a crash between the write and the delivery replays rather than loses it.
fn queue_approval_resolved(
    store: &BlockedApprovalStore,
    session_id: &str,
    request_id: &str,
    approved: bool,
    via: &str,
    resolved_by: &str,
) {
    let event = ServerEvent::ApprovalResolved {
        request_id: request_id.to_string(),
        approved,
        via: via.to_string(),
        resolved_by: resolved_by.to_string(),
    };
    match serde_json::to_value(&event) {
        Ok(pending) => {
            let update = store.update_with(request_id, move |stored| {
                if stored.pending_event.is_none() {
                    stored.pending_event = Some(pending);
                }
            });
            if let Err(error) = update {
                crate::logging::warn(&format!(
                    "approval event for request {request_id} could not be recorded: {error}"
                ));
            }
        }
        Err(error) => crate::logging::warn(&format!(
            "approval event for request {request_id} could not be serialized: {error}"
        )),
    }
    emit_approval_resolved(session_id, request_id, approved, via, resolved_by);
}

/// Stop the calling turn because another owner took the blocked wait over.
///
/// Two signals are needed. Firing the turn's cancel signal stops the next
/// provider request; the typed error stops this assistant message, so its
/// remaining tool calls do not run and the error is not recorded as a result
/// the model would answer. The out-of-band mark names the reason for a turn
/// loop that only observes the signal.
fn stop_turn_for_takeover(session_id: &str, request_id: &str) -> anyhow::Error {
    crate::tool::superseded::mark(session_id);
    for signal in crate::turn_cancel_registry::active_turn_signals(session_id) {
        signal.fire();
    }
    anyhow::Error::new(super::SupersededToolCall::new(format!(
        "Permission request {request_id} was taken over by the ambient runner; this turn stops so the resumed turn continues the work"
    )))
}

/// A missing or closed sink is normal for an unattended run: the record keeps
/// the event, so this only logs and never fails the tool.
fn report_emit(session_id: &str, request_id: &str, event: &str, outcome: EmitOutcome) {
    match outcome {
        EmitOutcome::Delivered => {}
        EmitOutcome::NoSink => crate::logging::info(&format!(
            "{event} for request {request_id} had no client attached to session {session_id}"
        )),
        EmitOutcome::Closed => crate::logging::warn(&format!(
            "{event} for request {request_id} could not reach session {session_id}: its sink is closed"
        )),
    }
}

// ===========================================================================
// ScheduleTool — available to normal sessions to queue future ambient tasks
// ===========================================================================

pub struct ScheduleTool;

impl Default for ScheduleTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ScheduleToolInput {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    schedule_id: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default, deserialize_with = "super::serde_coerce::opt_u32_from_string_or_number")]
    wake_in_minutes: Option<u32>,
    #[serde(default)]
    wake_at: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    relevant_files: Vec<String>,
    #[serde(default)]
    background_context: Option<String>,
    #[serde(default)]
    success_criteria: Option<String>,
    #[serde(default)]
    target: Option<String>,
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }

    fn description(&self) -> &str {
        "Schedule, list, or cancel future tasks."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "cancel"],
                    "description": "Action to perform. Defaults to create for backwards compatibility."
                },
                "schedule_id": {
                    "type": "string",
                    "description": "Scheduled task ID. Required for action=cancel."
                },
                "task": {
                    "type": "string",
                    "description": "Task. Required for action=create."
                },
                "wake_in_minutes": { "type": "integer" },
                "wake_at": { "type": "string" },
                "priority": {
                    "type": "string",
                    "enum": ["low", "normal", "high"]
                },
                "relevant_files": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "background_context": {
                    "type": "string",
                    "description": "Optional background context for the scheduled task."
                },
                "success_criteria": { "type": "string" },
                "target": {
                    "type": "string",
                    "enum": ["resume", "spawn", "ambient"],
                    "description": "Delivery target. Defaults to resuming this session; 'spawn' runs one new child session."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ScheduleToolInput = serde_json::from_value(input)?;

        match params.action.as_deref().unwrap_or("create") {
            "create" => self.execute_create(params, ctx).await,
            "list" => self.execute_list().await,
            "cancel" => self.execute_cancel(params).await,
            other => anyhow::bail!("Invalid action '{}'. Expected one of: create, list, cancel", other),
        }
    }
}

impl ScheduleTool {
    async fn execute_create(&self, params: ScheduleToolInput, ctx: ToolContext) -> Result<ToolOutput> {
        let task = params
            .task
            .clone()
            .ok_or_else(|| anyhow::anyhow!("task is required for action=create"))?;

        if params.wake_in_minutes.is_none() && params.wake_at.is_none() {
            anyhow::bail!(
                "Either wake_in_minutes or wake_at is required. \
                 This tool is for scheduling future tasks."
            );
        }

        let wake_at = if let Some(ref ts) = params.wake_at {
            Some(
                ts.parse::<chrono::DateTime<Utc>>()
                    .map_err(|e| anyhow::anyhow!("Invalid wake_at timestamp: {}", e))?,
            )
        } else {
            None
        };

        let working_dir = ctx.working_dir.as_ref().map(|p| p.display().to_string());

        let git_branch = ctx
            .working_dir
            .as_ref()
            .and_then(|wd| {
                std::process::Command::new("git")
                    .args(["rev-parse", "--abbrev-ref", "HEAD"])
                    .current_dir(wd)
                    .output()
                    .ok()
            })
            .and_then(|out| {
                if out.status.success() {
                    String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
                } else {
                    None
                }
            });

        let target = parse_schedule_target(params.target.as_deref(), &ctx.session_id)?;
        let target_summary = format_schedule_target(&target);

        let request = ScheduleRequest {
            wake_in_minutes: params.wake_in_minutes,
            wake_at,
            context: task.clone(),
            priority: parse_priority(params.priority.as_deref()),
            target,
            created_by_session: ctx.session_id.clone(),
            working_dir: working_dir.clone(),
            task_description: Some(task.clone()),
            relevant_files: params.relevant_files.clone(),
            git_branch,
            additional_context: {
                let mut parts = Vec::new();
                if let Some(ref bg) = params.background_context {
                    parts.push(format!("Background: {}", bg));
                }
                if let Some(ref sc) = params.success_criteria {
                    parts.push(format!("Success criteria: {}", sc));
                }
                parts.push(format!("Scheduled by session: {}", ctx.session_id));
                Some(parts.join("\n"))
            },
        };

        let mut manager = AmbientManager::new()?;
        let id = manager.schedule(request)?;
        nudge_schedule_runner();

        let when = if let Some(ref ts) = params.wake_at {
            ts.clone()
        } else if let Some(mins) = params.wake_in_minutes {
            format!("in {}", crate::ambient::format_minutes_human(mins))
        } else {
            "unspecified".to_string()
        };

        let mut summary = format!("Scheduled task '{}' for {} (id: {})", task, when, id);
        if let Some(ref wd) = working_dir {
            summary.push_str(&format!("\nWorking directory: {}", wd));
        }
        if !params.relevant_files.is_empty() {
            summary.push_str(&format!("\nRelevant files: {}", params.relevant_files.join(", ")));
        }
        summary.push_str(&format!("\nTarget: {}", target_summary));

        Ok(ToolOutput::new(summary).with_title(format!("scheduled: {}", task)))
    }

    async fn execute_list(&self) -> Result<ToolOutput> {
        let manager = AmbientManager::new()?;
        let mut items: Vec<&ScheduledItem> = manager.queue().items().iter().collect();
        items.sort_by_key(|item| item.scheduled_for);

        if items.is_empty() {
            return Ok(ToolOutput::new("No scheduled tasks."));
        }

        let mut summary = format!("{} scheduled task(s):", items.len());
        for item in items {
            summary.push('\n');
            summary.push_str(&format_scheduled_item(item));
        }

        Ok(ToolOutput::new(summary).with_title("scheduled tasks"))
    }

    async fn execute_cancel(&self, params: ScheduleToolInput) -> Result<ToolOutput> {
        let id = params
            .schedule_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("schedule_id is required for action=cancel"))?;

        let mut manager = AmbientManager::new()?;
        let Some(item) = manager.cancel_schedule(id)? else {
            anyhow::bail!("No scheduled task found with id '{}'", id);
        };
        nudge_schedule_runner();

        Ok(ToolOutput::new(format!(
            "Cancelled scheduled task '{}' for {} (id: {})",
            item.task_description.as_deref().unwrap_or(&item.context),
            item.scheduled_for,
            item.id
        ))
        .with_title(format!("cancelled: {}", item.id)))
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn parse_priority(s: Option<&str>) -> Priority {
    match s {
        Some("low") => Priority::Low,
        Some("high") => Priority::High,
        _ => Priority::Normal,
    }
}

fn parse_schedule_target(s: Option<&str>, session_id: &str) -> Result<ScheduleTarget> {
    Ok(match s {
        Some("ambient") => ScheduleTarget::Ambient,
        Some("spawn") => ScheduleTarget::Spawn {
            parent_session_id: session_id.to_string(),
        },
        Some("resume") | None => ScheduleTarget::Session {
            session_id: session_id.to_string(),
        },
        Some(other) => anyhow::bail!("Invalid target '{}'. Expected one of: resume, spawn, ambient", other),
    })
}

fn format_schedule_target(target: &ScheduleTarget) -> String {
    match target {
        ScheduleTarget::Ambient => "ambient agent".to_string(),
        ScheduleTarget::Session { session_id } => format!("resume session {}", session_id),
        ScheduleTarget::Spawn { parent_session_id } => {
            format!("spawn one child session from {}", parent_session_id)
        }
    }
}

fn format_scheduled_item(item: &ScheduledItem) -> String {
    format!(
        "- {} | {} | {:?} | {} | {}",
        item.id,
        item.scheduled_for,
        item.priority,
        format_schedule_target(&item.target),
        item.task_description.as_deref().unwrap_or(&item.context)
    )
}

fn nudge_schedule_runner() {
    let runner = SCHEDULE_RUNNER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    if let Some(runner) = runner {
        runner.nudge();
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "ambient/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ambient/request_permission_tests.rs"]
mod request_permission_tests;

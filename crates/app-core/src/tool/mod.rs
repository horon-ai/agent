pub(crate) mod acp_session;
mod agentgrep;
pub mod ambient;
pub mod applet;
mod apply_patch;
pub(crate) mod approval_override;
mod arg_repair;
pub(crate) mod artifacts;
mod ask;
pub(crate) mod ask_bridge;
mod ast_tools;
mod aux_complete;
mod bash;
mod bash_interceptor;
mod batch;
mod bg;
#[cfg(unix)]
pub(crate) mod bridge_reload;
mod browser;
mod calendar;
pub(crate) mod client_bridge;
pub(crate) mod client_fs;
mod codemode;
mod communicate;
mod compile_remote;
#[cfg(target_os = "macos")]
mod computer;
mod config_edit_notice;
mod context_notes;
mod conversation_search;
mod debug_socket;
mod desktop_selfdev;
mod discover;
mod discover_secrets;
mod edit;
mod edit_stats;
mod file_diff;
pub(crate) mod file_lock;
mod generated_guard;
mod git_guard;
mod gmail;
mod goal;
mod hashline_edit;
mod horon_agent_docs;
pub mod inflight;
mod invalid;
mod irc;
mod ls;
mod lsp;
pub mod mcp;
mod memory;
mod minimizer;
mod monitor;
mod name_suggestions;
pub(crate) mod nested_agents;
mod open;
mod panel;
mod patch;
pub mod path_policy;
mod path_suffix;
pub(crate) mod permission_bridge;
mod read;
mod read_selector;
pub(crate) mod read_state;
mod replace;
pub(crate) mod sdk;
pub mod selfdev;
pub(crate) mod serde_coerce;
pub(crate) mod session_mcp;
mod session_search;
pub(crate) mod session_search_index;
mod side_panel;
mod skill;
mod submit_plan;
mod todo;
pub use todo::todo_call_plan;
mod tool_names;
use tool_names::is_fixed_mcp_tool;
pub(crate) use tool_names::{tool_name_is_allowed, tool_name_is_disabled};
mod toolbox;
pub(crate) use self::toolbox::session_can_reach;
mod undo;
mod user_response;
mod webfetch;
mod websearch;
mod write;
pub(crate) mod yield_result;

use crate::compaction::CompactionManager;
use crate::provider::Provider;
use crate::skill::SkillRegistry;
use anyhow::Result;
use futures::future::BoxFuture;
use horon_agent_message_types::ToolDefinition;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

thread_local! {
    /// Allow-list for a built-in construction pass on this thread.
    ///
    /// Set only around a restricted `base_tools` build, so the constructor list
    /// can skip an excluded tool instead of constructing it and filtering it out
    /// afterwards. `None` means "construct everything".
    static BUILD_ALLOW: std::cell::RefCell<Option<HashSet<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether a built-in should be constructed in the current build pass.
fn build_allows(name: &str) -> bool {
    BUILD_ALLOW.with(|cell| match cell.borrow().as_ref() {
        None => true,
        Some(set) => tool_name_is_allowed(set, name),
    })
}

/// Release every per-session cache this module owns.
///
/// A closed session must not keep a monitor runtime (and its sampler thread), an
/// answer ledger, or a durable input store alive. The journals keep the durable
/// rows; this releases only the in-memory handles.
pub(crate) fn forget_session_caches(session_id: &str) {
    monitor::forget_session(session_id);
    ask_bridge::forget_session(session_id);
    read_state::forget_session(session_id);
    // A structured-result child that never reached the completion path (or whose
    // last status update was missed) must not leave its schema slot behind.
    yield_result::discard(session_id);
}

use std::sync::{LazyLock, RwLock as StdRwLock};
use tokio::sync::RwLock;

/// Marker error for a tool call that stopped its own turn on purpose.
///
/// The ambient approval tool fires the turn's cancel signal when the runner
/// takes a blocked wait over. That signal only stops the *next* provider
/// request, so an ordinary tool error would still be appended to the
/// conversation, the model would answer it, and the remaining sibling calls in
/// the same assistant message would run alongside the resumed turn. Turn loops
/// detect this type and stop without recording anything.
#[derive(Debug)]
pub struct SupersededToolCall {
    message: String,
}

impl SupersededToolCall {
    /// Build the error a tool returns when another process took its wait over.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SupersededToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SupersededToolCall {}

/// Sessions whose in-flight turn was handed to another owner.
///
/// A takeover fires the turn's cancel signal, which a turn loop cannot tell
/// apart from a server reload, and the tool may not have returned its
/// [`SupersededToolCall`] yet when the loop notices the signal. The reason
/// therefore travels out of band, keyed by session. A turn clears the mark when
/// it starts and when it consumes one, so a finished takeover never relabels a
/// later turn.
pub mod superseded {
    use std::collections::HashSet;
    use std::sync::{LazyLock, Mutex};

    static SESSIONS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

    fn sessions() -> std::sync::MutexGuard<'static, HashSet<String>> {
        SESSIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record that `session_id`'s turn is stopping because another owner took
    /// its work over.
    pub fn mark(session_id: &str) {
        sessions().insert(session_id.to_string());
    }

    /// Whether `session_id`'s turn was handed over.
    pub fn is_marked(session_id: &str) -> bool {
        sessions().contains(session_id)
    }

    /// Clear the mark for `session_id`.
    pub fn clear(session_id: &str) {
        sessions().remove(session_id);
    }
}

pub(crate) use horon_agent_tool_core::intent_schema_property;
pub use horon_agent_tool_core::{
    ConcurrencyClass, StdinInputRequest, Tool, ToolContext, ToolEffects, ToolExecutionMode,
};
pub use horon_agent_tool_types::{ToolImage, ToolOutput};
pub(crate) use session_search::spawn_recent_index_warmup;
pub use session_search::{SearchOptions, search_sessions_blocking};
#[cfg(test)]
pub(crate) use session_search::{search_options_for_test, session_search_report_for_test};

#[derive(Clone, Debug, Default)]
struct SessionToolPolicy {
    allowed_tools: Option<HashSet<String>>,
    disabled_tools: HashSet<String>,
    /// The tiers the session resolved when it started, so a dispatcher such as
    /// `toolbox` reads the same frozen snapshot the definition list uses.
    load_tiers: Option<crate::config::ToolLoadTiers>,
    /// Whether `mcp_search`/`mcp_call` are listed rather than eager here.
    ///
    /// The Agent publishes it when it takes the decision, because the two tools
    /// that read it - `toolbox` and the `mcp` management tool - hold a registry
    /// and cannot reach the session's own state.
    listed_mcp_surface: bool,
    owner: Option<u64>,
}

static SESSION_TOOL_POLICIES: LazyLock<StdRwLock<HashMap<String, SessionToolPolicy>>> =
    LazyLock::new(|| StdRwLock::new(HashMap::new()));
static NEXT_SESSION_TOOL_POLICY_OWNER: AtomicU64 = AtomicU64::new(1);

/// Serializes the three hook invocations `Registry::execute` AWAITS: the
/// input transformer (`pre_tool_transform`, which MUTATES the tool input before
/// it runs), the `pre_tool` gate (which can block a call), and
/// `post_tool_feedback` (which feeds text back into the result).
///
/// A concurrent tool group would otherwise invoke the user's hook scripts in
/// parallel, and those scripts write to shared files. The transformer is the
/// worst of the three to overlap: two rewrites of the same input would race and
/// the surviving input would depend on scheduling. The lock is taken on
/// EVERY call, not only group members: `Registry::execute` cannot know whether
/// its caller grouped it, and one uniform rule is simpler to reason about and
/// to test than a conditional one.
///
/// The detached observers (`post_tool`, turn and session events) are NOT locked
/// and must not be: `fire_post_tool_hook` goes through
/// `hooks::dispatch_observer`, which spawns a detached child and returns
/// immediately (`crates/base/src/hooks.rs:320-347`). Those scripts
/// already run concurrently today, and no lock in this process can serialize
/// bodies that run after the call returns.
///
/// The guard is held only across the hook call itself - never across the
/// tool's own execution - so unrelated tools do not serialize on it.
static HOOK_SERIAL: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Margin added on top of the longest hook deadline when bounding how long a
/// tool call may queue for `HOOK_SERIAL`.
///
/// The lock is shared by three awaited hooks, so a holder can be any of them and
/// the longest legitimate hold is the largest of their deadlines. Bounding the
/// queue wait by that maximum plus this margin means a queued session never
/// expires while a holder is still within its own budget, and still cannot wait
/// indefinitely behind one that is wedged.
const HOOK_SERIAL_QUEUE_MARGIN_MS: u64 = 1_000;

/// Budget for acquiring `HOOK_SERIAL`, distinct from any single hook's execution
/// deadline: the deadlines bound one hook call, this bounds how long another
/// session may queue behind that call.
fn hook_serial_queue_budget() -> std::time::Duration {
    let hooks = &crate::config::config().hooks;
    let longest_hold_ms = hooks
        .pre_tool_transform_timeout_ms
        .max(hooks.pre_tool_timeout_ms)
        .max(hooks.post_tool_feedback_timeout_ms)
        .max(1);
    std::time::Duration::from_millis(longest_hold_ms.saturating_add(HOOK_SERIAL_QUEUE_MARGIN_MS))
}

/// Acquire the process-global hook gate, giving up after `budget`.
///
/// `None` means the budget expired and the caller must not assume the gated
/// hook ran. Cancellation-safe: dropping the future before the permit is
/// granted releases nothing, so an interrupted session cannot leak the gate.
/// The permit itself is released when the returned guard drops.
async fn acquire_hook_serial(budget: std::time::Duration) -> Option<tokio::sync::MutexGuard<'static, ()>> {
    tokio::time::timeout(budget, HOOK_SERIAL.lock()).await.ok()
}

/// Milliseconds between `started_at` and now, saturating into `u64`.
fn elapsed_ms(started_at: std::time::Instant) -> u64 {
    started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

/// Stage breakdown of one tool dispatch.
///
/// The dispatch spans everything the caller waited for: the input-transformer
/// queue and run, the `pre_tool` gate, the tool itself, the post-tool feedback
/// hooks, and the context-overflow guard. Measuring only `Tool::execute` hid a
/// wedged or slow hook behind a metric that read as instant.
///
/// Durations only. A lifecycle trace never carries tool input, output, file
/// text, or credentials, so nothing payload-bearing belongs in these fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DispatchTimings {
    /// Waiting for `HOOK_SERIAL` plus the input transformer itself.
    pub(crate) hook_transform_ms: u64,
    /// Waiting for `HOOK_SERIAL` plus the `pre_tool` gate itself.
    pub(crate) hook_gate_ms: u64,
    /// The tool's own execution, the span the old metric measured alone.
    pub(crate) execute_ms: u64,
    /// The awaited `post_tool_feedback` hooks.
    pub(crate) post_feedback_ms: u64,
    /// The context-overflow guard.
    pub(crate) overflow_guard_ms: u64,
    /// The whole dispatch, measured from before the transformer.
    pub(crate) total_ms: u64,
}

impl DispatchTimings {
    /// Duration fields for a `TOOL_LIFECYCLE` line.
    ///
    /// `elapsed_ms` keeps its name and now means the dispatch total, so existing
    /// log consumers read real dispatch latency; the execute span stays
    /// available, unchanged in meaning, as `execute_ms`.
    pub(crate) fn fields(&self) -> Vec<(String, String)> {
        vec![
            ("elapsed_ms".to_string(), self.total_ms.to_string()),
            ("execute_ms".to_string(), self.execute_ms.to_string()),
            ("hook_transform_ms".to_string(), self.hook_transform_ms.to_string()),
            ("hook_gate_ms".to_string(), self.hook_gate_ms.to_string()),
            ("post_feedback_ms".to_string(), self.post_feedback_ms.to_string()),
            ("overflow_guard_ms".to_string(), self.overflow_guard_ms.to_string()),
        ]
    }

    /// Sum of the measured stages. The total covers a little more (the start
    /// lifecycle line and result delivery), so this is never the larger number.
    #[cfg(test)]
    pub(crate) fn stage_sum_ms(&self) -> u64 {
        self.hook_transform_ms + self.hook_gate_ms + self.execute_ms + self.post_feedback_ms + self.overflow_guard_ms
    }
}

/// Test-only sink for the stage breakdown the real dispatch path measured, so
/// tests can read the numbers without parsing log output. Keyed by tool call id
/// because tests in one process run in parallel.
#[cfg(test)]
static DISPATCH_TIMINGS: LazyLock<std::sync::Mutex<std::collections::HashMap<String, DispatchTimings>>> =
    LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
fn observe_dispatch_timings(tool_call_id: &str, timings: DispatchTimings) {
    DISPATCH_TIMINGS
        .lock()
        .expect("dispatch timing sink")
        .insert(tool_call_id.to_string(), timings);
}

#[cfg(not(test))]
fn observe_dispatch_timings(_tool_call_id: &str, _timings: DispatchTimings) {}

/// Stage breakdown recorded for `tool_call_id` by the real dispatch path.
#[cfg(test)]
pub(crate) fn dispatch_timings_for(tool_call_id: &str) -> Option<DispatchTimings> {
    DISPATCH_TIMINGS
        .lock()
        .expect("dispatch timing sink")
        .get(tool_call_id)
        .copied()
}

/// Run the `pre_tool` gate under the process-global hook lock, with a bounded
/// wait for that lock.
///
/// An expired wait is logged and fails open (`Allow`), matching the gate's
/// fail-open outcome for its own timeout: the queue wait must never become the
/// only path that blocks a tool call.
async fn run_gated_pre_tool_hook(
    session_id: &str,
    working_dir: Option<&str>,
    tool_name: &str,
    input_json: &str,
) -> crate::hooks::GateDecision {
    let queue_budget = hook_serial_queue_budget();
    match acquire_hook_serial(queue_budget).await {
        Some(_serial) => crate::hooks::run_pre_tool_gate(session_id, working_dir, tool_name, input_json).await,
        None => {
            crate::logging::warn(&format!(
                "pre_tool hook gate still busy after {}ms; allowing tool '{tool_name}' for session {session_id} without running the gate (fail-open)",
                queue_budget.as_millis()
            ));
            crate::hooks::GateDecision::Allow
        }
    }
}

/// Removes an Agent-owned policy when that Agent actually leaves memory.
///
/// The owner token prevents a stale Agent from removing the policy installed by
/// a successor connection for the same persisted session ID.
pub(crate) struct SessionToolPolicyRegistration {
    session_id: String,
    owner: u64,
}

impl Drop for SessionToolPolicyRegistration {
    fn drop(&mut self) {
        let mut policies = SESSION_TOOL_POLICIES
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if policies
            .get(&self.session_id)
            .is_some_and(|policy| policy.owner == Some(self.owner))
        {
            policies.remove(&self.session_id);
            sdk::remove_session(&self.session_id);
        }
    }
}

/// Widen one registered session's allow-list by a tool installed after the
/// policy snapshot.
///
/// A schema-spawned child's `yield` tool exists only once the spawn's
/// `output_schema` is known, which is after `Agent::build_base` froze the
/// session policy. Without this the dispatch gate would refuse the very tool
/// the child is told to call. A session with no policy entry is left alone:
/// `None` already means everything is allowed.
pub(crate) fn allow_session_tool(session_id: &str, tool_name: &str) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(policy) = policies.get_mut(session_id) {
        if let Some(allowed) = policy.allowed_tools.as_mut() {
            allowed.insert(tool_name.to_string());
        }
        policy.disabled_tools.remove(tool_name);
    }
}

pub(crate) fn register_session_tool_policy(
    session_id: &str,
    allowed_tools: Option<HashSet<String>>,
    disabled_tools: HashSet<String>,
) -> SessionToolPolicyRegistration {
    let owner = NEXT_SESSION_TOOL_POLICY_OWNER.fetch_add(1, Ordering::Relaxed);
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.insert(
        session_id.to_string(),
        SessionToolPolicy {
            allowed_tools,
            disabled_tools,
            load_tiers: None,
            listed_mcp_surface: false,
            owner: Some(owner),
        },
    );
    SessionToolPolicyRegistration {
        session_id: session_id.to_string(),
        owner,
    }
}

/// Attach the tiers an Agent resolved at session start to its registered policy.
///
/// A registry that executes a tool on the session's behalf (`toolbox`) needs
/// the same snapshot the definition list was built from; resolving the live
/// configuration there would let a reload disagree with the locked tool list.
/// A session with no policy entry is left alone, so this can never create an
/// entry whose lifetime nothing owns.
pub(crate) fn register_session_load_tiers(session_id: &str, tiers: crate::config::ToolLoadTiers) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(policy) = policies.get_mut(session_id) {
        policy.load_tiers = Some(tiers);
    }
}

/// Publish whether this session reaches `mcp_search`/`mcp_call` through
/// `toolbox` instead of carrying them eagerly.
///
/// Same lifetime rule as [`register_session_load_tiers`]: a session with no
/// policy entry is left alone, so this can never create an entry nothing owns.
pub(crate) fn register_session_listed_mcp(session_id: &str, listed: bool) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(policy) = policies.get_mut(session_id) {
        policy.listed_mcp_surface = listed;
    }
}

/// Whether this session's `mcp_search`/`mcp_call` live behind the dispatcher.
pub(crate) fn session_lists_mcp_surface(session_id: &str) -> bool {
    SESSION_TOOL_POLICIES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .is_some_and(|policy| policy.listed_mcp_surface)
}

#[cfg(test)]
pub(crate) fn set_session_tool_policy(
    session_id: &str,
    allowed_tools: Option<HashSet<String>>,
    disabled_tools: HashSet<String>,
) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.insert(
        session_id.to_string(),
        SessionToolPolicy {
            allowed_tools,
            disabled_tools,
            load_tiers: None,
            listed_mcp_surface: false,
            owner: None,
        },
    );
}

#[cfg(test)]
pub(crate) fn set_session_listed_mcp_for_test(session_id: &str, listed: bool) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.entry(session_id.to_string()).or_default().listed_mcp_surface = listed;
}

#[cfg(test)]
pub(crate) fn clear_session_tool_policy(session_id: &str) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.remove(session_id);
}

fn session_tool_policy(session_id: &str) -> Option<SessionToolPolicy> {
    let mut policy = SESSION_TOOL_POLICIES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .cloned();
    if let Some(config) = sdk::config(session_id) {
        let policy = policy.get_or_insert_with(SessionToolPolicy::default);
        if let Some(enabled) = config.enabled {
            policy.allowed_tools = Some(enabled.into_iter().collect());
            policy.disabled_tools.clear();
        }
        policy.disabled_tools.extend(config.disabled);
        if let Some(allowed) = policy.allowed_tools.as_mut() {
            allowed.extend(config.custom.into_iter().map(|t| t.name));
        }
    }
    policy
}

#[cfg(test)]
pub(crate) fn session_tool_policy_allows_tool_for_test(session_id: &str, tool_name: &str) -> Option<bool> {
    session_tool_policy(session_id).map(|policy| {
        policy
            .allowed_tools
            .as_ref()
            .is_none_or(|allowed| tool_name_is_allowed(allowed, tool_name))
            && !tool_name_is_disabled(&policy.disabled_tools, tool_name)
    })
}

/// Apply the current session policy to an MCP server tool invoked through a
/// fixed deferred surface. Explicitly enabling the fixed surface authorizes its
/// underlying MCP calls, while per-tool allow/deny entries remain effective.
#[cfg(test)]
pub(crate) fn session_mcp_dispatch_is_allowed(session_id: &str, dispatched_name: &str, fixed_surface: &str) -> bool {
    session_mcp_alias_is_allowed(session_id, dispatched_name, dispatched_name, fixed_surface)
}

/// Enforce both the actual alias and the legacy normalized identity. A deny on
/// either spelling wins, including when the caller uses the deferred surface.
pub(crate) fn session_mcp_alias_is_allowed(
    session_id: &str,
    alias: &str,
    legacy_name: &str,
    fixed_surface: &str,
) -> bool {
    let Some(policy) = session_tool_policy(session_id) else {
        return true;
    };
    let allowed = policy.allowed_tools.as_ref().is_none_or(|allowed| {
        tool_name_is_allowed(allowed, alias)
            || tool_name_is_allowed(allowed, legacy_name)
            || allowed.contains(fixed_surface)
    });
    allowed
        && !tool_name_is_disabled(&policy.disabled_tools, alias)
        && !tool_name_is_disabled(&policy.disabled_tools, legacy_name)
}

/// Whether a tool call opted in to receiving an oversized (truncated) result.
///
/// Read straight off the raw input rather than each tool's typed args, so every
/// tool honors the flag without having to declare it. Tools deserialize their
/// own args with `#[serde(default)]` fields and ignore unknown keys, so an extra
/// key here is inert for the tool itself.
///
/// Only a real JSON `true` counts. A string `"true"` is also accepted because
/// models routinely stringify booleans, but anything else (including `1`) is
/// treated as absent: accidentally spending the remaining context window should
/// take an unambiguous yes.
#[cfg(test)]
pub(crate) fn accept_large_output_schema_property_for_test() -> Value {
    horon_agent_tool_core::accept_large_output_schema_property()
}

fn accepts_large_output(input: &Value) -> bool {
    // Same key the schema advertises, so the documented flag and the honored
    // flag cannot drift apart.
    match input.get(horon_agent_tool_core::ACCEPT_LARGE_OUTPUT_KEY) {
        Some(Value::Bool(accepted)) => *accepted,
        Some(Value::String(raw)) => raw.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Metadata key the tool layer sets on an output whose call passed
/// `accept_large_output`.
///
/// The round-owned budget has to make the same truncate-vs-withhold decision
/// the tool layer made, evaluated at the round's running total, and that
/// decision depends on the flag. The flag lives in the call's input, which the
/// round never sees, so the layer that did see it records it on the output.
/// Internal: never rendered into the transcript or a prompt.
pub(crate) const ACCEPT_LARGE_OUTPUT_METADATA_KEY: &str = "_accept_large_output";

/// First line of the notice appended when an output is truncated instead of
/// withheld. Shared with the round-owned budget so it can recognise a notice
/// the tool layer already added and replace it rather than stacking a second
/// one.
pub(crate) const OUTPUT_TRUNCATED_MARKER: &str = "⚠️ OUTPUT TRUNCATED: you passed accept_large_output";

/// First line of the refusal returned when the context window is already spent.
pub(crate) const CONTEXT_LIMIT_REFUSAL_PREFIX: &str = "⚠️ CONTEXT LIMIT REACHED:";

/// First line of the refusal returned for an oversized output the caller did
/// not opt in to.
pub(crate) const OUTPUT_WITHHELD_PREFIX: &str = "⚠️ OUTPUT WITHHELD:";

/// Whether this output is one of the guard's refusals rather than content.
///
/// A refusal is a verdict: it carries no payload, and its sentence is what the
/// caller must act on. Both refusal sentences start with a fixed prefix, so a
/// layer that only sees the output can tell them apart from content without
/// re-deriving the guard's arithmetic.
pub(crate) fn is_context_refusal(output: &ToolOutput) -> bool {
    let text = output.output.trim_start();
    text.starts_with(CONTEXT_LIMIT_REFUSAL_PREFIX) || text.starts_with(OUTPUT_WITHHELD_PREFIX)
}

/// Record the opt-in on an output that actually carries content.
///
/// The round-owned budget reads this mark to repeat the truncate-vs-withhold
/// decision at its own running total. A refusal is not content: it carries no
/// payload and must reach the model whole, so marking it let the round treat
/// the actionable sentence as a payload and truncate it.
fn mark_accept_large_output(output: &mut ToolOutput) {
    if is_context_refusal(output) {
        return;
    }
    let metadata = output.metadata.get_or_insert_with(|| Value::Object(Default::default()));
    if let Some(object) = metadata.as_object_mut() {
        object.insert(ACCEPT_LARGE_OUTPUT_METADATA_KEY.to_string(), Value::Bool(true));
    }
}

/// Aliases are surface-dependent, but policy follows original identities for
/// the lifetime of the registry, including aliases retired after a reconnect.
#[derive(Default)]
struct McpPolicyIndex {
    current: HashMap<String, (String, String)>,
    history: HashMap<(String, String), HashSet<String>>,
}

/// Decides whether a tool that was registered inactive may run now.
///
/// The callback receives the registry name the caller asked for and returns
/// whether that name is callable after the attempt. It runs with no registry
/// lock held, so an activator may register the tools it just turned on.
pub type LazyToolActivator = Arc<dyn Fn(String) -> BoxFuture<'static, bool> + Send + Sync>;

/// Both halves of lazy activation under one lock: the callback, and the names
/// it has not turned on yet.
///
/// One lock keeps the two consistent - a name is either inactive with the
/// callback that can turn it on, or it is callable and absent from the set.
#[derive(Default)]
struct LazyActivation {
    activator: Option<LazyToolActivator>,
    inactive: HashSet<String>,
}

/// The scratch directory a child shell receives, re-exported so the approval
/// gate can assess the same path bash does.
#[cfg(not(windows))]
pub(crate) use bash::tool_scratch_dir_for_risk;

/// Registry of available tools (Arc-wrapped for sharing)
///
/// Clone creates a fresh CompactionManager so each subagent gets independent
/// message history tracking. Tools and skills are shared via Arc.
///
/// The tool registry: name to implementation, plus the dispatch gate every call
/// passes through.
pub struct Registry {
    tools: Arc<RwLock<HashMap<String, Arc<dyn Tool>>>>,
    mcp_policy: Arc<StdRwLock<McpPolicyIndex>>,
    /// Enabled servers the last MCP registration found, shared with every
    /// clone.
    ///
    /// The MCP fixed surface (`mcp_search`/`mcp_call`) is the discovery path
    /// for those servers, so a session keeps it eager only while this is above
    /// zero: with no server to discover the two schemas are pure overhead.
    configured_mcp_servers: Arc<AtomicUsize>,
    skills: Arc<RwLock<SkillRegistry>>,
    compaction: Arc<RwLock<CompactionManager>>,
    /// Shared with every clone: a tool activated in one clone must be active
    /// in all of them, and an activator registered once must survive the
    /// per-call and per-subagent clones.
    lazy_activation: Arc<StdRwLock<LazyActivation>>,
    /// ACP editor MCP proxies of this session (`session_mcp`), shared with
    /// every clone and kept out of `tools`.
    session_mcp: Arc<StdRwLock<session_mcp::Overlay>>,
}

/// Non-owning handle used by tools stored inside a registry.
///
/// A tool cannot strongly own the registry containing it without creating an
/// Arc cycle. Upgrade this handle only for the duration of a tool call.
pub(super) struct WeakRegistry {
    tools: std::sync::Weak<RwLock<HashMap<String, Arc<dyn Tool>>>>,
    mcp_policy: Arc<StdRwLock<McpPolicyIndex>>,
    configured_mcp_servers: Arc<AtomicUsize>,
    skills: Arc<RwLock<SkillRegistry>>,
    compaction: Arc<RwLock<CompactionManager>>,
    lazy_activation: Arc<StdRwLock<LazyActivation>>,
    /// ACP editor MCP proxies of this session (`session_mcp`), shared with
    /// every clone and kept out of `tools`.
    session_mcp: Arc<StdRwLock<session_mcp::Overlay>>,
}

impl WeakRegistry {
    pub(super) fn upgrade(&self) -> Option<Registry> {
        Some(Registry {
            tools: self.tools.upgrade()?,
            mcp_policy: Arc::clone(&self.mcp_policy),
            configured_mcp_servers: Arc::clone(&self.configured_mcp_servers),
            skills: Arc::clone(&self.skills),
            compaction: Arc::clone(&self.compaction),
            lazy_activation: Arc::clone(&self.lazy_activation),
            session_mcp: Arc::clone(&self.session_mcp),
        })
    }
}

impl Clone for Registry {
    fn clone(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            mcp_policy: Arc::clone(&self.mcp_policy),
            configured_mcp_servers: Arc::clone(&self.configured_mcp_servers),
            skills: self.skills.clone(),
            // Each clone gets a fresh CompactionManager to prevent parallel
            // subagents from corrupting each other's message history
            compaction: Arc::new(RwLock::new(CompactionManager::new())),
            lazy_activation: Arc::clone(&self.lazy_activation),
            session_mcp: Arc::clone(&self.session_mcp),
        }
    }
}

impl Registry {
    /// Clone for a caller that will EXECUTE a tool on behalf of the same
    /// session, such as the streaming loop's per-call clone or a member of a
    /// tool round.
    ///
    /// Everything is shared exactly as `Clone` shares it, except the
    /// `CompactionManager`, which is shared with the parent instead of being
    /// replaced by a fresh one. The context-overflow guard in `execute` reads
    /// that manager, so a plain clone makes the guard see the overhead floor
    /// against the default budget while this one sees the session's real
    /// budget and token count.
    ///
    /// Do NOT use this for a subagent: a subagent must keep an independent
    /// budget, which is exactly what `Clone` provides.
    pub(crate) fn clone_sharing_compaction(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            mcp_policy: Arc::clone(&self.mcp_policy),
            configured_mcp_servers: Arc::clone(&self.configured_mcp_servers),
            skills: self.skills.clone(),
            compaction: Arc::clone(&self.compaction),
            lazy_activation: Arc::clone(&self.lazy_activation),
            session_mcp: Arc::clone(&self.session_mcp),
        }
    }

    /// A registry whose tool map is a private copy of this one's, for a child
    /// that needs tools of its own.
    ///
    /// The tools themselves are the same `Arc`s, but registering or removing a
    /// name on the copy leaves the parent and every sibling clone untouched.
    /// A plain [`Clone`] shares the map, so a child-only tool (the `yield`
    /// submitter of a schema-spawned child) would leak into the coordinator's
    /// advertised surface and stay registered after the child finishes.
    ///
    /// Async because the copy reads the live map under its lock; a `tokio` read
    /// guard cannot be taken synchronously from an async context.
    pub async fn with_isolated_tools(&self) -> Self {
        let tools = self.tools.read().await.clone();
        let mut registry = self.clone();
        registry.tools = Arc::new(RwLock::new(tools));
        registry
    }

    /// A registry holding only `keep` from this one's map, with a private MCP
    /// alias index.
    ///
    /// [`Self::with_isolated_tools`] copies the map but keeps the shared MCP
    /// policy: removing or registering MCP tools on that copy rewrites an index
    /// this registry's siblings still use. A child that narrows its surface
    /// uses this instead, so the bookkeeping of the narrowed surface stays
    /// private too.
    pub(crate) async fn with_isolated_tool_subset(&self, keep: &[&str]) -> Self {
        let tools = {
            let guard = self.tools.read().await;
            guard
                .iter()
                .filter(|(name, _)| keep.contains(&name.as_str()))
                .map(|(name, tool)| (name.clone(), Arc::clone(tool)))
                .collect::<HashMap<_, _>>()
        };
        let mut registry = self.clone();
        registry.tools = Arc::new(RwLock::new(tools));
        registry.mcp_policy = Arc::default();
        registry
    }

    /// Desktop opts in through its inline response renderer capability.
    /// Keep this out of the base map so terminal/headless registries never
    /// advertise or execute it, including through batch and codemode.
    pub(crate) async fn set_user_response_enabled(&self, enabled: bool) {
        if enabled {
            self.register("user_response".into(), Arc::new(user_response::UserResponseTool))
                .await;
        } else {
            self.unregister("user_response").await;
        }
    }

    fn downgrade(&self) -> WeakRegistry {
        WeakRegistry {
            tools: Arc::downgrade(&self.tools),
            mcp_policy: Arc::clone(&self.mcp_policy),
            configured_mcp_servers: Arc::clone(&self.configured_mcp_servers),
            skills: Arc::clone(&self.skills),
            compaction: Arc::clone(&self.compaction),
            lazy_activation: Arc::clone(&self.lazy_activation),
            session_mcp: Arc::clone(&self.session_mcp),
        }
    }

    pub(crate) fn shared_skills_registry() -> Arc<RwLock<SkillRegistry>> {
        SkillRegistry::shared_registry()
    }

    fn insert_tool<T>(tools: &mut HashMap<String, Arc<dyn Tool>>, name: &str, tool: T)
    where
        T: Tool + 'static,
    {
        tools.insert(name.into(), Arc::new(tool) as Arc<dyn Tool>);
    }

    fn insert_tool_timed<T>(
        tools: &mut HashMap<String, Arc<dyn Tool>>,
        timings: &mut Vec<(String, u128)>,
        name: &str,
        make_tool: impl FnOnce() -> T,
    ) where
        T: Tool + 'static,
    {
        let start = std::time::Instant::now();
        Self::insert_tool(tools, name, make_tool());
        timings.push((name.to_string(), start.elapsed().as_millis()));
    }

    /// Create a lightweight empty registry (no tools, no skill loading).
    /// Used by remote-mode clients that don't execute tools locally.
    pub fn empty() -> Self {
        Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
            mcp_policy: Arc::default(),
            configured_mcp_servers: Arc::default(),
            skills: Arc::new(RwLock::new(SkillRegistry::default())),
            compaction: Arc::new(RwLock::new(CompactionManager::new())),
            // A registry with no tools has nothing to activate.
            lazy_activation: Arc::default(),
            session_mcp: Arc::default(),
        }
    }

    /// Base tools that are stateless and can be shared across sessions.
    /// Created once and cached in a OnceLock, then cloned (cheap Arc bumps) per session.
    pub(crate) fn base_tools(
        skills: &Arc<RwLock<SkillRegistry>>,
        allowed: Option<&HashSet<String>>,
    ) -> HashMap<String, Arc<dyn Tool>> {
        use std::sync::OnceLock;
        static BASE: OnceLock<HashMap<String, Arc<dyn Tool>>> = OnceLock::new();
        let base = match allowed {
            // The unrestricted surface is the common case and stays cached.
            None => BASE.get_or_init(Self::build_base_tools).clone(),
            // A restricted profile builds only what it may use: a built-in the
            // allow-list excludes is never constructed at all, instead of being
            // built first and filtered out of the tool list afterwards.
            Some(set) => {
                BUILD_ALLOW.with(|cell| *cell.borrow_mut() = Some(set.clone()));
                let built = Self::build_base_tools();
                BUILD_ALLOW.with(|cell| *cell.borrow_mut() = None);
                built
            }
        };

        // Clone the Arc entries (cheap refcount bumps, not deep copies)
        let mut tools = base;
        // SkillTool needs the skills registry reference (shared across sessions)
        Self::insert_tool(&mut tools, "skill_manage", skill::SkillTool::new(skills.clone()));
        // The swarm tool captures the user-editable swarm prompt in its
        // description. Construct it once per session rather than sharing the
        // process-wide instance. Existing sessions keep their stable tool
        // definition (and provider KV cache), while newly created agents see
        // prompt edits immediately.
        Self::insert_tool(&mut tools, "swarm", communicate::CommunicateTool::new());
        tools
    }

    /// Builds every built-in this process can construct on its own.
    ///
    /// BUILD_ALLOW carries the allow-list for this build on this thread, so a
    /// tool outside it is skipped before its constructor runs.
    fn build_base_tools() -> HashMap<String, Arc<dyn Tool>> {
        let init_start = std::time::Instant::now();
        let mut timings = Vec::new();
        let mut m = HashMap::new();
        if build_allows("read") {
            Self::insert_tool_timed(&mut m, &mut timings, "read", read::ReadTool::new);
        }
        if build_allows("write") {
            Self::insert_tool_timed(&mut m, &mut timings, "write", write::WriteTool::new);
        }
        if build_allows("agentgrep") {
            Self::insert_tool_timed(&mut m, &mut timings, "agentgrep", agentgrep::AgentGrepTool::new);
        }
        if build_allows("side_panel") {
            Self::insert_tool_timed(&mut m, &mut timings, "side_panel", side_panel::SidePanelTool::new);
        }
        if build_allows("panel") {
            Self::insert_tool_timed(&mut m, &mut timings, "panel", panel::PanelTool::new);
        }
        if build_allows("applet") {
            Self::insert_tool_timed(&mut m, &mut timings, "applet", applet::AppletTool::new);
        }
        if build_allows("edit") {
            Self::insert_tool_timed(&mut m, &mut timings, "edit", edit::EditTool::new);
        }
        // `multiedit` merged into `edit`, and `patch` into `apply_patch`.
        // Both old names still resolve through `resolve_tool_name`.
        if build_allows("replace") {
            Self::insert_tool_timed(&mut m, &mut timings, "replace", replace::ReplaceTool::new);
        }
        if build_allows("apply_patch") {
            Self::insert_tool_timed(&mut m, &mut timings, "apply_patch", apply_patch::ApplyPatchTool::new);
        }
        if build_allows("ls") {
            Self::insert_tool_timed(&mut m, &mut timings, "ls", ls::LsTool::new);
        }
        if build_allows("bash") {
            Self::insert_tool_timed(&mut m, &mut timings, "bash", bash::BashTool::new);
        }
        Self::insert_tool_timed(
            &mut m,
            &mut timings,
            "compile_remote",
            compile_remote::CompileRemoteTool::new,
        );
        if build_allows("browser") {
            Self::insert_tool_timed(&mut m, &mut timings, "browser", browser::BrowserTool::new);
        }
        if build_allows("open") {
            Self::insert_tool_timed(&mut m, &mut timings, "open", open::OpenTool::new);
        }
        #[cfg(target_os = "macos")]
        if build_allows("macos_computer_use") {
            Self::insert_tool_timed(&mut m, &mut timings, "macos_computer_use", computer::ComputerTool::new);
        }
        if build_allows("webfetch") {
            Self::insert_tool_timed(&mut m, &mut timings, "webfetch", webfetch::WebFetchTool::new);
        }
        if build_allows("websearch") {
            Self::insert_tool_timed(&mut m, &mut timings, "websearch", websearch::WebSearchTool::new);
        }
        if build_allows("invalid") {
            Self::insert_tool_timed(&mut m, &mut timings, "invalid", invalid::InvalidTool::new);
        }
        if build_allows("irc") {
            Self::insert_tool_timed(&mut m, &mut timings, "irc", irc::IrcTool::new);
        }
        Self::insert_tool_timed(
            &mut m,
            &mut timings,
            "horon_agent_docs",
            horon_agent_docs::HoronAgentDocsTool::new,
        );
        if build_allows("todo") {
            Self::insert_tool_timed(&mut m, &mut timings, "todo", todo::TodoTool::new);
        }
        if build_allows("goal") {
            Self::insert_tool_timed(&mut m, &mut timings, "goal", goal::InitiativeTool::new);
        }
        if build_allows("bg") {
            Self::insert_tool_timed(&mut m, &mut timings, "bg", bg::BgTool::new);
        }
        if build_allows("monitor") {
            Self::insert_tool_timed(&mut m, &mut timings, "monitor", monitor::MonitorTool::new);
        }
        Self::insert_tool_timed(
            &mut m,
            &mut timings,
            "session_search",
            session_search::SessionSearchTool::new,
        );
        if build_allows("memory") {
            Self::insert_tool_timed(&mut m, &mut timings, "memory", memory::MemoryTool::new);
        }
        if build_allows("gmail") {
            Self::insert_tool_timed(&mut m, &mut timings, "gmail", gmail::GmailTool::new);
        }
        if build_allows("calendar") {
            Self::insert_tool_timed(&mut m, &mut timings, "calendar", calendar::CalendarTool::new);
        }
        if build_allows("schedule") {
            Self::insert_tool_timed(&mut m, &mut timings, "schedule", ambient::ScheduleTool::new);
        }
        if build_allows("selfdev") {
            Self::insert_tool_timed(&mut m, &mut timings, "selfdev", selfdev::SelfDevTool::new);
        }
        if build_allows("desktop_selfdev") {
            Self::insert_tool_timed(
                &mut m,
                &mut timings,
                "desktop_selfdev",
                desktop_selfdev::DesktopSelfDevTool::new,
            );
        }
        let nonzero: Vec<String> = timings
            .iter()
            .filter(|(_, ms)| *ms > 0)
            .map(|(name, ms)| format!("{name}={ms}ms"))
            .collect();
        crate::logging::info(&format!(
            "[TIMING] registry_base_tools_init: total={}ms, nonzero=[{}]",
            init_start.elapsed().as_millis(),
            nonzero.join(", ")
        ));
        m
    }

    pub async fn new(_provider: Arc<dyn Provider>) -> Self {
        let start = std::time::Instant::now();
        let skills_start = std::time::Instant::now();
        let skills = Self::shared_skills_registry();
        let skills_ms = skills_start.elapsed().as_millis();
        let compaction_start = std::time::Instant::now();
        let compaction = Arc::new(RwLock::new(CompactionManager::new()));
        let compaction_ms = compaction_start.elapsed().as_millis();
        let registry_struct_start = std::time::Instant::now();
        let registry = Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
            mcp_policy: Arc::default(),
            configured_mcp_servers: Arc::default(),
            skills: skills.clone(),
            compaction: compaction.clone(),
            lazy_activation: Arc::default(),
            session_mcp: Arc::default(),
        };
        let registry_struct_ms = registry_struct_start.elapsed().as_millis();

        let base_start = std::time::Instant::now();
        // The allow-list is resolved BEFORE any built-in is constructed, so a
        // tool this profile excludes never runs its constructor at all.
        let construction_allow = crate::config::config().tools.allowed_tools();
        let mut tools_map = Self::base_tools(&skills, construction_allow.as_ref());
        let base_ms = base_start.elapsed().as_millis();

        // Per-session tools that need provider/registry references
        let session_tools_start = std::time::Instant::now();
        Self::insert_tool(&mut tools_map, "batch", batch::BatchTool::new(registry.downgrade()));
        Self::insert_tool(
            &mut tools_map,
            "conversation_search",
            conversation_search::ConversationSearchTool::new(compaction),
        );
        // Codemode is opt-in (`[tools] codemode = true`). When off, the tool
        // is never registered, so it costs nothing and never reaches a model.
        if codemode::enabled() {
            Self::insert_tool(
                &mut tools_map,
                "codemode",
                codemode::CodemodeTool::new(registry.downgrade()),
            );
        }
        Self::insert_tool(&mut tools_map, "undo", undo::UndoTool::new());
        Self::insert_tool(&mut tools_map, "ask", ask::AskTool::new());
        if undo::redo_enabled() {
            Self::insert_tool(&mut tools_map, "redo", undo::RedoTool::new());
        }
        if hashline_edit::enabled() {
            Self::insert_tool(&mut tools_map, "hashline_edit", hashline_edit::HashlineEditTool::new());
        }
        if ast_tools::enabled() {
            Self::insert_tool(&mut tools_map, "ast_grep", ast_tools::AstGrepTool::new());
            Self::insert_tool(&mut tools_map, "ast_edit", ast_tools::AstEditTool::new());
        }
        // The dispatcher for the surface a profile does not send eagerly exists
        // under `full` and `core`. It is deliberately absent from `CORE_TOOLS`
        // and from the `acp` allow-list; the `core` allow-list joins it
        // explicitly, and the tier resolver pins it `Essential` so the one tool
        // that can reach the listed surface is never itself deferred.
        // `lsp` is opt-in (`[tools] lsp = true`): it starts a language server
        // process, so it stays out of every default profile until asked for.
        if lsp::enabled() {
            Self::insert_tool(&mut tools_map, "lsp", lsp::LspTool::new());
        }
        // Context notes are opt-in (`[tools] context_notes = true`): the notes
        // are injected into every request, so the switch also gates the prompt
        // section, not just this tool.
        if context_notes::enabled() {
            Self::insert_tool(&mut tools_map, "context_notes", context_notes::ContextNotesTool::new());
        }
        if toolbox::enabled() {
            Self::insert_tool(
                &mut tools_map,
                crate::config::TOOLBOX_TOOL,
                toolbox::ToolboxTool::new(registry.downgrade()),
            );
        }
        // Integration discovery is on by default (opt-out); when disabled the
        // tool is never registered and no discovery endpoint is ever
        // contacted.
        if crate::config::config().sponsors.enabled {
            Self::insert_tool(&mut tools_map, "integration_tools", discover::DiscoverToolsTool::new());
        }
        let session_tools_ms = session_tools_start.elapsed().as_millis();

        let write_start = std::time::Instant::now();
        *registry.tools.write().await = tools_map;
        let write_ms = write_start.elapsed().as_millis();
        crate::logging::info(&format!(
            "[TIMING] registry_new: skills={}ms, compaction={}ms, registry_struct={}ms, base_tools={}ms, session_tools={}ms, write={}ms, total={}ms",
            skills_ms,
            compaction_ms,
            registry_struct_ms,
            base_ms,
            session_tools_ms,
            write_ms,
            start.elapsed().as_millis()
        ));
        // The approval tool is the one tool whose registration depends on a
        // setting rather than a profile: an unattended run has nobody to answer
        // unless the operator opted in. Registering here, inside the constructor
        // every production path uses, is what makes the allow-list entry in
        // `selection_with_ambient` reachable for a run that never becomes an
        // ambient session.
        registry.register_approval_tool_if_enabled().await;
        registry.register_aux_completion_tool_if_enabled().await;
        registry.register_plan_tool_if_enabled().await;
        registry
    }

    /// Get all tool definitions for the API
    ///
    /// A tool registered inactive is absent here until its activator turns it
    /// on: the definition list is what reaches the model, so an inactive tool
    /// must not appear in it at all.
    pub async fn definitions(&self, allowed_tools: Option<&HashSet<String>>) -> Vec<ToolDefinition> {
        let tools = self.tools.read().await;
        let inactive = self.inactive_lazy_tools();
        let overlay = self.session_mcp();
        let mut defs: Vec<ToolDefinition> = Self::visible_tools(&tools, overlay.as_ref())
            .filter(|(name, _)| !inactive.contains(*name))
            .filter(|(name, _)| allowed_tools.is_none_or(|set| self.tool_is_allowed(set, name)))
            .map(|(name, tool)| {
                let mut def = tool.to_definition();
                // Use registry key as the tool name (important for MCP tools where
                // the registry key is "mcp__server__tool" but Tool::name() returns
                // just the raw tool name)
                if def.name != *name {
                    def.name = name.clone();
                }
                def
            })
            .collect();

        // Sort by name for deterministic ordering - critical for prompt cache hits
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Definitions a session may call with the allow and deny filters applied.
    pub async fn definitions_filtered(
        &self,
        allowed_tools: Option<&HashSet<String>>,
        disabled_tools: &HashSet<String>,
    ) -> Vec<ToolDefinition> {
        let mut defs = self.definitions(allowed_tools).await;
        if !disabled_tools.is_empty() {
            defs.retain(|def| !self.tool_is_disabled(disabled_tools, &def.name));
        }
        defs
    }

    pub async fn tool_names(&self) -> Vec<String> {
        let tools = self.tools.read().await;
        let inactive = self.inactive_lazy_tools();
        let overlay = self.session_mcp();
        Self::visible_tools(&tools, overlay.as_ref())
            .map(|(name, _)| name)
            .filter(|name| !inactive.contains(*name))
            .cloned()
            .collect()
    }

    /// The tools the turn running now sees: the registry's own, minus those
    /// an ACP session's editor servers shadow, plus the editor proxies. With
    /// no visible overlay this is exactly `tools`.
    fn visible_tools<'a>(
        tools: &'a HashMap<String, Arc<dyn Tool>>,
        overlay: Option<&'a session_mcp::ActiveOverlay>,
    ) -> impl Iterator<Item = (&'a String, &'a Arc<dyn Tool>)> {
        tools
            .iter()
            .filter(move |(name, tool)| overlay.is_none_or(|overlay| !overlay.hides(name, tool)))
            .chain(overlay.into_iter().flat_map(|overlay| overlay.tools().iter()))
    }

    /// The tool `name` resolves to for the turn running now; see
    /// [`Self::visible_tools`].
    fn visible_tool(
        tools: &HashMap<String, Arc<dyn Tool>>,
        overlay: Option<&session_mcp::ActiveOverlay>,
        name: &str,
    ) -> Option<Arc<dyn Tool>> {
        if let Some(tool) = overlay.and_then(|overlay| overlay.get(name)) {
            return Some(tool);
        }
        tools
            .get(name)
            .filter(|tool| overlay.is_none_or(|overlay| !overlay.hides(name, tool)))
            .cloned()
    }

    /// Concurrency class of the tool a model-supplied name resolves to.
    ///
    /// The name resolves the way `execute` resolves a built-in: a `functions.`
    /// prefix is stripped and provider aliases are mapped, so every name that
    /// reaches a tool in `execute` is classified here too. A name that resolves
    /// to nothing answers `Ordered`, because an unknown tool must never be
    /// treated as safe to overlap.
    ///
    /// A session-scoped SDK custom tool is a callback, not a registry entry, so
    /// it answers `Ordered` unless its name is also a built-in alias, in which
    /// case this reports the built-in's class while `execute` would call the
    /// callback. A scheduler must therefore consult `sdk::custom` as well.
    pub async fn concurrency_class(&self, name: &str) -> ConcurrencyClass {
        let resolved_name = Self::resolve_tool_name(name);
        // An inactive tool is not callable, so it must never be treated as safe
        // to overlap.
        if self.is_lazy_inactive(resolved_name) {
            return ConcurrencyClass::Ordered;
        }
        let tools = self.tools.read().await;
        Self::visible_tool(&tools, self.session_mcp().as_ref(), resolved_name)
            .map(|tool| tool.concurrency_class())
            .unwrap_or(ConcurrencyClass::Ordered)
    }

    /// Effects the named tool declares for this input, without activating it.
    ///
    /// `None` means the name cannot be classified here: an SDK callback, a
    /// lazily inactive tool, or an unknown name. Every caller treats that as a
    /// write barrier, because a read-only gate must never be opened by a tool
    /// whose reach it cannot see.
    pub(crate) async fn declared_effects(&self, name: &str, input: &Value) -> Option<ToolEffects> {
        if self.is_lazy_inactive(name) {
            return None;
        }
        let tools = self.tools.read().await;
        Self::visible_tool(&tools, self.session_mcp().as_ref(), name).map(|tool| tool.effects_for_input(input))
    }

    /// Synchronous counterpart for `Agent::validate_tool_allowed`, which runs
    /// before a round dispatches and cannot await the map. A contended map, an
    /// unknown name, and a lazily inactive tool all answer `None`, which callers
    /// read as a barrier.
    pub(crate) fn declared_effects_sync(&self, name: &str) -> Option<ToolEffects> {
        if self.is_lazy_inactive(name) {
            return None;
        }
        let tools = self.tools.try_read().ok()?;
        Self::visible_tool(&tools, self.session_mcp().as_ref(), name).map(|tool| tool.effects())
    }

    /// Synchronous [`Tool::effects_for_input`] lookup for a caller that must
    /// classify a call without awaiting: `toolbox` answers its own
    /// `effects_for_input` with the effects of the tool it would run.
    ///
    /// The name resolves exactly as [`Registry::execute`] resolves it. A
    /// contended map, an unknown name, and a lazily inactive tool answer `None`,
    /// which callers read as a barrier. `try_read` succeeds alongside other
    /// readers and fails only while a writer holds the lock or is already queued
    /// ahead of it, so a nested lookup under a held read guard degrades to the
    /// write barrier instead of waiting; that costs parallelism, never safety.
    pub(crate) fn declared_effects_for_input_sync(&self, name: &str, input: &Value) -> Option<ToolEffects> {
        let resolved_name = Self::resolve_tool_name(name);
        if self.is_lazy_inactive(resolved_name) {
            return None;
        }
        let tools = self.tools.try_read().ok()?;
        Self::visible_tool(&tools, self.session_mcp().as_ref(), resolved_name).map(|tool| tool.effects_for_input(input))
    }

    /// Declared effects of one call to the tool a model-supplied name resolves to.
    ///
    /// The name resolves exactly as [`Registry::concurrency_class`] resolves it.
    /// The effects come from [`Tool::effects_for_input`], so a tool whose reach
    /// depends on its input is measured per call: `read` of a remote path
    /// spends a network request while `read` of a local file does not.
    ///
    /// A name that resolves to nothing, or to an inactive lazy tool, answers
    /// [`ToolEffects::write`]: an unknown tool must never be treated as safe to
    /// overlap.
    pub async fn effects_for_call(&self, name: &str, input: &Value) -> ToolEffects {
        let resolved_name = Self::resolve_tool_name(name);
        if self.is_lazy_inactive(resolved_name) {
            return ToolEffects::write();
        }
        self.declared_effects(resolved_name, input)
            .await
            .unwrap_or_else(ToolEffects::write)
    }

    /// Tool definitions a session may call, honoring its registered allow and
    /// deny policy. Per-server MCP tools are always listed individually and
    /// the fixed `mcp_search`/`mcp_call` surface is omitted, because scripts
    /// address MCP tools directly. Used by `codemode`.
    pub(crate) async fn definitions_for_session(&self, session_id: &str) -> Vec<ToolDefinition> {
        let policy = session_tool_policy(session_id);
        let allowed = policy.as_ref().and_then(|p| p.allowed_tools.clone());
        let disabled = policy.as_ref().map(|p| p.disabled_tools.clone()).unwrap_or_default();
        let mut defs = self.definitions_filtered(allowed.as_ref(), &disabled).await;
        defs.retain(|def| !is_fixed_mcp_tool(&def.name));
        defs
    }

    /// Enable test mode for memory tools (isolated storage)
    /// Called when session is marked as debug
    pub async fn enable_memory_test_mode(&self) {
        let mut tools = self.tools.write().await;

        // Replace memory tool with test version
        tools.insert(
            "memory".to_string(),
            Arc::new(memory::MemoryTool::new_test()) as Arc<dyn Tool>,
        );

        crate::logging::info("Memory test mode enabled - using isolated storage");
    }

    /// Resolve tool name aliases.
    ///
    /// When using OAuth, the API presents tools with Claude Code names
    /// (e.g. `file_grep`, `shell_exec`). The model uses those names in
    /// sub-tool calls (e.g. inside `batch`), but our registry uses internal
    /// names (`grep`, `bash`). This mapping ensures both forms resolve
    /// correctly.
    ///
    /// The canonical mapping lives in `horon-agent-tool-types::resolve_tool_name` so
    /// lower-level crates (e.g. config) can normalize tool names without
    /// depending on the tool subsystem; this method delegates to it.
    pub(crate) fn resolve_tool_name(name: &str) -> &str {
        horon_agent_tool_types::resolve_tool_name(name)
    }

    /// Estimate token count for a string (chars / 4, matching compaction heuristic)
    fn estimate_tokens(s: &str) -> usize {
        crate::util::estimate_tokens(s)
    }

    /// Paths a tool call names in its input, labelled by the input key they
    /// came from, resolved against the session working directory. Covers the
    /// common path keys, `paths` arrays (first 8), and `apply_patch` file
    /// headers (Codex envelope and unified diff).
    fn touched_paths(input: &Value, ctx: &ToolContext) -> Vec<(&'static str, std::path::PathBuf)> {
        let mut touched = Vec::new();
        let Some(object) = input.as_object() else {
            return touched;
        };
        let resolve = |path: &str| ctx.resolve_path(std::path::Path::new(path));
        for key in ["file_path", "path", "target", "target_path", "old_path", "new_path"] {
            if let Some(path) = object.get(key).and_then(Value::as_str) {
                touched.push((key, resolve(path)));
            }
        }
        if let Some(paths) = object.get("paths").and_then(Value::as_array) {
            for path in paths.iter().filter_map(Value::as_str).take(8) {
                touched.push(("paths", resolve(path)));
            }
        }
        if let Some(patch) = object.get("patch_text").and_then(Value::as_str) {
            for path in Self::patch_paths(patch) {
                if !touched.iter().any(|(_, existing)| *existing == resolve(path)) {
                    touched.push(("patch", resolve(path)));
                }
            }
        }
        touched
    }

    /// File paths named in patch headers. Deletions are included because the
    /// path was still touched; `/dev/null` sides of unified diffs are not.
    fn patch_paths(patch: &str) -> Vec<&str> {
        let mut paths = Vec::new();
        for line in patch.lines() {
            let path = [
                "*** Add File: ",
                "*** Update File: ",
                "*** Delete File: ",
                "*** Move to: ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .or_else(|| {
                let header = line.strip_prefix("+++ ").or_else(|| line.strip_prefix("--- "))?;
                let header = header.split('\t').next().unwrap_or(header).trim();
                if header == "/dev/null" {
                    return None;
                }
                Some(
                    header
                        .strip_prefix("a/")
                        .or_else(|| header.strip_prefix("b/"))
                        .unwrap_or(header),
                )
            });
            if let Some(path) = path.map(str::trim).filter(|path| !path.is_empty())
                && !paths.contains(&path)
            {
                paths.push(path);
            }
        }
        paths
    }

    /// The request already opened for one approval decision, keyed by session,
    /// tool and input.
    ///
    /// The key is the decision the user is answering, not the individual call: a
    /// model that retries a refused call issues a new `tool_call_id` each time, so
    /// keying on the call id would enqueue one request per retry and leave the
    /// mode permanently unsatisfiable.
    fn open_approval_requests() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
        static OPEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
            std::sync::OnceLock::new();
        OPEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    /// A stable key for the decision a call needs.
    fn approval_key(session_id: &str, tool: &str, input: &Value, ctx: &ToolContext) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(input).unwrap_or_default().hash(&mut hasher);
        // The working directory is part of the decision: `rm build` in one
        // directory is not the same request as in another, and an approval must
        // not carry across.
        ctx.working_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default()
            .hash(&mut hasher);
        format!("{session_id}|{tool}|{:x}", hasher.finish())
    }

    /// What the approval policy decided for this call, now.
    ///
    /// `None` means the call may proceed: either a request this decision opened
    /// was approved, or there is no request for it yet and the caller has already
    /// decided the call needs approval.
    ///
    /// A refusal is only useful if answering it changes the next attempt, so this
    /// consults the decision recorded for the request this decision already
    /// opened:
    ///
    /// - no request yet: queue one and refuse, naming it;
    /// - request still pending: refuse, naming the same request, and queue
    ///   nothing new;
    /// - request approved: let the call through;
    /// - request denied: refuse with the denial, and queue nothing new.
    ///
    /// The queue is the existing [`crate::safety::PermissionRequest`] queue; this
    /// adds no second store and no file format.
    fn approval_refusal(
        resolved_name: &str,
        reason: &str,
        is_dual_confirm: bool,
        input: &Value,
        ctx: &ToolContext,
    ) -> Option<String> {
        use crate::safety::PermissionResult;

        let system = crate::tool::ambient::shared_safety_system();
        let key = Self::approval_key(&ctx.session_id, resolved_name, input, ctx);

        // Claim the slot under one lock. Lookup and insert were separate before,
        // so two concurrent identical calls both missed and both enqueued, leaving
        // one request orphaned. Inserting a placeholder here means the loser sees
        // a request in flight and waits for it rather than minting a second one.
        let claimed = {
            let mut open = Self::open_approval_requests()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match open.get(&key) {
                Some(existing) => Some(existing.clone()),
                None => {
                    open.insert(key.clone(), String::new());
                    None
                }
            }
        };

        if let Some(request_id) = claimed {
            if request_id.is_empty() {
                // Another thread claimed this key and is queueing the request
                // right now. Refuse without enqueueing; the next attempt finds it.
                return Some(format!(
                    "Tool '{resolved_name}' needs approval before it runs: {reason}. A request for this \
                     call is being queued right now; retry to get its id. Nothing was executed."
                ));
            }
            if let Some(decision) = system.decision_for(&request_id) {
                // An approval releases this call once and the key is dropped, so a
                // second identical call asks again. A denial is different: it is
                // the user saying no, and it has to keep holding. Dropping the key
                // on denial let the very next retry forget the answer and mint a
                // fresh request, which also wrote the denied id back into the
                // queue as if it were still pending.
                if decision.approved {
                    Self::open_approval_requests()
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&key);
                }
                // Clear the prompt a client was shown for this request. Emitting
                // only the pending event would leave it waiting on a decision
                // that has already been made.
                crate::tool::ambient::emit_approval_resolved(
                    &ctx.session_id,
                    &request_id,
                    decision.approved,
                    decision.decided_via.as_str(),
                    "approval-gate",
                );
                if decision.approved {
                    let mut fields = Self::tool_lifecycle_fields("approved", resolved_name, resolved_name, input, ctx);
                    fields.push(("block_reason".to_string(), "approval".to_string()));
                    fields.push(("approval_request_id".to_string(), request_id));
                    crate::logging::event_info("TOOL_LIFECYCLE", fields);
                    return None;
                }
                return Some(format!(
                    "Tool '{resolved_name}' was denied: request {request_id} was answered no. Nothing was executed."
                ));
            }
            if system.pending_requests().iter().any(|request| request.id == request_id) {
                return Some(format!(
                    "Tool '{resolved_name}' needs approval before it runs: {reason}. Request {request_id} \
                     is queued; answer it through the approval surface. Nothing was executed."
                ));
            }
        }

        let request = Self::gate_permission_request(
            crate::safety::new_request_id(),
            resolved_name,
            reason,
            is_dual_confirm,
            input,
            ctx,
            // The answer arrives through the approval surface after this call
            // returns; blocking here would hold the turn open for it.
            false,
        );

        let description = request.description.clone();
        let queued = system.request_permission(request);
        match queued {
            PermissionResult::Queued { request_id } => {
                Self::open_approval_requests()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(key, request_id.clone());
                // A durable record, written the way ambient's permission tool
                // does, so `horon-agent approvals resolve` can answer this request
                // from the CLI. Without one the CLI bails with "no blocked approval
                // record for request id"; the failure was recorded in
                // .omo/evidence/autonomous-approval-and-failover/task-f3-manual-qa.md
                // before this gate existed, and this call is what removes it.
                Self::record_blocked_approval(&ctx.session_id, &request_id);
                // Emit the pending event so a client that already has a session
                // event sink can render the prompt and answer through the existing
                // `permission_decision` frame.
                //
                // Measured limit: the sink is registered in
                // `start_processing_message`, so a client that has subscribed but
                // not started a turn has no sink, `emit_session_event` returns
                // `NoSink`, and the frame is dropped - the daemon logs "had no
                // client attached". Registering on connection setup is a change to
                // the server's fanout lifecycle, which this plan does not cover.
                // The request is answerable by whoever holds its id: the refusal
                // text carries it, and `record_blocked_approval` below gives the
                // CLI a record to resolve.
                crate::tool::ambient::emit_approval_pending(
                    &ctx.session_id,
                    &request_id,
                    resolved_name,
                    chrono::Utc::now() + chrono::Duration::seconds(1800),
                );
                crate::safety::dispatch_permission_notification(resolved_name, &description, &request_id);
                Some(format!(
                    "Tool '{resolved_name}' needs approval before it runs: {reason}. Request {request_id} \
                     is queued; answer it through the approval surface. Nothing was executed."
                ))
            }
            // Defensive: `request_permission` reports `Queued` unconditionally
            // today and has no failure return. The arm stays so a future failure
            // return is reported instead of being mistaken for a queue entry.
            other => {
                // Release the claim so a later attempt can try again rather than
                // waiting on a placeholder forever.
                Self::open_approval_requests()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&key);
                Some(Self::unqueued_refusal(resolved_name, reason, &other))
            }
        }
    }

    /// The refusal for a request the safety queue did not accept.
    fn unqueued_refusal(resolved_name: &str, reason: &str, result: &crate::safety::PermissionResult) -> String {
        format!(
            "Tool '{resolved_name}' needs approval before it runs: {reason}. The request could not \
             be queued ({result:?}), so there is nothing to answer. Nothing was executed."
        )
    }

    /// The queue entry the approval gate opens for one call.
    ///
    /// Shared by the refuse-and-retry path and the ACP permission bridge, so a
    /// request looks the same in the queue, the durable record and the CLI
    /// whichever surface ends up answering it. `wait` says whether a caller
    /// actually blocks on the answer.
    fn gate_permission_request(
        id: String,
        resolved_name: &str,
        reason: &str,
        is_dual_confirm: bool,
        input: &Value,
        ctx: &ToolContext,
        wait: bool,
    ) -> crate::safety::PermissionRequest {
        use crate::safety::{ApprovalDefault, PermissionRequest, Urgency};

        let now = chrono::Utc::now();
        PermissionRequest {
            id,
            action: resolved_name.to_string(),
            description: format!("{resolved_name} was refused by the approval policy: {reason}"),
            rationale: if is_dual_confirm {
                "This tool belongs to a class the configuration lists as requiring a second \
                 confirmation, so it asks even when approval mode is off."
                    .to_string()
            } else {
                format!("The configured approval mode asks before {resolved_name} runs.")
            },
            urgency: if is_dual_confirm {
                Urgency::High
            } else {
                Urgency::Normal
            },
            wait,
            created_at: now,
            context: Some(serde_json::json!({
                "session_id": ctx.session_id.clone(),
                "message_id": ctx.message_id.clone(),
                "tool_call_id": ctx.tool_call_id.clone(),
                "working_dir": ctx.working_dir.as_ref().map(|dir| dir.display().to_string()),
                "requested_at": now.to_rfc3339(),
                "input": input,
            })),
            run_id: None,
            expires_at: None,
            default_on_timeout: ApprovalDefault::Deny,
        }
    }

    /// Write the durable record an external approver resolves against.
    ///
    /// `BlockedApprovalStore` is the same store ambient's `request_permission`
    /// writes to, and `horon-agent approvals resolve` reads. A request queued by
    /// this gate without a record is answerable only by whoever knows its id and
    /// can send `permission_decision` directly.
    /// Write the record the approval gate writes, for a test that needs to read
    /// it back rather than construct it.
    #[cfg(test)]
    pub(crate) fn record_blocked_approval_for_test(session_id: &str, request_id: &str) {
        Self::record_blocked_approval(session_id, request_id);
    }

    /// Write the durable record an external approver resolves against.
    ///
    /// `BlockedApprovalStore` is the same store ambient's `request_permission`
    /// writes to, and `horon-agent approvals resolve` reads. A request queued by
    /// this gate without a record is answerable only by whoever knows its id and
    /// can send `permission_decision` directly.
    ///
    /// The record carries a terminal `outcome` so the ambient resume sweep leaves
    /// it alone; see the field comment.
    fn record_blocked_approval(session_id: &str, request_id: &str) {
        use crate::ambient::{BlockedApproval, BlockedApprovalStore};

        let Some(store) = BlockedApprovalStore::open_default().ok() else {
            return;
        };
        let record = BlockedApproval {
            session_id: session_id.to_string(),
            request_id: request_id.to_string(),
            created_at: chrono::Utc::now(),
            // `wait: false` - the gate does not block the turn, so there is no
            // waiter pid to lease.
            //
            // A terminal `outcome` keeps the ambient resume sweep's hands off this
            // record. `classify_blocked_approval` (ambient/resume.rs:591) returns
            // `Idle` for `resumed`, `denied`, `timeout_denied` and `expired`, and
            // treats a recorded approval as `Resume` otherwise - which runs a whole
            // resume turn. A reviewer read this chain and confirmed by mutation
            // that the marker is what keeps the sweep out; a daemon run from
            // approval through a resume turn was not performed.
            //
            // `expired` rather than `denied`: the marker says "no runner owns this",
            // not "the user said no". `denied` would be a false claim about a
            // decision, and the CLI's own update rules would not overwrite it.
            //
            // The gate does not need the runner: it reads the decision itself on
            // the retry and applies it there. So this record exists to be resolvable
            // from the CLI and to keep the sweep away from it.
            outcome: Some("expired".to_string()),
            ..BlockedApproval::default()
        };
        if let Err(error) = store.add(record) {
            crate::logging::warn(&format!(
                "approval record for request {request_id} could not be created: {error}"
            ));
        }
    }

    fn tool_lifecycle_fields(
        phase: &str,
        requested_name: &str,
        resolved_name: &str,
        input: &Value,
        ctx: &ToolContext,
    ) -> Vec<(String, String)> {
        let cwd = ctx
            .working_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "none".to_string());
        let input_json = serde_json::to_string(input).unwrap_or_default();
        let mut fields = vec![
            ("phase".to_string(), phase.to_string()),
            ("tool_name".to_string(), requested_name.to_string()),
            ("resolved_tool_name".to_string(), resolved_name.to_string()),
            ("session_id".to_string(), ctx.session_id.clone()),
            ("message_id".to_string(), ctx.message_id.clone()),
            ("tool_call_id".to_string(), ctx.tool_call_id.clone()),
            ("execution_mode".to_string(), format!("{:?}", ctx.execution_mode)),
            ("cwd".to_string(), cwd),
            ("input_json_bytes".to_string(), input_json.len().to_string()),
        ];

        if let Some(object) = input.as_object() {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            fields.push(("input_keys".to_string(), keys.join(",")));

            let touched_paths = Self::touched_paths(input, ctx)
                .into_iter()
                .map(|(key, path)| format!("{key}:{}", path.display()))
                .collect::<Vec<_>>();
            if !touched_paths.is_empty() {
                fields.push(("touched_paths".to_string(), touched_paths.join(",")));
                fields.push(("touched_path_count".to_string(), touched_paths.len().to_string()));
            }

            for text_key in ["command", "prompt", "task", "query", "content"] {
                if let Some(text) = object.get(text_key).and_then(Value::as_str) {
                    fields.push((format!("{text_key}_bytes"), text.len().to_string()));
                    fields.push((format!("{text_key}_chars"), text.chars().count().to_string()));
                }
            }
        }

        fields
    }

    /// Maximum fraction of context budget a single tool output may consume.
    /// Outputs that would push total context beyond this are truncated.
    pub(crate) const CONTEXT_GUARD_THRESHOLD: f32 = 0.90;

    /// Token ceiling the context guard enforces: the budget times
    /// [`Self::CONTEXT_GUARD_THRESHOLD`]. One function, so a round-owned
    /// budget and the tool layer cannot disagree about where the line is.
    pub(crate) fn context_guard_threshold_tokens(budget: usize) -> usize {
        (budget as f32 * Self::CONTEXT_GUARD_THRESHOLD) as usize
    }

    /// Ceiling for one output: proportional to the budget, but also absolutely
    /// capped, because 30% of a 1M-token window is 300k tokens and no single
    /// tool result is worth that.
    pub(crate) fn single_output_max_tokens(budget: usize) -> usize {
        ((budget as f32 * Self::SINGLE_OUTPUT_MAX_FRACTION) as usize).min(Self::SINGLE_OUTPUT_MAX_TOKENS)
    }

    /// Message returned when the context already sits at the guard's ceiling.
    ///
    /// There is no room left to spend, so opting in cannot buy anything: the
    /// caller is told to make room first. The round-owned budget prints the
    /// same sentence, with the round's own running total, through this function.
    pub(crate) fn exhausted_context_refusal(output_tokens: usize, current_tokens: usize, budget: usize) -> String {
        format!(
            "{CONTEXT_LIMIT_REFUSAL_PREFIX} cannot return this tool output (~{:.0}k tokens) \
             because the context window is nearly full ({:.0}k/{}k tokens). \
             accept_large_output does not apply here: there is no room left to spend. \
             Use /compact to free space, then retry with a narrower query.",
            output_tokens as f32 / 1000.0,
            current_tokens as f32 / 1000.0,
            budget / 1000,
        )
    }

    /// Notice appended when an output is truncated because its caller passed
    /// `accept_large_output`.
    ///
    /// One definition, so the tool layer and the round-owned budget print the
    /// same sentence and a caller cannot tell which layer truncated it.
    pub(crate) fn truncated_output_notice(
        output_tokens: usize,
        max_tokens: usize,
        current_tokens: usize,
        budget: usize,
    ) -> String {
        format!(
            "{OUTPUT_TRUNCATED_MARKER}, so this ~{:.0}k \
             token result was returned truncated instead of withheld \
             ({:.0}k/{}k tokens were already used). Only the first ~{:.0}k tokens are \
             above, and they are now spent from this session's context. If the answer \
             is not in them, narrow the query rather than repeating this call.",
            output_tokens as f32 / 1000.0,
            current_tokens as f32 / 1000.0,
            budget / 1000,
            max_tokens as f32 / 1000.0,
        )
    }

    /// Whether the call that produced `output` passed `accept_large_output`.
    ///
    /// The tool layer records the flag on the output because the round-owned
    /// budget never sees the call's input.
    pub(crate) fn accept_large_output_was_passed(output: &ToolOutput) -> bool {
        output
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(ACCEPT_LARGE_OUTPUT_METADATA_KEY))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Event payload shared by the `post_tool` observer and the
    /// `post_tool_feedback` hook.
    fn post_tool_event(
        event_name: &'static str,
        resolved_name: &str,
        input: &Value,
        ctx: &ToolContext,
        result: std::result::Result<&ToolOutput, &anyhow::Error>,
        latency_ms: u64,
    ) -> crate::hooks::HookEvent {
        let mut unique_paths: Vec<String> = Vec::new();
        for (_, path) in Self::touched_paths(input, ctx) {
            let path = path.display().to_string();
            if !unique_paths.contains(&path) {
                unique_paths.push(path);
            }
        }
        let mut event = crate::hooks::HookEvent::new(event_name)
            .session_id(ctx.session_id.clone())
            .field("TOOL_NAME", resolved_name)
            .field("TOOL_CALL_ID", ctx.tool_call_id.clone())
            .field("TOOL_INPUT", crate::hooks::truncate_tool_input(&input.to_string()))
            .field("TOUCHED_PATHS", unique_paths.join("\n"))
            .field("STATUS", if result.is_ok() { "ok" } else { "error" })
            .field("DURATION_MS", latency_ms.to_string());
        if let Some(dir) = &ctx.working_dir {
            event = event.cwd(dir.display().to_string());
        }
        match result {
            Ok(output) => {
                event = event.field("OUTPUT_BYTES", output.output.len().to_string());
            }
            Err(error) => {
                const ERROR_LIMIT: usize = 1000;
                let message: String = error.to_string().chars().take(ERROR_LIMIT).collect();
                event = event.field("ERROR", message);
            }
        }
        event
    }

    /// Fire the `post_tool` observer hook with tool outcome metadata.
    /// No-op (without building the payload) when the hook is not configured.
    ///
    /// Deliberately NOT serialized by `HOOK_SERIAL`: `dispatch_observer` spawns a
    /// detached child and returns at once, so the script body runs after this
    /// call and cannot be ordered by any lock held here. `post_tool` (and the
    /// turn/session observers) already behave that way today; this keeps the
    /// documented detached contract unchanged.
    fn fire_post_tool_hook(
        resolved_name: &str,
        input: &Value,
        ctx: &ToolContext,
        result: &Result<ToolOutput>,
        latency_ms: u64,
    ) {
        if !crate::hooks::hook_configured("post_tool") {
            return;
        }
        crate::hooks::dispatch_observer(Self::post_tool_event(
            "post_tool",
            resolved_name,
            input,
            ctx,
            result.as_ref(),
            latency_ms,
        ));
    }

    /// Run the synchronous `post_tool_feedback` hooks after a successful tool
    /// call and append whatever they print to the tool result, so linters and
    /// checkers can feed findings straight back to the model.
    ///
    /// One of the three awaited hooks serialized by `HOOK_SERIAL`; the input
    /// transformer and the `pre_tool` gate take the same lock.
    async fn apply_post_tool_feedback(
        resolved_name: &str,
        input: &Value,
        ctx: &ToolContext,
        latency_ms: u64,
        output: &mut ToolOutput,
    ) {
        if !crate::hooks::hook_configured("post_tool_feedback") {
            return;
        }
        let event = Self::post_tool_event("post_tool_feedback", resolved_name, input, ctx, Ok(output), latency_ms);
        let input_json = input.to_string();
        let feedback = {
            let _serial = HOOK_SERIAL.lock().await;
            crate::hooks::run_post_tool_feedback(&event, &input_json).await
        };
        if let Some(feedback) = feedback {
            if !output.output.is_empty() && !output.output.ends_with('\n') {
                output.output.push('\n');
            }
            output.output.push_str("\n[post_tool_feedback]\n");
            output.output.push_str(&feedback);
        }
    }

    /// Maximum fraction of context budget a single tool output may occupy.
    /// Even if we have room, a single output shouldn't dominate the context.
    const SINGLE_OUTPUT_MAX_FRACTION: f32 = 0.30;

    /// Hard ceiling on a single tool output, independent of context budget.
    ///
    /// A fraction alone is not enough. On a model reporting a 1M-token window,
    /// 30% permits a 300k-token single result, so a repo-wide grep sailed
    /// through the guard and cost 233k tokens in one call. No individual tool
    /// result is worth that much of any window: past roughly 50k tokens the
    /// caller is reading a haystack, not an answer, and should narrow the query.
    /// The effective ceiling is the smaller of this and the budget fraction, so
    /// small windows still get proportional protection.
    const SINGLE_OUTPUT_MAX_TOKENS: usize = 50_000;

    /// Message returned instead of an oversized tool result.
    ///
    /// It has one job: make the price legible and the retry obvious. The caller
    /// gets the exact cost, what would survive truncation, the cheap fixes, and
    /// the exact flag to pass if they really want the whole thing.
    ///
    /// The round-owned budget prints this same sentence when the single-output
    /// ceiling is what bound an output, so a caller cannot tell which layer
    /// refused it.
    pub(crate) fn oversized_output_refusal(
        output_tokens: usize,
        affordable_tokens: usize,
        current_tokens: usize,
        budget: usize,
    ) -> String {
        let percent_of_budget = if budget > 0 {
            (output_tokens as f32 / budget as f32) * 100.0
        } else {
            0.0
        };
        format!(
            "{OUTPUT_WITHHELD_PREFIX} this result is ~{output}k tokens ({percent:.0}% of the \
             {budget}k context budget, of which {used}k is already used), so it was not \
             returned. Nothing was added to the context except this message.\n\n\
             Narrow the request first: add or tighten `path`, `glob`, or `type`, set \
             `max_files`/`max_regions`, use `paths_only` when you only need locations, or \
             read a specific line range. A targeted query is almost always the better \
             answer than a truncated dump.\n\n\
             If you genuinely need this output and accept the token cost, repeat the same \
             call with `\"accept_large_output\": true`. That returns the first ~{affordable}k \
             tokens and permanently spends them from this session's context.",
            output = output_tokens as f32 / 1000.0,
            percent = percent_of_budget,
            budget = budget / 1000,
            used = current_tokens / 1000,
            affordable = affordable_tokens as f32 / 1000.0,
        )
    }

    /// Resolve a model-supplied tool name for a session: strips a
    /// `functions.` namespace and maps aliases, but leaves SDK custom tool
    /// names untouched so they are never rewritten to a built-in.
    pub(crate) fn resolve_tool_name_for_session<'a>(session_id: &str, name: &'a str) -> &'a str {
        let unqualified_name = name.strip_prefix("functions.").unwrap_or(name);
        if sdk::custom(session_id, unqualified_name) {
            unqualified_name
        } else {
            Self::resolve_tool_name(unqualified_name)
        }
    }

    /// Execute a tool by name
    pub async fn execute(&self, name: &str, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        // Mark this call in-flight for the whole execution so the missing
        // tool-output repair paths do not mistake a slow tool for an
        // interrupted one and inject a duplicate synthetic result. See
        // `tool::inflight`.
        let _in_flight = inflight::mark_tool_in_flight(&ctx.tool_call_id);
        let resolved_name = Self::resolve_tool_name_for_session(&ctx.session_id, name);
        crate::crash::record_tool_operation(resolved_name, &input);
        let is_custom = sdk::custom(&ctx.session_id, resolved_name);
        // `toolbox` is policy-transparent: it runs a tool on this session's
        // behalf, and the inner `Registry::execute` inside it applies plan mode,
        // approval, and the tool hooks to that tool with that tool's own name
        // and input. Running those stages here as well would ask twice, gate the
        // wrapper instead of the target, and hand a hook a name no script
        // expects. The allow-list and disabled checks below stay: they decide
        // whether the dispatcher itself is reachable. An SDK callback that merely
        // carries the name is not the dispatcher, so it keeps every stage.
        let policy_on_self = is_custom || resolved_name != crate::config::TOOLBOX_TOOL;
        if is_custom && let Some(config) = sdk::config(&ctx.session_id) {
            let disabled = config.disabled.into_iter().collect();
            anyhow::ensure!(
                !self.tool_is_disabled(&disabled, resolved_name),
                "Tool '{}' is disabled",
                resolved_name
            );
        }
        // Enforce product separation here too: batch/subcalls dispatch through
        // the registry without going through Agent::validate_tool_allowed.
        if !is_custom
            && matches!(
                resolved_name,
                "selfdev" | "debug_socket" | "desktop_selfdev" | "horon_agent_docs"
            )
        {
            let desktop = ctx
                .working_dir
                .as_deref()
                .and_then(horon_agent_selfdev_types::desktop_repo_root)
                .is_some();
            if desktop && resolved_name == "horon_agent_docs" {
                anyhow::bail!(
                    "Tool 'horon_agent_docs' is disabled in Desktop self-development mode. Read the working tree documentation instead."
                );
            }
            if desktop && matches!(resolved_name, "selfdev" | "debug_socket") {
                anyhow::bail!(
                    "Tool '{}' targets HoronAgent CLI, not Desktop. Use 'desktop_selfdev'.",
                    resolved_name
                );
            }
            if !desktop && resolved_name == "desktop_selfdev" {
                anyhow::bail!("Tool 'desktop_selfdev' requires a HoronAgent Desktop source checkout.");
            }
        }
        if !is_custom && let Some(policy) = session_tool_policy(&ctx.session_id) {
            if let Some(allowed) = policy.allowed_tools.as_ref()
                && !self.tool_is_allowed(allowed, resolved_name)
            {
                return Err(anyhow::anyhow!("Tool '{}' is not allowed", resolved_name));
            }
            if self.tool_is_disabled(&policy.disabled_tools, resolved_name) {
                return Err(anyhow::anyhow!("Tool '{}' is disabled", resolved_name));
            }
        }
        // Plan mode is read-only: refuse a call that declares a barrier effect
        // before the tool is resolved, so a planning session cannot write, spawn
        // a process, or reach a tool this registry cannot classify. The
        // dispatcher's reach is its target's reach, so it is not refused here;
        // the inner call refuses the target by the target's own effects.
        let plan_effects = if is_custom || !policy_on_self {
            None
        } else {
            self.declared_effects(resolved_name, &input).await
        };
        if policy_on_self
            && !crate::plan_mode::allows_effects(&ctx.session_id, plan_effects.unwrap_or(ToolEffects::write()))
        {
            let mut fields = Self::tool_lifecycle_fields("blocked", name, resolved_name, &input, &ctx);
            fields.push(("block_reason".to_string(), "plan_mode".to_string()));
            crate::logging::event_warn("TOOL_LIFECYCLE", fields);
            return Err(anyhow::anyhow!(crate::plan_mode::block_message(resolved_name)));
        }
        let tool: Arc<dyn Tool> = if is_custom {
            Arc::new(sdk::CallbackTool(resolved_name.into()))
        } else {
            // An inactive tool is registered but hidden. `resolve_registered_tool`
            // drops every guard before it calls the activator and re-reads the
            // map afterwards, because an activator that registers tools needs
            // the write lock this call would otherwise be holding.
            match self.resolve_registered_tool(resolved_name).await {
                (Some(tool), _) => tool,
                (None, _) => {
                    // List available tools so the model can recover instead of
                    // spiraling through hallucinated names like "ToolSearch" (#104).
                    let available = self.advertised_tool_names().await;
                    return Err(Self::unknown_tool_error(name, &available));
                }
            }
        };

        let working_dir = ctx.working_dir.as_ref().map(|dir| dir.display().to_string());
        // The dispatch clock starts before the hooks. A transformer or gate that
        // queues on `HOOK_SERIAL` or runs long is time the caller waited, and
        // starting at `Tool::execute` made that wait invisible.
        let dispatch_started_at = std::time::Instant::now();
        // User-configured input transformer: may replace the tool input before
        // anything reads it, so it is serialized with the other awaited hooks;
        // see `HOOK_SERIAL`. It is skipped entirely when not configured, and for
        // the dispatcher, whose payload the target's own transformer rewrites.
        let input = if policy_on_self {
            let _serial = HOOK_SERIAL.lock().await;
            crate::hooks::transform_tool_input(&ctx.session_id, working_dir.as_deref(), resolved_name, input).await
        } else {
            input
        };
        let hook_transform_ms = elapsed_ms(dispatch_started_at);

        // Schema-directed repair: a provider's JSON writer can quote a number,
        // flatten `a[0].b` into a literal key, or send `"true"` for a boolean.
        // Repair against the tool's own schema before the gates read the input,
        // so an approval decision is made on what will actually run. Anything
        // the schema cannot prove is left alone and still fails in the tool.
        let input = {
            let (repaired, changes) = arg_repair::repair(&tool.parameters_schema(), input);
            if !changes.is_empty() {
                crate::logging::debug(&format!("[tool:{resolved_name}] repaired arguments: {changes:?}"));
            }
            repaired
        };

        // Approval mode: the user's configured floor on friction, checked before
        // the `pre_tool` hook so a call that must ask is never observed by an
        // external gate as if it had been dispatched. The command-risk hard
        // refusal inside `bash` is untouched; this only decides whether the user
        // must answer first. The dispatcher asks nothing on its own behalf: the
        // inner call applies the same mode to the tool it runs, so one prompt
        // names that tool and its arguments.
        let refusal = if policy_on_self {
            // An ACP connection may set the mode for the turns it starts; every other
            // turn, including a desktop or daemon turn on the same session, keeps the
            // configured mode.
            let approval_effects = plan_effects.unwrap_or(ToolEffects::write());
            let approval_mode =
                approval_override::get(&ctx.session_id).unwrap_or(crate::config::config().tools.approval_mode);
            let dual_confirm = crate::config::config().tools.dual_confirm_classes.clone();
            let danger = crate::approval::danger_class(resolved_name, &input, ctx.working_dir.as_deref());
            // Explicit `[[tools.bash_patterns]]` rules are the user's own policy,
            // so they are consulted before the mode. A deny refuses the call here;
            // a prompt or allow travels into the evaluation, where a dual-confirm
            // class still outranks it.
            let bash_pattern = if resolved_name == "bash" {
                input
                    .get("command")
                    .and_then(Value::as_str)
                    .and_then(crate::approval::bash_pattern_verdict)
            } else {
                None
            };
            if let Some(verdict) = bash_pattern.as_ref()
                && verdict.approval == crate::config::BashPatternApproval::Deny
            {
                let mut fields = Self::tool_lifecycle_fields("blocked", name, resolved_name, &input, &ctx);
                fields.push(("block_reason".to_string(), "bash_pattern".to_string()));
                crate::logging::event_warn("TOOL_LIFECYCLE", fields);
                return Err(anyhow::anyhow!("Blocked by bash pattern '{}' (deny).", verdict.pattern));
            }
            let evaluation = crate::approval::evaluate_and_audit(
                resolved_name,
                approval_effects,
                approval_mode,
                &dual_confirm,
                danger,
                bash_pattern.as_ref(),
                &ctx.tool_call_id,
            );
            match evaluation {
                crate::approval::ApprovalEvaluation::RequiresApproval {
                    reason,
                    is_dual_confirm,
                } => match permission_bridge::for_session(&ctx.session_id) {
                    // The editor that started this turn can answer now, so the call
                    // waits for it instead of refusing and asking for a retry.
                    Some(bridge) => {
                        permission_bridge::await_permission(
                            &bridge,
                            resolved_name,
                            &reason,
                            is_dual_confirm,
                            &input,
                            &ctx,
                        )
                        .await
                    }
                    None => Self::approval_refusal(resolved_name, &reason, is_dual_confirm, &input, &ctx),
                },
                crate::approval::ApprovalEvaluation::AutoApproved { .. } => None,
            }
        } else {
            None
        };
        if let Some(refusal) = refusal {
            let mut fields = Self::tool_lifecycle_fields("blocked", name, resolved_name, &input, &ctx);
            fields.push(("block_reason".to_string(), "approval".to_string()));
            crate::logging::event_warn("TOOL_LIFECYCLE", fields);
            return Err(anyhow::anyhow!(refusal));
        }

        // User-configured pre_tool gate: external policy hook that can block
        // this call (exit 2). Skipped entirely when not configured.
        //
        // `restore_then_gate` is what makes the gate review the command that will
        // actually run: a gate reading `«SECRET:...»` approves a command nobody
        // executes, because the tool itself always gets the restored input.
        let gate_started_at = std::time::Instant::now();
        let input = if policy_on_self && crate::hooks::hook_configured("pre_tool") {
            let gate_session = ctx.session_id.clone();
            let gate_cwd = working_dir.clone();
            let gate_tool = resolved_name.to_string();
            let (restored, decision) =
                crate::secrets::restore_then_gate(input, &ctx.session_id, move |payload| async move {
                    // Serialized with `pre_tool_transform` and `post_tool_feedback`;
                    // see `HOOK_SERIAL`. The wait for the gate is bounded, so a
                    // session queued behind a wedged hook is released instead of
                    // blocked forever; the gate is fail-open on timeout, so an
                    // expired queue wait allows the call with a warning.
                    run_gated_pre_tool_hook(&gate_session, gate_cwd.as_deref(), &gate_tool, &payload).await
                })
                .await;
            if let crate::hooks::GateDecision::Block { reason } = decision {
                let mut fields = Self::tool_lifecycle_fields("blocked", name, resolved_name, &restored, &ctx);
                fields.push(("block_reason".to_string(), reason.clone()));
                crate::logging::event_warn("TOOL_LIFECYCLE", fields);
                return Err(anyhow::anyhow!("Tool call blocked by pre_tool hook: {reason}"));
            }
            restored
        } else {
            crate::secrets::restore_tool_input(input, &ctx.session_id)
        };
        let hook_gate_ms = elapsed_ms(gate_started_at);

        crate::logging::event_info(
            "TOOL_LIFECYCLE",
            Self::tool_lifecycle_fields("start", name, resolved_name, &input, &ctx),
        );

        let execute_started_at = std::time::Instant::now();
        let result = tool.execute(input.clone(), ctx.clone()).await;
        let execute_ms = elapsed_ms(execute_started_at);

        // The `post_tool` observer fires as soon as the tool returns, before the
        // feedback and guard stages exist, so its DURATION_MS stays the execute
        // span. Dispatch latency is reported by the lifecycle line below.
        if policy_on_self {
            Self::fire_post_tool_hook(resolved_name, &input, &ctx, &result, execute_ms);
        }

        let mut output = match result {
            Ok(output) => output,
            Err(error) => {
                // A failed dispatch runs no feedback or guard stage, so its
                // total ends here.
                let timings = DispatchTimings {
                    hook_transform_ms,
                    hook_gate_ms,
                    execute_ms,
                    total_ms: elapsed_ms(dispatch_started_at),
                    ..DispatchTimings::default()
                };
                observe_dispatch_timings(&ctx.tool_call_id, timings);
                let mut fields = Self::tool_lifecycle_fields("error", name, resolved_name, &input, &ctx);
                fields.extend(timings.fields());
                fields.push(("error".to_string(), crate::util::format_error_chain(&error)));
                crate::logging::event_warn("TOOL_LIFECYCLE", fields);
                return Err(error);
            }
        };

        let feedback_started_at = std::time::Instant::now();
        if policy_on_self {
            Self::apply_post_tool_feedback(resolved_name, &input, &ctx, execute_ms, &mut output).await;
        }
        let post_feedback_ms = elapsed_ms(feedback_started_at);

        // Context overflow guard: check if this output would push us over the limit.
        // The flag is recorded on the output too, so the round-owned budget can
        // repeat this decision at the round's running total instead of guessing.
        let guard_started_at = std::time::Instant::now();
        let accept_large_output = accepts_large_output(&input);
        // Any tool without its own budget (MCP, agentgrep, ...) is condensed
        // here, with the full text kept as an `artifact://`. `read` pages on disk
        // and enforces its own cap; an explicit `accept_large_output` keeps the
        // overflow guard's existing path.
        if resolved_name != "read"
            && !accept_large_output
            && output.output.len() > artifacts::GENERIC_CONDENSE_BUDGET_BYTES
        {
            let text = std::mem::take(&mut output.output);
            output.output = artifacts::condense_for_context(
                &ctx.session_id,
                resolved_name,
                text,
                artifacts::GENERIC_CONDENSE_BUDGET_BYTES,
            );
        }
        output = self.guard_context_overflow(name, output, accept_large_output).await;
        if accept_large_output {
            mark_accept_large_output(&mut output);
        }
        let overflow_guard_ms = elapsed_ms(guard_started_at);

        let timings = DispatchTimings {
            hook_transform_ms,
            hook_gate_ms,
            execute_ms,
            post_feedback_ms,
            overflow_guard_ms,
            total_ms: elapsed_ms(dispatch_started_at),
        };
        observe_dispatch_timings(&ctx.tool_call_id, timings);

        let mut fields = Self::tool_lifecycle_fields("done", name, resolved_name, &input, &ctx);
        fields.extend(timings.fields());
        fields.push(("output_bytes".to_string(), output.output.len().to_string()));
        fields.push(("output_chars".to_string(), output.output.chars().count().to_string()));
        fields.push(("image_count".to_string(), output.images.len().to_string()));
        crate::logging::event_info("TOOL_LIFECYCLE", fields);

        Ok(output)
    }

    /// Check if a tool output would overflow the context window and truncate if needed.
    /// Returns the (possibly truncated) output.
    ///
    /// An oversized result is **refused** rather than truncated. Truncating by
    /// default was the worse failure: the caller still paid the full remaining
    /// context for a prefix that usually did not contain the answer, and the
    /// damage was already done by the time they read the warning. Refusing costs
    /// a few dozen tokens, states the price, and lets the caller either narrow
    /// the query or knowingly pay by passing `accept_large_output`.
    async fn guard_context_overflow(
        &self,
        tool_name: &str,
        output: ToolOutput,
        accept_large_output: bool,
    ) -> ToolOutput {
        let compaction = self.compaction.read().await;
        let budget = compaction.token_budget();
        if budget == 0 {
            return output;
        }

        let current_tokens = compaction.effective_token_count();
        let output_tokens = Self::estimate_tokens(&output.output);

        // Check 1: Would adding this output push us over the safety threshold?
        let projected = current_tokens + output_tokens;
        let threshold_tokens = Self::context_guard_threshold_tokens(budget);

        // Check 2: Is this single output unreasonably large?
        let single_max_tokens = Self::single_output_max_tokens(budget);

        let needs_truncation = projected > threshold_tokens || output_tokens > single_max_tokens;

        if !needs_truncation {
            return output;
        }

        // Past the safety threshold there is no room left to spend, so opting in
        // cannot buy anything: returning a slice would push the conversation over
        // the window instead of merely being expensive. Refuse outright, and say
        // how to make room. This is checked before computing an affordable size
        // so the outcome does not depend on budget arithmetic happening to land
        // under a character floor.
        if current_tokens >= threshold_tokens {
            crate::logging::info(&format!(
                "Context guard: refused {} output of ~{}k tokens, context exhausted \
                 ({}k/{}k)",
                tool_name,
                output_tokens / 1000,
                current_tokens / 1000,
                budget / 1000,
            ));
            return ToolOutput {
                output: Self::exhausted_context_refusal(output_tokens, current_tokens, budget),
                title: output.title,
                metadata: output.metadata,
                images: output.images,
            };
        }

        // How much of this output the remaining context could absorb.
        let max_tokens = (threshold_tokens - current_tokens).min(single_max_tokens);

        // Convert token limit back to approximate character limit
        let max_chars = max_tokens * 4;

        if output.output.len() <= max_chars {
            return output;
        }

        if !accept_large_output {
            crate::logging::info(&format!(
                "Context guard: refused {} output of ~{}k tokens \
                 (context: {}k/{}k, {:.0}% used); caller may retry with accept_large_output",
                tool_name,
                output_tokens / 1000,
                current_tokens / 1000,
                budget / 1000,
                (current_tokens as f32 / budget as f32) * 100.0,
            ));
            return ToolOutput {
                output: Self::oversized_output_refusal(output_tokens, max_tokens, current_tokens, budget),
                title: output.title,
                metadata: output.metadata,
                images: output.images,
            };
        }

        crate::logging::info(&format!(
            "Context guard: truncating {} output from ~{}k to ~{}k tokens \
             (context: {}k/{}k, {:.0}% used)",
            tool_name,
            output_tokens / 1000,
            max_tokens / 1000,
            current_tokens / 1000,
            budget / 1000,
            (current_tokens as f32 / budget as f32) * 100.0,
        ));

        // Truncate the output, keeping the beginning (usually most relevant)
        // Keep the beginning, which is usually the most relevant part, and leave
        // headroom for the notice itself. `max_chars` is at least 800 here: the
        // exhaustion check above already returned for anything tighter.
        let kept = &output.output[..output.output.floor_char_boundary(max_chars.saturating_sub(150))];
        let truncated = format!(
            "{}\n\n{}",
            kept,
            Self::truncated_output_notice(output_tokens, max_tokens, current_tokens, budget),
        );

        ToolOutput {
            output: truncated,
            title: output.title,
            metadata: output.metadata,
            images: output.images,
        }
    }

    /// Install the callback that turns inactive tools on.
    ///
    /// One activator serves the whole registry and every clone of it, so a tool
    /// activated through any handle is callable through all of them.
    pub fn register_lazy_tool_activator(&self, activator: LazyToolActivator) {
        self.lazy_activation
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .activator = Some(activator);
    }

    /// Register a tool that must not be advertised until it is activated.
    ///
    /// The tool is present so its activator can hand it out, but every surface
    /// that reaches the model - `definitions*`, `tool_names`, and the
    /// unknown-name suggestions - omits it until `execute` activates it.
    pub async fn register_inactive(&self, name: String, tool: Arc<dyn Tool>) {
        self.register(name.clone(), tool).await;
        self.lazy_activation
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inactive
            .insert(name);
    }

    fn inactive_lazy_tools(&self) -> HashSet<String> {
        self.lazy_activation
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inactive
            .clone()
    }

    fn is_lazy_inactive(&self, name: &str) -> bool {
        self.lazy_activation
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inactive
            .contains(name)
    }

    /// Ask the activator to turn `name` on. Answers whether it is now active.
    ///
    /// No registry lock is held across the callback: an activator normally
    /// registers the tools it just enabled, and that needs the tools write
    /// lock, so holding any guard here would deadlock.
    async fn activate_if_inactive(&self, name: &str) -> bool {
        let activator = {
            let state = self
                .lazy_activation
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state.inactive.contains(name) {
                return false;
            }
            match state.activator.clone() {
                Some(activator) => activator,
                None => return false,
            }
        };
        if !(activator)(name.to_string()).await {
            return false;
        }
        self.lazy_activation
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inactive
            .remove(name);
        crate::logging::info(&format!("[TOOL] lazily activated {name}"));
        true
    }

    /// The registered tool `name` resolves to, plus whether this call had to
    /// activate it.
    async fn resolve_registered_tool(&self, name: &str) -> (Option<Arc<dyn Tool>>, bool) {
        if !self.is_lazy_inactive(name) {
            let tools = self.tools.read().await;
            return (Self::visible_tool(&tools, self.session_mcp().as_ref(), name), false);
        }
        if !self.activate_if_inactive(name).await {
            return (None, false);
        }
        let tools = self.tools.read().await;
        (Self::visible_tool(&tools, self.session_mcp().as_ref(), name), true)
    }

    /// Registered names the model may call: every inactive tool is omitted, so
    /// an error message can never leak a hidden name.
    async fn advertised_tool_names(&self) -> Vec<String> {
        let tools = self.tools.read().await;
        let inactive = self.inactive_lazy_tools();
        let overlay = self.session_mcp();
        let mut names: Vec<String> = Self::visible_tools(&tools, overlay.as_ref())
            .map(|(name, _)| name)
            .filter(|name| !inactive.contains(*name))
            .cloned()
            .collect();
        names.sort_unstable();
        names
    }

    /// Error for a name that resolves to no callable tool.
    fn unknown_tool_error(requested: &str, available: &[String]) -> anyhow::Error {
        let names: Vec<&str> = available.iter().map(String::as_str).collect();
        let suggestions = Self::closest_tool_names(requested, &names);
        let mut msg = format!("Unknown tool: {requested}.");
        if !suggestions.is_empty() {
            msg.push_str(&format!(" Did you mean: {}?", suggestions.join(", ")));
        }
        msg.push_str(&format!(" Available tools: {}.", names.join(", ")));
        anyhow::anyhow!(msg)
    }

    /// Register a tool dynamically (for MCP tools, etc.)
    pub async fn register(&self, name: String, tool: Arc<dyn Tool>) {
        self.lazy_activation
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inactive
            .remove(&name);
        let mut tools = self.tools.write().await;
        let is_mcp = tool.mcp_identity().is_some();
        tools.insert(name, tool);
        if is_mcp {
            self.record_mcp_surface(&tools);
        }
    }

    fn record_mcp_surface(&self, tools: &HashMap<String, Arc<dyn Tool>>) {
        let mut index = self.mcp_policy.write().unwrap_or_else(|e| e.into_inner());
        index.current.clear();
        for (alias, tool) in tools {
            if let Some((server, raw)) = tool.mcp_identity() {
                let identity = (server.to_string(), raw.to_string());
                index.current.insert(alias.clone(), identity.clone());
                let names = index.history.entry(identity).or_default();
                names.insert(alias.clone());
                names.insert(crate::mcp::dispatch_name(server, raw));
            }
        }
    }

    fn mcp_alias(&self, server: &str, raw: &str) -> Option<String> {
        self.mcp_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .iter()
            .find(|(_, identity)| identity.0 == server && identity.1 == raw)
            .map(|(alias, _)| alias.clone())
    }

    fn mcp_policy_names(&self, server: &str, raw: &str) -> HashSet<String> {
        let index = self.mcp_policy.read().unwrap_or_else(|e| e.into_inner());
        let mut names = index
            .history
            .get(&(server.into(), raw.into()))
            .cloned()
            .unwrap_or_default();
        names.insert(crate::mcp::dispatch_name(server, raw));
        names
    }

    fn policy_names(&self, name: &str) -> HashSet<String> {
        let index = self.mcp_policy.read().unwrap_or_else(|e| e.into_inner());
        let mut names = index
            .current
            .get(name)
            .and_then(|identity| index.history.get(identity))
            .cloned()
            .unwrap_or_default();
        names.insert(name.to_string());
        names
    }

    pub(crate) fn tool_is_allowed(&self, allowed: &HashSet<String>, name: &str) -> bool {
        self.policy_names(name)
            .iter()
            .any(|name| tool_name_is_allowed(allowed, name))
    }

    pub(crate) fn tool_is_disabled(&self, disabled: &HashSet<String>, name: &str) -> bool {
        self.policy_names(name)
            .iter()
            .any(|name| tool_name_is_disabled(disabled, name))
    }

    /// Original `(server, tool)` for a registered MCP alias. Aliases are
    /// sanitized for providers, so they cannot be split back reliably.
    pub(crate) fn mcp_identity_for_alias(&self, alias: &str) -> Option<(String, String)> {
        if let Some(identity) = self.session_mcp().and_then(|overlay| overlay.identity(alias)) {
            return Some(identity);
        }
        self.mcp_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .get(alias)
            .cloned()
    }

    /// Whether this exact configured server has registered tools. Dispatch
    /// aliases are normalized and may be collision-suffixed, so callers must
    /// not infer server membership by matching a name prefix.
    pub fn has_mcp_tools_for_server(&self, server: &str) -> bool {
        self.mcp_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .values()
            .any(|(owner, _)| owner == server)
    }

    fn mcp_dispatch_is_allowed(&self, session: &str, server: &str, raw: &str, alias: &str, surface: &str) -> bool {
        let Some(policy) = session_tool_policy(session) else {
            return true;
        };
        let mut names = self.mcp_policy_names(server, raw);
        names.insert(alias.to_string());
        policy.allowed_tools.as_ref().is_none_or(|allowed| {
            allowed.contains(surface) || names.iter().any(|name| tool_name_is_allowed(allowed, name))
        }) && !names
            .iter()
            .any(|name| tool_name_is_disabled(&policy.disabled_tools, name))
    }

    /// Refresh connected servers, retaining cached proxies for offline servers.
    /// Recompute aliases over the combined surface, not only the live subset.
    pub async fn refresh_mcp_tools(&self, fresh: Vec<(String, Arc<dyn Tool>)>, refreshed_servers: &[String]) {
        let mut tools = self.tools.write().await;
        let mut proxies: Vec<Arc<dyn Tool>> = tools
            .values()
            .filter(|tool| {
                tool.mcp_identity()
                    .is_some_and(|(server, _)| !refreshed_servers.iter().any(|s| s == server))
            })
            .cloned()
            .collect();
        proxies.extend(fresh.into_iter().map(|(_, tool)| tool));
        let defs: Vec<_> = proxies
            .iter()
            .map(|tool| {
                let (server, raw) = tool.mcp_identity().expect("MCP proxy");
                (
                    server.to_string(),
                    crate::mcp::McpToolDef {
                        name: raw.to_string(),
                        description: None,
                        input_schema: Value::Null,
                    },
                )
            })
            .collect();
        let names = crate::mcp::dispatch_names(&defs);
        tools.retain(|_, tool| tool.mcp_identity().is_none());
        tools.extend(names.into_iter().zip(proxies));
        self.record_mcp_surface(&tools);
    }

    /// Atomically replace the MCP surface. Aliases depend on the complete set,
    /// so inserting only new keys leaves obsolete cached aliases callable.
    pub async fn reconcile_mcp_tools(&self, fresh: Vec<(String, Arc<dyn Tool>)>) {
        let mut tools = self.tools.write().await;
        tools.retain(|_, tool| tool.mcp_identity().is_none());
        tools.extend(fresh);
        self.record_mcp_surface(&tools);
    }

    /// Remove exact server membership, never a lossy normalized prefix.
    pub async fn unregister_mcp_server(&self, server: &str) -> Vec<String> {
        let mut tools = self.tools.write().await;
        let mut removed = Vec::new();
        tools.retain(|name, tool| {
            let keep = !tool.mcp_identity().is_some_and(|(owner, _)| owner == server);
            if !keep {
                removed.push(name.clone());
            }
            keep
        });
        self.record_mcp_surface(&tools);
        removed
    }

    /// How many MCP servers the most recent registration call found enabled in
    /// this registry's configuration.
    ///
    /// Zero also covers a registry where MCP was never registered at all, which
    /// is the same thing for a caller deciding whether the fixed discovery
    /// surface has anything to discover.
    pub fn configured_mcp_servers(&self) -> usize {
        self.configured_mcp_servers.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn set_configured_mcp_servers_for_test(&self, count: usize) {
        self.configured_mcp_servers.store(count, Ordering::Relaxed);
    }

    /// Register MCP tools (MCP management and server tools)
    /// Connections happen in background to avoid blocking startup.
    /// If `event_tx` is provided, sends an McpStatus event when connections complete.
    /// If `shared_pool` is provided, shared servers reuse processes from the pool.
    pub async fn register_mcp_tools(
        &self,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::protocol::ServerEvent>>,
        shared_pool: Option<std::sync::Arc<crate::mcp::SharedMcpPool>>,
        session_id: Option<String>,
    ) {
        self.register_mcp_tools_for_dir(event_tx, shared_pool, session_id, None)
            .await
    }

    /// Like [`Self::register_mcp_tools`], but resolves project-local MCP config
    /// (`.mcp.json`, `.horon-agent/mcp.json`, `.claude/mcp.json`) against
    /// `working_dir` instead of the server process cwd. Remote/client sessions
    /// must pass their session working directory here (issue #420).
    pub async fn register_mcp_tools_for_dir(
        &self,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::protocol::ServerEvent>>,
        shared_pool: Option<std::sync::Arc<crate::mcp::SharedMcpPool>>,
        session_id: Option<String>,
        working_dir: Option<std::path::PathBuf>,
    ) {
        use crate::mcp::McpManager;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let mcp_manager = if let Some(pool) = shared_pool {
            let sid = session_id.unwrap_or_else(|| "unknown".to_string());
            Arc::new(RwLock::new(McpManager::with_shared_pool_for_dir(
                pool,
                sid,
                working_dir,
            )))
        } else {
            Arc::new(RwLock::new(McpManager::new()))
        };

        // A server that announces `notifications/tools/list_changed` must
        // become visible without a manual reload. The manager refreshes its own
        // catalog, then calls back here to rebuild the registry surface:
        // `refresh_mcp_tools` drops the changed server's old proxies and
        // re-registers what the server now lists, so tools it removed
        // disappear too. Deferred mode keeps its cache-stable prefix because
        // the new tools are only reachable through `mcp_search` until the next
        // locked tool snapshot.
        {
            // Both handles are weak on purpose: the callback lives inside the
            // manager, and the manager is reachable from the tools the registry
            // owns. A strong registry or manager here would form a cycle with
            // the tool map, so neither the map nor the manager could ever be
            // freed (and an isolated registry would silently keep the parent's
            // map alive).
            let registry = self.downgrade();
            let manager = Arc::downgrade(&mcp_manager);
            mcp_manager
                .write()
                .await
                .set_on_tools_changed(Arc::new(move |server: String| {
                    let Some(registry) = registry.upgrade() else {
                        return;
                    };
                    let Some(manager) = manager.upgrade() else {
                        return;
                    };
                    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                        return;
                    };
                    runtime.spawn(async move {
                        let tools = crate::mcp::create_mcp_tools(manager).await;
                        registry.refresh_mcp_tools(tools, std::slice::from_ref(&server)).await;
                    });
                }));
        }

        // Register MCP management tool immediately (with registry for dynamic tool registration)
        let mcp_tool = mcp::McpManagementTool::new(Arc::clone(&mcp_manager)).with_registry(self.clone());
        self.register("mcp".to_string(), Arc::new(mcp_tool) as Arc<dyn Tool>)
            .await;
        self.register(
            "mcp_search".to_string(),
            Arc::new(mcp::McpSearchTool::new(Arc::clone(&mcp_manager)).with_registry(self.clone())) as Arc<dyn Tool>,
        )
        .await;
        self.register(
            "mcp_call".to_string(),
            Arc::new(mcp::McpCallTool::new(Arc::clone(&mcp_manager)).with_registry(self.clone())) as Arc<dyn Tool>,
        )
        .await;

        // Check if we have enabled servers to connect to. Disabled servers stay
        // configured (visible to the mcp management tool, connectable by name)
        // but are not spawned, advertised, or shown as connecting (issue #436).
        let (enabled_count, disabled_count) = {
            let manager = mcp_manager.read().await;
            let enabled = manager.config().servers.values().filter(|cfg| cfg.is_enabled()).count();
            (enabled, manager.config().servers.len() - enabled)
        };
        // The session decides whether the fixed `mcp_search`/`mcp_call` surface
        // stays eager from this count, so it is recorded here, while the
        // configuration is known, and not read back from disk later.
        self.configured_mcp_servers.store(enabled_count, Ordering::Relaxed);

        if disabled_count > 0 {
            crate::logging::info(&format!(
                "MCP: {} disabled server(s) in config (kept, not spawned)",
                disabled_count
            ));
        }

        if enabled_count > 0 {
            crate::logging::info(&format!("MCP: Found {} server(s) in config", enabled_count));

            // Send immediate "connecting" status so the TUI shows loading state
            // Server names with count 0 means "connecting..."
            if let Some(ref tx) = event_tx {
                let server_names: Vec<String> = {
                    let manager = mcp_manager.read().await;
                    manager
                        .config()
                        .servers
                        .iter()
                        .filter(|(_, cfg)| cfg.is_enabled())
                        .map(|(name, _)| format!("{}:0", name))
                        .collect()
                };
                let _ = tx.send(crate::protocol::ServerEvent::McpStatus { servers: server_names });
            }

            // Advertise-early: register proxy tools for each configured server
            // from the on-disk schema cache *before* connections settle, so the
            // first locked tool snapshot already contains MCP tools and we avoid
            // the intentional prompt-cache miss entirely (#206 Phase 2). The
            // proxies connect-on-first-call. Servers with no cached schemas yet
            // (cold start, or reconfigured) fall back to the post-connect
            // registration + one-shot late-register rebuild below.
            let schema_cache = crate::mcp::McpSchemaCache::load();
            let mut advertised_servers: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            {
                let config_servers: Vec<(String, crate::mcp::McpServerConfig)> = {
                    let manager = mcp_manager.read().await;
                    manager
                        .config()
                        .servers
                        .iter()
                        .filter(|(_, cfg)| cfg.is_enabled())
                        .map(|(name, cfg)| (name.clone(), cfg.clone()))
                        .collect()
                };
                let mut advertised_tool_count = 0usize;
                let mut cached_tools = Vec::new();
                for (server, cfg) in &config_servers {
                    if let Some(cached) = schema_cache.tools_for(server, cfg) {
                        advertised_tool_count += cached.len();
                        cached_tools.extend(cached.iter().cloned().map(|tool| (server.clone(), tool)));
                        advertised_servers.insert(server.clone());
                    }
                }
                self.reconcile_mcp_tools(crate::mcp::create_mcp_tools_from_cached_many(
                    &cached_tools,
                    Arc::clone(&mcp_manager),
                ))
                .await;
                if advertised_tool_count > 0 {
                    crate::logging::info(&format!(
                        "MCP: advertised {} cached tool(s) from {} server(s) at spawn \
                         (connect-on-first-call); zero prompt-cache miss expected (#206)",
                        advertised_tool_count,
                        advertised_servers.len()
                    ));
                    // Reflect the advertised tools in the status indicator
                    // immediately so the UI shows them before connections settle.
                    if let Some(ref tx) = event_tx {
                        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
                        for (server, cfg) in &config_servers {
                            if let Some(cached) = schema_cache.tools_for(server, cfg) {
                                counts.insert(server.clone(), cached.len());
                            }
                        }
                        let servers: Vec<String> = counts
                            .into_iter()
                            .map(|(name, count)| format!("{}:{}", name, count))
                            .collect();
                        let _ = tx.send(crate::protocol::ServerEvent::McpStatus { servers });
                    }
                }
            }

            // Publish each completed connection independently. A slow handshake
            // must not hide an already-connected peer until an entire batch ends.
            let registry = self.clone();
            tokio::spawn(async move {
                use futures::StreamExt;

                let configs: Vec<_> = mcp_manager
                    .read()
                    .await
                    .config()
                    .servers
                    .iter()
                    .filter(|(_, cfg)| cfg.is_enabled())
                    .map(|(name, cfg)| (name.clone(), cfg.clone()))
                    .collect();
                let mut connections = futures::stream::FuturesUnordered::new();
                for (name, config) in configs {
                    let manager = Arc::clone(&mcp_manager);
                    connections.push(async move {
                        let result = manager.read().await.connect(&name, &config).await;
                        (name, result)
                    });
                }

                while let Some((name, result)) = connections.next().await {
                    if let Err(error) = result {
                        crate::logging::event_rate_limited(
                            crate::logging::LogLevel::Error,
                            &format!("mcp_register_failed:{name}"),
                            std::time::Duration::from_secs(60),
                            "MCP_REGISTER_FAILED",
                            vec![("server", name), ("error", format!("{error:#}"))],
                        );
                        continue;
                    }
                    crate::logging::info(&format!("MCP: Connected to '{name}'"));

                    // Register MCP server tools and collect server info
                    let tools = crate::mcp::create_mcp_tools(Arc::clone(&mcp_manager)).await;
                    let mut server_counts: std::collections::BTreeMap<String, usize> =
                        std::collections::BTreeMap::new();
                    for (_, tool) in &tools {
                        if let Some((server, _)) = tool.mcp_identity() {
                            *server_counts.entry(server.to_string()).or_default() += 1;
                        }
                    }
                    let connected = mcp_manager.read().await.connected_servers().await;
                    registry.refresh_mcp_tools(tools, &connected).await;

                    // Reconcile the on-disk schema cache with the live schemas so the
                    // next spawn can advertise the up-to-date tools with zero cache
                    // miss. Group live tool defs by server and update each entry
                    // under the current config fingerprint; prune servers that are
                    // no longer configured. (#206 Phase 2)
                    {
                        // Live tool defs grouped by server, plus a snapshot of the
                        // configured servers, captured under one read lock.
                        type LiveToolsByServer = std::collections::BTreeMap<String, Vec<crate::mcp::McpToolDef>>;
                        type ConfigSnapshot = Vec<(String, crate::mcp::McpServerConfig)>;
                        let (live_by_server, config_snapshot): (LiveToolsByServer, ConfigSnapshot) = {
                            let manager = mcp_manager.read().await;
                            let mut grouped: std::collections::BTreeMap<String, Vec<crate::mcp::McpToolDef>> =
                                std::collections::BTreeMap::new();
                            for (server, def) in manager.all_tools().await {
                                grouped.entry(server).or_default().push(def);
                            }
                            // A connected server with zero tools is a valid live
                            // catalog, not an offline server. Cache its empty list
                            // so a later startup cannot advertise removed tools.
                            for server in manager.connected_servers().await {
                                grouped.entry(server).or_default();
                            }
                            let configs = manager
                                .config()
                                .servers
                                .iter()
                                .map(|(name, cfg)| (name.clone(), cfg.clone()))
                                .collect();
                            (grouped, configs)
                        };

                        let mut cache = crate::mcp::McpSchemaCache::load();
                        let mut dirty = false;
                        for (server, cfg) in &config_snapshot {
                            if let Some(defs) = live_by_server.get(server) {
                                // Only cache connected servers, including empty catalogs.
                                if cache.update(server, cfg, defs.clone()) {
                                    dirty = true;
                                }
                            }
                        }
                        let configured_names: Vec<String> = config_snapshot.iter().map(|(n, _)| n.clone()).collect();
                        if cache.retain_servers(&configured_names) {
                            dirty = true;
                        }
                        if dirty {
                            cache.save();
                            crate::logging::info("MCP: updated on-disk tool-schema cache from live connection (#206)");
                        }
                    }

                    // Notify client of MCP status
                    if let Some(ref tx) = event_tx {
                        let servers: Vec<String> = server_counts
                            .into_iter()
                            .map(|(name, count)| format!("{}:{}", name, count))
                            .collect();
                        let _ = tx.send(crate::protocol::ServerEvent::McpStatus { servers });
                    }
                }
            });
        }
    }

    /// Register self-dev tools (only for canary/self-dev sessions)
    pub async fn register_selfdev_tools(&self) {
        // Self-dev management tool
        let selfdev_tool = selfdev::SelfDevTool::new();
        self.register("selfdev".to_string(), Arc::new(selfdev_tool) as Arc<dyn Tool>)
            .await;

        // Debug socket tool for direct debug socket access
        let debug_socket_tool = debug_socket::DebugSocketTool::new();
        self.register("debug_socket".to_string(), Arc::new(debug_socket_tool) as Arc<dyn Tool>)
            .await;
    }

    /// Register `request_permission` for runs that are not ambient sessions.
    ///
    /// Gated on `[ambient] approval_for_clientless_runs`: with the setting off,
    /// a clientless run keeps the tool out of its surface entirely instead of
    /// advertising a tool whose gate rejects every call. Registration is
    /// idempotent (the registry is a map keyed by name), so an ambient session
    /// that also calls [`Self::register_ambient_tools`] keeps one entry.
    pub async fn register_approval_tool_if_enabled(&self) {
        if !crate::config::config().ambient.approval_for_clientless_runs {
            return;
        }
        self.register(
            crate::config::CLIENTLESS_APPROVAL_TOOL.to_string(),
            Arc::new(ambient::RequestPermissionTool::new()) as Arc<dyn Tool>,
        )
        .await;
    }

    /// Register `aux_complete` when `[tools] aux_completion` is on.
    ///
    /// The tool is opt-in because every call spends a model request on a side
    /// task. With the switch off it is never registered, so it neither appears
    /// in a definition list nor resolves as a callable name. Registration is
    /// idempotent (the registry is a map keyed by name).
    pub async fn register_aux_completion_tool_if_enabled(&self) {
        if !aux_complete::enabled() {
            return;
        }
        self.register(
            aux_complete::AUX_COMPLETE_TOOL.to_string(),
            Arc::new(aux_complete::AuxCompleteTool::new()) as Arc<dyn Tool>,
        )
        .await;
    }

    /// Register `submit_plan` when `[tools] plan_mode` is on.
    ///
    /// Plan mode is opt-in, so with the switch off the tool is never registered
    /// and cannot be resolved by name. Registration is idempotent (the registry
    /// is a map keyed by name).
    pub async fn register_plan_tool_if_enabled(&self) {
        if !submit_plan::enabled() {
            return;
        }
        self.register(
            submit_plan::SUBMIT_PLAN_TOOL.to_string(),
            Arc::new(submit_plan::SubmitPlanTool::new()) as Arc<dyn Tool>,
        )
        .await;
    }

    /// Register ambient-mode tools (only for ambient sessions)
    pub async fn register_ambient_tools(&self) {
        self.register(
            "end_ambient_cycle".to_string(),
            Arc::new(ambient::EndAmbientCycleTool::new()) as Arc<dyn Tool>,
        )
        .await;

        self.register(
            "schedule_ambient".to_string(),
            Arc::new(ambient::ScheduleAmbientTool::new()) as Arc<dyn Tool>,
        )
        .await;

        self.register(
            "request_permission".to_string(),
            Arc::new(ambient::RequestPermissionTool::new()) as Arc<dyn Tool>,
        )
        .await;

        self.register(
            "send_message".to_string(),
            Arc::new(ambient::SendChannelMessageTool::new()) as Arc<dyn Tool>,
        )
        .await;
    }

    /// Unregister a tool
    pub async fn unregister(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.lazy_activation
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inactive
            .remove(name);
        let mut tools = self.tools.write().await;
        let removed = tools.remove(name);
        self.record_mcp_surface(&tools);
        removed
    }

    /// Unregister all tools matching a prefix
    pub async fn unregister_prefix(&self, prefix: &str) -> Vec<String> {
        let mut tools = self.tools.write().await;
        let to_remove: Vec<String> = tools.keys().filter(|k| k.starts_with(prefix)).cloned().collect();
        for name in &to_remove {
            tools.remove(name);
        }
        self.record_mcp_surface(&tools);
        drop(tools);
        self.lazy_activation
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inactive
            .retain(|name| !name.starts_with(prefix));
        to_remove
    }

    /// Get shared access to the skill registry
    pub fn skills(&self) -> Arc<RwLock<SkillRegistry>> {
        self.skills.clone()
    }

    /// Get shared access to the compaction manager
    pub fn compaction(&self) -> Arc<RwLock<CompactionManager>> {
        self.compaction.clone()
    }
}

#[cfg(test)]
mod mcp_allow_list_tests {
    use super::{tool_name_is_allowed, tool_name_is_disabled};
    use std::collections::HashSet;

    #[test]
    fn allowing_mcp_also_allows_dynamic_server_tools() {
        let allowed = HashSet::from(["mcp".to_string()]);

        assert!(tool_name_is_allowed(&allowed, "mcp"));
        assert!(tool_name_is_allowed(&allowed, "mcp__filesystem__read_file"));
        assert!(!tool_name_is_allowed(&allowed, "mcpish"));
        assert!(!tool_name_is_allowed(&allowed, "bash"));
    }

    #[test]
    fn disabling_mcp_also_disables_dynamic_server_tools() {
        let disabled = HashSet::from(["mcp".to_string()]);

        assert!(tool_name_is_disabled(&disabled, "mcp"));
        assert!(tool_name_is_disabled(&disabled, "mcp__filesystem__read_file"));
        assert!(!tool_name_is_disabled(&disabled, "mcpish"));
        assert!(!tool_name_is_disabled(&disabled, "bash"));
    }
}

#[cfg(test)]
mod activator_clone;
#[cfg(test)]
mod ask_registry_tests;
#[cfg(test)]
mod aux_complete_tests;
#[cfg(test)]
mod compaction_sharing_tests;
#[cfg(test)]
mod concurrency_class_tests;
#[cfg(test)]
mod context_condense_perf_tests;
#[cfg(test)]
mod lazy_activation;
#[cfg(test)]
#[path = "monitor_tests.rs"]
mod monitor_tool;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod toolbox_tests;
#[cfg(test)]
mod undo_registry_tests;

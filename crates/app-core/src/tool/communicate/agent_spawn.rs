//! Spawn request construction for the `swarm` tool, including named agent
//! definitions and the `spawn_many` fan-out.
//!
//! The definition lookup and the batch fan-out live here rather than in
//! `communicate.rs` so the tool module itself stays under its size budget; the
//! resolution rules are in `horon_agent_base::agent_defs`.

use super::{CommunicateInput, REQUEST_ID};
use crate::agent_defs::{AgentDefinition, compose_initial_message, discover, resolve};
use crate::protocol::{Request, ServerEvent};
use crate::tool::{ToolContext, ToolOutput};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

/// Separator between the parts of a child's first message.
const SEGMENT_SEPARATOR: &str = "\n\n---\n\n";

/// Tools a consult child gets. It answers from the source transcript, so it may
/// read and search but never write, run a shell, or reach the network.
const CONSULT_TOOLS: [&str; 4] = crate::session_scope::CONSULT_TOOLS;

/// One child of a `spawn_many` call.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SpawnBatchTask {
    label: Option<String>,
    #[serde(default)]
    initial_message: Option<String>,
    /// Alias for `initial_message`, matching the single `spawn` action.
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    isolation: Option<String>,
    #[serde(default)]
    output_schema: Option<Value>,
}

impl SpawnBatchTask {
    fn normalized_label(&self) -> Option<&str> {
        self.label.as_deref().map(str::trim).filter(|label| !label.is_empty())
    }

    fn initial_message(&self) -> Option<String> {
        self.initial_message
            .as_ref()
            .filter(|message| !message.trim().is_empty())
            .cloned()
            .or_else(|| self.prompt.as_ref().filter(|prompt| !prompt.trim().is_empty()).cloned())
    }
}

/// The resolved spawn arguments of one child, before request construction.
struct SpawnSpec {
    label: String,
    initial_message: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    isolation: Option<String>,
    output_schema: Option<Value>,
    /// Shared batch background; prepended to the child's first message.
    context: Option<String>,
    /// Session to fork a read-only consult child from.
    source_session: Option<String>,
}

/// Build one `CommSpawn` request, resolving a named agent definition.
///
/// Explicit spawn parameters win over the definition, so a caller can reuse a
/// role and still change its model for one run. An unknown name fails here,
/// before any session exists, and reports the names that do exist.
pub(super) fn spawn_request(ctx: &ToolContext, params: &CommunicateInput, label: String) -> Result<Request> {
    build_request(
        ctx,
        params,
        SpawnSpec {
            label,
            initial_message: params.spawn_initial_message(),
            agent: params.agent.clone(),
            model: params.model.clone(),
            effort: params.effort.clone(),
            isolation: params.isolation.clone(),
            output_schema: params.output_schema.clone(),
            context: None,
            source_session: params.source_session.clone(),
        },
    )
}

fn build_request(ctx: &ToolContext, params: &CommunicateInput, spec: SpawnSpec) -> Result<Request> {
    let definition = match spec.agent.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => {
            Some(resolve(name, ctx.working_dir.as_deref()).map_err(|error| anyhow::anyhow!(error.to_string()))?)
        }
        None => None,
    };
    let mut initial_message = match definition.as_ref() {
        Some(definition) => compose_initial_message(&definition.prompt, spec.initial_message),
        None => spec.initial_message,
    };
    if let Some(context) = spec
        .context
        .as_deref()
        .map(str::trim)
        .filter(|context| !context.is_empty())
    {
        initial_message = Some(match initial_message {
            Some(message) => format!("{context}{SEGMENT_SEPARATOR}{message}"),
            None => context.to_string(),
        });
    }
    let output_schema = spec.output_schema.or_else(|| {
        definition
            .as_ref()
            .and_then(|definition| definition.output_schema.clone())
    });
    // A consult replaces the definition's tools with the read-only set: the
    // child's job is to answer from the source transcript, not to act.
    let allowed_tools = if spec.source_session.is_some() {
        Some(CONSULT_TOOLS.iter().map(|tool| tool.to_string()).collect())
    } else {
        definition.as_ref().and_then(|definition| definition.tools.clone())
    };
    Ok(Request::CommSpawn {
        id: REQUEST_ID,
        session_id: ctx.session_id.clone(),
        working_dir: params.working_dir.clone(),
        initial_message,
        request_nonce: None,
        spawn_mode: params.spawn_mode.clone(),
        model: spec
            .model
            .or_else(|| definition.as_ref().and_then(|definition| definition.model.clone())),
        effort: spec
            .effort
            .or_else(|| definition.as_ref().and_then(|definition| definition.effort.clone())),
        label: Some(spec.label),
        isolation: spec.isolation.or_else(|| params.isolation.clone()),
        iso_apply: params.iso_apply.clone(),
        output_schema,
        allowed_tools,
        source_session: spec.source_session,
    })
}

/// The `spawn` action body.
pub(super) async fn spawn_single(ctx: &ToolContext, params: &CommunicateInput) -> Result<ToolOutput> {
    let label = params.required_spawn_label()?;
    let request = spawn_request(ctx, params, label)?;
    match super::send_request(request).await {
        Ok(ServerEvent::CommSpawnResponse { new_session_id, .. }) if !new_session_id.is_empty() => {
            Ok(ToolOutput::new(format!("Spawned new agent: {}", new_session_id))
                .with_metadata(json!({"session_id": new_session_id})))
        }
        Ok(response) => {
            super::ensure_success(&response)?;
            Err(anyhow::anyhow!("Spawn succeeded but new session ID was not returned."))
        }
        Err(error) => Err(anyhow::anyhow!("Failed to spawn agent: {}", error)),
    }
}

/// One task, prepared into a request or the error that stopped it.
type PreparedTask = std::result::Result<(String, Request), (String, String)>;

/// What one child produced: its session id or the reason it did not start.
type SpawnOutcome = std::result::Result<String, String>;

/// The `spawn_many` action body. One entry per task, in input order.
pub(super) async fn spawn_many(ctx: &ToolContext, params: &CommunicateInput) -> Result<ToolOutput> {
    let tasks = match params.tasks.as_deref() {
        Some(tasks) => tasks,
        None => &[],
    };
    if tasks.is_empty() {
        anyhow::bail!("'tasks' is required for spawn_many and must not be empty");
    }
    // Reject duplicates before any child exists, matching omp's name uniqueness.
    let mut seen: HashSet<String> = HashSet::with_capacity(tasks.len());
    for task in tasks {
        if let Some(label) = task.normalized_label()
            && !seen.insert(label.to_lowercase())
        {
            anyhow::bail!("spawn_many tasks must have unique labels; '{label}' appears more than once");
        }
    }
    // Prepare every task before sending any request, so a bad task cannot start
    // a child that a later validation would have refused.
    let mut prepared: Vec<PreparedTask> = Vec::with_capacity(tasks.len());
    for task in tasks {
        prepared.push(prepare_task(ctx, params, task));
    }
    let outcomes = futures::future::join_all(prepared.into_iter().map(|prepared| async move {
        match prepared {
            Ok((label, request)) => (label, run_spawn(request).await),
            Err((label, error)) => (label, Err(error)),
        }
    }))
    .await;
    Ok(render_outcomes(outcomes))
}

/// Resolve one task into a label and a request, or the label and the reason it
/// could not be prepared.
pub(super) fn prepare_task(ctx: &ToolContext, params: &CommunicateInput, task: &SpawnBatchTask) -> PreparedTask {
    let label = task.normalized_label().unwrap_or("(unnamed task)").to_string();
    if task.normalized_label().is_none() {
        return Err((label, "every spawn_many task needs a non-blank label".to_string()));
    }
    let spec = SpawnSpec {
        label: label.clone(),
        initial_message: task.initial_message(),
        agent: task.agent.clone().or_else(|| params.agent.clone()),
        model: task.model.clone().or_else(|| params.model.clone()),
        effort: task.effort.clone().or_else(|| params.effort.clone()),
        isolation: task.isolation.clone().or_else(|| params.isolation.clone()),
        output_schema: task.output_schema.clone().or_else(|| params.output_schema.clone()),
        context: params.context.clone(),
        source_session: params.source_session.clone(),
    };
    match build_request(ctx, params, spec) {
        Ok(request) => Ok((label, request)),
        Err(error) => Err((label, error.to_string())),
    }
}

/// Send one prepared request and reduce the response to its child id.
async fn run_spawn(request: Request) -> SpawnOutcome {
    match super::send_request(request).await {
        Ok(ServerEvent::CommSpawnResponse { new_session_id, .. }) if !new_session_id.is_empty() => Ok(new_session_id),
        Ok(response) => Err(match super::ensure_success(&response) {
            Ok(()) => "spawn succeeded but no new session id was returned".to_string(),
            Err(error) => error.to_string(),
        }),
        Err(error) => Err(error.to_string()),
    }
}

fn render_outcomes(outcomes: Vec<(String, SpawnOutcome)>) -> ToolOutput {
    let spawned = outcomes.iter().filter(|(_, outcome)| outcome.is_ok()).count();
    let mut output = format!("Spawned {spawned} of {} agent(s):\n", outcomes.len());
    let mut sessions = Vec::new();
    for (label, outcome) in &outcomes {
        match outcome {
            Ok(session_id) => {
                output.push_str(&format!("- {label}: {session_id}\n"));
                sessions.push(json!({"label": label, "session_id": session_id}));
            }
            Err(error) => output.push_str(&format!("- {label}: failed: {error}\n")),
        }
    }
    let failed = outcomes.len() - spawned;
    if failed > 0 {
        output.push_str(&format!("{failed} task(s) failed; the others started.\n"));
    }
    ToolOutput::new(output).with_metadata(json!({
        "spawned": spawned,
        "failed": failed,
        "sessions": sessions,
    }))
}

/// Declare the `spawn`/`spawn_many` arguments that live beside the macro's
/// single-spawn fields.
pub(super) fn spawn_schema_properties(props: &mut serde_json::Map<String, Value>) {
    props.insert(
        "source_session".to_string(),
        json!({
            "type": "string",
            "description": "For spawn: session id whose transcript the child forks as a read-only consult. The child gets only read, agentgrep, ls and yield; the source session is never modified. An id outside this session's spawn tree is refused before any child starts. See SWARM_TASK_GRAPH 8d."
        }),
    );
    props.insert(
        "tasks".to_string(),
        json!({
            "type": "array",
            "description": "spawn_many children, in input order. Each: {label, initial_message? (alias prompt), agent?, model?, effort?, isolation?, output_schema?}. Labels must be unique, case-insensitively, and a task that fails to start reports its own error.",
            "items": {
                "type": "object",
                "required": ["label"],
                "additionalProperties": true,
                "properties": {
                    "label": { "type": "string", "minLength": 1 },
                    "initial_message": { "type": "string" },
                    "prompt": { "type": "string" },
                    "agent": { "type": "string" },
                    "model": { "type": "string" },
                    "effort": { "type": "string" },
                    "isolation": { "type": "string", "enum": ["none", "worktree", "clone"] },
                    "output_schema": { "type": "object" }
                }
            }
        }),
    );
    props.insert(
        "context".to_string(),
        json!({
            "type": "string",
            "description": "Shared background prepended to each spawn_many child's first message."
        }),
    );
}

/// Render the discovered agent definitions for the `list_agent_defs` action.
pub(super) fn format_definitions(definitions: &[AgentDefinition]) -> String {
    if definitions.is_empty() {
        return "No agent definitions found. Add Markdown files under .horon/agents/ or ~/.horon/agents/.".to_string();
    }
    let mut out = format!("{} agent definition(s):\n", definitions.len());
    for definition in definitions {
        let summary = definition.description.as_deref().unwrap_or("(no description)");
        out.push_str(&format!("- {}: {}", definition.name, summary));
        if let Some(tools) = definition.tools.as_ref() {
            out.push_str(&format!(" [tools: {}]", tools.join(", ")));
        }
        if let Some(model) = definition.model.as_deref() {
            out.push_str(&format!(" [model: {model}]"));
        }
        out.push('\n');
    }
    out
}

/// The `list_agent_defs` action body.
pub(super) fn list_definitions(ctx: &ToolContext) -> String {
    format_definitions(&discover(ctx.working_dir.as_deref()))
}

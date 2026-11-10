//! Spawn request construction for the `swarm` tool, including named agent
//! definitions.
//!
//! The definition lookup lives here rather than in `communicate.rs` so the
//! tool module itself stays under its size budget; the resolution rules are in
//! `horon_agent_base::agent_defs`.

use super::CommunicateInput;
use super::REQUEST_ID;
use crate::agent_defs::{AgentDefinition, compose_initial_message, discover, resolve};
use crate::protocol::Request;
use crate::tool::ToolContext;
use anyhow::Result;

use super::REQUEST_ID;

/// Build one `CommSpawn` request, resolving a named agent definition.
///
/// Explicit spawn parameters win over the definition, so a caller can reuse a
/// role and still change its model for one run. An unknown name fails here,
/// before any session exists, and reports the names that do exist.
pub(super) fn spawn_request(ctx: &ToolContext, params: &CommunicateInput, label: String) -> Result<Request> {
    let definition = match params.agent.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => {
            Some(resolve(name, ctx.working_dir.as_deref()).map_err(|error| anyhow::anyhow!(error.to_string()))?)
        }
        None => None,
    };
    let initial_message = match definition.as_ref() {
        Some(definition) => compose_initial_message(&definition.prompt, params.spawn_initial_message()),
        None => params.spawn_initial_message(),
    };
    let output_schema = params.output_schema.clone().or_else(|| {
        definition
            .as_ref()
            .and_then(|definition| definition.output_schema.clone())
    });
    Ok(Request::CommSpawn {
        id: REQUEST_ID,
        session_id: ctx.session_id.clone(),
        working_dir: params.working_dir.clone(),
        initial_message,
        request_nonce: None,
        spawn_mode: params.spawn_mode.clone(),
        model: params
            .model
            .clone()
            .or_else(|| definition.as_ref().and_then(|definition| definition.model.clone())),
        effort: params
            .effort
            .clone()
            .or_else(|| definition.as_ref().and_then(|definition| definition.effort.clone())),
        label: Some(label),
        isolation: params.isolation.clone(),
        iso_apply: params.iso_apply.clone(),
        output_schema,
        allowed_tools: definition.and_then(|definition| definition.tools),
    })
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

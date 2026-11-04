//! `toolbox`: the dispatcher that reaches a built-in tool the session did not
//! carry eagerly.
//!
//! Under the `full` profile every built-in outside [`crate::config::CORE_TOOLS`]
//! is sent as a deferred definition, so its schema never costs a place in the
//! cached prefix. Under the `core` profile only
//! [`crate::config::ESSENTIAL_TOOLS`] is sent eagerly and the remaining core
//! tools are listed by name in the system prompt. `toolbox` is the
//! always-visible handle to both surfaces: `list` names the tools whose schemas
//! are deferred, `describe` returns one input schema, and `run` calls a tool
//! through [`Registry::execute`], which is the same seam a direct call uses, so
//! session policy, hooks, client events, the context guard, and error shaping
//! are identical.
//!
//! The dispatcher is policy-transparent on its own behalf. Plan mode, approval,
//! and the tool hooks belong to the tool it reaches: the inner
//! [`Registry::execute`] applies them with that tool's name and input, exactly
//! once. `Registry::execute` and `Agent::validate_tool_allowed` therefore skip
//! those stages for the outer `toolbox` call while keeping the allow-list and
//! disabled checks, which are what make the dispatcher reachable at all.
//!
//! What this module guarantees: `describe` and `run` both put the target name in
//! `metadata.tool_references`, and the transcript conversion in
//! `agent/tools.rs` turns that into one `ContentBlock::ToolReference`. On a
//! provider with native deferred loading that reference is what makes the
//! deferred schema callable from the next turn; on any other provider the
//! deferred schema never reaches the wire, so the system prompt's
//! `# Additional tools` index is the way back to the tool and `describe` is how
//! its parameters are read.
//!
//! What it deliberately does NOT do: it never promotes a tool to the eager
//! list, never edits the session's definitions, never writes the transcript,
//! and never reaches a tool whose load mode is `Off`. There is no `promote`
//! action: the dispatcher loads a schema by reference and nothing else.

use super::{ConcurrencyClass, Registry, Tool, ToolContext, ToolEffects, ToolOutput, WeakRegistry};
use crate::config::{LoadMode, TOOLBOX_TOOL};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Metadata key the transcript conversion in `agent/tools.rs` reads to emit
/// `ContentBlock::ToolReference` entries. Kept in sync with that module's
/// `TOOL_REFERENCES_METADATA_KEY`, which is private to `agent`.
const TOOL_REFERENCES_METADATA_KEY: &str = "tool_references";

/// Whether the dispatcher belongs on this session's surface.
///
/// It is registered under `full` and `core`. `full` sends every built-in outside
/// [`crate::config::CORE_TOOLS`] deferred, and `core` sends only the essential
/// tools eagerly and lists the rest, so both need the dispatcher. `acp` and
/// every other profile keep their closed allow-lists, so the dispatcher stays
/// absent there rather than merely disabled.
pub(crate) fn enabled() -> bool {
    let profile = &crate::config::config().tools.profile;
    let profile = profile.trim();
    profile.eq_ignore_ascii_case("full") || profile.eq_ignore_ascii_case(crate::config::CORE_TOOL_PROFILE)
}

pub(crate) struct ToolboxTool {
    registry: WeakRegistry,
}

impl ToolboxTool {
    pub(super) fn new(registry: WeakRegistry) -> Self {
        Self { registry }
    }

    /// Effects of the tool this call would reach, or the write barrier when the
    /// registry cannot classify it.
    fn target_effects(&self, input: &Value) -> ToolEffects {
        let Some(name) = input.get("tool").and_then(Value::as_str).map(str::trim) else {
            return ToolEffects::write();
        };
        if name.is_empty() || Registry::resolve_tool_name(name) == TOOLBOX_TOOL {
            // `run` refuses to call the dispatcher itself, so the call reaches
            // nothing this hook can classify.
            return ToolEffects::write();
        }
        let Some(args) = input.get("args") else {
            return ToolEffects::write();
        };
        let Some(registry) = self.registry.upgrade() else {
            return ToolEffects::write();
        };
        registry
            .declared_effects_for_input_sync(name, args)
            .unwrap_or(ToolEffects::write())
    }
}

#[derive(Deserialize)]
struct Input {
    action: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    args: Option<Value>,
}

const DESCRIPTION: &str = "\
Call a tool whose schema is not loaded, including those listed under \"Additional tools\". \
`list` names them, `describe` returns one schema, `run` calls one with its own arguments. \
Policy, approval, and hooks apply to that tool, as for a direct call.";

#[async_trait]
impl Tool for ToolboxTool {
    fn name(&self) -> &str {
        TOOLBOX_TOOL
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    /// A dispatcher can reach a write, so it must never overlap another call.
    fn concurrency_class(&self) -> ConcurrencyClass {
        ConcurrencyClass::Ordered
    }

    /// Effects of one dispatcher call, resolved to the tool it reaches.
    ///
    /// `list` and `describe` only read what the registry already holds, so a
    /// planning session may use them. `run` reaches whatever the target declares
    /// for the arguments it was handed, through the same synchronous lookup
    /// `Registry::declared_effects_sync` uses, because this hook cannot await.
    /// Every call that dispatches nothing - a missing action, an action this
    /// build does not know - and every answer this hook cannot resolve is the
    /// write barrier: a read-only gate must never be opened by a call whose
    /// reach it cannot see.
    fn effects_for_input(&self, input: &Value) -> ToolEffects {
        match input.get("action").and_then(Value::as_str) {
            Some("list" | "describe") => ToolEffects::read(),
            Some("run") => self.target_effects(input),
            _ => ToolEffects::write(),
        }
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "describe", "run"],
                    "description": "`list`, `describe`, or `run`."
                },
                "tool": {
                    "type": "string",
                    "description": "Target tool name."
                },
                "args": {
                    "type": "object",
                    "description": "`run` arguments."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let input: Input = serde_json::from_value(input)?;
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| anyhow!("toolbox is unavailable: its registry was dropped"))?;

        match input.action.as_str() {
            "list" => list(&registry, &ctx).await,
            "describe" => {
                let name = require_tool(&input)?;
                describe(&registry, &ctx, name).await
            }
            "run" => {
                let name = require_tool(&input)?.to_string();
                let args = input
                    .args
                    .ok_or_else(|| anyhow!("toolbox run needs `args`, the object the target tool takes"))?;
                run(&registry, &ctx, &name, args).await
            }
            other => bail!("Unknown toolbox action {other:?}; expected one of: list, describe, run"),
        }
    }
}

fn require_tool(input: &Input) -> Result<&str> {
    input
        .tool
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("toolbox {} needs `tool`", input.action))
}

/// The session's load tiers, plus the policy they were resolved against.
///
/// The Agent resolves the tiers once at session start and registers that
/// snapshot with the session policy, so a configuration reload cannot move a
/// tool between the eager and deferred sets mid-session. A registry built
/// without an Agent (a bare `Registry::new` in a test or a remote client) has
/// no snapshot, and resolves the current configuration instead.
fn tiers_for(ctx: &ToolContext) -> (crate::config::ToolLoadTiers, Option<super::SessionToolPolicy>) {
    tiers_for_session(&ctx.session_id)
}

/// The same snapshot, for a caller that holds a session id and no tool call.
pub(crate) fn tiers_for_session(session_id: &str) -> (crate::config::ToolLoadTiers, Option<super::SessionToolPolicy>) {
    let policy = super::session_tool_policy(session_id);
    let tiers = policy
        .as_ref()
        .and_then(|policy| policy.load_tiers.clone())
        .unwrap_or_else(|| {
            crate::config::config().tools.load_tiers(
                policy.as_ref().and_then(|policy| policy.allowed_tools.as_ref()),
                // A registry with no Agent has no session snapshot to read a
                // listing decision from, so it resolves the session as not
                // core-listed.
                false,
            )
        });
    (tiers, policy)
}

/// Whether this session can reach `name` at all.
///
/// `list`, `describe` and `run` all apply this decision, and the system-prompt
/// index advertises exactly the set they reach, so the two surfaces cannot
/// disagree: a tool one of them refuses is not named by the other. The
/// decision is the frozen tier snapshot plus the session policy, which already
/// carries an SDK overlay's `enabled` and `disabled` entries.
pub(crate) fn session_can_reach(registry: &Registry, session_id: &str, name: &str) -> bool {
    let (tiers, policy) = tiers_for_session(session_id);
    reachable(registry, policy.as_ref(), &tiers, name)
}

/// Reject a tool this session cannot reach, and report the tier when it can.
fn ensure_reachable(
    registry: &Registry,
    policy: Option<&super::SessionToolPolicy>,
    tiers: &crate::config::ToolLoadTiers,
    name: &str,
) -> Result<LoadMode> {
    let mode = tiers.tier(name);
    if mode == LoadMode::Off {
        bail!("Tool '{name}' is off in this session (load mode `off`), so toolbox cannot reach it");
    }
    if let Some(policy) = policy {
        if let Some(allowed) = policy.allowed_tools.as_ref()
            && !registry.tool_is_allowed(allowed, name)
        {
            bail!("Tool '{name}' is not allowed in this session");
        }
        if registry.tool_is_disabled(&policy.disabled_tools, name) {
            bail!("Tool '{name}' is disabled in this session");
        }
    }
    Ok(mode)
}

fn reachable(
    registry: &Registry,
    policy: Option<&super::SessionToolPolicy>,
    tiers: &crate::config::ToolLoadTiers,
    name: &str,
) -> bool {
    ensure_reachable(registry, policy, tiers, name).is_ok()
}

/// Per-server `mcp__*` proxies, which have a discovery surface of their own, so
/// `list` does not duplicate them. The fixed `mcp_search`/`mcp_call` surface is
/// listed only when this session carries it that way (see
/// [`super::session_lists_mcp_surface`]).
fn is_mcp_surface(name: &str) -> bool {
    name.starts_with("mcp__")
}

fn is_fixed_mcp_surface(name: &str) -> bool {
    matches!(name, "mcp_search" | "mcp_call")
}

async fn list(registry: &Registry, ctx: &ToolContext) -> Result<ToolOutput> {
    let (tiers, policy) = tiers_for(ctx);
    let listed_mcp_surface = super::session_lists_mcp_surface(&ctx.session_id);
    let mut rows: Vec<(String, String)> = registry
        .definitions(None)
        .await
        .into_iter()
        .filter(|definition| definition.name != TOOLBOX_TOOL && !is_mcp_surface(&definition.name))
        .filter(|definition| {
            tiers.tier(&definition.name) == LoadMode::Discoverable
                || (listed_mcp_surface && is_fixed_mcp_surface(&definition.name))
        })
        .filter(|definition| reachable(registry, policy.as_ref(), &tiers, &definition.name))
        .map(|definition| {
            (
                definition.name,
                crate::prompt::one_line_description(&definition.description),
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    if rows.is_empty() {
        return Ok(ToolOutput::new(
            "No discoverable tools: this session already carries every tool it can reach eagerly.",
        ));
    }

    let mut output =
        String::from("Discoverable tools. Call one with `toolbox run`, read its schema with `toolbox describe`:\n");
    for (name, description) in rows {
        output.push_str(&format!("- {name}: {description}\n"));
    }
    Ok(ToolOutput::new(output.trim_end().to_string()).with_title("Discoverable tools"))
}

async fn describe(registry: &Registry, ctx: &ToolContext, name: &str) -> Result<ToolOutput> {
    let name = Registry::resolve_tool_name(name);
    let (tiers, policy) = tiers_for(ctx);
    ensure_reachable(registry, policy.as_ref(), &tiers, name)?;

    let definition = registry
        .definitions(None)
        .await
        .into_iter()
        .find(|definition| definition.name == name)
        .ok_or_else(|| anyhow!("Unknown tool: {name}"))?;
    let schema = serde_json::to_string_pretty(&definition.input_schema)?;
    let output = format!("{name}\n{}\n\nInput schema:\n{schema}", definition.description.trim());
    Ok(ToolOutput::new(output)
        .with_title(format!("Schema for {name}"))
        .with_metadata(json!({ TOOL_REFERENCES_METADATA_KEY: [name] })))
}

async fn run(registry: &Registry, ctx: &ToolContext, name: &str, args: Value) -> Result<ToolOutput> {
    let name = Registry::resolve_tool_name(name);
    if name == TOOLBOX_TOOL {
        bail!("toolbox cannot run itself");
    }
    let (tiers, policy) = tiers_for(ctx);
    ensure_reachable(registry, policy.as_ref(), &tiers, name)?;
    if !args.is_object() {
        bail!("toolbox run needs `args` to be a JSON object, got {}", json_kind(&args));
    }

    // The subcall EXECUTES a tool for the same session, so share the session's
    // compaction manager exactly as `batch` and `codemode` do: the overflow
    // guard inside `execute` must read this session's real budget.
    let registry = registry.clone_sharing_compaction();
    let sub_ctx = ctx.for_subcall(format!("toolbox-{name}"));
    let mut output = registry.execute(name, args, sub_ctx).await?;

    let mut metadata = output
        .metadata
        .take()
        .and_then(|value| match value {
            Value::Object(object) => Some(object),
            _ => None,
        })
        .unwrap_or_else(Map::new);
    metadata.insert(TOOL_REFERENCES_METADATA_KEY.to_string(), json!([name]));
    output.metadata = Some(Value::Object(metadata));
    Ok(output)
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

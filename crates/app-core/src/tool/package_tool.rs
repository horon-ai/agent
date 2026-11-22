//! The tools a user-installed package declares, on a session's tool registry.
//!
//! A package tool is not a second dispatch path. The turn boundary captures the
//! package generation, reads each listed package's installed declaration, and
//! registers one [`PackageTool`] per declared tool under `ext__<id>__<name>`.
//! From there every call is an ordinary `Registry::execute`: plan mode refuses
//! its process effect, the approval mode decides whether the user must answer,
//! the allow and block lists and the SDK overlay decide whether the name is
//! reachable, the load tiers decide whether its schema is sent eagerly or
//! deferred, and the `pre_tool` gate reviews the payload the child would
//! receive. Only the tool body talks to the child; that lives in
//! [`crate::extensions::invoke`].
//!
//! What this module decides is exposure, and it decides it from the session's
//! own frozen policy:
//!
//! - a profile that names its tools (`minimal`, `none`, `acp`) exposes no
//!   package tool, because a package name is not on its list;
//! - the untouched `core` profile lists them behind the dispatcher, the same
//!   way it lists the core tools it does not send eagerly, and the `full`
//!   default defers them;
//! - an explicit `[tools] enabled` list or an SDK `enabled` overlay exposes
//!   exactly the names it names, eagerly;
//! - `[tools] disabled`, `[tools.load_mode] = "off"`, and any `off` tier hide
//!   the name outright;
//! - a registry entry that already holds the name wins: the package never
//!   replaces a tool this host already had.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use anyhow::Result;
use async_trait::async_trait;
use horon_agent_message_types::ToolDefinition;
use horon_agent_tool_core::ensure_intent_in_schema;
use serde_json::Value;

use super::{Registry, Tool, ToolContext, ToolEffects, ToolOutput, sdk};
use crate::config::LoadMode;
use crate::extensions::invoke;
use crate::extensions::package_snapshot::PackageSession;

/// The registry name of one declared tool: `ext__<package id>__<declared name>`.
///
/// The installer caps the declared part so the whole name stays within 64
/// bytes, which is the limit a provider alias already respects.
pub(crate) fn public_tool_name(package_id: &str, tool: &str) -> String {
    format!("ext__{package_id}__{tool}")
}

/// How one declared tool appears on the session's surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// The session must not see or reach the name.
    Hidden,
    /// The schema travels with the tool list.
    Eager,
    /// The schema stays out of the cached prefix and the name is discovered.
    Deferred,
}

/// One declared package tool, registered under its public name.
struct PackageTool {
    name: String,
    /// The name the child knows: exactly what the declaration said.
    declared: String,
    package_id: String,
    description: String,
    schema: Value,
    deferred: bool,
}

#[async_trait]
impl Tool for PackageTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }

    /// A package tool runs a native process, so it is a write barrier and never
    /// overlaps another call in the round.
    fn effects(&self) -> ToolEffects {
        ToolEffects::process()
    }

    fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: ensure_intent_in_schema(self.schema.clone()),
            defer_loading: self.deferred,
        }
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let text = invoke::call_tool(&ctx.session_id, &self.package_id, &self.declared, input).await?;
        Ok(ToolOutput::new(text))
    }
}

/// Names this host registered, per session, so a later turn can withdraw them.
///
/// The stored value is the tool this module installed: a name that now maps to
/// anything else was installed by someone else and is left alone.
type SessionTools = HashMap<String, Arc<dyn Tool>>;
type RegisteredTools = HashMap<String, SessionTools>;
static REGISTERED: LazyLock<Mutex<RegisteredTools>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_registered() -> MutexGuard<'static, RegisteredTools> {
    REGISTERED.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Expose the tools the pinned packages declare on this session's registry.
///
/// Runs at the turn boundary, after the snapshot was captured and before the
/// tool list freezes. A name that is already registered is left as it is, and a
/// name this module registered earlier and no longer wants is withdrawn. The
/// names it exposed come back so the caller can widen the session's own
/// allow-list: the tool list is filtered by that list, not by the registry.
pub(crate) async fn sync_session_tools(registry: &Registry, session_id: &str, session: &PackageSession) -> Vec<String> {
    let Some(snapshot) = session.current() else {
        return Vec::new();
    };
    let store = session.store();
    let mut desired: Vec<(String, Arc<dyn Tool>)> = Vec::new();
    for record in snapshot.packages() {
        let declarations = match invoke::declarations_for(store, record).await {
            Ok(declarations) => declarations,
            Err(error) => {
                crate::logging::warn(&format!("package {} does not expose its tools: {error:#}", record.id));
                continue;
            }
        };
        for declared in &declarations.manifest.tools {
            let name = public_tool_name(&record.id, &declared.name);
            let stage = stage_for(session_id, &name);
            if stage == Stage::Hidden {
                continue;
            }
            let tool: Arc<dyn Tool> = Arc::new(PackageTool {
                name: name.clone(),
                declared: declared.name.clone(),
                package_id: record.id.clone(),
                description: declared.description.clone(),
                schema: declared.input_schema.clone(),
                deferred: stage == Stage::Deferred,
            });
            desired.push((name, tool));
        }
    }

    let present: HashSet<String> = {
        let tools = registry.tools.read().await;
        desired
            .iter()
            .filter(|(name, _)| tools.contains_key(name))
            .map(|(name, _)| name.clone())
            .collect()
    };
    for (name, tool) in &desired {
        if present.contains(name) {
            continue;
        }
        registry.register(name.clone(), Arc::clone(tool)).await;
        lock_registered()
            .entry(session_id.to_owned())
            .or_default()
            .insert(name.clone(), Arc::clone(tool));
    }

    let wanted: HashSet<&str> = desired.iter().map(|(name, _)| name.as_str()).collect();
    let stale: Vec<(String, Arc<dyn Tool>)> = lock_registered()
        .get(session_id)
        .into_iter()
        .flat_map(|registered| registered.iter())
        .filter(|(name, _)| !wanted.contains(name.as_str()))
        .map(|(name, tool)| (name.clone(), Arc::clone(tool)))
        .collect();
    for (name, ours) in stale {
        let current = {
            let tools = registry.tools.read().await;
            tools.get(&name).cloned()
        };
        if current.is_some_and(|current| Arc::ptr_eq(&current, &ours)) {
            registry.unregister(&name).await;
        }
        if let Some(registered) = lock_registered().get_mut(session_id) {
            registered.remove(&name);
        }
    }
    desired.into_iter().map(|(name, _)| name).collect()
}

/// Drop one session's registration record and its children.
pub(crate) fn forget_session(session_id: &str) {
    lock_registered().remove(session_id);
    invoke::forget_session(session_id);
}

/// How this session's policy exposes one package tool name.
fn stage_for(session_id: &str, name: &str) -> Stage {
    if let Some(config) = sdk::config(session_id) {
        // A client that names its own tools names them exactly, and a custom
        // tool under the same name already owns it.
        if config.disabled.iter().any(|existing| existing == name)
            || config.custom.iter().any(|custom| custom.name == name)
        {
            return Stage::Hidden;
        }
        if let Some(enabled) = config.enabled.as_ref() {
            return if enabled.iter().any(|existing| existing == name) {
                Stage::Eager
            } else {
                Stage::Hidden
            };
        }
    }
    let Some(policy) = super::session_tool_policy(session_id) else {
        return Stage::Eager;
    };
    if policy.disabled_tools.contains(name) {
        return Stage::Hidden;
    }
    if let Some(tiers) = policy.load_tiers.as_ref() {
        if tiers.tier(name) == LoadMode::Off {
            return Stage::Hidden;
        }
        if let Some(explicit) = tiers.explicit_tier(name) {
            return match explicit {
                LoadMode::Essential => Stage::Eager,
                LoadMode::Discoverable => Stage::Deferred,
                LoadMode::Off => Stage::Hidden,
            };
        }
        // `core` lists the tools it does not send eagerly, and `full` defers
        // everything outside its eager set; a package schema is neither small
        // nor stable, so both defer it the same way.
        if tiers.core_listed() || tiers.default_tier() == LoadMode::Discoverable {
            return Stage::Deferred;
        }
    }
    if let Some(allowed) = policy.allowed_tools.as_ref()
        && !allowed.contains(name)
    {
        return Stage::Hidden;
    }
    Stage::Eager
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    /// An SDK `enabled` list is a client naming its tools exactly, so a package
    /// tool is exposed only when it is one of those names.
    #[test]
    fn an_sdk_enabled_list_exposes_exactly_the_names_it_gives() {
        let session = "package-tool-stage-sdk-list";
        let _registration = crate::tool::register_session_tool_policy(
            session,
            Some(HashSet::from(["read".to_owned()])),
            HashSet::new(),
        );
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        crate::tool::sdk::configure(
            session,
            "package-tool-stage-owner",
            crate::protocol::SessionToolConfig {
                enabled: Some(vec!["read".to_owned(), "ext__demo__greet".to_owned()]),
                disabled: Vec::new(),
                custom: Vec::new(),
            },
            sender,
        )
        .expect("configure the SDK tool list");

        assert_eq!(stage_for(session, "ext__demo__greet"), Stage::Eager);
        assert_eq!(stage_for(session, "ext__demo__other"), Stage::Hidden);
    }

    /// A custom tool the client ships under the same name already owns it.
    #[test]
    fn an_sdk_custom_tool_owns_a_colliding_package_name() {
        let session = "package-tool-stage-sdk-custom";
        let _registration = crate::tool::register_session_tool_policy(
            session,
            Some(HashSet::from(["read".to_owned()])),
            HashSet::new(),
        );
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        crate::tool::sdk::configure(
            session,
            "package-tool-stage-owner",
            crate::protocol::SessionToolConfig {
                enabled: Some(vec!["ext__demo__greet".to_owned()]),
                disabled: Vec::new(),
                custom: vec![crate::protocol::SessionToolDefinition {
                    name: "ext__demo__greet".to_owned(),
                    description: "The client's own tool".to_owned(),
                    parameters: serde_json::json!({"type": "object"}),
                }],
            },
            sender,
        )
        .expect("configure the SDK tool list");

        assert_eq!(stage_for(session, "ext__demo__greet"), Stage::Hidden);
    }
}

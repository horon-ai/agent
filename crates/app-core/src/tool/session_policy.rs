use super::{eval, sdk, tool_name_is_allowed, tool_name_is_disabled};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock as StdRwLock};

#[derive(Clone, Debug, Default)]
pub(in crate::tool) struct SessionToolPolicy {
    pub(super) allowed_tools: Option<HashSet<String>>,
    pub(super) disabled_tools: HashSet<String>,
    /// The tiers the session resolved when it started, so a dispatcher such as
    /// `toolbox` reads the same frozen snapshot the definition list uses.
    pub(super) load_tiers: Option<crate::config::ToolLoadTiers>,
    /// Whether `mcp_search`/`mcp_call` are listed rather than eager here.
    ///
    /// The Agent publishes it when it takes the decision, because the two tools
    /// that read it - `toolbox` and the `mcp` management tool - hold a registry
    /// and cannot reach the session's own state.
    pub(super) listed_mcp_surface: bool,
    /// A read-only consult child: no client tool configuration can widen its stored allow list.
    pub(super) consult: bool,
    /// Ephemeral `--no-session` run: skip sidecar persistence under `$HORON_HOME/sessions/`.
    pub(super) memory_only: bool,
    /// Additional explicitly granted workspace directories (`--add-dir`).
    pub(super) additional_dirs: Vec<std::path::PathBuf>,
    owner: Option<u64>,
}

pub(in crate::tool) static SESSION_TOOL_POLICIES: LazyLock<StdRwLock<HashMap<String, Arc<SessionToolPolicy>>>> =
    LazyLock::new(|| StdRwLock::new(HashMap::new()));
static NEXT_SESSION_TOOL_POLICY_OWNER: AtomicU64 = AtomicU64::new(1);

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
            eval::forget_session(&self.session_id);
            crate::extensions::package_snapshot::forget_session(&self.session_id);
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
    if let Some(policy) = policies.get_mut(session_id).map(Arc::make_mut) {
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
        Arc::new(SessionToolPolicy {
            allowed_tools,
            disabled_tools,
            owner: Some(owner),
            ..Default::default()
        }),
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
    if let Some(policy) = policies.get_mut(session_id).map(Arc::make_mut) {
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
    if let Some(policy) = policies.get_mut(session_id).map(Arc::make_mut) {
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

/// Mark whether `session_id` is an ephemeral `--no-session` run that must never
/// write sidecars beneath `$HORON_HOME/sessions/`.
pub(crate) fn register_session_memory_only(session_id: &str, memory_only: bool) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(policy) = policies.get_mut(session_id).map(Arc::make_mut) {
        policy.memory_only = memory_only;
    }
}

/// Whether `session_id` is marked ephemeral (`--no-session`).
pub(crate) fn session_is_memory_only(session_id: &str) -> bool {
    SESSION_TOOL_POLICIES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .is_some_and(|policy| policy.memory_only)
}

/// Publish extra workspace directories (`--add-dir`) reachable by file tools.
pub(crate) fn register_session_additional_dirs(session_id: &str, dirs: &[String]) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(policy) = policies.get_mut(session_id).map(Arc::make_mut) {
        policy.additional_dirs = dirs.iter().map(std::path::PathBuf::from).collect();
    }
}

/// Extra workspace directories granted for `session_id` beyond `working_dir`.
pub(crate) fn session_additional_dirs(session_id: &str) -> Vec<std::path::PathBuf> {
    let policies = SESSION_TOOL_POLICIES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match policies.get(session_id) {
        Some(policy) => policy.additional_dirs.clone(),
        None => Vec::new(),
    }
}

/// Update load tiers, ephemeral `--no-session` flag, and `--add-dir` roots in one call.
pub(crate) fn register_session_runtime_options(
    session_id: &str,
    tiers: crate::config::ToolLoadTiers,
    memory_only: bool,
    additional_dirs: &[String],
) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(policy) = policies.get_mut(session_id).map(Arc::make_mut) {
        policy.load_tiers = Some(tiers);
        policy.memory_only = memory_only;
        policy.additional_dirs = additional_dirs.iter().map(std::path::PathBuf::from).collect();
    }
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
        Arc::new(SessionToolPolicy {
            allowed_tools,
            disabled_tools,
            ..Default::default()
        }),
    );
}

#[cfg(test)]
pub(crate) fn set_session_listed_mcp_for_test(session_id: &str, listed: bool) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::make_mut(policies.entry(session_id.to_string()).or_default()).listed_mcp_surface = listed;
}

#[cfg(test)]
pub(crate) fn clear_session_tool_policy(session_id: &str) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.remove(session_id);
}

/// The session's policy as one shared snapshot.
///
/// A session without an SDK overlay gets the stored policy itself, so a
/// dispatch reads it without copying the name sets. An overlay is merged into
/// a copy: the stored policy never carries the client's entries.
pub(super) fn session_tool_policy_shared(session_id: &str) -> Option<Arc<SessionToolPolicy>> {
    let stored = SESSION_TOOL_POLICIES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .cloned();
    let Some(config) = sdk::config(session_id).filter(|_| !stored.as_deref().is_some_and(|p| p.consult)) else {
        return stored;
    };
    let mut policy = stored.map_or_else(SessionToolPolicy::default, Arc::unwrap_or_clone);
    if let Some(enabled) = config.enabled {
        policy.allowed_tools = Some(enabled.into_iter().collect());
        policy.disabled_tools.clear();
    }
    policy.disabled_tools.extend(config.disabled);
    if let Some(allowed) = policy.allowed_tools.as_mut() {
        allowed.extend(config.custom.into_iter().map(|t| t.name));
    }
    Some(Arc::new(policy))
}

pub(super) fn session_tool_policy(session_id: &str) -> Option<SessionToolPolicy> {
    session_tool_policy_shared(session_id).map(Arc::unwrap_or_clone)
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

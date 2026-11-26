//! The tool list a swarm worker runs with.

use std::collections::HashSet;

/// The tools a swarm worker may call.
///
/// A worker cannot spawn or coordinate workers of its own, and it applies the
/// configured allow-list. It sends every tool it keeps eagerly and has no
/// index, so under the default `core` profile it keeps only the core tools
/// rather than every schema of the open default surface.
pub(crate) async fn worker_allow_list(registry: &crate::tool::Registry) -> HashSet<String> {
    let mut allowed: HashSet<String> = registry.tool_names().await.into_iter().collect();
    for blocked in ["subagent", "task", "swarm", "todo", "session_search"] {
        allowed.remove(blocked);
    }
    let tools = &crate::config::config().tools;
    tools.apply_to_allowed_set(&mut allowed);
    if tools.is_core_surface() {
        allowed.retain(|name| crate::config::CORE_TOOLS.contains(&name.as_str()));
    }
    allowed
}

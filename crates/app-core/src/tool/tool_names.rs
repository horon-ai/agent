//! Name matching shared by the registry, the SDK overlay, and profile code.
//!
//! These helpers decide whether an allow-list or a disable-list admits a tool
//! name, including the umbrella `mcp` entry and the fixed discovery tools.

use std::collections::HashSet;

pub(crate) fn tool_name_is_allowed(allowed: &HashSet<String>, name: &str) -> bool {
    allowed.contains(name)
        || (allowed.contains("mcp") && is_mcp_tool_name(name))
        || (is_fixed_mcp_tool(name) && allowed.iter().any(|tool| tool.starts_with("mcp__")))
}

pub(crate) fn tool_name_is_disabled(disabled: &HashSet<String>, name: &str) -> bool {
    disabled.contains(name) || (disabled.contains("mcp") && is_mcp_tool_name(name))
}

pub(crate) fn is_fixed_mcp_tool(name: &str) -> bool {
    matches!(name, "mcp_search" | "mcp_call")
}

pub(crate) fn is_mcp_tool_name(name: &str) -> bool {
    name == "mcp" || name.starts_with("mcp__") || is_fixed_mcp_tool(name)
}

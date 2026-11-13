//! Behavioral check for the headless core tool profile (IS-4).
//!
//! Lives as an integration test so it compiles and runs independently of the
//! `horon-agent-base` unit-test lib, and so it exercises the profile through the
//! public API a caller actually uses.

use horon_agent_base::config::{CORE_TOOL_PROFILE, TOOLBOX_TOOL, ToolConfig};

/// The eighteen-tool headless default, exactly as `AGENTS.md` and `CORE_TOOLS`
/// state it. `toolbox` sits beside this set as the dispatcher for the deferred
/// entries, not inside it.
const KEEP: &[&str] = &[
    "bash",
    "read",
    "write",
    "edit",
    "replace",
    "apply_patch",
    "agentgrep",
    "ls",
    "webfetch",
    "websearch",
    "skill_manage",
    "mcp",
    "monitor",
    "undo",
    "ask",
    "todo",
    "session_search",
    "swarm",
];

/// Tools the core profile still refuses. `memory` and `irc` are off in the
/// shipped default; the rest are operator and self-development surfaces that
/// only the `full` profile carries.
const DROP: &[&str] = &[
    "batch",
    "bg",
    "memory",
    "selfdev",
    "desktop_selfdev",
    "compile_remote",
    "schedule",
    "conversation_search",
    "irc",
];

#[test]
fn core_profile_keeps_exactly_the_headless_tool_set() {
    let cfg = ToolConfig {
        profile: CORE_TOOL_PROFILE.to_string(),
        ..ToolConfig::default()
    };
    let allowed = cfg.allowed_tools().expect("core profile is an allow-list");

    assert_eq!(
        allowed.len(),
        KEEP.len() + 1,
        "core profile must expose the kept set plus the dispatcher, got {allowed:?}"
    );
    for kept in KEEP {
        assert!(allowed.contains(*kept), "core profile must keep {kept}");
    }
    assert!(
        allowed.contains(TOOLBOX_TOOL),
        "core profile must reach the dispatcher that lists the other core tools"
    );
    assert!(allowed.contains("skill_manage"), "skill tool must stay");
    assert!(allowed.contains("mcp"), "mcp tool must stay");
    for dropped in DROP {
        assert!(!allowed.contains(*dropped), "core profile must not expose {dropped}");
    }
}

#[test]
fn core_profile_is_the_documented_headless_default() {
    assert_eq!(CORE_TOOL_PROFILE, "core");
}

#[test]
fn an_explicit_full_profile_widens_back_to_the_unrestricted_set() {
    let cfg = ToolConfig {
        profile: "full".to_string(),
        ..ToolConfig::default()
    };
    assert!(
        cfg.allowed_tools().is_none(),
        "the full profile must remain unrestricted so it can override the default"
    );
}

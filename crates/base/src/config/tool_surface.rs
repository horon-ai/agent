//! The default `core` tool surface: which built-in tools it reaches and which
//! of them it sends eagerly.

use super::{CHECKPOINT_TOOLS, IRC_TOOL};

/// Tools exposed by [`CORE_TOOL_PROFILE`](super::CORE_TOOL_PROFILE).
///
/// Every built-in tool this build has is reachable in the default profile: this
/// constant plus [`CORE_DISCOVERABLE_TOOLS`] plus [`TOOLBOX_TOOL`](super::TOOLBOX_TOOL). Only
/// mode-only and per-session tools stay out: `schedule`, `invalid`,
/// `request_permission`, `submit_plan`, and the ambient cycle tools. `irc`,
/// `checkpoint`, and `rewind` join through their own switches; a tool whose
/// switch is off is never registered, and `[tools] enabled`/`disabled` and
/// `[tools.load_mode]` still remove any of them. `skill_manage` is the registry
/// key for the skill tool (`resolve_tool_name` maps `skill` to it); `mcp` is the
/// wildcard covering every `mcp__server__tool` plus the fixed
/// `mcp_search`/`mcp_call` surface.
///
/// The list is the reachability contract, not the eager surface: a `core`
/// listing sends only [`ESSENTIAL_TOOLS`] eagerly and names the rest in its
/// system prompt, where `toolbox` reaches them; others send all of them.
pub const CORE_TOOLS: &[&str] = &[
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

/// Built-in tools the default profile reaches only through `toolbox`.
///
/// The default surface is open: these join the `core` allow-list beside
/// [`CORE_TOOLS`] and are listed in the session's system prompt instead of
/// paying for their schemas in the cached prefix. Each one is still gated by the
/// switch that registers it (`eval`, `redo`, `hashline_edit`, `ast_tools`,
/// `lsp`, `context_notes`, `checkpoint`, `codemode`, and the model roles
/// `generate_image` needs), by `[tools] disabled`, and
/// by `[tools.load_mode]`, so any of them can be removed without touching the
/// others. `irc`, `checkpoint`, and `rewind` are absent on purpose: they join
/// the `core` list only through their own switches
/// (`config/tool_selection.rs`), while the tier resolver still knows them, so a
/// joined tool arrives listed rather than eager in a core session.
pub const CORE_DISCOVERABLE_TOOLS: &[&str] = &[
    "ast_edit",
    "ast_grep",
    "batch",
    "bg",
    "codemode",
    "context_notes",
    "conversation_search",
    "documentation",
    "eval",
    "generate_image",
    "goal",
    "hashline_edit",
    "lsp",
    "memory",
    "new_context",
    "redo",
];

/// Tools a `core`-listed session keeps eager.
///
/// These are the tools almost every coding turn touches, so their schemas stay
/// in the cached prefix. The other reachable entries are listed by name in the
/// system prompt and called through `toolbox`; an operator who wants one
/// of them eager again sets `[tools.load_mode] <tool> = "essential"`. `eval`
/// is not here because its schema is only worth sending when a Python
/// interpreter exists on PATH; the session resolves that at start-up.
pub const ESSENTIAL_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "apply_patch",
    "bash",
    "ls",
    "agentgrep",
    "todo",
    "bg",
    "websearch",
    "webfetch",
];

/// Whether a tool is reachable in the default `core` profile.
///
/// The allow-list joins [`CORE_TOOLS`] and [`CORE_DISCOVERABLE_TOOLS`], while
/// `irc`, `checkpoint`, and `rewind` join at the same tier through their own
/// switches. One predicate keeps the allow-list and the tier resolver agreeing
/// about what that profile reaches.
pub(super) fn is_core_reachable(name: &str) -> bool {
    CORE_TOOLS.contains(&name)
        || CORE_DISCOVERABLE_TOOLS.contains(&name)
        || name == IRC_TOOL
        || CHECKPOINT_TOOLS.contains(&name)
}

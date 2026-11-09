//! `[mcp]`: which MCP server configs beside horon-agent's own are read.
//!
//! horon-agent always reads `~/.horon/mcp.json` and, in a trusted
//! workspace, the project files. Another host's user-wide config is a separate
//! decision: its servers were reviewed for that host, and loading them starts
//! processes here. Each host therefore has its own switch, off by default. The
//! source table in `mcp::sources` consults these values before it opens a file.

use serde::{Deserialize, Serialize};

/// MCP config sources horon-agent does not own.
///
/// `[mcp]` in config.toml. With a switch off the servers are not loaded, and
/// one log line per process names how many are waiting in that file.
/// `HORON_AGENT_DISABLE_CLAUDE_MCP` turns the Claude source off even when its
/// switch is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSourcesConfig {
    /// Read Claude Code's `~/.claude.json` (global servers and this project's
    /// entry) and `~/.claude/mcp.json`. Off by default.
    pub claude_user: bool,
    /// Read Codex CLI's `[mcp_servers.*]` in `~/.codex/config.toml`. Off by
    /// default; the file is read live, never copied.
    pub codex_user: bool,
}

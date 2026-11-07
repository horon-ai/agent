//! Shared terminal-identity helpers for hook launchers and clients.
//!
//! horon-agent never opens a terminal window, so nothing here spawns one. The
//! crate re-exports the client terminal-env snapshot/re-export, shell escaping,
//! and the hook command-line splitter that lifecycle hooks and ACP clients use.

pub use horon_agent_terminal_launch::{
    apply_client_terminal_env, parse_hook_command, sh_escape, snapshot_client_terminal_env,
};

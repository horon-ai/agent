#![allow(
    unknown_lints,
    clippy::collapsible_match,
    clippy::manual_checked_ops,
    clippy::unnecessary_sort_by,
    clippy::useless_conversion
)]
// The `swarm` tool's `json!` parameter schema is large; the default macro
// recursion limit (128) is exceeded once more properties are added.
#![recursion_limit = "256"]

//! Application core for horon-agent (upper layer).
//!
//! This crate holds the server/tool/agent layer and its presentation-adjacent
//! leaves. The foundational layer it builds on (provider, auth, config, session,
//! message, memory, ...) lives in the `horon-agent-base` crate and is
//! re-exported here via `pub use horon_agent_base::*`, so every existing
//! `crate::<module>` path (e.g. `crate::config`, `crate::provider`) keeps
//! resolving unchanged across this crate and the root `horon-agent` crate, which in
//! turn re-exports this crate via `pub use horon_agent_app_core::*`.

// Foundational layer: re-export every `horon-agent-base` module so `crate::<module>`
// paths resolve here exactly as they did before the split.
pub use horon_agent_base::*;

// Upper layer (server / tool / agent and supporting leaves).
pub mod acp_render;
pub mod agent;
pub mod agent_result_cache;
pub mod ambient;
pub mod ambient_runner;
pub mod ambient_scheduler;
pub mod approval;
pub mod build;
pub mod catchup;
pub mod durable_input_runtime;
pub use horon_agent_base::external_auth;
pub mod mission;
pub mod network_retry;
pub mod notifications;
pub mod overnight;
pub mod perf;
pub mod plan_mode;
pub mod running_tool_registry;
pub mod server;
pub mod server_spawn;
pub mod session_effort;
pub mod session_launch;
pub mod session_rebuild;
pub mod ssh_remote;
pub mod startup_profile;
pub mod tool;
pub mod turn_cancel_registry;
pub mod update;

#[cfg(test)]
#[path = "session_reader_parity_tests.rs"]
mod session_reader_parity_tests;

use std::sync::Mutex;

static CURRENT_SESSION_ID: Mutex<Option<String>> = Mutex::new(None);

pub fn set_current_session(session_id: &str) {
    if let Ok(mut guard) = CURRENT_SESSION_ID.lock() {
        *guard = Some(session_id.to_string());
    }
}

pub fn get_current_session() -> Option<String> {
    CURRENT_SESSION_ID.lock().ok()?.clone()
}

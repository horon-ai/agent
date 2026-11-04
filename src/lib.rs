#![allow(
    unknown_lints,
    clippy::collapsible_match,
    clippy::manual_checked_ops,
    clippy::unnecessary_sort_by,
    clippy::useless_conversion
)]
// Tests hold the process-wide env lock across awaits on purpose.
#![cfg_attr(test, allow(clippy::await_holding_lock))]

//! Root `horon-agent` crate: the entrypoint + cli layer on top of the `horon-agent-app-core`
//! crate (which in turn re-exports the `horon-agent-base` foundation).
//!
//! Every non-cli module lives in `horon-agent-app-core` and is re-exported here via
//! `pub use horon_agent_app_core::*`, so existing `crate::<module>` paths (e.g.
//! `crate::config`, `crate::server`) keep resolving unchanged across the cli
//! code.

// Re-export the application core so `crate::<app-core module>` paths resolve.
pub use horon_agent_app_core::*;

// Cli + entrypoint layer (kept in the root crate).
pub mod cli;

use anyhow::Result;

pub async fn run() -> Result<()> {
    cli::startup::run().await
}

#![allow(
    unknown_lints,
    clippy::collapsible_match,
    clippy::manual_checked_ops,
    clippy::unnecessary_sort_by,
    clippy::useless_conversion
)]
// Tests hold the process-wide env lock across awaits on purpose.
#![cfg_attr(test, allow(clippy::await_holding_lock))]
// Apple's linker reports that the `__eh_frame` section of this crate's
// unoptimized lib-test executable passes its 16 MB limit for the compact unwind
// table, so panics in that test binary unwind through the slower DWARF path.
// The binary exists only to run tests; shipped binaries link without the message.
// Scoped to the one target that produces it.
#![cfg_attr(all(test, target_os = "macos"), allow(linker_messages))]

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

//! `[harness_api]` section of `config.toml`: the opt-in TCP listener the API
//! bridge can serve beside its local socket.
//!
//! The section holds no secret. A bearer token lives only in the owner-only
//! credential store (`horon-agent api-credentials`), and the bridge reads that
//! file itself, so nothing here has to be redacted from a printed config. This
//! type is serde-only: reading the section from the file, the environment and
//! the CLI flags belongs to the application layer (`horon_agent_base::harness_api`),
//! because that reader also decides whether an exposure is safe.

use serde::{Deserialize, Serialize};

/// The `[harness_api]` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessApiConfig {
    /// TCP address (`IP:PORT`) to serve the harness API on, beside the local
    /// socket. Absent means the local socket only, which is the default.
    pub bind: Option<String>,
    /// Acknowledgement that a TLS-terminating proxy fronts `bind`. A bind that
    /// is not a loopback address is refused without it, because the bearer
    /// token would travel in cleartext otherwise.
    pub insecure_behind_proxy: bool,
}

#[cfg(test)]
#[path = "harness_api_tests.rs"]
mod tests;

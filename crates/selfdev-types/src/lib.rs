use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod desktop;
pub use desktop::desktop_repo_root;

/// Environment variable that marks a child process as running in self-dev client
/// mode. Defined here (a low-level crate) so cross-cutting consumers (process
/// title, server tester spawning) can reference it without depending on the
/// `cli` subsystem.
pub const CLIENT_SELFDEV_ENV: &str = "HORON_AGENT_CLIENT_SELFDEV_MODE";

/// Returns true if the current process was launched in self-dev client mode
/// (i.e. `CLIENT_SELFDEV_ENV` is set). Defined in this low-level crate so any
/// consumer can check self-dev mode without depending on the `cli` subsystem.
pub fn client_selfdev_requested() -> bool {
    std::env::var(CLIENT_SELFDEV_ENV).is_ok()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfDevBuildCommand {
    pub program: String,
    pub args: Vec<String>,
    pub display: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SelfDevBuildTarget {
    Auto,
    Tui,
    All,
}

impl SelfDevBuildTarget {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "tui" | "horon-agent" => Ok(Self::Tui),
            "all" | "both" => Ok(Self::All),
            other => anyhow::bail!("invalid selfdev build target `{}`; expected auto, tui, or all", other),
        }
    }
}

/// Which binary to use.
#[derive(Debug, Clone)]
pub enum BinaryChoice {
    /// Use the stable version.
    Stable(String),
    /// Use the canary version for testing.
    Canary(String),
    /// Use current running binary because no versioned builds exist yet.
    Current,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingActivation {
    pub session_id: String,
    pub new_version: String,
    pub previous_current_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_shared_server_version: Option<String>,
    pub source_fingerprint: Option<String>,
    pub requested_at: DateTime<Utc>,
}

/// Status of a canary build being tested
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CanaryStatus {
    /// Build is currently being tested
    #[serde(alias = "Testing")]
    Testing,
    /// Build passed all tests and is ready for promotion
    #[serde(alias = "Passed")]
    Passed,
    /// Build failed testing
    #[serde(alias = "Failed")]
    Failed,
}

/// Information about a crash during canary testing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashInfo {
    /// Build hash that crashed
    pub build_hash: String,
    /// Exit code
    pub exit_code: i32,
    /// Stderr output (truncated)
    pub stderr: String,
    /// Timestamp of crash
    pub crashed_at: DateTime<Utc>,
    /// Git diff that was being tested
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

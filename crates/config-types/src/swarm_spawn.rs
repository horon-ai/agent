//! How swarm-created agents are spawned.

use serde::{Deserialize, Serialize};

/// How swarm-created agents should be spawned.
///
/// Every mode runs the worker in this process; the agent never opens a terminal
/// window. The mode names how the coordinator surfaces the worker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SwarmSpawnMode {
    /// Create the worker in-process.
    Headless,
    /// Like headless, but the coordinator renders a live inline gallery
    /// viewport of each worker's streaming output.
    #[default]
    #[serde(alias = "visible", alias = "headed")]
    Inline,
    /// Historical "try visible first" mode. Resolves to the configured mode,
    /// which is in-process, so an old config keeps working unchanged.
    Auto,
}

impl SwarmSpawnMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            // `visible`/`headed` selected the removed terminal-window spawn;
            // they now mean in-process like `inline`.
            "visible" | "headed" => Some(Self::Inline),
            "headless" => Some(Self::Headless),
            "inline" => Some(Self::Inline),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    /// Canonical lowercase string for this mode (matches the config/env values).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::Inline => "inline",
            Self::Auto => "auto",
        }
    }
}

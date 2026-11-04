//! `[menubar]` section of the config: the macOS menu bar session indicator.

use serde::{Deserialize, Serialize};

/// macOS menu bar session indicator configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MenubarConfig {
    /// Show the horon-agent session count in the macOS menu bar. When false, horon-agent
    /// never starts the menu bar helper and a running helper exits on its next
    /// refresh. The `HORON_AGENT_NO_MENUBAR` environment variable also disables it.
    /// Default: true.
    pub enabled: bool,
}

impl Default for MenubarConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

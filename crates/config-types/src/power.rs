//! `[power]` section of the config: sleep and lid-close inhibition.

use serde::{Deserialize, Serialize};

/// Power-management configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerConfig {
    /// Prevent automatic system sleep while any horon-agent session is actively
    /// streaming/processing. Linux also asks logind to block lid-switch suspend.
    /// Windows cannot override a user-initiated lid close or power-button action;
    /// those remain controlled by the active Windows power plan. The display is
    /// still allowed to sleep. Default: true.
    ///
    /// Honored by the shared `horon-agent serve` daemon. The `HORON_AGENT_DISABLE_POWER_INHIBIT`
    /// environment variable forces this off regardless of the config value.
    pub prevent_sleep_while_streaming: bool,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            prevent_sleep_while_streaming: true,
        }
    }
}

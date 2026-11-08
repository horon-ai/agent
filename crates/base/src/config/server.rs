//! Daemon-level settings that are not about one session.
//!
//! Split out of `config.rs` so the settings table stays readable and the
//! oversized-file budget for that file does not grow with every new switch.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Ownership model for autonomous wake requests.
    pub wake_mode: WakeMode,
    /// Seconds a session must stay idle before its derived caches are released.
    ///
    /// `0` disables the sweep. The daemon checks every ten seconds, so the
    /// effective release happens up to one tick after the threshold passes.
    /// Only reproducible caches are dropped; the transcript is never touched.
    pub idle_hibernation_secs: u64,
}

/// Default idle window before a session's derived caches are released.
pub const DEFAULT_IDLE_HIBERNATION_SECS: u64 = 1800;

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            wake_mode: WakeMode::default(),
            idle_hibernation_secs: DEFAULT_IDLE_HIBERNATION_SECS,
        }
    }
}

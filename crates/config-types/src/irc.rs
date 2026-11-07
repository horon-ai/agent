//! `[irc]` section of the config: limits for the in-process agent mailbox bus.

use serde::{Deserialize, Serialize};

/// Bounds applied to the in-process IRC mailbox bus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct IrcConfig {
    /// Messages kept per mailbox before the oldest is dropped. Default: 100.
    pub mailbox_cap: usize,
    /// Largest accepted message body in bytes. Default: 65536.
    pub max_body_bytes: usize,
    /// Longest a single `wait` may block, in seconds. Default: 300.
    pub wait_max_secs: u64,
    /// Broadcast legs delivered concurrently. Default: 16.
    pub broadcast_concurrency: usize,
}

impl Default for IrcConfig {
    fn default() -> Self {
        Self {
            mailbox_cap: 100,
            max_body_bytes: 65_536,
            wait_max_secs: 300,
            broadcast_concurrency: 16,
        }
    }
}

impl IrcConfig {
    /// The same limits with every value floored at one.
    ///
    /// A zero cap or concurrency would make the bus drop every message or never
    /// make progress, so the runtime clamps instead of trusting the file.
    pub fn clamped(&self) -> Self {
        Self {
            mailbox_cap: self.mailbox_cap.max(1),
            max_body_bytes: self.max_body_bytes.max(1),
            wait_max_secs: self.wait_max_secs.max(1),
            broadcast_concurrency: self.broadcast_concurrency.max(1),
        }
    }
}

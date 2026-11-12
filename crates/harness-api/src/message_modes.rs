//! `[messages]`: how a daemon delivers messages that arrive while it is busy.
//!
//! The values are daemon configuration, not session state, so a client reads
//! them to explain or debug what it observes. They are reported as the strings
//! the config file uses, so a client never has to map an enum back.

use serde::{Deserialize, Serialize};

/// Delivery policy for messages that arrive while a turn is running.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageModes {
    /// Queued messages the first delivery point of a turn carries.
    /// `all` or `one-at-a-time`.
    pub follow_up_mode: String,
    /// Queued messages each later delivery point of the same turn carries.
    /// `all` or `one-at-a-time`.
    pub steering_mode: String,
    /// Whether a queued message waits for the running tool call (`wait`) or
    /// stops it (`immediate`).
    pub interrupt_mode: String,
}

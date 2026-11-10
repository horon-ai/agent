//! `[messages]`: how messages that arrive while the agent is busy are delivered.

use serde::{Deserialize, Serialize};

/// How many queued messages one delivery carries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MessageDeliveryMode {
    /// Deliver every queued message at the next delivery point (default).
    ///
    /// This is what horon has always done and differs from omp, whose default
    /// is `one-at-a-time`.
    #[default]
    All,
    /// Deliver one queued message per delivery point, oldest first.
    OneAtATime,
}

impl MessageDeliveryMode {
    /// How many messages one delivery moves. `None` means every queued message.
    pub fn batch_limit(self) -> Option<usize> {
        match self {
            Self::All => None,
            Self::OneAtATime => Some(1),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::OneAtATime => "one-at-a-time",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "one-at-a-time" | "one_at_a_time" | "oneatatime" => Some(Self::OneAtATime),
            _ => None,
        }
    }
}

/// Whether a queued message interrupts the tool call that is running now.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum InterruptMode {
    /// Wait for the running tool call before injecting (default).
    ///
    /// A message marked urgent still skips the remaining tool calls, so this is
    /// the timing horon has always had.
    #[default]
    Wait,
    /// Treat every queued message as urgent: stop the remaining tool calls and
    /// inject at the first safe point.
    Immediate,
}

impl InterruptMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Immediate => "immediate",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "wait" => Some(Self::Wait),
            "immediate" => Some(Self::Immediate),
            _ => None,
        }
    }
}

/// Delivery policy for messages that arrive while a turn is running.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct MessagesConfig {
    /// How many queued messages the first delivery point of a turn carries.
    /// Those are left over from the previous turn, so this is omp's
    /// `followUpMode`. `all` (default) or `one-at-a-time`.
    pub follow_up_mode: MessageDeliveryMode,
    /// How many queued messages each later delivery point of the same turn
    /// carries. `all` (default) or `one-at-a-time`.
    pub steering_mode: MessageDeliveryMode,
    /// Whether a queued message waits for the running tool call (`wait`, the
    /// default and horon's existing timing) or stops it (`immediate`).
    pub interrupt_mode: InterruptMode,
}

impl MessagesConfig {
    /// True when a non-urgent queued message still stops the remaining tool
    /// calls, because `interrupt_mode = "immediate"`.
    pub fn interrupts_running_tools(&self) -> bool {
        matches!(self.interrupt_mode, InterruptMode::Immediate)
    }
}

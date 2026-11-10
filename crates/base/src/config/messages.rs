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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(source: &str) -> crate::config::Config {
        toml::from_str(source).expect("config parses")
    }

    #[test]
    fn a_config_without_the_table_keeps_the_shipped_defaults() {
        let messages = config("").messages;

        assert_eq!(messages.follow_up_mode, MessageDeliveryMode::All);
        assert_eq!(messages.steering_mode, MessageDeliveryMode::All);
        assert_eq!(messages.interrupt_mode, InterruptMode::Wait);
        assert!(
            !messages.interrupts_running_tools(),
            "wait is the timing horon already had, so an unconfigured daemon behaves as before"
        );
    }

    #[test]
    fn the_kebab_case_values_parse() {
        let messages = config(
            r#"[messages]
follow_up_mode = "one-at-a-time"
steering_mode = "one-at-a-time"
interrupt_mode = "immediate"
"#,
        )
        .messages;

        assert_eq!(messages.follow_up_mode.batch_limit(), Some(1));
        assert_eq!(messages.steering_mode.batch_limit(), Some(1));
        assert!(messages.interrupts_running_tools());
    }

    #[test]
    fn all_delivers_every_queued_message() {
        assert_eq!(MessageDeliveryMode::All.batch_limit(), None);
        assert_eq!(MessageDeliveryMode::All.as_str(), "all");
        assert_eq!(MessageDeliveryMode::OneAtATime.as_str(), "one-at-a-time");
        assert_eq!(InterruptMode::Wait.as_str(), "wait");
        assert_eq!(InterruptMode::Immediate.as_str(), "immediate");
    }
}

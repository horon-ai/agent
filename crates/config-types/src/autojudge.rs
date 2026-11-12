//! `[autojudge]` section of the config: the semantic judgment that runs before
//! a turn is continued after an unexpected stop.
//!
//! The section is off by default. Its one consumer is turn recovery, which asks
//! the `judge` role a single typed question when the switch is on; with the
//! switch off nothing is requested, so a session pays nothing for the feature
//! merely existing.

use serde::{Deserialize, Serialize};

/// Longest attempt budget a configuration may ask for.
///
/// A judgment holds up the turn it is judging, so an hour-long budget is a typo
/// rather than a request.
pub const MAX_AUTOJUDGE_ATTEMPT_TIMEOUT_MS: u64 = 30_000;

/// Attempt budget a configuration gets when it names none.
///
/// A judgment is one extra request in front of a turn's continuation, so the
/// budget is short enough that the user waits less than they would notice.
pub const DEFAULT_AUTOJUDGE_ATTEMPT_TIMEOUT_MS: u64 = 3_000;

/// Automatic end-of-turn execution judging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoJudgeConfig {
    /// Enable autojudge by default for new and resumed sessions (default:
    /// false).
    pub enabled: bool,
    /// Optional model override for the judgment, ahead of the `judge` role.
    pub model: Option<String>,
    /// Longest a single judgment attempt may take, in milliseconds (default:
    /// 3000).
    ///
    /// A judgment holds up the turn it is judging, so there is no unbounded
    /// value: `0` means the default. A judgment that runs out of budget fails
    /// open, and the caller keeps its own decision.
    pub attempt_timeout_ms: u64,
}

impl AutoJudgeConfig {
    /// The attempt budget to use, with `0` replaced by the default and an absurd
    /// value clamped to [`MAX_AUTOJUDGE_ATTEMPT_TIMEOUT_MS`].
    ///
    /// The result is never `0`: a judgment either finishes inside its budget or
    /// fails open. Waiting without a deadline would stall the turn on a provider
    /// that never answers, which is the failure this budget exists to bound.
    pub fn attempt_timeout_ms(&self) -> u64 {
        match self.attempt_timeout_ms {
            0 => DEFAULT_AUTOJUDGE_ATTEMPT_TIMEOUT_MS,
            asked => asked.min(MAX_AUTOJUDGE_ATTEMPT_TIMEOUT_MS),
        }
    }
}

impl Default for AutoJudgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: None,
            attempt_timeout_ms: DEFAULT_AUTOJUDGE_ATTEMPT_TIMEOUT_MS,
        }
    }
}

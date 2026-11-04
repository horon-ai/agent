//! Bounded automatic continuation after a turn stops in a way the harness did
//! not expect.
//!
//! The classifier in `horon_agent_agent_runtime::turn_recovery` is pure over the
//! provider stop reason and the final assistant text. This module owns the part
//! that cannot be pure: deciding whether the stop is a real provider stop at
//! all, spending a bounded continuation budget, and writing the synthetic nudge
//! into the transcript. Both turn loops call it from the same place, so the two
//! mechanisms cannot drift.

use horon_agent_agent_runtime::turn_recovery::{MAX_AUTO_CONTINUATIONS, RecoveryClass, RecoveryMode, classify};

use super::*;

/// Stable opening of the nudge this module appends. Tests and operators grep for
/// it, and the loops never produce it any other way.
///
/// It deliberately does not open with `<system-reminder>`:
/// [`Agent::messages_end_with_tool_result`] treats that opening as evidence that
/// tool results are in play, so a nudge carrying it would make the
/// empty-post-tool continuation fire on the following response.
pub(crate) const TURN_RECOVERY_NUDGE_PREFIX: &str = "[System reminder: your previous response ended unexpectedly";

/// Stable opening of the terminal notice written once the continuation budget
/// is spent.
pub(crate) const TURN_RECOVERY_EXHAUSTED_PREFIX: &str = "Automatic turn recovery was exhausted";

/// What the caller should do after the automatic-continuation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoContinuation {
    /// One nudge is in the transcript; re-issue the request.
    Continued,
    /// The budget is spent. The notice is already in the transcript, and the
    /// caller ends the turn with it.
    Exhausted(String),
}

/// Provider stop tokens that mean the response genuinely ended.
///
/// Matching is by whole token, never by substring: the classifier's own
/// substring matching would otherwise read an error string such as
/// `"upstream error: budget exceeded"` as a truncation and continue a turn that
/// actually failed.
const CLEAN_STOP_TOKENS: &[&str] = &["stop", "end_turn", "end", "stop_sequence", "completed", "eos"];

/// Provider stop tokens that report the response was cut off by a length or
/// token budget.
const TRUNCATED_STOP_TOKENS: &[&str] = &["length", "max_tokens", "max_output_tokens", "truncated", "incomplete"];

impl Agent {
    /// The configured recovery mode, mapped onto the runtime classifier's mode.
    pub(super) fn configured_recovery_mode() -> RecoveryMode {
        match crate::config::config().features.turn_recovery {
            crate::config::TurnRecoveryMode::Off => RecoveryMode::Off,
            crate::config::TurnRecoveryMode::Conservative => RecoveryMode::Conservative,
            crate::config::TurnRecoveryMode::Aggressive => RecoveryMode::Aggressive,
        }
    }

    /// The normalized stop token when the provider reported a stop this module
    /// understands, and `None` for anything else (a missing reason, a refusal, a
    /// paused server-tool turn, a stranded `tool_use`, or an error message that
    /// merely mentions a token name).
    ///
    /// This is the mandatory gate: the classifier is only ever handed a string
    /// that this function recognized as an actual provider stop variant.
    fn recovery_stop_token(stop_reason: Option<&str>) -> Option<String> {
        let token = stop_reason?.trim().to_ascii_lowercase();
        if token.is_empty() || token.split_whitespace().count() != 1 {
            return None;
        }
        // Refusals are deliberate and paused turns resume without a synthetic
        // message, so neither may take the nudge path.
        if Self::is_guardrail_stop_reason(Some(&token)) || Self::is_pause_turn_stop_reason(&token) {
            return None;
        }
        let known = CLEAN_STOP_TOKENS.contains(&token.as_str()) || TRUNCATED_STOP_TOKENS.contains(&token.as_str());
        known.then_some(token)
    }

    /// The detected reason in one clause, for the nudge and the notice.
    fn recovery_reason_label(class: RecoveryClass) -> &'static str {
        match class {
            RecoveryClass::BudgetTruncated => "the provider's output budget truncated the response",
            RecoveryClass::UnclosedStructure => "the final message leaves a code fence or list item open",
            RecoveryClass::SemanticPrematureStop => "the final sentence announces work that never started",
            RecoveryClass::CleanStop => "the response ended cleanly",
        }
    }

    /// Classify a finished, tool-call-free stop and, when the configured mode
    /// acts on it, append one synthetic continue-nudge and ask the caller to
    /// re-issue the request.
    ///
    /// Returns `None` when nothing should happen, so the caller keeps its
    /// existing recovery chain untouched: a clean stop, an unrecognized stop
    /// reason, a refusal, an abort, or a mode that does not act on this class.
    ///
    /// Callers must already have established that the turn actually finished
    /// (not aborted) and carried no tool calls. The spent budget lives in
    /// [`TurnState`](horon_agent_agent_runtime::TurnState), so a loop never
    /// keeps a counter of its own.
    pub(super) fn maybe_auto_continue_unexpected_stop(
        &mut self,
        stop_reason: Option<&str>,
        final_text: &str,
    ) -> Result<Option<AutoContinuation>> {
        if self.is_graceful_shutdown() {
            return Ok(None);
        }
        let mode = Self::configured_recovery_mode();
        if mode == RecoveryMode::Off {
            return Ok(None);
        }
        let Some(token) = Self::recovery_stop_token(stop_reason) else {
            return Ok(None);
        };
        let class = classify(&token, final_text);
        if class.is_clean() || !mode.acts_on(class) {
            return Ok(None);
        }

        let label = Self::recovery_reason_label(class);

        if self.turn_state.auto_continuations() >= MAX_AUTO_CONTINUATIONS {
            let attempts = self.turn_state.auto_continuations();
            let notice = format!(
                "{TURN_RECOVERY_EXHAUSTED_PREFIX} after {MAX_AUTO_CONTINUATIONS} automatic continuations (last detected: {label}). The turn ended with possibly incomplete output."
            );
            logging::warn(&format!(
                "Turn recovery exhausted after {} continuations (class={:?}, stop_reason={}); ending turn",
                attempts, class, token
            ));
            self.record_turn_recovery_notice(&notice);
            self.save_session()?;
            return Ok(Some(AutoContinuation::Exhausted(notice)));
        }

        let attempts = self.turn_state.record_auto_continuation();
        logging::warn(&format!(
            "Unexpected stop classified as {:?} (stop_reason={}); requesting automatic continuation {}/{}",
            class, token, attempts, MAX_AUTO_CONTINUATIONS
        ));
        self.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!(
                    "{TURN_RECOVERY_NUDGE_PREFIX} (detected: {label}; stop_reason: {token}). Continue exactly where you left off, do not repeat completed content, and if the next step is a tool call, emit the tool call now.]"
                ),
                cache_control: None,
            }],
        );
        self.save_session()?;
        Ok(Some(AutoContinuation::Continued))
    }

    /// Store the terminal notice the exhausted budget leaves behind.
    ///
    /// The notice is an assistant-role text message displayed as a system
    /// notice, so the model reads it on the next turn and the user reads it in
    /// the transcript.
    pub(super) fn record_turn_recovery_notice(&mut self, notice: &str) {
        self.add_message_with_display_role(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: notice.to_string(),
                cache_control: None,
            }],
            Some(crate::session::StoredDisplayRole::System),
        );
    }
}

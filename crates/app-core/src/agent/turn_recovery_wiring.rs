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
use horon_agent_provider_core::judge::{JudgeItem, JudgeQuestion, JudgeRequest};

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

/// How much probability a judgment needs before the semantic class is acted on.
///
/// `0.5` is where the judge's own prompt says it does not know, so a tie is not
/// a reason to continue: the turn keeps the mode's own decision, exactly as a
/// failed judgment does. Every value at or above it, from an uncalibrated
/// model, is compared once and never reweighted; the threshold is a policy
/// choice, not a claim about the model's calibration.
const JUDGED_STOP_ACT_THRESHOLD: f32 = 0.5;

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
    /// The semantic class is the one place the classifier only guesses: it reads
    /// the last sentence and matches an announcement pattern. When `[autojudge]`
    /// is enabled this asks the `judge` role for a typed second opinion on that
    /// class alone, and acts on the answer; every other class is deterministic
    /// and never judged. A judgment that is unavailable, fails, times out or
    /// answers with an unusable probability leaves the deterministic decision in
    /// place, so a broken judge can only cost a request, never change a turn.
    ///
    /// Because the switch itself is off by default, enabling it is what extends
    /// `conservative` to the semantic class; that reach is stated in
    /// `docs/runtime/TURNS.md` rather than implied. A judgment still runs on the
    /// stop that spends the last continuation, because its answer decides
    /// whether the terminal notice claims output may be missing.
    ///
    /// Callers must already have established that the turn actually finished
    /// (not aborted) and carried no tool calls. The spent budget lives in
    /// [`TurnState`](horon_agent_agent_runtime::TurnState), so a loop never
    /// keeps a counter of its own.
    pub(super) async fn maybe_auto_continue_unexpected_stop(
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
        if class.is_clean() {
            return Ok(None);
        }
        let mut acts = mode.acts_on(class);
        // A judgment is spent only where its answer can still change the turn:
        // it can grant a continuation, or, once the budget is gone, it can still
        // decide whether the terminal notice is recorded. A mode that neither
        // acts on this class nor is widened by the judge is not asked at all.
        // The bound is one judgment per unexpected stop, and the turn ends
        // immediately after the notice, so a run spends at most
        // `MAX_AUTO_CONTINUATIONS + 1`.
        if class == RecoveryClass::SemanticPrematureStop
            && (acts || Self::autojudge_widens_into(mode))
            && let Some(judged) = self.judged_semantic_stop(final_text).await
        {
            acts = judged;
        }
        if !acts {
            return Ok(None);
        }
        let budget_left = self.turn_state.auto_continuations() < MAX_AUTO_CONTINUATIONS;

        let label = Self::recovery_reason_label(class);

        if !budget_left {
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

    /// Whether enabling the judge extends `mode` to the semantic class.
    ///
    /// `aggressive` already acts on every non-clean class, so a judgment there
    /// only confirms or withdraws that decision. `conservative` deliberately
    /// leaves the semantic guess alone; turning the judge on is the one thing
    /// that changes that, and only because the answer comes from a model rather
    /// than a sentence pattern.
    fn autojudge_widens_into(mode: RecoveryMode) -> bool {
        mode == RecoveryMode::Conservative
    }

    /// Ask the `judge` role whether a semantic stop was premature.
    ///
    /// `Some(true)` means the judge is confident enough to act on the class,
    /// `Some(false)` means it is not, and `None` means no judgment was obtained
    /// and the caller keeps its deterministic decision. Every path that returns
    /// `None` is a fail-open path: the setting is off, no judge model resolves,
    /// the request cannot be built, or the attempt failed.
    ///
    /// This never retries. A judgment gates a turn that is already waiting, so a
    /// second attempt spends the user's time on the same coin flip.
    async fn judged_semantic_stop(&mut self, final_text: &str) -> Option<bool> {
        let config = crate::config::config();
        let enabled = self.autojudge_enabled().unwrap_or(config.autojudge.enabled);
        if !enabled {
            return None;
        }
        let target = match horon_agent_base::judge::judge_target_for_session(&self.session, &config.autojudge) {
            Ok(target) => target,
            Err(error) => {
                crate::logging::event_rate_limited(
                    crate::logging::LogLevel::Info,
                    "turn_recovery_judge_unavailable",
                    std::time::Duration::from_secs(300),
                    "TURN_RECOVERY_JUDGE_UNAVAILABLE",
                    vec![("reason", error.to_string())],
                );
                return None;
            }
        };
        let last_user = self.last_user_text();
        let state = horon_agent_base::judge::turn_recovery_state(&last_user, final_text);
        let request = JudgeRequest::new(
            state,
            vec![JudgeItem::new(
                "premature_stop",
                JudgeQuestion::boolean(
                    "The assistant announced work and then ended its turn with no tool call. \
                     Did it stop before finishing the work the user asked for?",
                ),
            )],
        )
        .inspect_err(|error| {
            crate::logging::event_rate_limited(
                crate::logging::LogLevel::Warn,
                "turn_recovery_judge_failed",
                std::time::Duration::from_secs(60),
                "TURN_RECOVERY_JUDGE_FAILED",
                vec![("reason", error.to_string())],
            );
        });
        let Ok(request) = request else {
            return None;
        };

        match horon_agent_base::judge::judge_with(
            &self.provider,
            &target.model,
            config.autojudge.attempt_timeout_ms(),
            &request,
        )
        .await
        {
            Ok(outcome) => {
                let probability = outcome.answers.probability("premature_stop");
                crate::logging::event_info(
                    "TURN_RECOVERY_JUDGE_ANSWER",
                    vec![
                        ("model", outcome.model),
                        (
                            "probability",
                            probability.map_or_else(String::new, |p| format!("{p:.3}")),
                        ),
                        ("elapsed_ms", outcome.elapsed.as_millis().to_string()),
                        (
                            "output_tokens",
                            outcome
                                .usage
                                .output_tokens
                                .map_or_else(String::new, |tokens| tokens.to_string()),
                        ),
                    ],
                );
                probability.map(|probability| probability >= JUDGED_STOP_ACT_THRESHOLD)
            }
            Err(error) => {
                crate::logging::event_rate_limited(
                    crate::logging::LogLevel::Warn,
                    "turn_recovery_judge_failed",
                    std::time::Duration::from_secs(60),
                    "TURN_RECOVERY_JUDGE_FAILED",
                    vec![("reason", error.to_string()), ("model", target.model)],
                );
                None
            }
        }
    }

    /// The most recent user text in the active context, excluding the synthetic
    /// recovery nudges this module appends.
    fn last_user_text(&self) -> String {
        self.session
            .active_context()
            .iter()
            .rev()
            .filter(|message| message.role == Role::User)
            .map(|message| {
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .find(|text| !text.trim().is_empty() && !text.starts_with(TURN_RECOVERY_NUDGE_PREFIX))
            .map_or_else(String::new, String::from)
    }
}

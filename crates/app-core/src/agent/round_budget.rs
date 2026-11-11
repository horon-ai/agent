//! Round-owned context budget for one tool round.
//!
//! The tool layer refuses an oversized output with
//! `CompactionManager::effective_token_count()` (`tool/mod.rs`), and that count
//! does not move while a round is being applied: its message estimate is
//! computed with ZERO messages (`base/src/compaction.rs`) and its observed
//! count only updates after a provider response. Every member of a round
//! therefore sees the count from BEFORE the round, so N outputs can jointly
//! overflow the window even though each one passed the per-output guard.
//!
//! This counter is seeded once at round start from the agent's own compaction
//! manager and the session's messages, then advanced by every output the round
//! actually applies. Both turn loops (`turn_loops` and `turn_streaming_mpsc`)
//! use this one type, so the rule cannot drift between them, and it is never
//! re-seeded between the groups of one round: re-seeding per group is exactly
//! the stale-state bug it exists to prevent.
//!
//! The round applies the tool layer's own decision rule, not a second policy.
//! That rule has two ceilings - the cumulative one for the round and the
//! single-output one for any one result - and both apply here: an output that is
//! past the single-output ceiling is bounded even when the round has room for
//! it, because the identical tool result would have been. In particular
//! `accept_large_output` means the same thing in both places: a caller who asked
//! to pay receives the prefix that fits, truncated, rather than a refusal that
//! the same call would not have received on its own. The flag is read from the
//! output, where the tool layer recorded it, because the round never sees the
//! call's input.
//!
//! An SDK-answered call has no tool layer in the path at all: the provider ran
//! the tool and streamed the result back, so nothing marked an opt-in and
//! nothing capped the payload beyond the protocol's 512K-character ceiling. That
//! result still lands in history and still costs the window, so it is admitted
//! through [`RoundTokenBudget::admit_sdk_result`] like any other applied output.
//! The one thing the round has to supply itself is the opt-in, which it reads
//! from the call's input - the same key the tool layer reads - so the refusal's
//! advice stays true for this path too. An error result is the one case that is
//! admitted intact: a failure sentence is a verdict the model must read, and
//! replacing it with a context refusal would hide the failure while still
//! spending the round's room.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;

use crate::compaction::CompactionManager;
use crate::message::Message;
use crate::tool::{
    CONTEXT_LIMIT_REFUSAL_PREFIX, OUTPUT_TRUNCATED_MARKER, Registry, ToolOutput, ToolVerdict, is_context_refusal,
};

pub(super) struct RoundTokenBudget {
    /// `0` disables the guard, the same way `Registry::guard_context_overflow`
    /// treats it.
    budget: usize,
    /// The tool layer computes this ceiling, so the two cannot disagree.
    threshold_tokens: usize,
    seed_tokens: usize,
    applied_tokens: usize,
}

impl RoundTokenBudget {
    /// Seed at round start from the AGENT's compaction manager.
    ///
    /// The registry a group executes with is a clone that shares this manager
    /// (`clone_sharing_compaction`), so seeding from the agent's own handle is
    /// what keeps the round and the tool layer reading the same budget.
    pub(super) async fn seed(compaction: &Arc<RwLock<CompactionManager>>, messages: &[Message]) -> Self {
        let manager = compaction.read().await;
        let budget = manager.token_budget();
        let seed_tokens = manager.effective_token_count_with(messages);
        Self::with_seed(budget, seed_tokens)
    }

    fn with_seed(budget: usize, seed_tokens: usize) -> Self {
        Self {
            budget,
            threshold_tokens: Registry::context_guard_threshold_tokens(budget),
            seed_tokens,
            applied_tokens: 0,
        }
    }

    /// Admit an SDK-provided result as this round's applied output.
    ///
    /// The provider executed the call, so no tool layer ran and nothing capped
    /// the payload or recorded an opt-in. Both are supplied here: the protocol
    /// ceiling is applied first (it protects history and the prompt cache, and
    /// it is cheap), then the round's own rule decides what actually fits.
    ///
    /// `call_input` is read for `accept_large_output` because the tool layer is
    /// not in this path to mark the output, and the model may have passed the
    /// flag on the call it made. Without it the round would withhold a payload
    /// and tell the caller to retry with a flag the same call already carried.
    ///
    /// `is_error` bypasses the round's decision, as the executed path's error
    /// arm does - a failure sentence is a verdict the model must read to act,
    /// not content it chose to spend, so replacing it with a context refusal
    /// would hide the failure. Unlike that arm it is still counted, so the
    /// round's running total cannot drift below what it has actually applied.
    /// The protocol ceiling still applies first, so an error result is bounded
    /// even when it is long.
    pub(super) fn admit_sdk_result(
        &mut self,
        session_id: &str,
        tool_name: &str,
        call_input: &Value,
        content: String,
        is_error: bool,
    ) -> String {
        let content = super::cap_sdk_tool_content_for_history(tool_name, session_id, content);
        if is_error {
            self.applied_tokens += crate::util::estimate_tokens(&content);
            return content;
        }
        let mut output = ToolOutput::new(content);
        if sdk_caller_opted_in(call_input) {
            output.metadata = Some(serde_json::json!({ crate::tool::ACCEPT_LARGE_OUTPUT_METADATA_KEY: true }));
        }
        self.admit(tool_name, output).output
    }

    /// The seed plus every output applied since; the number the next
    /// [`Self::admit`] compares against the threshold.
    pub(super) fn current_tokens(&self) -> usize {
        self.seed_tokens + self.applied_tokens
    }

    /// Admit one output into the round, or apply the tool layer's own
    /// decision at the round's running total.
    ///
    /// The comparison is the tool layer's own, run against the round's running
    /// total instead of the frozen count the tool layer sees. The rule is the
    /// same one `Registry::guard_context_overflow` applies, in the same order:
    ///
    /// 1. nothing to decide (`projected <= threshold` AND this output is within
    ///    the single-output ceiling): admit. Landing exactly on the ceiling is
    ///    admitted, exactly as it is outside a round.
    /// 2. the window is already spent (`current >= threshold`): refuse with the
    ///    tool layer's exhausted sentence. No room is left, so opting in cannot
    ///    buy anything and that sentence is accurate here.
    /// 3. room remains but this output does not fit whole: whichever ceiling is
    ///    stricter for it - the round's remaining room or the single-output
    ///    ceiling - sets how much can be returned. A caller who passed
    ///    `accept_large_output` gets the prefix that fits, with the tool layer's
    ///    truncation notice, exactly as it would when the call runs alone; a
    ///    caller who did not is withheld and told the flag exists.
    ///
    /// The single-output ceiling is not a rule for rounds that are near their
    /// ceiling: a payload well inside the round can still be larger than any
    /// one output should be, and the tool layer bounds it there too. Leaving it
    /// out of step 1 admitted a payload the identical tool result had withheld.
    ///
    /// The flag is read from the output because the tool layer recorded it
    /// there; without it the round would have to guess, and guessing is what
    /// made the same call return content alone and a refusal in a group.
    pub(super) fn admit(&mut self, tool_name: &str, output: ToolOutput) -> ToolOutput {
        if self.budget == 0 {
            return output;
        }

        // An output the tool layer already refused is a verdict, not content:
        // it carries no payload, and re-deciding it at the round's total either
        // truncated the actionable sentence or restated it with the round's
        // numbers. Admit it as it stands and count what it costs.
        if is_context_refusal(&output) {
            self.applied_tokens += crate::util::estimate_tokens(&output.output);
            return output;
        }

        let current_tokens = self.current_tokens();
        let output_tokens = crate::util::estimate_tokens(&output.output);
        let projected_tokens = current_tokens.saturating_add(output_tokens);
        let single_max_tokens = Registry::single_output_max_tokens(self.budget);

        // The tool layer's own predicate (`needs_truncation`): it acts when the
        // running total would cross the ceiling OR when this one output is past
        // the single-output ceiling. The second half is not implied by the
        // first, and that is the whole point - a round with room still refuses
        // an output no single result is allowed to be.
        if projected_tokens <= self.threshold_tokens && output_tokens <= single_max_tokens {
            self.applied_tokens += output_tokens;
            return output;
        }

        if current_tokens >= self.threshold_tokens {
            crate::logging::info(&format!(
                "Round context guard: refused {} output of ~{}k tokens, round context exhausted \
                 ({}k/{}k tokens, threshold {}k)",
                tool_name,
                output_tokens / 1000,
                current_tokens / 1000,
                self.budget / 1000,
                self.threshold_tokens / 1000,
            ));
            let refusal = Registry::exhausted_context_refusal(output_tokens, current_tokens, self.budget);
            self.applied_tokens += crate::util::estimate_tokens(&refusal);
            return ToolOutput {
                output: refusal,
                verdict: Some(ToolVerdict::Withheld),
                ..output
            };
        }

        // Room remains. Whichever ceiling is stricter for this output decides
        // what fits: the room the round has left, or the most any single output
        // may take. The tool layer computes the same minimum, so the same call
        // cannot be truncated alone and withheld in a group.
        let remaining_tokens = self.threshold_tokens - current_tokens;
        let max_tokens = remaining_tokens.min(single_max_tokens);
        // Which of the two produced that minimum, so the refusal names the real
        // reason: the round's spent window, or this output's own size.
        let ceiling_bound = single_max_tokens < remaining_tokens;

        if !Registry::accept_large_output_was_passed(&output) {
            let refusal = if ceiling_bound {
                crate::logging::info(&format!(
                    "Round context guard: withheld {} output of ~{}k tokens, past the \
                     single-output ceiling of {}k ({}k/{}k tokens, threshold {}k)",
                    tool_name,
                    output_tokens / 1000,
                    single_max_tokens / 1000,
                    current_tokens / 1000,
                    self.budget / 1000,
                    self.threshold_tokens / 1000,
                ));
                Registry::oversized_output_refusal(output_tokens, max_tokens, current_tokens, self.budget)
            } else {
                crate::logging::info(&format!(
                    "Round context guard: withheld {} output of ~{}k tokens, round near its ceiling \
                     ({}k/{}k tokens, threshold {}k)",
                    tool_name,
                    output_tokens / 1000,
                    current_tokens / 1000,
                    self.budget / 1000,
                    self.threshold_tokens / 1000,
                ));
                Self::withheld_output_refusal(output_tokens, max_tokens, current_tokens, self.budget)
            };
            self.applied_tokens += crate::util::estimate_tokens(&refusal);
            return ToolOutput {
                output: refusal,
                verdict: Some(ToolVerdict::Withheld),
                ..output
            };
        }

        // The caller asked to pay, so return what fits. A notice the tool layer
        // already appended is replaced, never stacked.
        let body = match output.output.find(OUTPUT_TRUNCATED_MARKER) {
            Some(notice_at) => &output.output[..notice_at],
            None => output.output.as_str(),
        };
        let kept = crate::util::truncate_str(body, max_tokens.saturating_mul(4).saturating_sub(150));
        let truncated = format!(
            "{}\n\n{}",
            kept,
            Registry::truncated_output_notice(output_tokens, max_tokens, current_tokens, self.budget),
        );
        crate::logging::info(&format!(
            "Round context guard: truncated {} output from ~{}k to ~{}k tokens \
             ({}k/{}k tokens, threshold {}k)",
            tool_name,
            output_tokens / 1000,
            max_tokens / 1000,
            current_tokens / 1000,
            self.budget / 1000,
            self.threshold_tokens / 1000,
        ));
        self.applied_tokens += crate::util::estimate_tokens(&truncated);
        ToolOutput {
            output: truncated,
            verdict: Some(ToolVerdict::after_cut(output.verdict)),
            ..output
        }
    }

    /// Message returned instead of an output the round's own remaining room
    /// could not absorb when its caller did not pass `accept_large_output`.
    ///
    /// Used when the spent window is the binding constraint. When the output's
    /// own size is the binding constraint, the round prints the tool layer's
    /// `oversized_output_refusal` instead, because there the round has room and
    /// the reason is the payload, not the round's total.
    ///
    /// The tool layer's exhausted sentence claims the flag cannot help, which is
    /// only true once the window is spent. Here room remains, and the flag buys
    /// the prefix that fits in the next round, so the sentence names it instead
    /// of denying it. The ceiling and the `/compact` remedy stay.
    fn withheld_output_refusal(
        output_tokens: usize,
        affordable_tokens: usize,
        current_tokens: usize,
        budget: usize,
    ) -> String {
        format!(
            "{CONTEXT_LIMIT_REFUSAL_PREFIX} cannot return this tool output (~{:.0}k tokens) \
             because this round has already spent the context window ({:.0}k/{}k tokens \
             were used before it). Nothing was added to the context except this message. \
             Use /compact to free space, narrow the request, or repeat the same call with \
             `\"accept_large_output\": true` to receive the first ~{:.0}k tokens that still fit.",
            output_tokens as f32 / 1000.0,
            current_tokens as f32 / 1000.0,
            budget / 1000,
            affordable_tokens as f32 / 1000.0,
        )
    }
}

/// Whether the call that produced an SDK-answered result passed
/// `accept_large_output`.
///
/// The tool layer marks an output when its caller opted in, and the round reads
/// that mark. An SDK-answered call never reaches the tool layer, so the mark is
/// missing on every one of them and the round has to read the call's own input
/// with the same key and the same accepted spellings. Keeping the key in one
/// place means the flag cannot mean two things on the two paths.
fn sdk_caller_opted_in(call_input: &Value) -> bool {
    match call_input.get(horon_agent_tool_core::ACCEPT_LARGE_OUTPUT_KEY) {
        Some(Value::Bool(accepted)) => *accepted,
        Some(Value::String(raw)) => raw.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

#[cfg(test)]
#[path = "round_budget_tests.rs"]
mod tests;

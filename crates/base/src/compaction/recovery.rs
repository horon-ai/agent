//! Compaction failure recovery (plan D8).
//!
//! A summary request can fail, time out, or come back with nothing usable. This
//! module owns what happens next, and it deliberately keeps three things apart:
//!
//! * **Not a success.** An empty response, a length notice, or a native
//!   fallback that carries neither a replayable window nor real text is never
//!   adopted as a summary. [`is_usable_summary_text`] is the single gate.
//! * **A bounded, deterministic fallback.** When the previous valid summary,
//!   the newest user instruction and the complete recent tool chain fit the
//!   budget, [`plan_lossy_fallback`] builds the replacement without another
//!   provider call. The result is labelled lossy everywhere it is surfaced.
//! * **A durable refusal.** When no safe suffix fits, the attempt is recorded as
//!   blocked for one exact source/provider/model/settings identity
//!   ([`RecoveryFingerprint`]) so automatic repetition stops instead of burning
//!   provider calls on the same doomed input.
//!
//! Originals are never mutated here: the plan only describes which suffix stays
//! live, and the manager applies it to its own bookkeeping.

use super::source_projection::{CoveredRange, SourceProjection};
use super::summary::{
    MAX_RESTORATION_BYTES, MAX_RESTORATION_ENTRIES, RestorationEntry, RestorationManifest,
    collect_restoration_with_remaining,
};
use crate::message::{ContentBlock, Message, Role};
use horon_agent_compaction_core::{CHARS_PER_TOKEN_UPPER_BOUND, estimate_compaction_tokens_from_text};
use horon_agent_provider_core::NativeCompactionResult;

/// Marker phrase that identifies a summary produced by the deterministic
/// fallback. Consumers can detect a lossy summary from the text alone.
pub const LOSSY_EMERGENCY_MARKER: &str = "## Recovery notice";

/// Longest text that can still be classified as a bare length notice. A real
/// summary that mentions a context limit does so inside a longer document.
const MAX_LENGTH_NOTICE_CHARS: usize = 800;

/// Phrases a provider uses to say "your input did not fit". They describe a
/// failure, never a summary.
const LENGTH_NOTICE_MARKERS: &[&str] = &[
    "context length",
    "context window",
    "context_length_exceeded",
    "maximum context",
    "max context",
    "prompt is too long",
    "input is too long",
    "too many tokens",
    "token limit",
    "request too large",
    "reduce the length",
    "conversation is too long",
    "conversation too long",
    "truncated to fit",
];

/// Longest excerpt kept for a task-state reminder.
const TASK_REMINDER_CHARS: usize = 200;

/// Why a deterministic fallback could not be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryBlockReason {
    /// No usable previous summary to anchor the resumed work.
    NoPriorSummary,
    /// The active slice has no user instruction (only tool traffic or nothing).
    NoNewestUser,
    /// A tool call or tool result in the kept suffix has no partner.
    OrphanToolPair,
    /// The newest user instruction plus the complete tool chain exceed budget.
    TailExceedsBudget,
    /// There is nothing left to compact.
    NothingToCover,
}

impl RecoveryBlockReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoPriorSummary => "no_prior_summary",
            Self::NoNewestUser => "no_newest_user",
            Self::OrphanToolPair => "orphan_tool_pair",
            Self::TailExceedsBudget => "tail_exceeds_budget",
            Self::NothingToCover => "nothing_to_cover",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "no_prior_summary" => Some(Self::NoPriorSummary),
            "no_newest_user" => Some(Self::NoNewestUser),
            "orphan_tool_pair" => Some(Self::OrphanToolPair),
            "tail_exceeds_budget" => Some(Self::TailExceedsBudget),
            "nothing_to_cover" => Some(Self::NothingToCover),
            _ => None,
        }
    }
}

impl std::fmt::Display for RecoveryBlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identity a refused recovery applies to.
///
/// Every field must match for a repeat to be refused. A new message changes
/// `source_digest`; a route or settings change changes the other fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryFingerprint {
    pub source_digest: String,
    pub provider: String,
    pub model: String,
    pub settings_digest: String,
}

/// A refused automatic recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryBlocked {
    pub fingerprint: RecoveryFingerprint,
    pub reason: RecoveryBlockReason,
    pub detail: String,
}

/// What the deterministic fallback kept and dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossyEmergencyReport {
    /// Messages summarized away (moved out of the provider projection).
    pub covered_messages: usize,
    /// Messages that stay live verbatim.
    pub kept_messages: usize,
    /// Estimated tokens of the whole post-recovery projection.
    pub tail_tokens: usize,
    /// Tokens the kept tail was allowed to use.
    pub budget_tokens: usize,
    pub summary_chars: usize,
    pub restoration_entries: usize,
}

/// The decision one recovery attempt produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// The deterministic fallback ran; context is lossy but resumable.
    Applied(LossyEmergencyReport),
    /// No safe suffix fits; automatic repetition is refused for this identity.
    Blocked(RecoveryBlocked),
    /// The same identity was already refused, so nothing was attempted.
    Refused(RecoveryBlocked),
}

impl RecoveryOutcome {
    pub fn applied(&self) -> bool {
        matches!(self, Self::Applied(_))
    }

    pub fn report(&self) -> Option<&LossyEmergencyReport> {
        match self {
            Self::Applied(report) => Some(report),
            _ => None,
        }
    }

    pub fn blocked(&self) -> Option<&RecoveryBlocked> {
        match self {
            Self::Blocked(blocked) | Self::Refused(blocked) => Some(blocked),
            Self::Applied(_) => None,
        }
    }
}

/// One planned deterministic fallback.
#[derive(Clone, Debug)]
pub struct LossyPlan {
    /// Index in the active slice where the kept suffix begins.
    pub keep_from: usize,
    pub summary_text: String,
    pub restoration: RestorationManifest,
    pub tail_tokens: usize,
    pub budget_tokens: usize,
}

/// Whether `text` is only a provider length notice.
///
/// A notice is short, has no summary structure, and names an overflow. A real
/// summary that happens to mention the context limit carries section headings or
/// is far longer than any notice, so it is not caught here.
pub fn is_length_notice(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_LENGTH_NOTICE_CHARS {
        return false;
    }
    if trimmed.contains("## ") || trimmed.starts_with('{') || trimmed.starts_with('[') {
        return false;
    }
    let lowered = trimmed.to_ascii_lowercase();
    LENGTH_NOTICE_MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Whether `text` can stand as a summary.
pub fn is_usable_summary_text(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && !is_length_notice(trimmed)
}

/// Whether a provider-native fallback may be adopted.
///
/// A replayable native window is valid even with an empty text summary (D9); a
/// text-only fallback must be a real summary, not an empty string or a notice.
pub fn native_fallback_is_usable(native: &NativeCompactionResult, window_sendable: bool) -> bool {
    window_sendable || native.summary_text.as_deref().is_some_and(is_usable_summary_text)
}

/// Whether a finished compaction result may be applied.
///
/// Oversized native state is not replayable, so it cannot carry a result on its
/// own; the text summary has to be real in that case.
pub fn result_is_applicable(summary_text: &str, encrypted_content: Option<&str>, window_sendable: bool) -> bool {
    (encrypted_content.is_some() && window_sendable) || is_usable_summary_text(summary_text)
}

/// A user message that carries an instruction rather than tool output or an
/// injected reminder.
fn is_user_instruction(message: &Message) -> bool {
    if message.role != Role::User {
        return false;
    }
    let mut saw_text = false;
    for block in &message.content {
        match block {
            ContentBlock::ToolResult { .. } => return false,
            ContentBlock::Text { text, .. } => {
                let trimmed = text.trim_start();
                if !trimmed.starts_with("<system-reminder>") && !trimmed.is_empty() {
                    saw_text = true;
                }
            }
            _ => {}
        }
    }
    saw_text
}

/// Whether the message participates in a tool call/result exchange.
fn is_tool_exchange(message: &Message) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }))
}

/// Check that the suffix is a complete tool chain: every call has its result and
/// every result has its call, both inside the suffix.
fn complete_tool_chain(suffix: &[Message]) -> Result<(), (RecoveryBlockReason, String)> {
    let mut calls: Vec<&str> = Vec::new();
    let mut results: Vec<&str> = Vec::new();
    for message in suffix {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => calls.push(id.as_str()),
                ContentBlock::ToolResult { tool_use_id, .. } => results.push(tool_use_id.as_str()),
                _ => {}
            }
        }
    }
    let unmatched_calls: Vec<&&str> = calls.iter().filter(|id| !results.contains(id)).collect();
    let unmatched_results: Vec<&&str> = results.iter().filter(|id| !calls.contains(id)).collect();
    if unmatched_calls.is_empty() && unmatched_results.is_empty() {
        return Ok(());
    }
    Err((
        RecoveryBlockReason::OrphanToolPair,
        format!(
            "kept suffix has {} tool call(s) without a result and {} result(s) without a call",
            unmatched_calls.len(),
            unmatched_results.len()
        ),
    ))
}

/// The deterministic fallback summary text.
///
/// Built only from the previous summary, the failure class, and the dropped
/// count, so the same failure always produces the same text.
fn lossy_summary_text(prior: &str, failure: &str, covered_messages: usize) -> String {
    format!(
        "{}\n\n{LOSSY_EMERGENCY_MARKER}\n\
         The summary request failed ({failure}). {covered_messages} earlier message(s) were \
         compacted without a fresh summary, so this summary is incomplete. The live tail keeps \
         the newest user instruction and the complete recent tool chain; re-read the restoration \
         references and the newest user message before continuing.",
        prior.trim()
    )
}

/// First line of the newest user instruction, bounded for a task reminder.
fn task_reminder(message: &Message) -> String {
    let text = message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    let line = text.lines().map(str::trim).find(|line| !line.is_empty());
    let line = line.unwrap_or_default();
    line.chars().take(TASK_REMINDER_CHARS).collect()
}

/// Plan the deterministic fallback for a failed summary.
///
/// All three ingredients must be present and fit the budget: a usable previous
/// summary, the newest user instruction, and a complete recent tool chain. Any
/// missing ingredient is a refusal, not a degraded summary.
pub fn plan_lossy_fallback(
    active: &[Message],
    prior_summary: Option<&str>,
    failure: &str,
    budget_tokens: usize,
    context_window: usize,
) -> Result<LossyPlan, (RecoveryBlockReason, String)> {
    let Some(prior) = prior_summary.filter(|text| is_usable_summary_text(text)) else {
        return Err((
            RecoveryBlockReason::NoPriorSummary,
            "no usable previous summary to anchor the resumed work".to_string(),
        ));
    };

    let Some(newest_user) = active.iter().rposition(is_user_instruction) else {
        return Err((
            RecoveryBlockReason::NoNewestUser,
            "the active slice holds no user instruction".to_string(),
        ));
    };

    // Contiguous tool exchange at the tail. Stopping at the first non-tool
    // message keeps the suffix ordered and complete.
    let mut chain_start = active.len();
    while chain_start > 0 && is_tool_exchange(&active[chain_start - 1]) {
        chain_start -= 1;
    }
    if chain_start < active.len() {
        complete_tool_chain(&active[chain_start..])?;
    }

    let keep_from = newest_user.min(chain_start);
    if keep_from == 0 {
        return Err((
            RecoveryBlockReason::NothingToCover,
            "the safe suffix is the whole transcript".to_string(),
        ));
    }

    let kept = &active[keep_from..];
    let summary_text = lossy_summary_text(prior, failure, keep_from);
    // The tail is measured in tokens, the same unit the budget is in. The
    // summary text is priced by the estimator too, so a Korean summary is not
    // charged as if it were English.
    let tail_tokens = estimate_compaction_tokens_from_text(None, kept, budget_tokens)
        + horon_agent_compaction_core::estimate_text_tokens(&summary_text);
    if tail_tokens > budget_tokens {
        return Err((
            RecoveryBlockReason::TailExceedsBudget,
            format!(
                "the newest user instruction plus the complete tool chain need {tail_tokens} tokens, budget is {budget_tokens}"
            ),
        ));
    }

    // Restoration entries are still sized in characters, so this is the inverse
    // direction: what the budget buys once the kept tail and the summary are
    // paid for.
    let kept_chars: usize = kept.iter().map(horon_agent_compaction_core::message_char_count).sum();
    let remaining_chars = budget_tokens
        .saturating_mul(CHARS_PER_TOKEN_UPPER_BOUND)
        .saturating_sub(kept_chars.saturating_add(summary_text.len()));
    let projection = SourceProjection::from_messages(active);
    let mut restoration = collect_restoration_with_remaining(
        &projection,
        CoveredRange::new(0, keep_from),
        context_window,
        remaining_chars as u64,
    );

    // Task-state reminder: what the resumed agent is supposed to be doing. The
    // note is an excerpt, never a file body.
    let reminder = task_reminder(&active[newest_user]);
    if !reminder.is_empty() {
        restoration.entries.insert(
            0,
            RestorationEntry {
                path: "task:newest_user".to_string(),
                kind: "task".to_string(),
                note: Some(reminder),
            },
        );
        restoration.entries.truncate(MAX_RESTORATION_ENTRIES);
    }
    restoration.total_bytes = restoration
        .total_bytes
        .min(MAX_RESTORATION_BYTES)
        .min(remaining_chars as u64);

    Ok(LossyPlan {
        keep_from,
        summary_text,
        restoration,
        tail_tokens,
        budget_tokens,
    })
}

#[cfg(test)]
mod recovery_budget_tests {
    use super::*;
    use horon_agent_compaction_core::CHARS_PER_TOKEN_UPPER_BOUND;
    use horon_agent_message_types::{ContentBlock, Message, Role};

    fn turn(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    /// A transcript long enough that the newest user instruction is not the whole
    /// thing, so there is something to cover and a plan is produced.
    fn a_conversation() -> Vec<Message> {
        let mut messages = Vec::new();
        for index in 0..12 {
            messages.push(turn(
                Role::User,
                &format!("step {index}: read the module and report what it does"),
            ));
            messages.push(turn(
                Role::Assistant,
                &format!("step {index}: the module parses config and validates it"),
            ));
        }
        messages.push(turn(Role::User, "now resume the migration"));
        messages
    }

    const SUMMARY: &str = "A usable prior summary of the work so far, long enough to anchor the \
                           resumed work and carry the decisions that were already made.";

    fn plan_for(budget_tokens: usize) -> LossyPlan {
        plan_lossy_fallback(
            &a_conversation(),
            Some(SUMMARY),
            "the summary call failed",
            budget_tokens,
            200_000,
        )
        .expect("a long conversation with a usable summary produces a plan")
    }

    /// The restoration allowance is the token budget converted at the constant,
    /// so a changed multiplier changes how much a bigger budget buys.
    ///
    /// F4's R1 named this multiply site as having no test deriving an expectation
    /// from `CHARS_PER_TOKEN_UPPER_BOUND`. A ceiling (`<=`) is not enough: it
    /// passes for any multiplier that makes the allowance smaller, which is the
    /// hardcoded-constant case the test exists to catch. This asserts the
    /// *difference* between two budgets, which is the multiplier itself and does
    /// not depend on how much content the manifest happened to carry.
    ///
    /// At the constant, +1_000 tokens is +4_000 characters. At a /3 divisor it
    /// would be +3_000, at /5 +5_000.
    #[test]
    fn the_restoration_allowance_scales_at_the_upper_bound_per_token() {
        assert_eq!(
            CHARS_PER_TOKEN_UPPER_BOUND, 4,
            "this test's arithmetic assumes the constant is 4"
        );

        let narrow = plan_for(1_000);
        let wide = plan_for(2_000);
        assert_eq!(narrow.budget_tokens, 1_000);
        assert_eq!(wide.budget_tokens, 2_000);

        let extra = wide
            .restoration
            .total_bytes
            .saturating_sub(narrow.restoration.total_bytes);
        assert_eq!(
            extra,
            (1_000 * CHARS_PER_TOKEN_UPPER_BOUND) as u64,
            "1_000 extra tokens must buy exactly 4_000 extra characters; got {extra}. \
             A hardcoded or changed multiplier moves this number."
        );
    }

    /// A larger token budget must not buy less restoration, which is the weaker
    /// property and holds for any positive multiplier. Kept as a second signal.
    #[test]
    fn a_larger_budget_does_not_reduce_the_restoration_allowance() {
        let narrow = plan_for(1_000);
        let wide = plan_for(2_000);
        assert!(
            wide.restoration.total_bytes >= narrow.restoration.total_bytes,
            "a larger token budget must not reduce what is restored: {} vs {}",
            wide.restoration.total_bytes,
            narrow.restoration.total_bytes
        );
    }
}

#[cfg(test)]
mod recovery_unit_tests {
    use super::*;

    fn text(role: Role, body: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: body.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    #[test]
    fn recovery_notice_detection_ignores_real_summaries() {
        assert!(is_length_notice(
            "This conversation exceeded the maximum context length. Please retry."
        ));
        assert!(is_length_notice("prompt is too long"));
        assert!(!is_length_notice(""));
        assert!(!is_length_notice(
            "## Goal\nResume the migration.\n\n## Evidence\ncontext length test passed"
        ));
        assert!(!is_length_notice(&"x".repeat(900)));
    }

    #[test]
    fn recovery_user_instruction_skips_tool_results_and_reminders() {
        let tool_result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "ok".to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        };
        let reminder = text(Role::User, "<system-reminder>recalled memory</system-reminder>");
        let instruction = text(Role::User, "finish the task");
        let active = [tool_result, reminder, instruction];
        assert_eq!(active.iter().rposition(is_user_instruction), Some(2));
    }
}

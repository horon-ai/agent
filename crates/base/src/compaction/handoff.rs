//! Handoff document generation for a compaction boundary.
//!
//! A compaction summary is written for the summarizer; a handoff document is
//! written for a fresh agent that has none of this conversation. This module
//! produces that document and reports where the verbatim tail begins.
//!
//! The split is decided by a token budget only. [`recent_cutoff_for_token_budget`]
//! keeps the newest messages that fit [`generate_handoff`]'s `recent_tokens`,
//! [`align_cutoff_to_turn_start`] pulls the cut back to a turn start when the
//! slack is cheap, and [`safe_compaction_cutoff`] refuses to leave a tool result
//! without its tool call. There is no turn-count knob: the budget is the whole
//! policy, which is why a smaller budget keeps fewer messages verbatim.
//!
//! Everything covered by the cut is rendered into one prompt through
//! [`render_source_range`], which bounds each message and labels it with the
//! event it came from. The prompt asks for the seven sections a resuming agent
//! needs (Goal, Constraints, Done, In progress, Next steps, Open risks, Key
//! files). Nothing here repairs the Markdown, but two answers are refused before
//! they can become a boundary: one with no text, and one without a `Goal`
//! heading. A document that drops any other heading is still a document, and the
//! caller commits it exactly as the model wrote it.
//!
//! A prompt that cannot carry the whole covered range is never truncated: the
//! covered range shrinks instead, because a document written over a silently
//! shortened range would advance the boundary past messages it never read. When
//! even the smallest range does not fit, generation fails with
//! [`HandoffError::CoveredRangeTooLarge`] and the caller keeps the transcript.
//!
//! The completion goes through [`aux_completion`], the tool-free one-shot path.
//! [`HandoffCompleter`] and [`generate_handoff`] exist so tests can supply an
//! answer without a network call; [`generate_handoff_with_aux`] is the
//! production entry point.
//!
//! Failure modes are distinct on purpose. [`HandoffError::NothingToCover`] means
//! the budget kept every message verbatim, so there is nothing to hand off.
//! [`HandoffError::Provider`] carries the completion error.
//! [`HandoffError::InvalidDocument`] means the completion answered but its text
//! cannot stand in for the covered messages. [`HandoffError::Cancelled`] is
//! reported as cancellation, never as a successful empty document, so a caller
//! can tell "the model failed" from "we stopped waiting".

use super::source_projection::{CoveredRange, SourceProjection, render_source_range};
use crate::aux_completion::{self, CompletionRequest, MAX_PROMPT_BYTES};
use crate::message::Message;
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_compaction_core::{
    MIN_TURNS_TO_KEEP, align_cutoff_to_turn_start, recent_cutoff_for_token_budget, safe_compaction_cutoff,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often a pending generation checks whether it was cancelled.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Shrink attempts a prompt gets before the covered range is declared too large.
const MAX_SHRINK_ATTEMPTS: u32 = 3;

/// System text for the handoff completion.
const HANDOFF_SYSTEM: &str = "You write handoff documents. Answer with Markdown only and do not call tools.";

/// Opening of the handoff prompt: the role and the requested sections.
const HANDOFF_PROMPT_HEADER: &str = "\
Write a handoff document for the conversation below so a fresh agent can resume the work \
without re-reading it.

Answer with Markdown and use exactly these sections:
## Goal
## Constraints
## Done
## In progress
## Next steps
## Open risks
## Key files

`## Constraints` is for limits the user imposed (scope, stack, things not to touch, \
approvals they refused). Copy them from the conversation; do not invent any. Under \
`## Next steps`, give every item its completion criterion: what must be true for it to \
be done.

Conversation:
";

/// Closing of the handoff prompt.
const HANDOFF_PROMPT_FOOTER: &str = "\
End of conversation. Write the handoff document now.
";

/// A generated handoff document and where the verbatim tail begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffDocument {
    /// Markdown exactly as the model produced it. It is never empty and always
    /// has a `Goal` heading; nothing else about it is checked. It is also never
    /// truncated: a prompt that cannot hold the whole covered range shrinks the
    /// range instead.
    pub document: String,
    /// Index of the first message kept verbatim in the caller's message slice.
    pub cutoff: usize,
}

/// Why a handoff document was not produced.
#[derive(Debug)]
pub enum HandoffError {
    /// The recent-token budget kept every message verbatim.
    NothingToCover,
    /// The completion itself failed, or the request could not be built.
    Provider { detail: String },
    /// The completion answered, but the document was refused: it was empty or
    /// had no `Goal` heading.
    InvalidDocument { reason: &'static str },
    /// Even the smallest range worth covering fills the prompt budget, so the
    /// document could not be written without dropping part of the range.
    CoveredRangeTooLarge { budget: usize },
    /// The caller cancelled before the completion answered.
    Cancelled,
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingToCover => write!(
                f,
                "nothing to cover: the recent-token budget keeps every message verbatim"
            ),
            Self::Provider { detail } => write!(f, "handoff generation failed: {detail}"),
            Self::InvalidDocument { reason } => write!(f, "handoff document rejected: {reason}"),
            Self::CoveredRangeTooLarge { budget } => write!(
                f,
                "the covered range does not fit the {budget}-byte prompt even after shrinking"
            ),
            Self::Cancelled => write!(f, "handoff generation was cancelled"),
        }
    }
}

impl std::error::Error for HandoffError {}

/// One tool-free completion, as the generator sees it.
///
/// The production implementation is [`AuxCompletion`]; tests implement this
/// trait so a fixture transcript needs no network.
#[async_trait]
pub trait HandoffCompleter: Send + Sync {
    /// Answer `request` with text. Implementations must not call tools.
    async fn complete(&self, request: CompletionRequest) -> Result<String>;
}

/// The [`HandoffCompleter`] that dispatches through [`aux_completion::complete`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AuxCompletion;

#[async_trait]
impl HandoffCompleter for AuxCompletion {
    async fn complete(&self, request: CompletionRequest) -> Result<String> {
        aux_completion::complete(request).await
    }
}

/// Generate a handoff document covering everything before the verbatim tail.
///
/// `recent_tokens` is the budget for the messages that stay verbatim; the
/// covered prefix is everything before that cut. `prior` is the text of the
/// boundary this one replaces, so a second handoff in one session keeps the goal
/// and constraints the first one recorded. `cancel` is polled while the
/// completion is pending, so a caller that stops waiting gets
/// [`HandoffError::Cancelled`] instead of a stale document.
pub async fn generate_handoff<C: HandoffCompleter + ?Sized>(
    messages: &[Message],
    recent_tokens: usize,
    prior: Option<&str>,
    completer: &C,
    cancel: &AtomicBool,
) -> Result<HandoffDocument, HandoffError> {
    let cutoff = handoff_cutoff(messages, recent_tokens);
    if cutoff == 0 {
        return Err(HandoffError::NothingToCover);
    }
    generate_handoff_at_cutoff(messages, cutoff, prior, completer, cancel).await
}

/// The same generation for a caller that already decided how far to cover.
///
/// The compaction job knows its own cutoff (the token budget and the semantic
/// or reactive mode chose it), so it passes it in instead of recomputing one
/// from a recent-token allowance. A cutoff of 0 covers nothing and is refused
/// with [`HandoffError::NothingToCover`], and a cutoff past the end of the
/// transcript is clamped.
pub async fn generate_handoff_at_cutoff<C: HandoffCompleter + ?Sized>(
    messages: &[Message],
    cutoff: usize,
    prior: Option<&str>,
    completer: &C,
    cancel: &AtomicBool,
) -> Result<HandoffDocument, HandoffError> {
    let mut cutoff = safe_compaction_cutoff(messages, cutoff.min(messages.len()));
    if cutoff == 0 {
        return Err(HandoffError::NothingToCover);
    }

    let projection = SourceProjection::from_messages(messages);
    let mut attempts = 0u32;
    let prompt = loop {
        match build_prompt(&projection, cutoff, prior) {
            Ok(prompt) => break prompt,
            Err(scaled) => {
                let next = safe_compaction_cutoff(messages, scaled);
                if attempts >= MAX_SHRINK_ATTEMPTS || next == 0 {
                    return Err(HandoffError::CoveredRangeTooLarge {
                        budget: MAX_PROMPT_BYTES,
                    });
                }
                attempts += 1;
                cutoff = next;
            }
        }
    };

    let request = CompletionRequest::new(prompt)
        .and_then(|request| request.with_system(HANDOFF_SYSTEM))
        .map_err(|error| HandoffError::Provider {
            detail: error.to_string(),
        })?;

    if cancel.load(Ordering::Relaxed) {
        return Err(HandoffError::Cancelled);
    }

    tokio::select! {
        biased;
        () = await_cancellation(cancel) => Err(HandoffError::Cancelled),
        result = completer.complete(request) => match result {
            Ok(document) => validate_document(&document).map(|()| HandoffDocument { document, cutoff }),
            Err(error) => Err(HandoffError::Provider {
                detail: format!("{error:#}"),
            }),
        },
    }
}

/// Generate a handoff document through the tool-free auxiliary completion path.
pub async fn generate_handoff_with_aux(
    messages: &[Message],
    recent_tokens: usize,
    prior: Option<&str>,
    cancel: &AtomicBool,
) -> Result<HandoffDocument, HandoffError> {
    generate_handoff(messages, recent_tokens, prior, &AuxCompletion, cancel).await
}

/// The same, for a cutoff the caller already decided.
pub async fn generate_handoff_at_cutoff_with_aux(
    messages: &[Message],
    cutoff: usize,
    prior: Option<&str>,
    cancel: &AtomicBool,
) -> Result<HandoffDocument, HandoffError> {
    generate_handoff_at_cutoff(messages, cutoff, prior, &AuxCompletion, cancel).await
}

/// Refuse an answer that cannot stand in for the covered messages.
///
/// A boundary replaces the covered prefix in every later request, so an answer
/// with no text, or one that never states the goal, would drop that prefix and
/// leave the successor nothing to resume from. The heading is matched on its
/// title alone: `# Goal`, `### goal` and `## Goal:` all count.
fn validate_document(document: &str) -> Result<(), HandoffError> {
    if document.trim().is_empty() {
        return Err(HandoffError::InvalidDocument {
            reason: "the document is empty",
        });
    }
    if !document.lines().any(is_goal_heading) {
        return Err(HandoffError::InvalidDocument {
            reason: "the document has no Goal section",
        });
    }
    Ok(())
}

/// Whether `line` is a Markdown heading whose title is `Goal`.
fn is_goal_heading(line: &str) -> bool {
    let line = line.trim();
    let title = line.trim_start_matches('#');
    title.len() < line.len()
        && title
            .trim()
            .trim_end_matches(':')
            .trim_end()
            .eq_ignore_ascii_case("goal")
}

/// Index of the first message the handoff keeps verbatim.
///
/// This is the same three-step cut the compaction manager uses: a token budget,
/// an alignment to the nearest turn start when the slack is under half the
/// allowance, then a tool-pair-safe boundary.
fn handoff_cutoff(messages: &[Message], recent_tokens: usize) -> usize {
    let cutoff = recent_cutoff_for_token_budget(messages, recent_tokens, MIN_TURNS_TO_KEEP);
    let cutoff = align_cutoff_to_turn_start(messages, cutoff, recent_tokens / 2).0;
    safe_compaction_cutoff(messages, cutoff)
}

/// Build the one prompt that covers `[0, cutoff)`.
///
/// `Ok` is a prompt inside [`MAX_PROMPT_BYTES`]. `Err(scaled)` means the rendered
/// body does not fit and carries the smaller cutoff to try instead, proportional
/// to the overshoot; the caller re-aligns it so a shrunken range never leaves a
/// tool result without its call. The body itself is never clipped: a clipped
/// body would let the document claim messages the model never read.
fn build_prompt(
    projection: &SourceProjection,
    cutoff: usize,
    prior: Option<&str>,
) -> std::result::Result<String, usize> {
    let prior_block = prior.map(prior_section).unwrap_or_default();
    let reserved = HANDOFF_PROMPT_HEADER.len() + prior_block.len() + HANDOFF_PROMPT_FOOTER.len();
    let budget = MAX_PROMPT_BYTES.saturating_sub(reserved);

    let body = render_source_range(projection, CoveredRange::new(0, cutoff), budget);
    if body.len() > budget {
        let scaled = cutoff.saturating_mul(budget) / body.len().max(1);
        return Err(scaled.min(cutoff.saturating_sub(1)));
    }

    let mut prompt = String::with_capacity(reserved + body.len());
    prompt.push_str(HANDOFF_PROMPT_HEADER);
    prompt.push_str(&prior_block);
    prompt.push_str(&body);
    prompt.push_str(HANDOFF_PROMPT_FOOTER);
    Ok(prompt)
}

/// The block that carries the boundary this document replaces.
fn prior_section(prior: &str) -> String {
    format!(
        "Your earlier handoff document for this same work follows. Keep its Goal and \
Constraints unless the conversation below revises them.\n\n{prior}\n\nConversation \
continues below.\n\n"
    )
}

/// Resolve once `cancel` is set.
async fn await_cancellation(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCELLATION_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod handoff_tests;

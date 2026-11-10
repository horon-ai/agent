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
//! event it came from. The prompt asks for the six sections a resuming agent
//! needs (Goal, Done, In progress, Next steps, Open risks, Key files) and the
//! answer is returned exactly as the model wrote it. Nothing here parses,
//! repairs or validates the Markdown: a document that drops a heading is still
//! a document, and the caller commits it as-is.
//!
//! The completion goes through [`aux_completion`], the tool-free one-shot path.
//! [`HandoffCompleter`] and [`generate_handoff`] exist so tests can supply an
//! answer without a network call; [`generate_handoff_with_aux`] is the
//! production entry point.
//!
//! Failure modes are distinct on purpose. [`HandoffError::NothingToCover`] means
//! the budget kept every message verbatim, so there is nothing to hand off.
//! [`HandoffError::Provider`] carries the completion error. [`HandoffError::Cancelled`]
//! is reported as cancellation, never as a successful empty document, so a
//! caller can tell "the model failed" from "we stopped waiting".

use super::source_projection::{CoveredRange, SourceProjection, render_source_range};
use crate::aux_completion::{self, CompletionRequest, MAX_PROMPT_BYTES};
use crate::message::Message;
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_compaction_core::{
    MIN_TURNS_TO_KEEP, align_cutoff_to_turn_start, recent_cutoff_for_token_budget, safe_compaction_cutoff,
    truncate_str_boundary,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often a pending generation checks whether it was cancelled.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// System text for the handoff completion.
const HANDOFF_SYSTEM: &str = "You write handoff documents. Answer with Markdown only and do not call tools.";

/// Opening of the handoff prompt: the role and the requested sections.
const HANDOFF_PROMPT_HEADER: &str = "\
Write a handoff document for the conversation below so a fresh agent can resume the work \
without re-reading it.

Answer with Markdown and use exactly these sections:
## Goal
## Done
## In progress
## Next steps
## Open risks
## Key files

Conversation:
";

/// Closing of the handoff prompt.
const HANDOFF_PROMPT_FOOTER: &str = "\
End of conversation. Write the handoff document now.
";

/// A generated handoff document and where the verbatim tail begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffDocument {
    /// Markdown exactly as the model produced it, unparsed and unvalidated.
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
/// covered prefix is everything before that cut. `cancel` is polled while the
/// completion is pending, so a caller that stops waiting gets
/// [`HandoffError::Cancelled`] instead of a stale document.
pub async fn generate_handoff<C: HandoffCompleter + ?Sized>(
    messages: &[Message],
    recent_tokens: usize,
    completer: &C,
    cancel: &AtomicBool,
) -> Result<HandoffDocument, HandoffError> {
    let cutoff = handoff_cutoff(messages, recent_tokens);
    if cutoff == 0 {
        return Err(HandoffError::NothingToCover);
    }

    let projection = SourceProjection::from_messages(messages);
    let prompt = build_prompt(&projection, cutoff);
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
            Ok(document) => Ok(HandoffDocument { document, cutoff }),
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
    cancel: &AtomicBool,
) -> Result<HandoffDocument, HandoffError> {
    generate_handoff(messages, recent_tokens, &AuxCompletion, cancel).await
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
/// The prompt must fit [`MAX_PROMPT_BYTES`], so the message rendering gets
/// whatever the fixed header and footer leave. Truncating the body instead of
/// the finished string keeps the instruction in front of the model either way.
fn build_prompt(projection: &SourceProjection, cutoff: usize) -> String {
    let reserved = HANDOFF_PROMPT_HEADER.len() + HANDOFF_PROMPT_FOOTER.len();
    let budget = MAX_PROMPT_BYTES.saturating_sub(reserved);

    let mut body = render_source_range(projection, CoveredRange::new(0, cutoff), budget);
    if body.len() > budget {
        let end = truncate_str_boundary(&body, budget).len();
        body.truncate(end);
    }

    let mut prompt = String::with_capacity(HANDOFF_PROMPT_HEADER.len() + body.len() + HANDOFF_PROMPT_FOOTER.len());
    prompt.push_str(HANDOFF_PROMPT_HEADER);
    prompt.push_str(&body);
    prompt.push_str(HANDOFF_PROMPT_FOOTER);
    prompt
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

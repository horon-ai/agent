//! Branch summary for the part of the conversation a rewind discards.
//!
//! A rewind drops every message after the stop point. Usually that is the point:
//! the branch was wrong and the operator wants the context gone. Sometimes the
//! useful part is *why* it was wrong, and a bare rewind loses that along with
//! the noise. This module writes the discarded branch up as a short report and
//! hands it back so the kept branch can carry it.
//!
//! The report answers three questions the next attempt actually needs: what was
//! tried, what failed and why, and which facts the branch established that are
//! still true. It is not a transcript copy and not an apology; the prompt asks
//! the model to skip anything it cannot support from the branch it was given.
//!
//! The completion goes through [`aux_completion`], the same tool-free one-shot
//! path the handoff document uses. [`RewindSummaryCompleter`] and
//! [`generate_rewind_summary`] exist so tests can supply an answer without a
//! network call; [`generate_rewind_summary_with_aux`] is the production entry.
//!
//! Failures are distinct on purpose. [`RewindSummaryError::NothingToSummarize`]
//! means the branch was empty, [`RewindSummaryError::Provider`] carries the
//! completion error, and [`RewindSummaryError::Cancelled`] reports cancellation
//! rather than a successful empty report.

use super::source_projection::{CoveredRange, SourceProjection, render_source_range};
use crate::aux_completion::{self, CompletionRequest, MAX_PROMPT_BYTES};
use crate::message::Message;
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_compaction_core::truncate_str_boundary;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often a pending generation checks whether it was cancelled.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// System text for the rewind summary completion.
const REWIND_SUMMARY_SYSTEM: &str =
    "You summarise an abandoned branch of a conversation. Answer with Markdown only and do not call tools.";

/// Opening of the rewind summary prompt: the role and the requested sections.
const REWIND_SUMMARY_PROMPT_HEADER: &str = "\
The conversation below is a branch that was just abandoned: everything after it \
was rewound away and the agent will continue from an earlier point. Write a short \
report so the next attempt does not repeat the same work.

Answer with Markdown and use exactly these sections:
## What was tried
## What failed and why
## Facts learned

State only what the branch itself supports. Leave a section out when the branch \
does not answer it; do not guess.

Abandoned branch:
";

/// Closing of the rewind summary prompt.
const REWIND_SUMMARY_PROMPT_FOOTER: &str = "\
End of abandoned branch. Write the report now.
";

/// One tool-free completion, as the generator sees it.
///
/// The production implementation is [`AuxCompletion`]; tests implement this
/// trait so a fixture branch needs no network.
#[async_trait]
pub trait RewindSummaryCompleter: Send + Sync {
    /// Answer `request` with text. Implementations must not call tools.
    async fn complete(&self, request: CompletionRequest) -> Result<String>;
}

/// The [`RewindSummaryCompleter`] that dispatches through [`aux_completion::complete`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AuxCompletion;

#[async_trait]
impl RewindSummaryCompleter for AuxCompletion {
    async fn complete(&self, request: CompletionRequest) -> Result<String> {
        aux_completion::complete(request).await
    }
}

/// Why a rewind summary was not produced.
#[derive(Debug)]
pub enum RewindSummaryError {
    /// The discarded branch had no messages to summarise.
    NothingToSummarize,
    /// The completion itself failed, or the request could not be built.
    Provider { detail: String },
    /// The caller cancelled before the completion answered.
    Cancelled,
}

impl std::fmt::Display for RewindSummaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingToSummarize => write!(f, "nothing to summarise: the rewind discarded no messages"),
            Self::Provider { detail } => write!(f, "rewind summary failed: {detail}"),
            Self::Cancelled => write!(f, "rewind summary was cancelled"),
        }
    }
}

impl std::error::Error for RewindSummaryError {}

/// Generate the report for a branch that a rewind is about to discard.
///
/// `max_tokens` is the allowance for the answer; callers clamp it to the range
/// [`aux_completion`] accepts. `cancel` is polled while the completion is
/// pending, so a caller that stops waiting gets [`RewindSummaryError::Cancelled`]
/// instead of a stale report.
pub async fn generate_rewind_summary<C: RewindSummaryCompleter + ?Sized>(
    messages: &[Message],
    max_tokens: u32,
    completer: &C,
    cancel: &AtomicBool,
) -> Result<String, RewindSummaryError> {
    if messages.is_empty() {
        return Err(RewindSummaryError::NothingToSummarize);
    }

    let projection = SourceProjection::from_messages(messages);
    let prompt = build_prompt(&projection, messages.len());
    let request = CompletionRequest::new(prompt)
        .and_then(|request| request.with_system(REWIND_SUMMARY_SYSTEM))
        .and_then(|request| request.with_max_tokens(max_tokens))
        .map_err(|error| RewindSummaryError::Provider {
            detail: error.to_string(),
        })?;

    if cancel.load(Ordering::Relaxed) {
        return Err(RewindSummaryError::Cancelled);
    }

    tokio::select! {
        biased;
        () = await_cancellation(cancel) => Err(RewindSummaryError::Cancelled),
        result = completer.complete(request) => match result {
            Ok(report) => Ok(report),
            Err(error) => Err(RewindSummaryError::Provider {
                detail: format!("{error:#}"),
            }),
        },
    }
}

/// The allowance for one summary, from the configured value.
///
/// `0` means "unset, use the default"; everything else is clamped into the
/// range [`aux_completion`] accepts, so a config typo degrades to the nearest
/// legal request instead of failing the summary.
pub fn clamp_max_tokens(configured: usize) -> u32 {
    horon_agent_config_types::rewind_summary::clamp_rewind_summary_tokens(configured) as u32
}

/// Generate the report through the tool-free auxiliary completion path.
pub async fn generate_rewind_summary_with_aux(
    messages: &[Message],
    max_tokens: u32,
    cancel: &AtomicBool,
) -> Result<String, RewindSummaryError> {
    generate_rewind_summary(messages, max_tokens, &AuxCompletion, cancel).await
}

/// Build the one prompt that covers `[0, covered)`.
///
/// The prompt must fit [`MAX_PROMPT_BYTES`], so the message rendering gets
/// whatever the fixed header and footer leave. Truncating the body instead of
/// the finished string keeps the instruction in front of the model either way.
fn build_prompt(projection: &SourceProjection, covered: usize) -> String {
    let reserved = REWIND_SUMMARY_PROMPT_HEADER.len() + REWIND_SUMMARY_PROMPT_FOOTER.len();
    let budget = MAX_PROMPT_BYTES.saturating_sub(reserved);

    let mut body = render_source_range(projection, CoveredRange::new(0, covered), budget);
    if body.len() > budget {
        let end = truncate_str_boundary(&body, budget).len();
        body.truncate(end);
    }

    let mut prompt =
        String::with_capacity(REWIND_SUMMARY_PROMPT_HEADER.len() + body.len() + REWIND_SUMMARY_PROMPT_FOOTER.len());
    prompt.push_str(REWIND_SUMMARY_PROMPT_HEADER);
    prompt.push_str(&body);
    prompt.push_str(REWIND_SUMMARY_PROMPT_FOOTER);
    prompt
}

/// Resolve once `cancel` is set.
async fn await_cancellation(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCELLATION_POLL_INTERVAL).await;
    }
}

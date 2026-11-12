//! The handoff completion that goes through the session provider.
//!
//! [`AuxCompletion`] dispatches through `Sidecar::for_session`, which resolves
//! the model, endpoint and credentials from the session record. A compaction job
//! already holds a `Provider` handle for the turn it is compacting, and the
//! structured path lets `[compaction] summary_model` pick a cheaper model for it.
//! This completer gives the handoff generator the same two things: the provider
//! the job captured, and the configured summary model when one is set.
//!
//! A summary model is a suggestion, not a requirement. [`Provider::fork`] plus
//! [`Provider::set_model`] can refuse — a provider whose routing is fixed answers
//! that way — and the refused model falls back to the session model with one
//! warning, exactly like [`super::summary::generate_structured_summary_with_model`].
//! A handoff that costs a slightly larger call is better than a turn that is not
//! compacted because a knob was honoured literally.
//!
//! The request is built from the prompt only. No tool list reaches the model, so
//! it cannot answer with a tool call instead of a document.

use super::handoff::{HandoffCompleter, HandoffError, generate_handoff_at_cutoff};
use crate::aux_completion::CompletionRequest;
use crate::message::Message;
use crate::provider::Provider;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// Handoff completion through a captured provider, optionally on `summary_model`.
pub struct ProviderHandoff {
    provider: Arc<dyn Provider>,
}

impl ProviderHandoff {
    /// Create a completer that sends its prompt through `provider`.
    ///
    /// `summary_model` is resolved once, here, so the fallback warning is logged
    /// when the job starts rather than on every retry inside it. An empty or
    /// whitespace-only value means "use the session model".
    pub fn new(provider: Arc<dyn Provider>, summary_model: Option<&str>) -> Self {
        let provider = match summary_model
            .map(str::trim)
            .filter(|model| !model.is_empty() && *model != provider.model())
        {
            Some(model) => {
                let alternate = provider.fork();
                match alternate.set_model(model) {
                    Ok(()) => alternate,
                    Err(error) => {
                        crate::logging::warn(&format!(
                            "compaction summary_model {model} rejected for the handoff: {error}; using the session model"
                        ));
                        provider
                    }
                }
            }
            None => provider,
        };
        Self { provider }
    }
}

#[async_trait]
impl HandoffCompleter for ProviderHandoff {
    async fn complete(&self, request: CompletionRequest) -> Result<String> {
        let (text, _usage) = self
            .provider
            .complete_simple_with_usage(request.prompt(), request.system_or_default())
            .await?;
        if text.trim().is_empty() {
            return Err(HandoffError::Provider {
                detail: "the handoff completion returned no text".to_string(),
            }
            .into());
        }
        Ok(text)
    }
}

/// Write a handoff document over `[0, cutoff)` through the job's provider.
///
/// The compaction job owns the cutoff (the token budget and the mode chose it),
/// so this takes it instead of recomputing one. `prior` is the boundary this
/// document replaces, and `cancel` is the job's own cancellation flag.
pub async fn handoff_document(
    provider: Arc<dyn Provider>,
    messages: &[Message],
    cutoff: usize,
    prior: Option<&str>,
    summary_model: Option<&str>,
    cancel: &AtomicBool,
) -> Result<super::handoff::HandoffDocument, HandoffError> {
    let completer = ProviderHandoff::new(provider, summary_model);
    generate_handoff_at_cutoff(messages, cutoff, prior, &completer, cancel).await
}

/// The job result for a committed handoff document.
///
/// A handoff leaves the provider's native state behind: the boundary it replaces
/// had a summary or a replayable window, and the document supersedes both, so no
/// encrypted content or native window travels with it.
pub(super) fn handoff_result(document: super::handoff::HandoffDocument, duration_ms: u64) -> super::CompactionResult {
    super::CompactionResult {
        job_id: 0,
        summary_text: document.document,
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        duration_ms,
        summarized_messages: document.cutoff,
        covered_messages: document.cutoff,
        input_tokens: None,
        output_tokens: None,
        restoration: None,
        handoff: true,
    }
}

/// Try the handoff strategy for one job, or `None` to fall back.
///
/// The fallback is one step, to the structured summarizer: a configured
/// `handoff` whose document cannot be produced this time still needs that turn
/// compacted, and a structured summary over the same range is the behaviour the
/// session had before the strategy was chosen. The failure is logged with its
/// reason so a persistent cause (a range too large for the prompt, a provider
/// that refuses the summary model) is visible instead of silently overridden.
pub(super) async fn try_handoff(
    provider: Arc<dyn Provider>,
    messages: &[Message],
    requested_end: usize,
    existing_summary: Option<&crate::compaction::Summary>,
    started: Instant,
) -> Option<super::CompactionResult> {
    let cutoff = requested_end.min(messages.len());
    let prior = existing_summary.map(|summary| summary.text.as_str());
    let summary_model = crate::config::config().compaction.summary_model.clone();
    match handoff_document(
        provider,
        &messages[..cutoff],
        cutoff,
        prior,
        summary_model.as_deref(),
        &AtomicBool::new(false),
    )
    .await
    {
        Ok(document) => Some(handoff_result(document, started.elapsed().as_millis() as u64)),
        Err(error) => {
            crate::logging::warn(&format!(
                "[compaction] handoff generation failed ({error}); falling back to a structured summary for this job"
            ));
            None
        }
    }
}

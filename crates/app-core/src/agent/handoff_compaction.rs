//! Handoff compaction: commit a generated handoff document as the boundary.
//!
//! The generator lives in [`crate::compaction::handoff`]; this module is the
//! commit hook. A successful document replaces the summary text of the stored
//! boundary and covers exactly the prefix the generator excluded from the
//! verbatim tail, so the next request is the document plus that tail. Nothing
//! is deleted: `history://current` still serves the whole transcript.
//!
//! A failed or cancelled generation commits nothing, and neither does a
//! document the generator refuses (empty, or without a `Goal` heading). The
//! transcript, the message count and the previous boundary all stay as they
//! were, which is why the outcome type names the failure instead of returning a
//! partial document.

use super::*;
use crate::compaction::handoff::{HandoffCompleter, HandoffDocument, HandoffError, generate_handoff};
use std::sync::atomic::AtomicBool;

/// What one requested handoff compaction did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffOutcome {
    /// The document became the boundary, covering `[0, covered)` of the active
    /// context.
    Committed { covered: usize },
    /// The recent-token budget kept every message verbatim; nothing was written.
    NothingToCover,
    /// The completion failed, or its document was refused (empty, or without a
    /// `Goal` heading). The transcript is unchanged.
    ProviderError(String),
    /// The caller cancelled before the completion answered. The transcript is
    /// unchanged.
    Cancelled,
}

impl Agent {
    /// Generate a handoff document and commit it as the compaction boundary.
    ///
    /// `recent_tokens` comes from `[compaction] recent_tokens`: it is the budget
    /// for the messages that stay verbatim, so the covered prefix is everything
    /// before that tail. The commit follows the same path as a model-requested
    /// rollover, with the generated document in place of the rollover marker.
    pub fn request_handoff_compaction(&mut self, cancel: &AtomicBool) -> HandoffOutcome {
        let recent_tokens = crate::config::config().compaction.recent_tokens;
        self.request_handoff_compaction_with(cancel, recent_tokens, &crate::compaction::handoff::AuxCompletion)
    }

    /// The same request with an explicit budget and completer, so a test needs
    /// neither the process config nor a network.
    fn request_handoff_compaction_with(
        &mut self,
        cancel: &AtomicBool,
        recent_tokens: usize,
        completer: &dyn HandoffCompleter,
    ) -> HandoffOutcome {
        let messages: Vec<Message> = self
            .session
            .active_context()
            .iter()
            .map(crate::session::StoredMessage::to_message)
            .collect();
        // A second handoff replaces the first one's boundary, so the earlier
        // document travels into the prompt: the goal and constraints it recorded
        // are the ones the covered range no longer carries.
        let prior = self.session.compaction.as_ref().map(|state| state.summary_text.clone());
        let context_window = self.provider.context_window();
        let document = match run_off_thread(generate_handoff(
            &messages,
            recent_tokens,
            context_window,
            prior.as_deref(),
            completer,
            cancel,
        )) {
            Ok(document) => document,
            Err(HandoffError::NothingToCover) => return HandoffOutcome::NothingToCover,
            Err(HandoffError::Provider { detail }) => return HandoffOutcome::ProviderError(detail),
            Err(error @ (HandoffError::InvalidDocument { .. } | HandoffError::CoveredRangeTooLarge { .. })) => {
                return HandoffOutcome::ProviderError(error.to_string());
            }
            Err(HandoffError::Cancelled) => return HandoffOutcome::Cancelled,
        };
        HandoffOutcome::Committed {
            covered: self.commit_handoff_boundary(document),
        }
    }

    /// Commit `document` as the boundary and return how many messages it covers.
    ///
    /// The state is built in active-context coordinates, exactly like the
    /// rollover path: the manager restores it relative to the active context and
    /// the persistence step adds the history offset.
    fn commit_handoff_boundary(&mut self, document: HandoffDocument) -> usize {
        let offset = self.session.active_history_offset();
        let active_len = self.session.active_context().len();
        let covered = document.cutoff.min(active_len);
        let state = crate::session::StoredCompactionState {
            summary_text: document.document,
            covers_up_to_turn: offset + covered,
            original_turn_count: offset + active_len,
            compacted_count: covered,
            structured: Some(crate::session::StoredSummaryMetadata {
                // A document is prose, so the paths it covered travel beside
                // it exactly like a structured summary's manifest.
                restoration: (!document.restoration.entries.is_empty()).then(|| document.restoration.to_stored()),
                handoff: true,
                ..Default::default()
            }),
            ..Default::default()
        };

        let compaction = self.registry.compaction();
        match compaction.try_write() {
            Ok(mut manager) => {
                manager.set_budget(self.provider.context_window());
                manager.restore_persisted_stored_state_with(&state, self.session.active_context());
                self.sync_session_compaction_state_from_manager(&mut manager);
                self.session.set_compaction_generation(manager.generation());
            }
            Err(_) => {
                // The manager lock is held by a turn that is still projecting.
                // The boundary is the session's state, so it is written there
                // and the next projection restores the manager from it.
                let mut stored = state;
                stored.compacted_count += offset;
                self.session.compaction = Some(stored);
                if let Err(error) = self.save_session() {
                    logging::error(&format!(
                        "Failed to persist the handoff boundary for session {}: {}",
                        self.session.id, error
                    ));
                }
            }
        }
        self.note_compaction_applied();
        logging::info(&format!(
            "[compaction/handoff] session {} committed a handoff boundary at message {} of {}",
            self.session.id,
            offset + covered,
            offset + active_len,
        ));
        covered
    }
}

/// Run one generation to completion from a synchronous caller.
///
/// The generation is a network call, so it needs a runtime, and the caller is
/// synchronous: it may be a plain thread or a runtime worker. A scoped worker
/// thread with its own current-thread runtime serves both without nesting
/// runtimes, and a panic inside the worker becomes a provider error instead of
/// unwinding into the caller.
fn run_off_thread<F>(future: F) -> Result<HandoffDocument, HandoffError>
where
    F: Future<Output = Result<HandoffDocument, HandoffError>> + Send,
{
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| HandoffError::Provider {
                    detail: format!("handoff runtime could not start: {error}"),
                })?;
            runtime.block_on(future)
        });
        worker.join().unwrap_or_else(|_| {
            Err(HandoffError::Provider {
                detail: "the handoff worker panicked".to_string(),
            })
        })
    })
}

#[cfg(test)]
#[path = "handoff_compaction_tests.rs"]
mod handoff_compaction_tests;

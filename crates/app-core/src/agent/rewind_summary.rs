//! Rewind with an optional branch summary.
//!
//! [`Agent::rewind_to_message`] discards everything after the stop point and
//! keeps no record of why. This module adds an opt-in report: when the request
//! asks for it and `[compaction] rewind_summary` is on, the abandoned branch is
//! summarised off-thread and the report is appended to the kept branch as one
//! hidden system reminder. The model reads it on the next turn, the transcript
//! view does not show it, and the summary is not itself a rewind target.
//!
//! Ordering matters. The discarded messages are captured and the rewind happens
//! before any network call, so a provider failure reports an error on top of a
//! rewind that still succeeded. [`RewindSummaryOutcome`] names which of those
//! happened; the failure detail also lands in `last_status_detail` so the
//! client-side history reply can show it.
//!
//! Undo needs no extra bookkeeping: [`Agent::undo_rewind`] restores the head
//! event recorded before the rewind, which orphans the appended summary event
//! along with every other message the rewind discarded.

use super::*;
use crate::compaction::rewind_summary::{RewindSummaryCompleter, RewindSummaryError, generate_rewind_summary};
use std::sync::atomic::AtomicBool;

/// What one requested rewind did about the branch summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindSummaryOutcome {
    /// The request did not ask for a summary; the rewind ran unchanged.
    NotRequested,
    /// The request asked for one but `[compaction] rewind_summary` is off.
    /// The rewind ran and nothing was appended.
    Disabled,
    /// The report was appended with this many bytes.
    Written { bytes: usize },
    /// The rewind discarded no messages, so there was nothing to report.
    NothingToSummarize,
    /// The rewind succeeded but the completion failed. Nothing was appended.
    ProviderError(String),
    /// The rewind succeeded but the completion was cancelled. Nothing was appended.
    Cancelled,
}

/// A completed rewind: how many messages it removed and what the summary did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RewindSummaryAttempt {
    /// Messages the rewind removed, as [`Agent::rewind_to_message`] reports it.
    pub removed: usize,
    /// What happened to the branch summary.
    pub outcome: RewindSummaryOutcome,
}

/// Where the summary goes: opening tag, body, closing tag.
const SUMMARY_PREFIX: &str = "<system-reminder>\nThe branch that was just rewound away is summarised below. \
This is background for the work ahead, not a request.\n\n";
const SUMMARY_SUFFIX: &str = "\n</system-reminder>";

impl Agent {
    /// Rewind to a 1-based visible message index, optionally summarising the
    /// branch the rewind discards.
    ///
    /// `summarize` false is exactly [`Agent::rewind_to_message`]: the session is
    /// byte-identical to a rewind that never heard of this setting.
    pub fn rewind_to_message_with_summary(&mut self, message_index: usize, summarize: bool) -> Result<usize> {
        let compaction = crate::config::config().compaction.clone();
        let outcome = self.rewind_to_message_and_summarise(
            message_index,
            summarize,
            compaction.rewind_summary,
            crate::compaction::rewind_summary::clamp_max_tokens(compaction.rewind_summary_tokens),
            &crate::compaction::rewind_summary::AuxCompletion,
        )?;
        self.record_rewind_summary_outcome(&outcome.outcome);
        Ok(outcome.removed)
    }

    /// The same rewind with the setting, the allowance and the completer passed
    /// in, so a test needs neither the process config nor a network.
    pub(crate) fn rewind_to_message_and_summarise(
        &mut self,
        message_index: usize,
        summarize: bool,
        enabled: bool,
        max_tokens: u32,
        completer: &dyn RewindSummaryCompleter,
    ) -> Result<RewindSummaryAttempt> {
        if !summarize {
            let removed = self.rewind_to_message(message_index).map_err(anyhow::Error::msg)?;
            return Ok(RewindSummaryAttempt {
                removed,
                outcome: RewindSummaryOutcome::NotRequested,
            });
        }
        if !enabled {
            let removed = self.rewind_to_message(message_index).map_err(anyhow::Error::msg)?;
            return Ok(RewindSummaryAttempt {
                removed,
                outcome: RewindSummaryOutcome::Disabled,
            });
        }

        let discarded = self.discarded_branch(message_index)?;
        // The rewind happens before the completion: a report is worth having only
        // for a branch that is actually gone, and a failed call must not leave the
        // conversation where it was.
        let removed = self.rewind_to_message(message_index).map_err(anyhow::Error::msg)?;
        if discarded.is_empty() {
            return Ok(RewindSummaryAttempt {
                removed,
                outcome: RewindSummaryOutcome::NothingToSummarize,
            });
        }

        let cancel = AtomicBool::new(false);
        let generated = run_off_thread(generate_rewind_summary(&discarded, max_tokens, completer, &cancel));
        let outcome = match generated {
            Ok(report) => self.append_rewind_summary(&report),
            Err(RewindSummaryError::NothingToSummarize) => RewindSummaryOutcome::NothingToSummarize,
            Err(RewindSummaryError::Cancelled) => RewindSummaryOutcome::Cancelled,
            Err(RewindSummaryError::Provider { detail }) => RewindSummaryOutcome::ProviderError(detail),
        };
        Ok(RewindSummaryAttempt { removed, outcome })
    }

    /// The messages a rewind to `message_index` would discard.
    ///
    /// The index is a visible transcript number, so it is resolved through the
    /// same `rewind_target_stored_indices` mapping the rewind itself uses; the
    /// discarded branch is every stored message after that one.
    fn discarded_branch(&self, message_index: usize) -> Result<Vec<Message>> {
        let snapshot = self.session.history_snapshot()?;
        let targets = snapshot.rewind_target_stored_indices();
        let Some(stored) = targets.get(message_index.saturating_sub(1)).copied() else {
            // The rewind itself reports the range error; an empty branch here
            // keeps this path from inventing a second message.
            return Ok(Vec::new());
        };
        let history = snapshot.history_messages()?;
        Ok(history.iter().skip(stored + 1).map(StoredMessage::to_message).collect())
    }

    /// Append `report` to the kept branch and return what was written.
    ///
    /// The message is a hidden system reminder: `StoredDisplayRole::System`
    /// keeps it out of the rendered transcript and out of rewind targets, and
    /// the `<system-reminder>` wrapper is what the provider projection needs to
    /// hand it to the model as background rather than as a user request.
    fn append_rewind_summary(&mut self, report: &str) -> RewindSummaryOutcome {
        let body = report.trim();
        if body.is_empty() {
            return RewindSummaryOutcome::NothingToSummarize;
        }
        let text = format!("{SUMMARY_PREFIX}{body}{SUMMARY_SUFFIX}");
        let bytes = text.len();
        self.add_message_with_display_role(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
            Some(StoredDisplayRole::System),
        );
        if let Err(error) = self.save_session() {
            logging::warn(&format!(
                "Could not persist the rewind summary for session {}: {error}",
                self.session.id
            ));
        }
        logging::info(&format!(
            "[rewind/summary] session {} appended a {bytes}-byte branch summary",
            self.session.id
        ));
        RewindSummaryOutcome::Written { bytes }
    }

    /// Report a summary failure where the client can see it.
    fn record_rewind_summary_outcome(&mut self, outcome: &RewindSummaryOutcome) {
        if let RewindSummaryOutcome::ProviderError(detail) = outcome {
            logging::warn(&format!(
                "[rewind/summary] session {} rewound, but the branch summary failed: {detail}",
                self.session.id
            ));
            // The history reply carries this as its status detail, so a caller
            // sees both the rewind and why the report is missing.
            self.last_status_detail = Some(format!("rewind summary failed: {detail}"));
        }
    }
}

/// Run one generation to completion from a synchronous caller.
///
/// Mirrors the handoff runner: a scoped worker thread with its own
/// current-thread runtime, so a plain thread and a runtime worker can both call
/// it without nesting runtimes, and a panic in the worker becomes a provider
/// error instead of unwinding into the caller.
fn run_off_thread<F>(future: F) -> Result<String, RewindSummaryError>
where
    F: Future<Output = Result<String, RewindSummaryError>> + Send,
{
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| RewindSummaryError::Provider {
                    detail: format!("rewind summary runtime could not start: {error}"),
                })?;
            runtime.block_on(future)
        });
        worker.join().unwrap_or_else(|_| {
            Err(RewindSummaryError::Provider {
                detail: "the rewind summary worker panicked".to_string(),
            })
        })
    })
}

#[cfg(test)]
#[path = "rewind_summary_tests.rs"]
mod rewind_summary_tests;

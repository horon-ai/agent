//! Handoff compaction at an overnight run's handoff-ready point.
//!
//! The supervisor crosses `handoff_ready_at` before the target wake time. That
//! pass is the last moment a handoff document is worth writing: the coordinator
//! turns after it read the document instead of the whole night's transcript.
//!
//! A provider failure or a cancellation leaves the transcript unchanged, so the
//! outcome is recorded as an event and the run continues. Ending a night's work
//! because one compaction call failed would lose the remaining turns for no
//! reason.

use super::*;
use crate::agent::HandoffOutcome;

/// Run the handoff-ready pass once.
///
/// Returns the prompt to send next when it fired on this pass, and `None` when
/// it already fired or the moment has not arrived. The compaction is a network
/// call, so it runs off the supervisor's worker thread.
pub(super) fn ready_pass(
    manifest: &OvernightManifest,
    notice_sent: bool,
    now: chrono::DateTime<Utc>,
    compact: impl FnOnce() -> HandoffOutcome,
) -> Result<Option<String>> {
    if notice_sent || now < manifest.handoff_ready_at || now >= manifest.target_wake_at {
        return Ok(None);
    }
    record_event(
        manifest,
        "handoff_ready_notice",
        "Entering handoff-ready mode".to_string(),
        json!({ "target_wake_at": manifest.target_wake_at }),
        true,
    )?;
    let outcome = off_worker(compact);
    let (summary, details) = outcome_event(&outcome);
    record_event(manifest, "handoff_compaction", summary, details, true)?;
    Ok(Some(build_handoff_ready_prompt(manifest)))
}

/// Event text for one outcome. The failure cases say the transcript is
/// unchanged, so an operator reading the log does not assume a boundary landed.
fn outcome_event(outcome: &HandoffOutcome) -> (String, Value) {
    match outcome {
        HandoffOutcome::Committed { covered } => (
            format!("Handoff compaction covered the first {covered} messages"),
            json!({ "outcome": "committed", "covered": covered }),
        ),
        HandoffOutcome::NothingToCover => (
            "Handoff compaction had nothing to cover".to_string(),
            json!({ "outcome": "nothing_to_cover" }),
        ),
        HandoffOutcome::ProviderError(detail) => (
            format!("Handoff compaction failed; the transcript is unchanged: {detail}"),
            json!({ "outcome": "provider_error", "detail": detail }),
        ),
        HandoffOutcome::Cancelled => (
            "Handoff compaction was cancelled; the transcript is unchanged".to_string(),
            json!({ "outcome": "cancelled" }),
        ),
    }
}

/// Run a blocking compaction off the async worker that drives the supervisor.
///
/// The supervisor runs on a spawned task whose runtime may be current-thread,
/// where `block_in_place` would panic.
fn off_worker<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

#[cfg(test)]
#[path = "overnight_handoff_tests.rs"]
mod overnight_handoff_tests;

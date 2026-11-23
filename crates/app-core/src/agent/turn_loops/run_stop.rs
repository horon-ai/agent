use super::super::*;
use horon_agent_agent_runtime::{
    RunIdentity, RunStop, StopProvenance, current_run, current_stop, note_conversation_save, observe_cause,
    observed_cause, provider_failure_provenance,
};

/// How long a reload marker still describes this process's own shutdown.
const RELOAD_MARKER_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(120);

/// The stop cause a failed turn names, read from the error it returned.
///
/// A context, token, or plan limit ends a turn through the same error path as a
/// provider fault, so the message decides between them. Every other failure is
/// what the provider reported.
pub(in crate::agent) fn cause_for_turn_failure(message: &str) -> StopProvenance {
    let lower = message.to_ascii_lowercase();
    if lower.contains("context limit") || lower.contains("token limit") || lower.contains("plan limit") {
        StopProvenance::Limit
    } else {
        provider_failure_provenance(message)
    }
}

impl Agent {
    /// Mint the run id a newly triggered input executes under.
    ///
    /// The id is a UUID v4 because the journal rejects any other spelling, so
    /// this is the one place a run id is created.
    pub(in crate::agent) fn mint_run_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Record a cause the loop observed at a point that ends the turn.
    ///
    /// Only a terminal observation belongs here: a retryable provider failure the
    /// turn recovered from is not a cause, so no path that continues the loop
    /// records one.
    pub(in crate::agent) fn observe_stop_cause(&self, provenance: StopProvenance) {
        observe_cause(&self.session.id, provenance);
    }

    /// Record whether this turn's conversation save succeeded.
    pub(in crate::agent) fn note_turn_conversation_save(&self, saved: bool) {
        note_conversation_save(&self.session.id, saved);
    }

    /// The cause this turn's run stopped for, from what the turn observed.
    ///
    /// An observation the loop made at the exact point the turn ended wins,
    /// because the outcome alone cannot tell a loop-guard stop from a normal one.
    /// Otherwise the recorded outcome names it: a truncation that spent its
    /// continuation budget is a limit, an abort is a shutdown only when this
    /// process is reloading and a cancel otherwise, and a failure is whatever its
    /// own error says.
    pub(in crate::agent) fn stop_cause_for(&self, error: Option<&str>) -> StopProvenance {
        if let Some(observed) = observed_cause(&self.session.id) {
            return observed;
        }
        match self.turn_state().outcome() {
            Some(TurnOutcome::Stop) => StopProvenance::Completed,
            Some(TurnOutcome::Length) => StopProvenance::Limit,
            Some(TurnOutcome::Aborted) => {
                if crate::server::reload_marker_active(RELOAD_MARKER_MAX_AGE) {
                    StopProvenance::Shutdown
                } else {
                    StopProvenance::UserCancelled
                }
            }
            Some(TurnOutcome::Error) => match error {
                Some(message) => cause_for_turn_failure(message),
                None => StopProvenance::ProviderError,
            },
            None => StopProvenance::InterruptedUnknown,
        }
    }

    /// The run executing now, if a run was started.
    pub fn current_run(&self) -> Option<RunIdentity> {
        current_run(&self.session.id)
    }

    /// The current run's stop, if it ended.
    pub fn current_stop(&self) -> Option<RunStop> {
        current_stop(&self.session.id)
    }

    /// Stops this session kept because their run was no longer current.
    pub fn stop_history(&self) -> Vec<RunStop> {
        horon_agent_agent_runtime::stop_history(&self.session.id)
    }
}

use super::*;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

/// Called only after automatic continuations have been exhausted.
pub(super) fn incomplete_turn_stop(stop_reason: Option<&str>) -> Option<ServerEvent> {
    let reason = stop_reason?;
    if Agent::should_continue_after_stop_reason(reason) || Agent::is_stranded_tool_use_stop(Some(reason)) {
        Some(ServerEvent::TurnStopped {
            reason: crate::protocol::TurnStopReason::LimitReached,
            message: format!(
                "The provider stopped with {reason} after automatic continuation attempts were exhausted. Output may be incomplete."
            ),
            provider_stop_reason: Some(reason.to_string()),
        })
    } else {
        None
    }
}

impl Agent {
    pub(in crate::agent) async fn run_turn_streaming_mpsc(
        &mut self,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()> {
        // Loop detection is per turn, and a reminder this turn's guard set must
        // not reach the next turn.
        self.loop_guard.reset();
        self.begin_turn_state();
        // Register this turn's cancel signal for the whole turn, including the
        // outcome bookkeeping below: the guard resets the signal when it drops,
        // so a scope that ended before the outcome was named would report every
        // cancelled turn as a plain stop.
        let _turn_cancel_guard =
            crate::turn_cancel_registry::register_active_turn(&self.session.id, self.graceful_shutdown.clone());
        let mut in_flight = Some(InFlightGuard::new(
            self.session.id.clone(),
            self.soft_interrupt_queue.clone(),
        ));
        let deadline = self
            .swarm_budget
            .as_mut()
            .and_then(super::swarm_budget::ChildBudget::deadline);
        let (outcome, expired_events) = if let Some(deadline) = deadline {
            let cancel = self.graceful_shutdown.clone();
            let turn = AssertUnwindSafe(self.run_turn_streaming_mpsc_inner(event_tx.clone(), in_flight.as_mut()))
                .catch_unwind();
            let (outcome, timed_out) = super::swarm_budget::run_until_deadline(turn, deadline, cancel).await;
            (outcome, timed_out.then_some(event_tx))
        } else {
            let outcome = AssertUnwindSafe(self.run_turn_streaming_mpsc_inner(event_tx, in_flight.as_mut()))
                .catch_unwind()
                .await;
            (outcome, None)
        };
        self.current_turn_system_reminder = None;
        match outcome {
            Ok(result) => {
                let result = if let Some(events) = expired_events {
                    Err(self.stop_child_at_limit(&events, "Swarm child runtime limit reached"))
                } else {
                    result
                };
                self.settle_soft_interrupt_turn(&mut in_flight, result.is_ok() && !self.is_graceful_shutdown())
                    .await;
                self.settle_streaming_turn_outcome(result.is_err());
                result
            }
            Err(payload) => {
                // A panic never reaches the normal settle point. Roll the
                // injected messages back here, then let the unwind continue
                // so the caller still observes the panic it caused.
                self.settle_soft_interrupt_turn(&mut in_flight, false).await;
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Record the streaming turn's exit reason when its loop left one open.
    fn settle_streaming_turn_outcome(&mut self, failed: bool) {
        if self.turn_state().outcome().is_some() {
            return;
        }
        let outcome = if failed {
            TurnOutcome::Error
        } else if self.is_graceful_shutdown() {
            TurnOutcome::Aborted
        } else {
            TurnOutcome::Stop
        };
        if let Err(error) = self.turn_state.finish(outcome) {
            logging::warn(&format!("Turn outcome not recorded: {error}"));
        }
    }
}

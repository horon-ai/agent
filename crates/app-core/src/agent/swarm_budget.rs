use super::Agent;
use crate::protocol::{ServerEvent, TurnStopReason};
use horon_agent_agent_runtime::{InterruptSignal, StopProvenance};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub(super) struct ChildBudget {
    runtime: Duration,
    deadline: Option<Instant>,
    requests_left: Option<u32>,
    request_notice: bool,
}

impl ChildBudget {
    pub(super) fn configured() -> Option<Self> {
        let config = &crate::config::config().agents;
        if config.swarm_child_runtime_secs == 0 && config.swarm_child_request_limit == 0 {
            return None;
        }
        Some(Self {
            runtime: Duration::from_secs(config.swarm_child_runtime_secs.into()),
            deadline: None,
            requests_left: (config.swarm_child_request_limit > 0).then_some(config.swarm_child_request_limit),
            request_notice: config.swarm_child_request_notice,
        })
    }

    pub(super) fn deadline(&mut self) -> Option<Instant> {
        (!self.runtime.is_zero()).then(|| *self.deadline.get_or_insert_with(|| Instant::now() + self.runtime))
    }

    pub(super) fn charge_request(&mut self) {
        if let Some(remaining) = &mut self.requests_left {
            *remaining -= 1;
        }
    }
}

impl Agent {
    pub(super) fn prepare_child_request(&mut self, events: &mpsc::UnboundedSender<ServerEvent>) -> anyhow::Result<()> {
        let Some(budget) = &self.swarm_budget else {
            return Ok(());
        };
        match budget.requests_left {
            Some(0) => Err(self.stop_child_at_limit(events, "Swarm child model request limit reached")),
            Some(1) if budget.request_notice => {
                const NOTICE: &str = "This is your last model request in this swarm child. Return your findings now.";
                Self::merge_system_reminder(&mut self.current_turn_system_reminder, NOTICE);
                if events
                    .send(ServerEvent::StatusDetail {
                        detail: NOTICE.to_string(),
                    })
                    .is_err()
                {
                    crate::logging::debug("Swarm child request budget notice has no event receiver");
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub(super) fn stop_child_at_limit(
        &self,
        events: &mpsc::UnboundedSender<ServerEvent>,
        message: &str,
    ) -> anyhow::Error {
        self.observe_stop_cause(StopProvenance::Limit);
        if events
            .send(ServerEvent::TurnStopped {
                reason: TurnStopReason::LimitReached,
                message: message.to_string(),
                provider_stop_reason: None,
            })
            .is_err()
        {
            crate::logging::debug("Swarm child limit stop has no event receiver");
        }
        anyhow::anyhow!("{message}")
    }
}

/// Let the regular turn cleanup finish after notifying its own cancellation signal.
pub(super) async fn run_until_deadline<T>(
    turn: impl Future<Output = T>,
    deadline: Instant,
    cancel: InterruptSignal,
) -> (T, bool) {
    if Instant::now() >= deadline {
        cancel.fire();
        return (turn.await, true);
    }
    let mut turn = std::pin::pin!(turn);
    tokio::select! {
        biased;
        output = &mut turn => (output, false),
        _ = tokio::time::sleep_until(deadline) => {
            cancel.fire();
            (turn.await, true)
        }
    }
}

#[cfg(test)]
mod tests;

use super::*;

/// What the turn loop should do after a provider error.
pub(super) enum ProviderErrorAction {
    /// Resend the request: the route changed, or a bounded rate-limit wait
    /// just elapsed. `attempt`/`max` describe that recovery attempt.
    Resend { attempt: u32, max: u32 },
    /// Fall through to the loop's pre-existing error handling.
    Proceed,
    /// End the turn with this error.
    Stop(anyhow::Error),
}

/// What a turn loop should do after a provider error that may be a typed
/// automatic-failover request.
pub(super) enum FailoverAttempt {
    /// Not an autonomous failover: no typed prompt, the feature is off, or a
    /// client is attached. The caller keeps its existing error path unchanged.
    NotApplicable,
    /// The route changed; the caller must resend the same turn.
    Retried { attempt: u32, max: u32 },
    /// Automatic failover applies but the chain cannot continue.
    Exhausted(anyhow::Error),
}

impl Agent {
    /// Start the failover attempt chain a new user prompt owns.
    ///
    /// Retries inside one turn keep this chain, so the cap counts switches per
    /// prompt rather than per process. With automatic failover off nothing is
    /// written: no attempt can be spent and every turn would otherwise pay for
    /// a state file it never reads.
    pub(super) fn begin_failover_chain_for_prompt(&mut self) {
        self.restore_model_fallback();
        if crate::config::config().provider.autonomous_failover {
            let _ = self.begin_failover_chain();
        }
    }

    /// `Retry-After` the provider already parsed out of its response headers.
    ///
    /// A turn loop never holds a `HeaderMap`: the runtimes convert the header
    /// into `StreamError::retry_after_secs` before the error reaches the loop,
    /// so this is the header's value and not a guess read back out of the text.
    fn retry_after_hint(error: &anyhow::Error) -> Option<Duration> {
        error
            .downcast_ref::<StreamError>()
            .and_then(|stream_error| stream_error.retry_after_secs)
            .map(Duration::from_secs)
    }

    /// The retry decision for a rate-limit rejection, or `None` when the error
    /// is not one.
    pub(super) fn rate_limit_decision(error: &anyhow::Error) -> Option<crate::network_retry::RetryAfterDecision> {
        crate::network_retry::classify_rate_limit_error(error.as_ref())?;
        let provider = &crate::config::config().provider;
        Some(crate::network_retry::retry_after_decision(
            Self::retry_after_hint(error),
            provider.retry_backoff_cap_secs,
            provider.autonomous_failover,
        ))
    }

    /// Apply the provider-error policy for one failed request.
    ///
    /// A rate-limit rejection is resolved by waiting at most
    /// `retry_backoff_cap_secs`, or, when the header asks for longer than that
    /// cap and autonomous failover is on, by switching routes instead. Those
    /// waits spend `max_retries`, never the failover budget. A typed
    /// `FailoverRequired` error switches without a client attached. Every other
    /// error keeps the loop's pre-existing handling, so a failure that is
    /// neither a rate limit nor a failover request behaves exactly as before.
    pub(super) async fn handle_provider_error(
        &mut self,
        error: &anyhow::Error,
        attached_client: bool,
    ) -> ProviderErrorAction {
        // A typed failover prompt decides on its own first. Its display text
        // usually quotes the provider reason ("429 ..."), so classifying it as
        // a rate limit would add a wait the attended countdown path never had.
        if crate::provider::failover_required_from_error(error).is_some() {
            if let Some(action) = self.try_model_fallback(error) {
                return action;
            }
            return match self.attempt_autonomous_failover(error, attached_client).await {
                FailoverAttempt::Retried { attempt, max } => ProviderErrorAction::Resend { attempt, max },
                FailoverAttempt::Exhausted(error) => ProviderErrorAction::Stop(error),
                FailoverAttempt::NotApplicable => ProviderErrorAction::Proceed,
            };
        }
        let Some(decision) = Self::rate_limit_decision(error) else {
            // Not a rate limit: the provider runtime already spent its own
            // retry budget before surfacing this, so the model list is next.
            return self.model_fallback_or_proceed(error);
        };
        let provider = &crate::config::config().provider;
        let wait = match decision {
            crate::network_retry::RetryAfterDecision::Wait(delay) => Some(delay),
            crate::network_retry::RetryAfterDecision::FailoverInstead => {
                // The next model is a cheaper move than another account or
                // provider: it resends the same input to the same endpoint.
                if let Some(action) = self.try_model_fallback(error) {
                    return action;
                }
                match self.attempt_autonomous_failover(error, attached_client).await {
                    FailoverAttempt::Retried { attempt, max } => {
                        return ProviderErrorAction::Resend { attempt, max };
                    }
                    FailoverAttempt::Exhausted(error) => return ProviderErrorAction::Stop(error),
                    // No typed prompt to switch to: wait at the cap instead of
                    // the longer delay the provider asked for.
                    FailoverAttempt::NotApplicable => Some(Duration::from_secs(provider.retry_backoff_cap_secs)),
                }
            }
            crate::network_retry::RetryAfterDecision::Backoff => None,
        };
        match wait {
            Some(_) if self.turn_state.rate_limit_retries() >= provider.max_retries => {
                self.model_fallback_or_proceed(error)
            }
            Some(delay) => {
                let attempt = self.turn_state.record_rate_limit_retry();
                logging::info(&format!(
                    "Rate limited: waiting {:.1}s before retry {}/{}",
                    delay.as_secs_f64(),
                    attempt,
                    provider.max_retries
                ));
                tokio::time::sleep(delay).await;
                ProviderErrorAction::Resend {
                    attempt,
                    max: provider.max_retries,
                }
            }
            None => ProviderErrorAction::Proceed,
        }
    }

    /// Turn a typed `FailoverRequired` provider error into an actual route
    /// switch when no client can approve the interactive countdown (GAP-3).
    ///
    /// The attempt is spent before the switch is applied, so a refused switch
    /// cannot loop, and the typed prompt is read before any `to_string()`
    /// flattens the error. `attached_client` is passed in rather than read here
    /// so the caller's gate stays visible at the call site.
    pub(super) async fn attempt_autonomous_failover(
        &mut self,
        error: &anyhow::Error,
        attached_client: bool,
    ) -> FailoverAttempt {
        let Some(prompt) = crate::provider::failover_required_from_error(error) else {
            return FailoverAttempt::NotApplicable;
        };
        let provider_config = &crate::config::config().provider;
        if !provider_config.autonomous_failover || attached_client {
            return FailoverAttempt::NotApplicable;
        }
        let max = provider_config.autonomous_failover_max_attempts;
        let chain_id = match self.failover_chain_id() {
            Some(chain_id) => chain_id.to_string(),
            None => self.begin_failover_chain(),
        };
        if !self.consume_failover_attempt(None, max) {
            let attempts = self.failover_attempts();
            let reason = if self.failover_budget_unknown() {
                "the persisted failover budget cannot be trusted"
            } else {
                "failover attempts exhausted"
            };
            return FailoverAttempt::Exhausted(anyhow::anyhow!(
                "Automatic provider failover stopped: {reason} after {attempts} attempt(s) in chain {chain_id} (cap {max}); last provider error: {error}"
            ));
        }
        // The attempt is spent before the switch is applied, on purpose. A
        // refused switch (ineligible route, unknown spec) leaves the route
        // where it was, so spending after the fact would let a session whose
        // targets are all ineligible retry the same refusal without a bound.
        // The cost is that such a session burns its budget without switching;
        // that is the conservative direction, and the error names the cap.
        let attempt = self.failover_attempts();
        let from = self.provider_model();
        if let Err(switch_error) = crate::server::apply_autonomous_failover(prompt, self).await {
            logging::warn(&format!("Autonomous failover was not applied: {switch_error}"));
            return FailoverAttempt::NotApplicable;
        }
        let to = self.provider_model();
        // `emit_session_event` records the announcement in `failover-events.json`
        // before it hands the event to any sink, so a crash between the switch
        // and delivery still re-announces it on the next start. Only a client
        // `FailoverAck` clears that record; emitting never does.
        let _ = crate::server::emit_session_event(
            &self.session.id,
            ServerEvent::FailoverSwitched {
                chain_id,
                from,
                to,
                reason: prompt.reason.clone(),
                attempt,
                max,
            },
        );
        FailoverAttempt::Retried { attempt, max }
    }
}

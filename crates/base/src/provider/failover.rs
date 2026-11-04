use super::*;
use horon_agent_provider_core::{FailoverDecision, ProviderFailoverPrompt};

impl MultiProvider {
    /// Eligibility must be identical for saved-account and catalog candidates.
    /// Unknown capacity is not a safe escape hatch after known windows fail.
    pub(super) fn failover_model_fits(model: &str, provider: &str, required_context: usize) -> bool {
        models::is_agent_model_for_failover(model, provider)
            && models::context_limit_for_model_with_provider(model, Some(provider))
                .is_some_and(|limit| limit >= required_context)
    }

    pub(super) fn provider_is_configured(&self, provider: ActiveProvider) -> bool {
        self.reconcile_auth_if_provider_missing(provider)
    }

    pub(super) fn build_failover_prompt(
        &self,
        from: ActiveProvider,
        to: ActiveProvider,
        reason: String,
        estimated_input_chars: usize,
        estimated_input_tokens: usize,
    ) -> ProviderFailoverPrompt {
        ProviderFailoverPrompt {
            from_provider: Self::provider_key(from).to_string(),
            from_label: Self::provider_label(from).to_string(),
            to_provider: Self::provider_key(to).to_string(),
            to_label: Self::provider_label(to).to_string(),
            target_route: None,
            reason,
            estimated_input_chars,
            estimated_input_tokens,
        }
    }

    pub(super) fn fallback_sequence(active: ActiveProvider) -> Vec<ActiveProvider> {
        horon_agent_provider_core::fallback_sequence(active)
    }

    pub(super) fn summarize_error(err: &anyhow::Error) -> String {
        err.to_string()
            .lines()
            .next()
            .unwrap_or("unknown error")
            .trim()
            .to_string()
    }

    pub(super) fn classify_failover_error(err: &anyhow::Error) -> FailoverDecision {
        horon_agent_provider_core::classify_failover_error_message(&err.to_string())
    }

    pub(super) fn additional_no_provider_guidance(&self) -> Vec<String> {
        [ActiveProvider::OpenAI]
            .into_iter()
            .filter_map(crate::provider::account_failover::account_switch_guidance)
            .collect()
    }

    pub(super) fn no_provider_available_error(&self, notes: &[String]) -> anyhow::Error {
        let mut msg = "OpenAI usage may be exhausted or its credentials may be unavailable.".to_string();
        if !notes.is_empty() {
            msg.push_str(" Details: ");
            msg.push_str(&notes.join(" | "));
        }
        let extra_guidance = self.additional_no_provider_guidance();
        if !extra_guidance.is_empty() {
            msg.push(' ');
            msg.push_str(&extra_guidance.join(" "));
        }
        msg.push_str(" Use `/usage` to check limits and `/login <provider>` to re-authenticate.");
        anyhow::anyhow!(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> ProviderFailoverPrompt {
        ProviderFailoverPrompt {
            from_provider: "claude".to_string(),
            from_label: "Anthropic".to_string(),
            to_provider: "openai".to_string(),
            to_label: "[OI]".to_string(),
            target_route: Some("openai-oauth@work:gpt-5.4".into()),
            reason: "rate limit".to_string(),
            estimated_input_chars: 1200,
            estimated_input_tokens: 300,
        }
    }

    #[test]
    fn typed_failover_error_keeps_the_text_form() {
        let prompt = prompt();
        let text = prompt.to_error_message();
        let error = anyhow::Error::new(FailoverRequired { prompt: prompt.clone() });

        assert_eq!(error.to_string(), text);
        assert_eq!(parse_failover_prompt_message(&error.to_string()), Some(prompt.clone()));
        assert_eq!(failover_required_from_error(&error), Some(&prompt));
    }

    #[test]
    fn plain_error_is_not_a_failover_signal() {
        let error = anyhow::anyhow!("boom");

        assert!(failover_required_from_error(&error).is_none());
        assert!(parse_failover_prompt_message(&error.to_string()).is_none());
    }
}

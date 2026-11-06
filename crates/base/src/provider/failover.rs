use super::*;
use horon_agent_provider_core::FailoverDecision;

impl MultiProvider {
    pub(super) fn provider_is_configured(&self, provider: ActiveProvider) -> bool {
        self.reconcile_auth_if_provider_missing(provider)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use horon_agent_provider_core::ProviderFailoverPrompt;

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

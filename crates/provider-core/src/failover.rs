use std::fmt;

use serde::{Deserialize, Serialize};

const PROVIDER_FAILOVER_PROMPT_PREFIX: &str = "[horon-agent-provider-failover]";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderFailoverPrompt {
    pub from_provider: String,
    pub from_label: String,
    pub to_provider: String,
    pub to_label: String,
    /// Exact pool-approved model request. Old prompts without a route require manual consent.
    #[serde(default)]
    pub target_route: Option<String>,
    pub reason: String,
    pub estimated_input_chars: usize,
    pub estimated_input_tokens: usize,
}

impl ProviderFailoverPrompt {
    pub fn to_error_message(&self) -> String {
        let payload = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        format!(
            "{PROVIDER_FAILOVER_PROMPT_PREFIX}{payload}\n{} is unavailable; switching to {} would resend about {} input tokens (~{} chars).",
            self.from_label, self.to_label, self.estimated_input_tokens, self.estimated_input_chars,
        )
    }
}

pub fn parse_failover_prompt_message(message: &str) -> Option<ProviderFailoverPrompt> {
    let line = message.lines().next()?.trim();
    let json = line.strip_prefix(PROVIDER_FAILOVER_PROMPT_PREFIX)?;
    serde_json::from_str(json).ok()
}

/// Typed carrier for a [`ProviderFailoverPrompt`].
///
/// [`Display`](fmt::Display) renders exactly the text that
/// [`ProviderFailoverPrompt::to_error_message`] produces, so callers that only
/// read the error string keep their current behaviour while new callers can
/// downcast the same error with [`failover_required_from_error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverRequired {
    pub prompt: ProviderFailoverPrompt,
}

impl FailoverRequired {
    /// Wrap the prompt the provider itself produced for the route it offers.
    ///
    /// The prompt is carried unchanged, including a `target_route` when the
    /// provider named one, and is read back with [`Self::prompt`].
    pub fn new(prompt: ProviderFailoverPrompt) -> Self {
        Self { prompt }
    }

    /// The prompt exactly as it arrived.
    ///
    /// An older prompt has no `target_route`; such a switch predates
    /// route-carrying failover and cannot be applied without a human's consent.
    pub fn prompt(&self) -> &ProviderFailoverPrompt {
        &self.prompt
    }
}

impl fmt::Display for FailoverRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.prompt.to_error_message())
    }
}

impl std::error::Error for FailoverRequired {}

/// Recover the prompt from an error that carries a [`FailoverRequired`].
///
/// The lookup walks the whole `anyhow` context chain, so wrapping the error
/// with `.context(..)` before it reaches a turn loop does not hide it.
pub fn failover_required_from_error(error: &anyhow::Error) -> Option<&ProviderFailoverPrompt> {
    error.downcast_ref::<FailoverRequired>().map(FailoverRequired::prompt)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailoverDecision {
    None,
    RetryNextProvider,
    RetryAndMarkUnavailable,
}

impl FailoverDecision {
    pub fn should_failover(self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn should_mark_provider_unavailable(self) -> bool {
        matches!(self, Self::RetryAndMarkUnavailable)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RetryNextProvider => "retry-next-provider",
            Self::RetryAndMarkUnavailable => "retry-and-mark-unavailable",
        }
    }
}

fn contains_independent_status_code(haystack: &str, code: &str) -> bool {
    let haystack_bytes = haystack.as_bytes();
    let code_len = code.len();

    haystack.match_indices(code).any(|(start, _)| {
        let before_ok = start == 0 || !haystack_bytes[start - 1].is_ascii_digit();
        let end = start + code_len;
        let after_ok = end == haystack_bytes.len() || !haystack_bytes[end].is_ascii_digit();
        before_ok && after_ok
    })
}

pub fn classify_failover_error_message(message: &str) -> FailoverDecision {
    let lower = message.to_ascii_lowercase();

    let request_size_or_context = [
        "context length",
        "context_length",
        "context window",
        "maximum context",
        "prompt is too long",
        "input is too long",
        "too many tokens",
        "max tokens",
        "token limit",
        "token_limit",
        "413 payload too large",
        "413 request entity too large",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || contains_independent_status_code(&lower, "413");
    if request_size_or_context {
        return FailoverDecision::RetryNextProvider;
    }

    let rate_or_quota = [
        "rate limit",
        "rate-limited",
        "too many requests",
        "quota",
        "credit balance",
        "credits have run out",
        "insufficient credit",
        "billing",
        "payment required",
        "usage tier",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || contains_independent_status_code(&lower, "429")
        || contains_independent_status_code(&lower, "402");
    if rate_or_quota {
        return FailoverDecision::RetryAndMarkUnavailable;
    }

    let auth_or_access = [
        "access denied",
        "not accessible by integration",
        "provider unavailable",
        "provider not available",
        "provider is unavailable",
        "provider currently unavailable",
        "provider not configured",
        "credentials are not configured",
        "no credentials",
        "token exchange failed",
        // Cached permanent refresh rejection no longer carries the original
        // OAuth error code or HTTP status, but still needs account failover.
        "refresh token was previously rejected",
        "authentication failed",
        "unauthorized",
        "forbidden",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || contains_independent_status_code(&lower, "401")
        || contains_independent_status_code(&lower, "403");
    if auth_or_access {
        return FailoverDecision::RetryAndMarkUnavailable;
    }

    FailoverDecision::None
}

/// Whether a failed model call may be retried on the next `fallback_models` entry.
///
/// Only a capacity or availability signal on the model itself is worth a second
/// model on the same endpoint: rate limits and 5xx, an overloaded backend, or an
/// id the endpoint does not serve. Credential, request-shape, and request-size
/// errors are decided by the caller and must not spend the fallback list.
pub fn classify_model_fallback_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();

    let request_or_credential = [
        "invalid_request",
        "invalid request",
        "context length",
        "context_length",
        "context window",
        "maximum context",
        "prompt is too long",
        "input is too long",
        "too many tokens",
        "token limit",
        "token_limit",
        "unauthorized",
        "forbidden",
        "authentication failed",
        "invalid api key",
        "invalid_api_key",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || ["400", "401", "403", "413"]
            .iter()
            .any(|code| contains_independent_status_code(&lower, code));
    if request_or_credential {
        return false;
    }

    let model_availability = [
        "overloaded",
        "service unavailable",
        "model not found",
        "model_not_found",
        "model unavailable",
        "model is not available",
        "model does not exist",
        "no such model",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    model_availability
        || ["429", "500", "502", "503", "504"]
            .iter()
            .any(|code| contains_independent_status_code(&lower, code))
}

#[cfg(test)]
#[path = "model_fallback_tests.rs"]
mod model_fallback_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_prompt() -> ProviderFailoverPrompt {
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
    fn failover_required_display_matches_the_text_form_byte_for_byte() {
        let prompt = sample_prompt();
        let text = prompt.to_error_message();
        let error = anyhow::Error::new(FailoverRequired::new(prompt.clone()));

        assert_eq!(error.to_string(), text);
        assert_eq!(failover_required_from_error(&error), Some(&prompt));
        // Both forms read the same error: text parsers and downcast callers
        // coexist without a migration.
        assert_eq!(parse_failover_prompt_message(&error.to_string()), Some(prompt));
    }

    #[test]
    fn failover_required_helper_returns_none_for_an_unrelated_error() {
        let error = anyhow::anyhow!("boom");

        assert!(failover_required_from_error(&error).is_none());
    }

    #[test]
    fn failover_required_survives_an_anyhow_context_chain() {
        let prompt = sample_prompt();
        let error = anyhow::Error::new(FailoverRequired::new(prompt.clone())).context("turn failed");

        assert_eq!(error.to_string(), "turn failed");
        assert_eq!(failover_required_from_error(&error), Some(&prompt));
    }

    #[test]
    fn failover_prompt_roundtrips_from_error_message() {
        let prompt = ProviderFailoverPrompt {
            from_provider: "claude".to_string(),
            from_label: "Anthropic".to_string(),
            to_provider: "openai".to_string(),
            to_label: "OpenAI".to_string(),
            target_route: Some("openai-oauth@work:gpt-5.4".into()),
            reason: "rate limit".to_string(),
            estimated_input_chars: 1200,
            estimated_input_tokens: 300,
        };

        let parsed = parse_failover_prompt_message(&prompt.to_error_message()).expect("prompt");
        assert_eq!(parsed, prompt);
    }

    #[test]
    fn legacy_prompt_without_target_route_requires_manual_selection() {
        let wire = r#"[horon-agent-provider-failover]{"from_provider":"claude","from_label":"Anthropic","to_provider":"openai","to_label":"OpenAI","reason":"rate limit","estimated_input_chars":1200,"estimated_input_tokens":300}"#;
        let prompt = parse_failover_prompt_message(wire).expect("legacy prompt");
        assert_eq!(prompt.target_route, None);
    }

    #[test]
    fn classifier_marks_rate_limits_unavailable() {
        assert_eq!(
            classify_failover_error_message("429 Too Many Requests"),
            FailoverDecision::RetryAndMarkUnavailable
        );
    }

    #[test]
    fn classifier_marks_cached_refresh_rejection_unavailable() {
        for message in [
            "Claude OAuth refresh token was previously rejected by Anthropic and cannot be refreshed. Run `horon-agent login --provider claude` to mint a fresh token.",
            "claude refresh token was previously rejected by the provider and cannot be refreshed. Run `horon-agent login`.",
            "Failed to load credentials: REFRESH TOKEN WAS PREVIOUSLY REJECTED by the provider",
        ] {
            assert_eq!(
                classify_failover_error_message(message),
                FailoverDecision::RetryAndMarkUnavailable,
                "cached permanent refresh rejection must permit account failover: {message}",
            );
        }
        // Generic refresh failures are not evidence of invalid credentials.
        for message in [
            "Claude token refresh failed: 503 Service Unavailable",
            "Claude token refresh failed: connection reset by peer",
            "Claude token refresh failed: request timed out",
        ] {
            assert_eq!(
                classify_failover_error_message(message),
                FailoverDecision::None,
                "transient refresh failure must not poison an account: {message}",
            );
        }
    }

    #[test]
    fn classifier_retries_context_errors_without_marking_unavailable() {
        assert_eq!(
            classify_failover_error_message("context length exceeded"),
            FailoverDecision::RetryNextProvider
        );
    }

    #[test]
    fn classifier_ignores_embedded_status_digits() {
        assert_eq!(
            classify_failover_error_message("model version 4130 failed"),
            FailoverDecision::None
        );
    }
}

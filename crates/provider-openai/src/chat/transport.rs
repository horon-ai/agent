//! HTTP transport for direct Chat Completions requests.
//!
//! All providers share one reqwest client and one transport-fault vocabulary, so
//! this module only adds the pieces a direct OpenAI-compatible endpoint needs:
//! how the credential rides on the request, how a failed status becomes an error
//! message the agent's failover classifier recognizes, and how a secret is kept
//! out of every message that leaves this crate.

use anyhow::Result;
use horon_agent_provider_core::retry_after::{RetryAfter, error_with_retry_after};
use horon_agent_provider_core::transport::is_transient_transport_error;
use reqwest::header::{HeaderName, HeaderValue};
use std::time::Duration;

/// Longest wait for response headers. Streaming body reads are not bounded by
/// this; the agent's own stream handling owns that budget.
pub const INITIAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

/// Upper bound on an upstream error body kept in an error message.
pub const MAX_ERROR_BODY_CHARS: usize = 2000;

/// Replacement text for a credential that would otherwise reach a message.
pub const REDACTED: &str = "[redacted]";

/// The display label Chat Completions failures have always opened with.
///
/// Other wires pass their own label to [`http_response_error_labeled`] and
/// [`transport_error_message_labeled`]; everything after the label keeps the
/// same shape so the shared failover classifier reads all four alike.
pub(crate) const CHAT_LABEL: &str = "OpenAI-compatible";

/// How a profile presents its credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAuth {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `<header>: <key>`, for endpoints that use their own header name.
    Header { header: String },
    /// No credential header at all.
    None,
}

/// Attach the profile's credential to a request.
///
/// A missing or blank key leaves the request unauthenticated instead of sending
/// an empty credential: `auth = "none"` profiles legitimately have no key, and an
/// unauthenticated request gets a clear 401 from the endpoint.
pub fn apply_auth(
    request: reqwest::RequestBuilder,
    auth: &ChatAuth,
    api_key: Option<&str>,
) -> Result<reqwest::RequestBuilder> {
    let Some(key) = api_key.filter(|key| !key.trim().is_empty()) else {
        return Ok(request);
    };
    match auth {
        ChatAuth::None => Ok(request),
        ChatAuth::Bearer => {
            let value = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                anyhow::anyhow!(
                    "the configured API key cannot be sent as an Authorization header value (it contains characters HTTP headers do not allow)"
                )
            })?;
            Ok(request.header(reqwest::header::AUTHORIZATION, value))
        }
        ChatAuth::Header { header } => {
            let name = HeaderName::from_bytes(header.as_bytes())
                .map_err(|_| anyhow::anyhow!("the openai-compatible profile configured an invalid auth header name"))?;
            let value = HeaderValue::from_str(key).map_err(|_| {
                anyhow::anyhow!(
                    "the configured API key cannot be sent as the configured auth header value (it contains characters HTTP headers do not allow)"
                )
            })?;
            Ok(request.header(name, value))
        }
    }
}

/// How a failed HTTP status is described, so callers and logs agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpErrorKind {
    Authentication,
    RateLimited,
    Server,
    Client,
}

/// Classify a failed HTTP status.
pub fn classify_http_status(status: u16) -> HttpErrorKind {
    match status {
        401 | 403 => HttpErrorKind::Authentication,
        429 => HttpErrorKind::RateLimited,
        status if status >= 500 => HttpErrorKind::Server,
        _ => HttpErrorKind::Client,
    }
}

/// Build the error for a non-success HTTP response.
///
/// The wording carries the markers the shared classifiers in
/// [`horon_agent_provider_core::failover`] and
/// [`horon_agent_provider_core::fallback_pick`] key on ("rate limit",
/// "authentication failed", the status code), and a server-provided retry delay
/// is preserved when the response carried one.
pub fn http_response_error(
    endpoint: &str,
    status: u16,
    retry_after: Option<RetryAfter>,
    body: &str,
    api_key: Option<&str>,
) -> anyhow::Error {
    http_response_error_with_hint(endpoint, status, retry_after, body, None, api_key)
}

/// [`http_response_error`], also carrying what the schema-recovery layer learned
/// about the failure.
///
/// The hint is appended after the truncated body so it survives the cut: a
/// provider error body is not something horon-agent controls, and the one line
/// saying whether a retry can fix it must not be the part that is dropped.
pub fn http_response_error_with_hint(
    endpoint: &str,
    status: u16,
    retry_after: Option<RetryAfter>,
    body: &str,
    hint: Option<&str>,
    api_key: Option<&str>,
) -> anyhow::Error {
    http_response_error_labeled(CHAT_LABEL, endpoint, status, retry_after, body, hint, api_key)
}

/// [`http_response_error_with_hint`], naming the wire the failure came from.
///
/// Every wire keeps the same sentence shape because the shared classifiers key
/// on the words ("authentication failed", "rate limit", the status code), not on
/// the prefix; only the label that opens the sentence changes. With
/// [`CHAT_LABEL`] the produced text is byte-identical to what the Chat
/// Completions path has always emitted.
pub(crate) fn http_response_error_labeled(
    label: &str,
    endpoint: &str,
    status: u16,
    retry_after: Option<RetryAfter>,
    body: &str,
    hint: Option<&str>,
    api_key: Option<&str>,
) -> anyhow::Error {
    let body = if body.trim().is_empty() {
        "<empty response body>".to_string()
    } else {
        snippet(body, MAX_ERROR_BODY_CHARS)
    };
    let body = match hint {
        Some(hint) => format!("{body} [{hint}]"),
        None => body,
    };
    let message = match classify_http_status(status) {
        HttpErrorKind::Authentication => {
            format!("{label} provider authentication failed (HTTP {status}) at {endpoint}: {body}")
        }
        HttpErrorKind::RateLimited => {
            format!("{label} provider rate limit exceeded (HTTP 429) at {endpoint}: {body}")
        }
        HttpErrorKind::Server => {
            format!("{label} provider server error (HTTP {status}) at {endpoint}: {body}")
        }
        HttpErrorKind::Client => {
            format!("{label} provider request failed (HTTP {status}) at {endpoint}: {body}")
        }
    };
    error_with_retry_after(redact_secret(message, api_key), retry_after)
}

/// Describe a transport-level failure, marking transient faults the shared
/// classifier recognizes.
pub fn transport_error_message(endpoint: &str, error: &str) -> String {
    transport_error_message_labeled(CHAT_LABEL, endpoint, error)
}

/// [`transport_error_message`], naming the wire the failure came from.
pub(crate) fn transport_error_message_labeled(label: &str, endpoint: &str, error: &str) -> String {
    let message = format!("{label} provider transport error at {endpoint}: {error}");
    if is_transient_transport_error(error) {
        format!("{message} (transient transport error)")
    } else {
        message
    }
}

/// Remove the profile credential from a message before it leaves the crate.
pub fn redact_secret(message: String, api_key: Option<&str>) -> String {
    match api_key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) if message.contains(key) => message.replace(key, REDACTED),
        _ => message,
    }
}

/// Bound a payload kept for diagnostics, counting characters so a multi-byte
/// payload is never cut in the middle of a code point.
pub fn snippet(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut bounded: String = trimmed.chars().take(max_chars).collect();
    bounded.push_str("… [truncated]");
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Chat Completions text is what every log line, failover decision and
    /// user-facing report has been reading, so parameterizing the label must
    /// not move a single byte of it.
    #[test]
    fn the_chat_label_reproduces_the_exact_messages() {
        let endpoint = "http://x/v1/chat/completions";
        let cases = [
            (
                401,
                "bad key",
                "OpenAI-compatible provider authentication failed (HTTP 401) at http://x/v1/chat/completions: bad key",
            ),
            (
                403,
                "forbidden",
                "OpenAI-compatible provider authentication failed (HTTP 403) at http://x/v1/chat/completions: forbidden",
            ),
            (
                429,
                "slow down",
                "OpenAI-compatible provider rate limit exceeded (HTTP 429) at http://x/v1/chat/completions: slow down",
            ),
            (
                500,
                "boom",
                "OpenAI-compatible provider server error (HTTP 500) at http://x/v1/chat/completions: boom",
            ),
            (
                404,
                "no model",
                "OpenAI-compatible provider request failed (HTTP 404) at http://x/v1/chat/completions: no model",
            ),
        ];
        for (status, body, expected) in cases {
            assert_eq!(
                http_response_error(endpoint, status, None, body, None).to_string(),
                expected,
                "{status}"
            );
        }
        assert_eq!(
            transport_error_message(endpoint, "connection reset by peer"),
            "OpenAI-compatible provider transport error at http://x/v1/chat/completions: connection reset by peer (transient transport error)"
        );
        assert_eq!(
            transport_error_message(endpoint, "dns says no"),
            "OpenAI-compatible provider transport error at http://x/v1/chat/completions: dns says no"
        );
    }

    /// The same sentence with another wire's label: the failover classifier keys
    /// on "authentication failed" and the status code, not on the prefix.
    #[test]
    fn another_label_shares_the_sentence_shape_and_still_hides_the_key() {
        let key = "sk-gemini-secret";
        let message = http_response_error_labeled(
            "Gemini-compatible",
            "http://x/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
            401,
            None,
            &format!("API key not valid: {key}"),
            None,
            Some(key),
        )
        .to_string();
        assert!(
            message.starts_with("Gemini-compatible provider authentication failed (HTTP 401) at "),
            "{message}"
        );
        assert!(message.contains("authentication failed"), "{message}");
        assert!(!message.contains(key), "the key must be redacted: {message}");
        assert!(message.contains(REDACTED), "{message}");

        let transport = transport_error_message_labeled(
            "Anthropic-compatible",
            "http://x/v1/messages",
            "connection reset by peer",
        );
        assert_eq!(
            transport,
            "Anthropic-compatible provider transport error at http://x/v1/messages: connection reset by peer (transient transport error)"
        );
    }

    /// A hint survives the body truncation on every label, because it explains
    /// whether a retry can help.
    #[test]
    fn a_hint_is_kept_for_another_label() {
        let long_body = "x".repeat(MAX_ERROR_BODY_CHARS + 100);
        let message = http_response_error_labeled(
            "Gemini-compatible",
            "http://x/m",
            400,
            None,
            &long_body,
            Some("retryable"),
            None,
        )
        .to_string();
        assert!(message.contains("[truncated]"), "{message}");
        assert!(message.ends_with(" [retryable]"), "{message}");
    }
}

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
    let body = if body.trim().is_empty() {
        "<empty response body>".to_string()
    } else {
        snippet(body, MAX_ERROR_BODY_CHARS)
    };
    let message = match classify_http_status(status) {
        HttpErrorKind::Authentication => {
            format!("OpenAI-compatible provider authentication failed (HTTP {status}) at {endpoint}: {body}")
        }
        HttpErrorKind::RateLimited => {
            format!("OpenAI-compatible provider rate limit exceeded (HTTP 429) at {endpoint}: {body}")
        }
        HttpErrorKind::Server => {
            format!("OpenAI-compatible provider server error (HTTP {status}) at {endpoint}: {body}")
        }
        HttpErrorKind::Client => {
            format!("OpenAI-compatible provider request failed (HTTP {status}) at {endpoint}: {body}")
        }
    };
    error_with_retry_after(redact_secret(message, api_key), retry_after)
}

/// Describe a transport-level failure, marking transient faults the shared
/// classifier recognizes.
pub fn transport_error_message(endpoint: &str, error: &str) -> String {
    let message = format!("OpenAI-compatible provider transport error at {endpoint}: {error}");
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

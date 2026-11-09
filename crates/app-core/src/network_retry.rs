use std::time::Duration;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use tokio::process::Command;
use tokio::time::sleep;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use tokio::time::timeout;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkWaitPlan {
    pub reason: String,
    pub listener_summary: String,
}

pub fn classify_network_interruption(error: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut parts = Vec::new();
    let mut current = Some(error);
    while let Some(err) = current {
        let text = err.to_string().to_ascii_lowercase();
        parts.push(text);
        current = err.source();
    }
    classify_text(&parts.join(" | "))
}

pub fn classify_message(message: &str) -> Option<String> {
    classify_text(&message.to_ascii_lowercase())
}

/// Classify a provider error as a rate-limit (HTTP 429) rejection.
///
/// Kept apart from [`classify_message`] on purpose: a rate limit resolves by
/// waiting, not by waiting for connectivity, so the two paths schedule
/// different waits. The markers mirror what the provider runtimes in this
/// repository actually emit:
///
/// - the OpenAI runtime matches `429 too many requests`, `rate limit` and
///   `rate_limit` (`openai_stream_runtime.rs`), and its formatted body reads
///   `Rate limited: Too many requests. Retry after ...`;
/// - Openference bodies read `Rate limit exceeded. Too many requests per minute.`;
/// - Bedrock reports throttling as `too many requests` / `rate exceeded`;
/// - Google-style APIs answer `HTTP 429 RESOURCE_EXHAUSTED`.
pub fn classify_rate_limit(message: &str) -> Option<String> {
    let text = message.to_ascii_lowercase();
    let markers = [
        "rate limit",
        "rate_limit",
        "rate-limited",
        "too many requests",
        "resource exhausted",
        "resource_exhausted",
        "throttl",
        "rate exceeded",
    ];
    if markers.iter().any(|marker| text.contains(marker)) || contains_status_code(&text, "429") {
        return Some("the provider is rate limiting requests".to_string());
    }
    None
}

/// [`classify_rate_limit`] over an error and its sources, so a wrapped provider
/// error keeps its classification.
pub fn classify_rate_limit_error(error: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut parts = Vec::new();
    let mut current = Some(error);
    while let Some(err) = current {
        parts.push(err.to_string().to_ascii_lowercase());
        current = err.source();
    }
    classify_rate_limit(&parts.join(" | "))
}

/// True when `code` appears as a standalone status code, so a figure such as
/// `14290` or `4290` does not read as 429. Mirrors the rule the provider
/// failover classifier applies to its status codes.
fn contains_status_code(text: &str, code: &str) -> bool {
    let bytes = text.as_bytes();
    let code_len = code.len();
    text.match_indices(code).any(|(start, _)| {
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_digit();
        let end = start + code_len;
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_digit();
        before_ok && after_ok
    })
}

/// What the retry path should do about a provider-requested `Retry-After`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryAfterDecision {
    /// Wait this long before retrying. Never exceeds the configured cap.
    Wait(Duration),
    /// No usable header: keep the existing exponential backoff.
    Backoff,
    /// The header asks for longer than the cap while autonomous failover is
    /// enabled, so switching routes is better than waiting.
    FailoverInstead,
}

/// Turn a parsed `Retry-After` value into the retry path's next move.
///
/// Clamping is exactly `min(retry_after, cap_secs)`: a header of 120 with a cap
/// of 30 waits 30 seconds, never the full 120 and never zero. A missing header,
/// an unparseable or negative one (both already surface as `None` from the
/// shared parser), and a `0` all fall back to [`RetryAfterDecision::Backoff`]:
/// a zero-length wait would turn the retry into a busy loop, and the existing
/// exponential backoff always has a positive floor. A zero cap falls back the
/// same way unless failover was chosen.
///
/// This function only decides; it never sleeps. The turn loops own the waiting.
pub fn retry_after_decision(
    retry_after: Option<Duration>,
    cap_secs: u64,
    autonomous_failover: bool,
) -> RetryAfterDecision {
    let cap = Duration::from_secs(cap_secs);
    match retry_after {
        Some(delay) if !delay.is_zero() => {
            if delay > cap && autonomous_failover {
                RetryAfterDecision::FailoverInstead
            } else if cap.is_zero() {
                RetryAfterDecision::Backoff
            } else {
                RetryAfterDecision::Wait(delay.min(cap))
            }
        }
        _ => RetryAfterDecision::Backoff,
    }
}

/// Header-level entry point: parse `Retry-After` with the shared parser in
/// `horon-agent-provider-core` and decide.
///
/// The shared parser caps a header at `MAX_RETRY_AFTER` (60 seconds) before the
/// value reaches this function, so a header above 60 seconds is
/// indistinguishable from 60 here. A configured cap above 60 therefore cannot
/// be exceeded by a header; only the body-derived hint keeps its longer wait.
pub fn retry_after_decision_from_headers(
    headers: &reqwest::header::HeaderMap,
    cap_secs: u64,
    autonomous_failover: bool,
) -> RetryAfterDecision {
    let retry_after = horon_agent_provider_core::retry_after::retry_after(headers).map(|hint| hint.remaining());
    retry_after_decision(retry_after, cap_secs, autonomous_failover)
}

fn classify_text(text: &str) -> Option<String> {
    let network_markers = [
        "connection reset",
        "connection aborted",
        "connection refused",
        "broken pipe",
        "network is unreachable",
        "network unreachable",
        "host is down",
        "no route to host",
        "not connected",
        // TLS-level drops. rustls reports an abrupt close as "peer closed
        // connection without sending TLS close_notify" (its docs URL spells
        // it "unexpected-eof", which the plain "unexpected eof" marker below
        // does not match).
        "close_notify",
        "peer closed connection",
        "tls handshake eof",
        // reqwest/hyper wrap connect-phase failures as "client error
        // (Connect)" and connection-level faults as "connection error: ...".
        "client error (connect)",
        "connection error",
        "dns error",
        "failed to lookup address",
        "temporary failure in name resolution",
        "name or service not known",
        "could not resolve host",
        "couldn't resolve host",
        "host is unreachable",
        "operation timed out",
        "timed out",
        "timeout",
        "error trying to connect",
        "connection closed before message completed",
        "unexpected eof",
        "end of file before message completed",
    ];
    if network_markers.iter().any(|marker| text.contains(marker)) {
        return Some("the network connection appears to have dropped".to_string());
    }
    None
}

pub fn wait_plan() -> NetworkWaitPlan {
    #[cfg(target_os = "linux")]
    {
        NetworkWaitPlan {
            reason: "stream interrupted by a likely network disconnect".to_string(),
            listener_summary:
                "listening for Linux netlink changes via `ip monitor`; also verifying with reconnect probes".to_string(),
        }
    }
    #[cfg(target_os = "macos")]
    {
        NetworkWaitPlan {
            reason: "stream interrupted by a likely network disconnect".to_string(),
            listener_summary:
                "listening for macOS route/interface changes via `route -n monitor`; also verifying with reconnect probes"
                    .to_string(),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        NetworkWaitPlan {
            reason: "stream interrupted by a likely network disconnect".to_string(),
            listener_summary: "waiting with lightweight reconnect probes".to_string(),
        }
    }
}

pub async fn wait_until_probably_online() {
    let mut delay = Duration::from_secs(1);
    loop {
        if probe_connectivity().await {
            return;
        }
        wait_for_platform_change_or_delay(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

pub async fn is_probably_online() -> bool {
    probe_connectivity().await
}

async fn probe_connectivity() -> bool {
    let client = horon_agent_provider_core::shared_http_client();
    let request = client
        .head("https://www.gstatic.com/generate_204")
        .timeout(Duration::from_secs(5));
    matches!(request.send().await, Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 204)
}

async fn wait_for_platform_change_or_delay(delay: Duration) {
    #[cfg(target_os = "linux")]
    {
        if command_exists("ip").await {
            let fut = wait_for_command_output("ip", &["monitor", "link", "address", "route"]);
            let _ = timeout(delay, fut).await;
            return;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if command_exists("route").await {
            let fut = wait_for_command_output("route", &["-n", "monitor"]);
            let _ = timeout(delay, fut).await;
            return;
        }
    }
    sleep(delay).await;
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn command_exists(command: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {} >/dev/null 2>&1", shell_escape(command)))
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn shell_escape(value: &str) -> String {
    value.replace('\'', "'\\''")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn wait_for_command_output(command: &str, args: &[&str]) {
    let mut command_builder = Command::new(command);
    command_builder
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = match command_builder.spawn() {
        Ok(child) => child,
        Err(_) => return,
    };
    if let Some(mut stdout) = child.stdout.take() {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1];
        let _ = stdout.read(&mut buf).await;
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_network_errors() {
        assert!(classify_message("connection reset by peer").is_some());
        assert!(classify_message("temporary failure in name resolution").is_some());
        assert!(classify_message("network is unreachable").is_some());
        assert!(classify_message("401 unauthorized").is_none());
    }

    /// Real error strings harvested from ~/.horon/logs. Every wifi-outage
    /// shape observed in the wild must classify as a network interruption so
    /// the wait-for-network path engages instead of failing the turn.
    #[test]
    fn classifies_real_world_outage_errors() {
        let real_errors = [
            // DNS failure: wifi down when a new request starts, or link up
            // before DHCP delivers DNS servers.
            "Failed to send request to Anthropic API: error sending request for url \
             (https://api.anthropic.com/v1/messages): client error (Connect): dns error: \
             failed to lookup address information: Name or service not known",
            "error sending request for url (https://models.dev/api.json): client error \
             (Connect): dns error: failed to lookup address information: Temporary failure \
             in name resolution",
            // Stale pooled HTTP/2 connection dying after an outage.
            "Failed to send request to Anthropic API: error sending request for url \
             (https://api.anthropic.com/v1/messages): client error (SendRequest): \
             http2 error: keep-alive timed out: operation timed out",
            // rustls abrupt-close shape (docs URL spells "unexpected-eof" so
            // the plain "unexpected eof" marker never matched it).
            "error sending request for url (https://generativelanguage.googleapis.com/v1beta/models): \
             client error (SendRequest): connection error: peer closed connection without \
             sending TLS close_notify: https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html#unexpected-eof",
            // Connect-phase timeout.
            "error sending request for url (https://html.duckduckgo.com/html/): \
             client error (Connect): operation timed out",
            // Connection-level timeout on an established connection.
            "error sending request for url (https://dblp.org/search/author/api): \
             client error (SendRequest): connection error: timed out",
            // TLS handshake dying mid-outage.
            "error sending request for url (https://generativelanguage.googleapis.com/v1beta/models): \
             client error (Connect): tls handshake eof",
            // Body cut off mid-stream.
            "error decoding response body: request or response body error: operation timed out",
        ];
        for error in real_errors {
            assert!(
                classify_message(error).is_some(),
                "should classify as network interruption: {error}"
            );
        }
    }

    /// Deterministic failures must NOT trigger the wait-for-network path,
    /// otherwise a bad API key or exhausted quota would hang forever waiting
    /// for connectivity that is already fine.
    #[test]
    fn does_not_classify_deterministic_failures() {
        let deterministic = [
            "401 unauthorized",
            "403 forbidden: permission_denied",
            "invalid x-api-key",
            "404 not_found_error: model: claude-fable-5",
            "insufficient_quota: your credit balance is too low",
            "400 bad request: context length exceeded",
            "content_policy_violation",
        ];
        for error in deterministic {
            assert!(
                classify_message(error).is_none(),
                "should NOT classify as network interruption: {error}"
            );
        }
    }

    fn retry_after_headers(value: &'static str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static(value),
        );
        headers
    }

    /// The exact strings the provider runtimes in this repository emit for a
    /// 429, plus the wrapped forms a turn sees after formatting.
    #[test]
    fn classifies_rate_limit_messages_from_providers() {
        let provider_messages = [
            // OpenAI runtime retry classification and formatted body.
            "429 too many requests",
            "429 Too Many Requests",
            "rate limit exceeded",
            "rate_limit_exceeded",
            "Rate limited: Too many requests. Retry after 1m 5s.",
            "Rate limited (retry after 9s): upstream temporarily unavailable",
            // Openference JSON body.
            "Rate limit exceeded. Too many requests per minute.",
            // Bedrock throttling and Google-style exhaustion.
            "ThrottlingException: rate exceeded",
            "HTTP 429 RESOURCE_EXHAUSTED",
            // Status code alone, as wrapped by reqwest.
            "HTTP status client error (429 Too Many Requests) for url",
        ];
        for message in provider_messages {
            assert!(
                classify_rate_limit(message).is_some(),
                "should classify as rate limit: {message}"
            );
        }
    }

    #[test]
    fn does_not_classify_unrelated_errors_as_rate_limit() {
        let unrelated = [
            "401 unauthorized",
            "connection reset by peer",
            "400 bad request: context length exceeded",
            // 429 inside a larger number is not a status code.
            "request used 14290 tokens",
            "model 4290 is not available",
        ];
        for message in unrelated {
            assert!(
                classify_rate_limit(message).is_none(),
                "should NOT classify as rate limit: {message}"
            );
        }
    }

    #[test]
    fn classifies_rate_limit_through_wrapped_errors() {
        let error = anyhow::anyhow!("429 Too Many Requests").context("request failed");
        assert!(classify_rate_limit_error(&*error).is_some());
        let plain = anyhow::anyhow!("connection reset by peer");
        assert!(classify_rate_limit_error(&*plain).is_none());
    }

    #[test]
    fn clamps_a_long_retry_after_to_the_cap() {
        // Retry-After: 120 with a 30 second cap waits 30 seconds, not 120.
        assert_eq!(
            retry_after_decision_from_headers(&retry_after_headers("120"), 30, false),
            RetryAfterDecision::Wait(Duration::from_secs(30))
        );
        assert_eq!(
            retry_after_decision(Some(Duration::from_secs(120)), 30, false),
            RetryAfterDecision::Wait(Duration::from_secs(30))
        );
    }

    #[test]
    fn honors_a_retry_after_under_the_cap_exactly() {
        assert_eq!(
            retry_after_decision(Some(Duration::from_secs(5)), 30, false),
            RetryAfterDecision::Wait(Duration::from_secs(5))
        );
        // Through the header parser the hint is a monotonic deadline, so it
        // lands just under the requested 5 seconds.
        match retry_after_decision_from_headers(&retry_after_headers("5"), 30, false) {
            RetryAfterDecision::Wait(delay) => {
                assert!(delay <= Duration::from_secs(5));
                assert!(delay > Duration::from_secs(4));
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn missing_header_falls_back_to_backoff() {
        assert_eq!(
            retry_after_decision_from_headers(&reqwest::header::HeaderMap::new(), 30, false),
            RetryAfterDecision::Backoff
        );
        assert_eq!(retry_after_decision(None, 30, false), RetryAfterDecision::Backoff);
    }

    /// A past HTTP date parses to a zero remaining delay and must not produce a
    /// zero-length wait; a future date is bounded by the shared parser.
    #[test]
    fn http_date_header_parses_to_a_sane_wait() {
        assert_eq!(
            retry_after_decision_from_headers(&retry_after_headers("Wed, 21 Oct 2099 07:28:00 GMT"), 30, false),
            RetryAfterDecision::Wait(Duration::from_secs(30))
        );
        assert_eq!(
            retry_after_decision_from_headers(&retry_after_headers("Wed, 21 Oct 2015 07:28:00 GMT"), 30, false),
            RetryAfterDecision::Backoff
        );
    }

    #[test]
    fn garbage_and_negative_headers_fall_back_to_backoff_without_panicking() {
        for value in ["abc", "-5", "", "1.5"] {
            assert_eq!(
                retry_after_decision_from_headers(&retry_after_headers(value), 30, false),
                RetryAfterDecision::Backoff,
                "header {value:?} must fall back to backoff"
            );
        }
    }

    /// A zero delay would busy-loop the retry, so it is treated as no hint.
    #[test]
    fn zero_retry_after_falls_back_to_backoff() {
        assert_eq!(
            retry_after_decision_from_headers(&retry_after_headers("0"), 30, false),
            RetryAfterDecision::Backoff
        );
        assert_eq!(
            retry_after_decision(Some(Duration::ZERO), 30, false),
            RetryAfterDecision::Backoff
        );
    }

    #[test]
    fn over_cap_retry_after_fails_over_when_autonomous() {
        assert_eq!(
            retry_after_decision_from_headers(&retry_after_headers("120"), 30, true),
            RetryAfterDecision::FailoverInstead
        );
        assert_eq!(
            retry_after_decision(Some(Duration::from_secs(120)), 30, true),
            RetryAfterDecision::FailoverInstead
        );
    }

    #[test]
    fn over_cap_retry_after_waits_at_the_cap_without_failover() {
        assert_eq!(
            retry_after_decision(Some(Duration::from_secs(120)), 30, false),
            RetryAfterDecision::Wait(Duration::from_secs(30))
        );
    }
}

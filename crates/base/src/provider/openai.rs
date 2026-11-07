//! OpenAI provider shared helpers.
//!
//! The OpenAI provider *runtime* (`OpenAIProvider`: Codex OAuth + API key,
//! Responses API over SSE and persistent WebSocket) is gone: model requests run
//! through the direct OpenAI-compatible Chat Completions provider, so nothing
//! here resolves a model server endpoint.
//!
//! Only shared helpers remain: the extended prompt-cache-retention predicate
//! and the cache-retention/TTL helpers below.

pub use horon_agent_provider_core::CredentialMode as OpenAICredentialMode;

/// Whether `model_id` supports the legacy `prompt_cache_retention: "24h"`.
///
/// Keep this list tied to the documented extended-retention models, not a
/// broad prefix match (for example, GPT-4.1 mini is not GPT-4.1).
/// https://developers.openai.com/api/docs/guides/prompt-caching#summary-of-model-differences
pub fn supports_extended_prompt_cache_retention(model_id: &str) -> bool {
    let model = model_id.trim().to_ascii_lowercase();
    let model = match model.as_str() {
        "gpt-5.4-1m" => "gpt-5.4",
        model => model,
    };
    [
        "gpt-5.5",
        "gpt-5.5-pro",
        "gpt-5.4",
        "gpt-5.2",
        "gpt-5.1-codex-max",
        "gpt-5.1",
        "gpt-5.1-codex",
        "gpt-5.1-codex-mini",
        "gpt-5.1-chat-latest",
        "gpt-5",
        "gpt-5-codex",
        "gpt-4.1",
    ]
    .iter()
    .any(|base| {
        model == *base
            || model.strip_prefix(base).is_some_and(|suffix| {
                // Dated API snapshots inherit the base model's policy.
                let bytes = suffix.as_bytes();
                bytes.len() == 11
                    && bytes[0] == b'-'
                    && bytes[5] == b'-'
                    && bytes[8] == b'-'
                    && bytes
                        .iter()
                        .enumerate()
                        .all(|(i, c)| matches!(i, 0 | 5 | 8) || c.is_ascii_digit())
            })
    })
}

/// GPT-5.6 and later use `prompt_cache_options.ttl` (default and only supported
/// value: `30m`), not the older `prompt_cache_retention` request field.
pub fn uses_prompt_cache_options(model_id: &str) -> bool {
    let model = model_id.trim().to_ascii_lowercase();
    let Some(version) = model.strip_prefix("gpt-").and_then(|v| v.split('-').next()) else {
        return false;
    };
    let (major, minor) = version.split_once('.').unwrap_or((version, "0"));
    match (major.parse::<u32>(), minor.parse::<u32>()) {
        (Ok(major), Ok(minor)) => major > 5 || (major == 5 && minor >= 6),
        _ => false,
    }
}

/// Normalize the supported legacy override values, shared by TTL reporting and
/// the request builder. Model-specific override compatibility is checked by the API.
pub fn normalize_prompt_cache_retention(value: &str) -> Option<&str> {
    match value.trim() {
        "in_memory" => Some("in_memory"),
        "24h" => Some("24h"),
        _ => None,
    }
}

pub fn prompt_cache_retention_from_env() -> Option<String> {
    let raw = std::env::var("HORON_AGENT_OPENAI_PROMPT_CACHE_RETENTION").ok()?;
    normalize_prompt_cache_retention(&raw).map(str::to_owned)
}

/// Retention the direct Chat Completions provider puts in the request body for
/// `model_id`: the configured override when it is supported, otherwise the
/// model's documented extended retention. The cache-TTL estimate consumes the
/// same answer, so reporting and sending cannot disagree.
pub fn effective_prompt_cache_retention<'a>(model_id: &str, configured: Option<&'a str>) -> Option<&'a str> {
    if uses_prompt_cache_options(model_id) {
        // Omit the obsolete field and rely on the documented 30m default.
        return None;
    }
    configured
        .and_then(normalize_prompt_cache_retention)
        .or_else(|| supports_extended_prompt_cache_retention(model_id).then_some("24h"))
}

/// API cache retention estimate, not a guaranteed cache hit or expiry time.
/// Legacy extended retention is typically 30m (24h is only its upper bound),
/// in-memory is typically 5-10m, and 30m is the minimum lifetime for GPT-5.6+.
/// Routing/eviction can still prevent cache hits.
pub fn prompt_cache_ttl_for_model(model_id: Option<&str>, configured: Option<&str>) -> u64 {
    let model = model_id.unwrap_or_default();
    if uses_prompt_cache_options(model) || effective_prompt_cache_retention(model, configured) == Some("24h") {
        30 * 60
    } else {
        300
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn extended_cache_retention_matches_documented_models_and_snapshots() {
        for model in [
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.4",
            "gpt-5.4-1m",
            "gpt-5.2",
            "gpt-5.1-codex-max",
            "gpt-5.1",
            "gpt-5.1-codex",
            "gpt-5.1-codex-mini",
            "gpt-5.1-chat-latest",
            "gpt-5",
            "gpt-5-codex",
            "gpt-4.1",
            "gpt-4.1-2025-04-14",
            " GPT-5.4 ",
        ] {
            assert!(supports_extended_prompt_cache_retention(model), "{model}");
        }
        for model in [
            "gpt-5.6",
            "gpt-6-astra",
            "gpt-4o",
            "gpt-4.1-mini",
            "gpt-4.1-nano",
            "gpt-5-mini",
            "gpt-5.4-mini",
            "gpt-5.50",
            "gpt-5.1-unknown",
            "unknown",
        ] {
            assert!(!supports_extended_prompt_cache_retention(model), "{model}");
        }
    }

    #[test]
    fn cache_retention_policy_and_ttl_share_override_semantics() {
        for (model, configured, retention, ttl) in [
            ("gpt-5.4", None, Some("24h"), 1800),
            ("gpt-5.4", Some(" in_memory "), Some("in_memory"), 300),
            ("gpt-5.4", Some("invalid"), Some("24h"), 1800),
            ("gpt-4o", None, None, 300),
            ("gpt-4o", Some("24h"), Some("24h"), 1800),
            ("gpt-5.6-sol", None, None, 1800),
            ("gpt-5.6-sol", Some("24h"), None, 1800),
            ("gpt-5.6-sol", Some("in_memory"), None, 1800),
            ("gpt-6-astra", None, None, 1800),
        ] {
            assert_eq!(
                effective_prompt_cache_retention(model, configured),
                retention,
                "{model}"
            );
            assert_eq!(prompt_cache_ttl_for_model(Some(model), configured), ttl, "{model}");
        }
        for invalid in ["", " ", "24H", "1h"] {
            assert_eq!(normalize_prompt_cache_retention(invalid), None);
        }
    }
}

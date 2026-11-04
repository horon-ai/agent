use crate::ModelRoute;
use std::borrow::Cow;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActiveProvider {
    OpenAI,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ProviderAvailability {
    pub openai: bool,
}

impl ProviderAvailability {
    pub fn is_configured(self, provider: ActiveProvider) -> bool {
        match provider {
            ActiveProvider::OpenAI => self.openai,
        }
    }
}

pub fn auto_default_provider(availability: ProviderAvailability) -> ActiveProvider {
    let _ = availability;
    ActiveProvider::OpenAI
}

pub fn parse_provider_hint(value: &str) -> Option<ActiveProvider> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" => Some(ActiveProvider::OpenAI),
        _ => None,
    }
}

pub fn provider_label(provider: ActiveProvider) -> &'static str {
    match provider {
        ActiveProvider::OpenAI => "OpenAI",
    }
}

pub fn provider_key(provider: ActiveProvider) -> &'static str {
    match provider {
        ActiveProvider::OpenAI => "openai",
    }
}

pub fn provider_from_model_key(key: &str) -> Option<ActiveProvider> {
    match key {
        "openai" => Some(ActiveProvider::OpenAI),
        _ => None,
    }
}

/// Translate a persisted session/runtime provider key (the `RuntimeKey`
/// stable-id or `ModelRouteApiMethod` vocabulary, e.g. `anthropic-api-key`,
/// `claude-oauth`, `openai-api-key`) into the CLI `--provider` argument value
/// (the `ProviderChoice` vocabulary, e.g. `anthropic-api`, `claude`,
/// `openai-api`).
///
/// These two vocabularies overlap but are NOT identical: the runtime key
/// distinguishes auth method (`anthropic-api-key` vs `claude-oauth`) while the
/// CLI `--provider` enum uses `anthropic-api` / `claude`. Passing a raw runtime
/// key straight to `--provider` makes clap reject it (`invalid value
/// 'anthropic-api-key'`) and the spawned process exits immediately.
///
/// Returns `None` when there is no clean, unambiguous CLI provider to pass; in
/// that case callers should omit the flag entirely and rely on the persisted
/// session (model + provider_key + route_api_method) to reconstruct the exact
/// route on resume.
pub fn cli_provider_arg_for_session_key(key: &str) -> Option<&'static str> {
    let normalized = key.trim().to_ascii_lowercase();
    let base = normalized
        .split_once(':')
        .map(|(prefix, _rest)| prefix)
        .unwrap_or(normalized.as_str());
    if let Some(route) = crate::auth_mode::AuthRoute::parse(base) {
        return Some(route.cli_provider_arg());
    }
    match base {
        "openai" => Some("openai"),
        _ => None,
    }
}

pub fn explicit_model_provider_prefix(model: &str) -> Option<(ActiveProvider, &'static str, &str)> {
    if let Some(rest) = model.strip_prefix("openai-api:") {
        Some((ActiveProvider::OpenAI, "openai-api:", rest))
    } else if let Some(rest) = model.strip_prefix("openai-oauth:") {
        Some((ActiveProvider::OpenAI, "openai-oauth:", rest))
    } else if let Some(rest) = model.strip_prefix("openai:") {
        Some((ActiveProvider::OpenAI, "openai:", rest))
    } else {
        None
    }
}

pub fn model_name_for_provider(provider: ActiveProvider, model: &str) -> Cow<'_, str> {
    let _ = provider;
    Cow::Borrowed(model)
}

/// Strip a provider runtime's own routing prefix from a model id.
///
/// Session restore and the model picker speak in routing specs such as
/// `antigravity:gemini-3-flash`. `MultiProvider` peels the prefix off before
/// delegating, but `--provider <name>` hands the concrete runtime back to the
/// agent directly, so the prefixed spec reaches `Provider::set_model`
/// untouched. Storing it verbatim makes every later turn ask the backend for a
/// model whose id contains our prefix, which the backend has never heard of
/// (Antigravity answers HTTP 404 "Requested entity was not found." on turn 2
/// of any resumed session).
///
/// A runtime only ever puts a bare model id on the wire, so each runtime calls
/// this with its own prefix when accepting a model id. Only the runtime's own
/// prefix is stripped: a foreign prefix is a real routing error and must stay
/// visible rather than being silently reinterpreted as a local model.
pub fn strip_own_model_prefix<'a>(model: &'a str, own_prefix: &str) -> &'a str {
    let model = model.trim();
    match model.strip_prefix(own_prefix) {
        Some(rest) if !own_prefix.is_empty() => rest.trim(),
        _ => model,
    }
}

pub fn dedupe_model_routes(routes: Vec<ModelRoute>) -> Vec<ModelRoute> {
    use std::collections::HashMap;

    let mut deduped: Vec<ModelRoute> = Vec::with_capacity(routes.len());
    // Bucket candidate duplicates by (provider, model). The api_method match is
    // fuzzy (generic vs profile openai-compatible), so buckets keep a linear
    // scan, but each bucket only holds the handful of routes for one model.
    // The previous full `deduped.iter().position(..)` scan was O(n^2) over
    // 2000+ routes and showed up in server connect-burst profiles.
    let mut buckets: HashMap<(String, String), Vec<usize>> = HashMap::with_capacity(routes.len());

    for route in routes {
        let key = (route.provider.clone(), route.model.clone());
        let bucket = buckets.entry(key).or_default();

        if let Some(existing_idx) = bucket
            .iter()
            .copied()
            .find(|&idx| duplicate_route_api_method(&deduped[idx].api_method, &route.api_method))
        {
            if should_replace_duplicate_route(&deduped[existing_idx], &route) {
                deduped[existing_idx] = route;
            }
            continue;
        }

        bucket.push(deduped.len());
        deduped.push(route);
    }

    deduped
}

fn duplicate_route_api_method(existing: &str, candidate: &str) -> bool {
    existing == candidate
        || (is_generic_openai_compatible_route(existing) && is_profile_openai_compatible_route(candidate))
        || (is_profile_openai_compatible_route(existing) && is_generic_openai_compatible_route(candidate))
}

fn is_generic_openai_compatible_route(api_method: &str) -> bool {
    api_method == "openai-compatible"
}

fn is_profile_openai_compatible_route(api_method: &str) -> bool {
    api_method.starts_with("openai-compatible:")
}

fn should_replace_duplicate_route(existing: &ModelRoute, candidate: &ModelRoute) -> bool {
    // A direct OpenAI-compatible provider can briefly appear twice in merged
    // catalogs: once as the generic transport and once as the named profile
    // transport. Keep the profile-scoped route so selection writes
    // `profile:model` rather than falling back to ambiguous generic routing.
    let existing_profile_scoped = is_profile_openai_compatible_route(&existing.api_method);
    let candidate_profile_scoped = is_profile_openai_compatible_route(&candidate.api_method);
    !existing_profile_scoped && candidate_profile_scoped
}

pub fn fallback_sequence(active: ActiveProvider) -> Vec<ActiveProvider> {
    match active {
        ActiveProvider::OpenAI => vec![ActiveProvider::OpenAI],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_hints() {
        assert_eq!(parse_provider_hint("openai"), Some(ActiveProvider::OpenAI));
        assert_eq!(parse_provider_hint("unknown"), None);
        assert_eq!(parse_provider_hint("claude"), None);
    }

    #[test]
    fn cli_provider_arg_translates_runtime_keys() {
        assert_eq!(cli_provider_arg_for_session_key("openai-oauth"), Some("openai"));
        assert_eq!(cli_provider_arg_for_session_key("openai-api-key"), Some("openai-api"));
        assert_eq!(cli_provider_arg_for_session_key("openai-compatible:zai"), None);
        assert_eq!(cli_provider_arg_for_session_key("totally-unknown"), None);
    }

    #[test]
    fn parses_model_provider_prefixes() {
        assert_eq!(provider_from_model_key("openai"), Some(ActiveProvider::OpenAI));
        assert_eq!(provider_from_model_key("gemini"), None);

        for (raw, prefix, model) in [
            ("openai:gpt-5", "openai:", "gpt-5"),
            ("openai-oauth:gpt-5", "openai-oauth:", "gpt-5"),
            ("openai-api:gpt-5", "openai-api:", "gpt-5"),
        ] {
            let (provider, got_prefix, got_model) = explicit_model_provider_prefix(raw).unwrap();
            assert_eq!(provider, ActiveProvider::OpenAI, "{raw}");
            assert_eq!(got_prefix, prefix, "{raw}");
            assert_eq!(got_model, model, "{raw}");
        }
        assert_eq!(explicit_model_provider_prefix("claude:sonnet"), None);
    }

    #[test]
    fn auto_default_is_always_the_openai_compatible_provider() {
        assert_eq!(
            auto_default_provider(ProviderAvailability::default()),
            ActiveProvider::OpenAI
        );
        assert_eq!(
            auto_default_provider(ProviderAvailability { openai: true }),
            ActiveProvider::OpenAI
        );
    }

    #[test]
    fn fallback_sequence_never_crosses_providers() {
        assert_eq!(fallback_sequence(ActiveProvider::OpenAI), vec![ActiveProvider::OpenAI]);
    }
}

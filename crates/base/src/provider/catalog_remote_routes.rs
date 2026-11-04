use super::*;
use crate::provider::{ALL_OPENAI_MODELS, model_availability_for_account};

pub fn remote_model_routes_fallback(
    remote_provider_name: Option<&str>,
    remote_available_entries: &[String],
) -> Vec<ModelRoute> {
    if remote_provider_name
        .is_some_and(|name| name.eq_ignore_ascii_case(crate::subscription_catalog::HORON_AGENT_PROVIDER_DISPLAY_NAME))
    {
        return remote_available_entries
            .iter()
            .filter(|model| is_listable_model_name(model))
            .map(|model| ModelRoute {
                model: model.clone(),
                provider: crate::subscription_catalog::HORON_AGENT_PROVIDER_DISPLAY_NAME.to_string(),
                api_method: crate::subscription_catalog::HORON_AGENT_ROUTE_API_METHOD.to_string(),
                available: true,
                detail: "horon-agent subscription routing · managed server-side".to_string(),
                usage: None,
                cheapness: None,
            })
            .collect();
    }

    let auth = AuthStatus::check_fast();
    let mut routes = Vec::new();
    for model in remote_available_entries {
        if !is_listable_model_name(model) {
            continue;
        }

        if model.contains('/')
            && let Some(route) = remote_openai_compatible_route_for_model(model)
        {
            routes.push(route);
            continue;
        }

        let mut added_any = false;

        if horon_agent_provider_core::model_id::matches_known_model(model, ALL_OPENAI_MODELS) {
            let availability = model_availability_for_account(model);
            let (available, detail) = if auth.openai == AuthState::NotConfigured {
                (false, "no credentials".to_string())
            } else {
                match availability.state {
                    AccountModelAvailabilityState::Available => (true, String::new()),
                    AccountModelAvailabilityState::Unavailable => (
                        false,
                        format_account_model_availability_detail(&availability)
                            .unwrap_or_else(|| "not available".to_string()),
                    ),
                    AccountModelAvailabilityState::Unknown => (
                        true,
                        format_account_model_availability_detail(&availability)
                            .unwrap_or_else(|| "availability unknown".to_string()),
                    ),
                }
            };
            routes.push(build_openai_oauth_route(model, available, detail));
            added_any = true;
        }

        if let Some(route) = remote_openai_compatible_route_for_model(model) {
            routes.push(route);
            added_any = true;
        }

        if !added_any && let Some(route) = remote_current_openai_compatible_route_for_model(remote_provider_name, model)
        {
            routes.push(route);
            added_any = true;
        }

        if !added_any {
            routes.push(ModelRoute {
                model: model.clone(),
                provider: "unknown".to_string(),
                api_method: "unknown".to_string(),
                available: false,
                detail: "no matching configured provider route".to_string(),
                usage: None,
                cheapness: None,
            });
        }
    }
    routes
}

pub fn remote_model_routes_lightweight_fallback(
    remote_provider_name: Option<&str>,
    remote_available_entries: &[String],
    current_model: &str,
) -> Vec<ModelRoute> {
    let is_horon_agent_subscription = remote_provider_name
        .is_some_and(|name| name.eq_ignore_ascii_case(crate::subscription_catalog::HORON_AGENT_PROVIDER_DISPLAY_NAME));
    let provider = remote_provider_name
        .map(str::to_string)
        .unwrap_or_else(|| "remote".to_string());
    let mut routes = Vec::new();
    for model in remote_available_entries {
        if !is_listable_model_name(model) {
            continue;
        }
        routes.push(ModelRoute {
            model: model.clone(),
            provider: provider.clone(),
            api_method: if is_horon_agent_subscription {
                crate::subscription_catalog::HORON_AGENT_ROUTE_API_METHOD.to_string()
            } else {
                "remote-catalog".to_string()
            },
            available: true,
            detail: if is_horon_agent_subscription {
                "horon-agent subscription routing · managed server-side".to_string()
            } else {
                "refreshing route details…".to_string()
            },
            usage: None,
            cheapness: None,
        });
    }

    if routes.is_empty() && !current_model.is_empty() && current_model != "unknown" {
        routes.push(ModelRoute {
            model: current_model.to_string(),
            provider,
            api_method: "current".to_string(),
            available: true,
            detail: "refreshing model catalog…".to_string(),
            usage: None,
            cheapness: None,
        });
    }

    routes
}

pub fn remote_current_openai_compatible_route_for_model(
    remote_provider_name: Option<&str>,
    model: &str,
) -> Option<ModelRoute> {
    if model.trim().is_empty() || (!model.contains('/') && provider_for_model(model).is_some()) {
        return None;
    }

    let provider_name = remote_provider_name?.trim();
    let profile_id = crate::provider_catalog::openai_compatible_profile_id_for_display_name(provider_name)?;
    let profile = crate::provider_catalog::openai_compatible_profile_by_id(profile_id)?;
    if !crate::provider_catalog::openai_compatible_profile_is_configured(profile) {
        return None;
    }
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    if model.contains('/')
        && !remote_openai_compatible_profile_models(&resolved, profile)
            .iter()
            .any(|candidate| candidate.0 == model)
    {
        return None;
    }

    Some(ModelRoute {
        model: model.to_string(),
        provider: resolved.display_name,
        api_method: format!("openai-compatible:{}", resolved.id),
        available: true,
        detail: resolved.api_base,
        usage: None,
        cheapness: None,
    })
}

pub fn remote_openai_compatible_route_for_model(model: &str) -> Option<ModelRoute> {
    for profile in crate::provider_catalog::openai_compatible_profiles().iter().copied() {
        if !crate::provider_catalog::openai_compatible_profile_is_configured(profile) {
            continue;
        }
        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
        let Some(from_live_catalog) = remote_openai_compatible_profile_models(&resolved, profile)
            .iter()
            .find_map(|candidate| (candidate.0 == model).then_some(candidate.1))
        else {
            continue;
        };
        let detail = if from_live_catalog {
            resolved.api_base.clone()
        } else if resolved.api_base.trim().is_empty() {
            "fallback: static provider model list".to_string()
        } else {
            format!("{}; fallback: static provider model list", resolved.api_base)
        };
        return Some(ModelRoute {
            model: model.to_string(),
            provider: resolved.display_name,
            api_method: format!("openai-compatible:{}", resolved.id),
            available: true,
            detail,
            usage: None,
            cheapness: None,
        });
    }
    named_provider_profile_route_for_model(model)
}

/// Route for `model` when it belongs to a user-defined `[providers.<name>]`
/// profile from config.toml.
///
/// Built-in compatible profiles are handled above; without this, a bare model
/// id from a custom profile (e.g. a local MLX server) matches no known provider
/// and gets an "unknown" route instead of one that resolves back to its profile
/// (issue #694).
pub(super) fn named_provider_profile_route_for_model(model: &str) -> Option<ModelRoute> {
    named_provider_profile_route_for_model_in(model, &crate::config::config().providers)
}

pub(super) fn named_provider_profile_route_for_model_in(
    model: &str,
    providers: &std::collections::BTreeMap<String, crate::config::NamedProviderConfig>,
) -> Option<ModelRoute> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    for (profile_name, profile_config) in providers {
        if !named_provider_profile_routes(profile_name, profile_config)
            .iter()
            .any(|route| route.model == model)
        {
            continue;
        }
        let detail = if profile_config.base_url.trim().is_empty() {
            "configured provider profile".to_string()
        } else {
            profile_config.base_url.trim().to_string()
        };
        return Some(ModelRoute {
            model: model.to_string(),
            provider: profile_name.clone(),
            api_method: format!("openai-compatible:{}", profile_name),
            available: true,
            detail,
            usage: None,
            cheapness: None,
        });
    }
    None
}

pub(super) fn remote_openai_compatible_profile_models(
    resolved: &crate::provider_catalog::ResolvedOpenAiCompatibleProfile,
    profile: crate::provider_catalog::OpenAiCompatibleProfile,
) -> Vec<(String, bool)> {
    let mut models = Vec::new();
    let mut push = |model: String, from_live_catalog: bool| {
        let model = model.trim().to_string();
        if !model.is_empty() && !models.iter().any(|(existing, _)| existing == &model) {
            models.push((model, from_live_catalog));
        }
    };

    if let Some(cache) = crate::provider::catalog_cache::load_disk_cache_entry_for_namespace(&resolved.id) {
        let source_matches = cache
            .source_api_base
            .as_deref()
            .and_then(crate::provider_catalog::normalize_api_base)
            == crate::provider_catalog::normalize_api_base(&resolved.api_base);
        if source_matches {
            for model in cache.models {
                push(model.id, true);
            }
        }
    }

    for model in crate::provider_catalog::openai_compatible_profile_static_models(profile) {
        push(model, false);
    }

    models
}

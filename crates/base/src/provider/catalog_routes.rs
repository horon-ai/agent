use crate::auth::{AuthState, AuthStatus};

use super::{
    AccountModelAvailabilityState, CHATGPT_WEB_MODEL, ModelRoute, MultiProvider, ProviderRegistry,
    build_chatgpt_web_route, build_openai_api_key_route, build_openai_oauth_route, dedupe_model_routes,
    direct_openai_compatible_profile_routes, format_account_model_availability_detail, is_listable_model_name,
    known_openai_model_ids_for_scope, model_availability_for_scope, provider_for_model,
};

/// Build the fast local route snapshot used by the TUI model picker while the
/// full provider catalog is hydrating.
///
/// This intentionally lives in the provider layer rather than the TUI so auth,
/// provider, and catalog policy have one source of truth. The TUI should only
/// group, sort, and render the returned routes.
pub fn simplified_model_routes_for_picker(
    current_provider_name: &str,
    current_model: &str,
    display_models: impl IntoIterator<Item = String>,
) -> Vec<ModelRoute> {
    let auth = AuthStatus::check_fast();
    let mut routes = Vec::new();

    for model in display_models {
        if model == CHATGPT_WEB_MODEL {
            routes.push(build_chatgpt_web_route());
            continue;
        }
        if !model.contains('/') && provider_for_model(&model) == Some("openai") {
            // Platform-API-only GPT Pro models: never advertise an OAuth route.
            if horon_agent_provider_core::is_openai_api_only_pro_model(&model) {
                routes.push(ModelRoute {
                    model: model.clone(),
                    provider: "OpenAI".to_string(),
                    api_method: "openai-api-key".to_string(),
                    available: auth.openai_has_api_key,
                    detail: if auth.openai_has_api_key {
                        String::new()
                    } else {
                        "requires OPENAI_API_KEY".to_string()
                    },
                    usage: None,
                    cheapness: None,
                });
                continue;
            }
            if auth.openai_has_oauth {
                routes.push(ModelRoute {
                    model: model.clone(),
                    provider: "OpenAI".to_string(),
                    api_method: "openai-oauth".to_string(),
                    available: true,
                    detail: String::new(),
                    usage: None,
                    cheapness: None,
                });
            }
            if auth.openai_has_api_key {
                routes.push(ModelRoute {
                    model: model.clone(),
                    provider: "OpenAI".to_string(),
                    api_method: "openai-api-key".to_string(),
                    available: true,
                    detail: String::new(),
                    usage: None,
                    cheapness: None,
                });
            }
            if auth.openai == AuthState::NotConfigured {
                routes.push(ModelRoute {
                    model,
                    provider: "OpenAI".to_string(),
                    api_method: "openai-oauth".to_string(),
                    available: false,
                    detail: "no credentials".to_string(),
                    usage: None,
                    cheapness: None,
                });
            }
            continue;
        }

        // Slash-qualified ids belong to a configured compatible profile; a
        // model that no profile serves gets no fabricated route here.
        if model.contains('/') {
            if let Some(route) = remote_openai_compatible_route_for_model(&model) {
                routes.push(route);
            }
            continue;
        }

        let (provider, api_method, available, detail) = match provider_for_model(&model) {
            Some("openai") => unreachable!("OpenAI models are handled above"),
            Some(other) => (other.to_string(), other.to_string(), true, String::new()),
            None => (
                current_provider_name.to_string(),
                "current".to_string(),
                true,
                String::new(),
            ),
        };

        routes.push(ModelRoute {
            model,
            provider,
            api_method,
            available,
            detail,
            usage: None,
            cheapness: None,
        });
    }

    if routes.is_empty() && !current_model.is_empty() && current_model != "unknown" {
        routes.push(ModelRoute {
            model: current_model.to_string(),
            provider: current_provider_name.to_string(),
            api_method: "current".to_string(),
            available: true,
            detail: "simplified catalog".to_string(),
            usage: None,
            cheapness: None,
        });
    }

    expand_saved_account_routes(routes)
}

/// Replace implicit OAuth routes with every saved account's own catalog.
/// API-key and other provider routes stay unchanged. The account-qualified API
/// method is carried through RPC, deduplication, selection, and restoration.
fn expand_saved_account_routes(mut routes: Vec<ModelRoute>) -> Vec<ModelRoute> {
    let openai = match crate::auth::codex::list_accounts() {
        Ok(accounts) => accounts,
        Err(error) => {
            crate::logging::warn(&format!("Cannot expand saved openai account routes: {error}"));
            Vec::new()
        }
    };
    if !openai.is_empty() {
        routes.retain(|route| route.api_method != "openai-oauth");
        for account in openai {
            for model in known_openai_model_ids_for_scope(&account.label) {
                if !is_listable_model_name(&model)
                    || model == CHATGPT_WEB_MODEL
                    || horon_agent_provider_core::is_openai_api_only_pro_model(&model)
                {
                    continue;
                }
                let availability = model_availability_for_scope(&account.label, &model);
                let mut route = build_openai_oauth_route(
                    &model,
                    availability.state != AccountModelAvailabilityState::Unavailable,
                    format_account_model_availability_detail(&availability).unwrap_or_default(),
                );
                apply_saved_account_status(
                    &mut route,
                    "openai",
                    &account.label,
                    !account.access_token.is_empty(),
                    !account.refresh_token.is_empty(),
                    account
                        .expires_at
                        .is_some_and(|expires| expires <= chrono::Utc::now().timestamp_millis()),
                );
                qualify_saved_account_route(&mut route, &account.label, account.email.as_deref());
                routes.push(route);
            }
        }
    }
    dedupe_model_routes(routes)
}

fn apply_saved_account_status(
    route: &mut ModelRoute,
    provider: &str,
    label: &str,
    has_access: bool,
    has_refresh: bool,
    expired: bool,
) {
    let status = if (!has_access || expired) && !has_refresh {
        route.available = false;
        Some("saved login expired or missing credentials; run /login".to_string())
    } else if let Some(detail) = super::models::provider_unavailability_detail_for_account_label(provider, Some(label))
    {
        route.available = false;
        Some(detail)
    } else if expired {
        Some("expired access token; refresh on selection".to_string())
    } else {
        None
    };
    if let Some(status) = status {
        route.detail = if route.detail.is_empty() {
            status
        } else {
            format!("{} · {status}", route.detail)
        };
    }
}

fn qualify_saved_account_route(route: &mut ModelRoute, label: &str, email: Option<&str>) {
    route.api_method = format!("{}@{label}", route.api_method);
    let identity = match email {
        Some(email) => format!("account {label} · {email}"),
        None => format!("account {label}"),
    };
    route.detail = if route.detail.is_empty() {
        identity
    } else {
        format!("{identity} · {}", route.detail)
    };
}

/// Build the full route catalog.
///
/// Orchestration only: each provider family contributes routes through its
/// own `append_*_routes` builder below, so provider-specific policy stays in
/// one place per provider instead of one 400-line function.
pub(super) fn multiprovider_model_routes(provider: &MultiProvider) -> Vec<ModelRoute> {
    let routes_started = std::time::Instant::now();
    provider.spawn_openai_catalog_refresh_if_needed();

    let mut routes = Vec::new();

    let openai_auth = crate::auth::AuthStatus::check_fast();

    append_openai_routes(provider, &mut routes, &openai_auth);
    let added_direct_openai_compatible_routes = append_openai_compatible_profile_routes(provider, &mut routes);

    let total_ms = routes_started.elapsed().as_millis();
    if total_ms >= 250 || std::env::var("HORON_AGENT_LOG_MODEL_PICKER_TIMING").is_ok() {
        crate::logging::info(&format!(
            "[TIMING] model_routes: routes={}, total={}ms",
            routes.len(),
            total_ms,
        ));
    }

    let routes_before_filter = routes.len();

    // Drop obviously non-chat models (embeddings, speech, rerankers, etc.) that
    // some compatible profiles (NVIDIA NIM / FPT / Chutes) dump wholesale into
    // their catalogs. Without this the picker is flooded with hundreds of
    // unusable entries.
    routes.retain(|route| is_listable_model_name(&route.model));

    let routes = dedupe_model_routes(routes);

    // Structured, always-on summary of catalog route building. This is the
    // single most useful line for the recurring "model picker empty /
    // configured provider's models missing" reports (issues #292, #268, #312,
    // #304): it records which credentials were detected and how many routes
    // each provider contributed, so a shared log explains exactly why a model
    // was or was not offered. No secrets here.
    log_model_routes_summary(
        "build",
        &routes,
        routes_before_filter,
        openai_auth.openai_has_oauth,
        openai_auth.openai_has_api_key,
        added_direct_openai_compatible_routes,
        total_ms,
    );

    expand_saved_account_routes(routes)
}

/// OpenAI models via OAuth and/or API key, with per-account availability.
///
/// The ChatGPT/Codex OAuth catalog and the platform API-key catalog list
/// different models, so each route is offered only for models in its own
/// credential's catalog and judged against that catalog's availability. Using
/// one shared list advertised API-only models under OAuth (and vice versa),
/// and the switch then failed against the other credential's catalog.
fn append_openai_routes(provider: &MultiProvider, routes: &mut Vec<ModelRoute>, openai_auth: &crate::auth::AuthStatus) {
    let has_runtime = provider.openai_provider().is_some();
    let oauth_scope = super::openai_catalog_scope_for_credential(true, "");
    let api_scope = crate::provider_catalog::load_api_key_from_env_or_config("OPENAI_API_KEY", "openai.env")
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .map(|key| super::openai_catalog_scope_for_credential(false, &key));

    let oauth_models = known_openai_model_ids_for_scope(&oauth_scope);
    let api_models = api_scope
        .as_deref()
        .filter(|_| openai_auth.openai_has_api_key)
        .map(|scope| {
            known_openai_model_ids_for_scope(scope)
                .into_iter()
                .filter(|model| is_listable_model_name(model))
                .collect::<Vec<_>>()
        });
    let mut openai_models = if openai_auth.openai_has_oauth || api_models.is_none() {
        oauth_models.clone()
    } else {
        Vec::new()
    };
    for model in api_models.iter().flatten() {
        if !openai_models.contains(model) {
            openai_models.push(model.clone());
        }
    }

    let route_availability = |scope: &str, model: &str| -> (bool, String) {
        if !has_runtime {
            return (false, "no credentials".to_string());
        }
        let availability = model_availability_for_scope(scope, model);
        match availability.state {
            AccountModelAvailabilityState::Available => (true, String::new()),
            AccountModelAvailabilityState::Unavailable => (
                false,
                format_account_model_availability_detail(&availability).unwrap_or_else(|| "not available".to_string()),
            ),
            AccountModelAvailabilityState::Unknown => {
                let detail = format_account_model_availability_detail(&availability)
                    .unwrap_or_else(|| "availability unknown".to_string());
                (true, detail)
            }
        }
    };

    for model in openai_models {
        if model == CHATGPT_WEB_MODEL {
            routes.push(build_chatgpt_web_route());
            continue;
        }
        // GPT Pro models are platform-API-only: never offer an OAuth route
        // for them (the Codex backend rejects them for ChatGPT accounts).
        if horon_agent_provider_core::is_openai_api_only_pro_model(&model) {
            if openai_auth.openai_has_api_key {
                routes.push(build_openai_api_key_route(&model, has_runtime, String::new()));
            } else {
                routes.push(build_openai_api_key_route(&model, false, "requires OPENAI_API_KEY"));
            }
            continue;
        }
        let in_oauth = oauth_models.contains(&model);
        if openai_auth.openai_has_oauth && in_oauth {
            let (available, detail) = route_availability(&oauth_scope, &model);
            routes.push(build_openai_oauth_route(&model, available, detail));
        }
        if let (Some(api_scope), Some(api_models)) = (api_scope.as_deref(), api_models.as_ref())
            && api_models.contains(&model)
        {
            let (available, detail) = route_availability(api_scope, &model);
            routes.push(build_openai_api_key_route(&model, available, detail));
        }
        if !openai_auth.openai_has_oauth && !openai_auth.openai_has_api_key {
            let (_, detail) = route_availability(&oauth_scope, &model);
            routes.push(build_openai_oauth_route(&model, false, detail));
        }
    }
}

/// Configured OpenAI-compatible profiles (NVIDIA NIM, Groq, ...), excluding
/// the active direct profile, which contributes its own live catalog.
/// Returns whether any routes were added.
fn append_openai_compatible_profile_routes(provider: &MultiProvider, routes: &mut Vec<ModelRoute>) -> bool {
    let active_profile_id = ProviderRegistry::new(provider).active_compatible_profile_id();
    let mut added_any = false;
    for profile in crate::provider_catalog::openai_compatible_profiles().iter().copied() {
        if !crate::provider_catalog::openai_compatible_profile_is_configured(profile) {
            continue;
        }
        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);

        // The active compatible provider contributes its own
        // live memory/disk catalog below. Do not preempt it with the generic
        // configured-profile path, because its in-memory catalog may be newer
        // than the disk snapshot that this non-active profile path can read.
        if active_profile_id.as_deref() == Some(resolved.id.as_str()) {
            continue;
        }

        let profile_routes = direct_openai_compatible_profile_routes(profile);
        added_any |= !profile_routes.is_empty();
        routes.extend(profile_routes);
    }

    // User-defined named provider profiles (`[providers.<name>]` in
    // config.toml). Their statically declared `[[providers.<name>.models]]`
    // entries (and `default_model`) must surface in the picker with a route
    // back to that profile, even when the profile is not the active provider
    // (issue #444).
    for (profile_name, profile_config) in &crate::config::config().providers {
        // The active runtime already contributes this profile's models (with
        // live-catalog freshness) via the active compatible-profile path.
        if active_profile_id.as_deref() == Some(profile_name.as_str()) {
            continue;
        }
        let named_routes = named_provider_profile_routes(profile_name, profile_config);
        added_any |= !named_routes.is_empty();
        routes.extend(named_routes);
    }
    added_any
}

/// Picker routes for one user-defined named provider profile from config.
///
/// Text-capable static models plus the profile's `default_model` are offered;
/// models declared image-only via `input = ["image"]` are excluded.
fn named_provider_profile_routes(
    profile_name: &str,
    profile_config: &crate::config::NamedProviderConfig,
) -> Vec<ModelRoute> {
    let mut models: Vec<String> = profile_config
        .models
        .iter()
        .filter(|model| {
            // `input` empty means unspecified (assume text-capable).
            model.input.is_empty() || model.input.iter().any(|input| input == "text")
        })
        .map(|model| model.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    if models.is_empty()
        && let Some(default_model) = profile_config
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
    {
        models.push(default_model.to_string());
    }

    let api_method = format!("openai-compatible:{}", profile_name);
    let detail = if profile_config.base_url.trim().is_empty() {
        "configured provider profile".to_string()
    } else {
        profile_config.base_url.trim().to_string()
    };

    let mut routes: Vec<ModelRoute> = Vec::new();
    for model in models {
        if !is_listable_model_name(&model) || routes.iter().any(|route| route.model == model) {
            continue;
        }
        routes.push(ModelRoute {
            model,
            provider: profile_name.to_string(),
            api_method: api_method.clone(),
            available: true,
            detail: detail.clone(),
            usage: None,
            cheapness: None,
        });
    }
    routes
}

/// Count routes per provider label (lowercased, spaces removed) so the catalog
/// summary log shows where the picker entries came from.
fn provider_route_counts(routes: &[ModelRoute]) -> std::collections::BTreeMap<String, usize> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for route in routes {
        let key = route.provider.trim().to_ascii_lowercase().replace(' ', "_");
        let key = if key.is_empty() { "unknown".to_string() } else { key };
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

/// Emit a structured, non-secret summary of model-route building. Callers pass
/// the credential-detection flags they already computed so the log explains why
/// each provider's routes were or were not produced.
fn log_model_routes_summary(
    phase: &str,
    routes: &[ModelRoute],
    routes_before_filter: usize,
    openai_oauth: bool,
    openai_api_key: bool,
    direct_openai_compatible: bool,
    total_ms: u128,
) {
    let available = routes.iter().filter(|route| route.available).count();
    let per_provider = provider_route_counts(routes)
        .into_iter()
        .map(|(provider, count)| format!("{provider}:{count}"))
        .collect::<Vec<_>>()
        .join(",");

    crate::logging::event_info(
        "model_routes_summary",
        vec![
            ("phase", phase.to_string()),
            ("routes_total", routes.len().to_string()),
            ("routes_available", available.to_string()),
            ("routes_before_filter", routes_before_filter.to_string()),
            (
                "routes_dropped",
                routes_before_filter.saturating_sub(routes.len()).to_string(),
            ),
            ("openai_oauth", openai_oauth.to_string()),
            ("openai_api", openai_api_key.to_string()),
            ("direct_openai_compatible", direct_openai_compatible.to_string()),
            ("by_provider", per_provider),
            ("build_ms", total_ms.to_string()),
        ],
    );
}

#[path = "catalog_remote_routes.rs"]
mod remote;
pub use remote::*;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_account_expiry_requires_login_only_without_refresh_credentials() {
        let _guard = EnvGuard::new();
        let mut expired = build_openai_oauth_route("gpt-5.5", true, "");
        apply_saved_account_status(&mut expired, "openai", "expired-test", true, false, true);
        assert!(!expired.available);
        assert!(expired.detail.contains("expired"));
        let mut refreshable = build_openai_oauth_route("gpt-5.5", true, "");
        apply_saved_account_status(&mut refreshable, "openai", "refresh-test", true, true, true);
        assert!(refreshable.available);
        assert!(refreshable.detail.contains("refresh on selection"));
    }

    struct EnvGuard {
        vars: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _temp: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = crate::storage::lock_test_env();
            let temp = tempfile::tempdir().expect("tempdir");
            let vars = vec![
                ("HORON_AGENT_HOME", std::env::var_os("HORON_AGENT_HOME")),
                ("OPENCODE_API_KEY", std::env::var_os("OPENCODE_API_KEY")),
            ];
            crate::env::set_var("HORON_AGENT_HOME", temp.path());
            crate::env::set_var("OPENCODE_API_KEY", "sk-test-opencode");
            Self {
                vars,
                _temp: temp,
                _lock: lock,
            }
        }

        fn save_opencode_cache(&self, source_api_base: &str, model_ids: &[&str]) {
            let horon_agent_home = std::env::var_os("HORON_AGENT_HOME").expect("HORON_AGENT_HOME set");
            let cache_dir = std::path::PathBuf::from(horon_agent_home).join("cache");
            std::fs::create_dir_all(&cache_dir).expect("create cache dir");
            let cache = crate::provider::catalog_cache::DiskCache {
                cached_at: crate::provider::catalog_cache::current_unix_secs().expect("current unix time"),
                source_api_base: Some(source_api_base.to_string()),
                models: model_ids
                    .iter()
                    .map(|id| crate::provider::catalog_cache::ModelInfo {
                        id: (*id).to_string(),
                        name: String::new(),
                        context_length: None,
                        pricing: crate::provider::catalog_cache::ModelPricing::default(),
                        created: None,
                        ..Default::default()
                    })
                    .collect(),
            };
            std::fs::write(
                cache_dir.join("opencode_models.json"),
                serde_json::to_string(&cache).expect("serialize cache"),
            )
            .expect("write cache");
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.vars.drain(..) {
                if let Some(value) = value {
                    crate::env::set_var(key, value);
                } else {
                    crate::env::remove_var(key);
                }
            }
        }
    }

    /// Issue #694: a bare model id from a user-defined `[providers.<name>]`
    /// profile must resolve to that profile instead of a generic provider
    /// heuristic that builds an id no runtime can resolve.
    #[test]
    fn named_provider_profile_model_routes_to_its_own_profile() {
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "omlx".to_string(),
            crate::config::NamedProviderConfig {
                base_url: "http://127.0.0.1:18000/v1".to_string(),
                default_model: Some("KAT-Coder-V2.5-Dev-OptiQ-4bit".to_string()),
                ..Default::default()
            },
        );

        let route = named_provider_profile_route_for_model_in("KAT-Coder-V2.5-Dev-OptiQ-4bit", &providers)
            .expect("custom profile model must resolve to its profile");
        assert_eq!(route.provider, "omlx");
        assert_eq!(route.api_method, "openai-compatible:omlx");
        assert_eq!(route.detail, "http://127.0.0.1:18000/v1");
        assert!(matches!(
            route.api_method_kind(),
            horon_agent_provider_core::ModelRouteApiMethod::OpenAiCompatible { .. }
        ));
    }

    #[test]
    fn named_anthropic_profile_preserves_profile_identity_in_picker_switch() {
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "corp-claude".to_string(),
            crate::config::NamedProviderConfig {
                provider_type: crate::config::NamedProviderType::AnthropicCompatible,
                base_url: "https://gateway.example/anthropic/v1".to_string(),
                default_model: Some("claude-custom".to_string()),
                ..Default::default()
            },
        );

        let route = named_provider_profile_route_for_model_in("claude-custom", &providers)
            .expect("Anthropic-compatible model must resolve to its profile");
        assert_eq!(route.provider, "corp-claude");
        assert_eq!(route.api_method, "openai-compatible:corp-claude");
        assert_eq!(
            MultiProvider::model_switch_request_for_session_route(
                &route.model,
                Some(&route.provider),
                Some(&route.api_method),
            ),
            "corp-claude:claude-custom"
        );
    }

    #[test]
    fn unknown_model_does_not_match_named_provider_profiles() {
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "omlx".to_string(),
            crate::config::NamedProviderConfig {
                base_url: "http://127.0.0.1:18000/v1".to_string(),
                default_model: Some("KAT-Coder-V2.5-Dev-OptiQ-4bit".to_string()),
                ..Default::default()
            },
        );

        assert!(named_provider_profile_route_for_model_in("some-other-model", &providers).is_none());
        assert!(named_provider_profile_route_for_model_in("", &providers).is_none());
    }

    /// Issue #694 through the real path a user hits: a custom
    /// `[providers.<name>]` profile in config.toml. The picker must route the
    /// model to that profile.
    #[test]
    fn custom_config_profile_model_is_routed_to_its_profile() {
        let _guard = EnvGuard::new();
        let horon_agent_home = std::env::var_os("HORON_AGENT_HOME").expect("HORON_AGENT_HOME set");
        std::fs::write(
            std::path::PathBuf::from(horon_agent_home).join("config.toml"),
            "[providers.omlx]\ntype = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:18000/v1\"\ndefault_model = \"KAT-Coder-V2.5-Dev-OptiQ-4bit\"\n",
        )
        .expect("write config.toml");
        crate::config::invalidate_config_cache();

        let model = "KAT-Coder-V2.5-Dev-OptiQ-4bit";
        let route = remote_openai_compatible_route_for_model(model)
            .expect("custom config profile model must be routed to its profile");
        assert_eq!(route.provider, "omlx");
        assert_eq!(route.api_method, "openai-compatible:omlx");

        // The full fallback builder (what the picker renders) agrees.
        let routes = remote_model_routes_fallback(Some("omlx"), &[model.to_string()]);
        assert!(
            routes.iter().any(|route| route.api_method == "openai-compatible:omlx"),
            "picker routes must include the profile route: {routes:?}"
        );
    }

    /// Issue #694 across both route sources. The picker is fed either by the
    /// server-built catalog (named profile routes) or, before that frame
    /// arrives, by the client-side fallback. Both must offer the profile route.
    #[test]
    fn custom_config_profile_model_routes_from_both_sources() {
        let _guard = EnvGuard::new();
        let horon_agent_home = std::env::var_os("HORON_AGENT_HOME").expect("HORON_AGENT_HOME set");
        std::fs::write(
            std::path::PathBuf::from(horon_agent_home).join("config.toml"),
            "[providers.omlx]\ntype = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:18000/v1\"\ndefault_model = \"KAT-Coder-V2.5-Dev-OptiQ-4bit\"\n",
        )
        .expect("write config.toml");
        crate::config::invalidate_config_cache();

        let model = "KAT-Coder-V2.5-Dev-OptiQ-4bit";

        // Source 1: the named-profile routes the server contributes.
        let named = named_provider_profile_routes(
            "omlx",
            crate::config::config().providers.get("omlx").expect("omlx profile"),
        );
        assert!(
            named
                .iter()
                .any(|route| route.model == model && route.api_method == "openai-compatible:omlx"),
            "server catalog must offer the profile route: {named:?}"
        );

        // Source 2: the client-side fallback, including the lightweight
        // variant used while route details are still refreshing.
        for routes in [
            remote_model_routes_fallback(Some("omlx"), &[model.to_string()]),
            remote_model_routes_lightweight_fallback(Some("omlx"), &[model.to_string()], model),
        ] {
            assert!(!routes.is_empty(), "fallback must offer the model");
            assert!(
                routes.iter().any(|route| route.provider == "omlx"),
                "no source may drop the profile route: {routes:?}"
            );
        }
    }

    #[test]
    fn remote_compatible_route_uses_live_cache_and_does_not_mark_fallback() {
        let guard = EnvGuard::new();
        guard.save_opencode_cache("https://opencode.ai/zen/v1", &["qwen3.6-plus"]);

        let route = remote_openai_compatible_route_for_model("qwen3.6-plus")
            .expect("live-cache-only OpenCode model should be routed");

        assert_eq!(route.provider, "OpenCode Zen");
        assert_eq!(route.api_method, "openai-compatible:opencode");
        assert_eq!(route.detail, "https://opencode.ai/zen/v1");
        assert!(!route.detail.contains("fallback"));
    }

    #[test]
    fn slash_model_fallback_prefers_matching_compatible_profile() {
        let guard = EnvGuard::new();
        let model = "vendouple/gpt-5.6-sol";
        guard.save_opencode_cache("https://opencode.ai/zen/v1", &[model]);

        let routes = remote_model_routes_fallback(Some("OpenCode Zen"), &[model.to_string()]);

        assert_eq!(routes.len(), 1, "unexpected fallback routes: {routes:?}");
        assert_eq!(routes[0].provider, "OpenCode Zen");
        assert_eq!(routes[0].api_method, "openai-compatible:opencode");
        assert!(routes[0].available);
    }

    #[test]
    fn current_compatible_profile_accepts_only_cataloged_slash_models() {
        let guard = EnvGuard::new();
        let model = "vendouple/gpt-5.6-sol";
        guard.save_opencode_cache("https://opencode.ai/zen/v1", &[model]);

        let route = remote_current_openai_compatible_route_for_model(Some("OpenCode Zen"), model)
            .expect("cataloged slash model should use the current compatible profile");
        assert_eq!(route.api_method, "openai-compatible:opencode");
        assert!(remote_current_openai_compatible_route_for_model(Some("OpenCode Zen"), "unknown/model").is_none());
    }

    #[test]
    fn remote_compatible_route_marks_static_model_list_fallback() {
        let _guard = EnvGuard::new();

        let route = remote_openai_compatible_route_for_model("glm-4.7")
            .expect("static OpenCode fallback model should be routed");

        assert_eq!(route.provider, "OpenCode Zen");
        assert!(route.detail.contains("fallback: static provider model list"));
    }

    #[test]
    fn remote_compatible_route_ignores_live_cache_from_wrong_api_base() {
        let guard = EnvGuard::new();
        guard.save_opencode_cache("https://wrong.example.test/v1", &["qwen3.6-plus"]);

        assert!(remote_openai_compatible_route_for_model("qwen3.6-plus").is_none());
    }
}

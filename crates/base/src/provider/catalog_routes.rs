use super::{
    ModelRoute, MultiProvider, ProviderRegistry, dedupe_model_routes, direct_openai_compatible_profile_routes,
    is_listable_model_name,
};

/// Build the full route catalog.
///
/// Every configured OpenAI-compatible profile contributes its models here; the
/// active direct profile contributes its own live catalog through the compatible
/// profile path, so it is skipped in this scan.
pub(super) fn multiprovider_model_routes(provider: &MultiProvider) -> Vec<ModelRoute> {
    let routes_started = std::time::Instant::now();
    provider.spawn_openai_catalog_refresh_if_needed();

    let mut routes = Vec::new();
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

    // Structured, always-on summary of catalog route building: it records how
    // many routes each provider contributed, so a shared log explains why a
    // model was or was not offered. No secrets here.
    log_model_routes_summary(
        "build",
        &routes,
        routes_before_filter,
        added_direct_openai_compatible_routes,
        total_ms,
    );

    routes
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
                ("HORON_HOME", std::env::var_os("HORON_HOME")),
                ("OPENCODE_API_KEY", std::env::var_os("OPENCODE_API_KEY")),
            ];
            crate::env::set_var("HORON_HOME", temp.path());
            crate::env::set_var("OPENCODE_API_KEY", "sk-test-opencode");
            Self {
                vars,
                _temp: temp,
                _lock: lock,
            }
        }

        fn save_opencode_cache(&self, source_api_base: &str, model_ids: &[&str]) {
            let horon_agent_home = std::env::var_os("HORON_HOME").expect("HORON_HOME set");
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
        let horon_agent_home = std::env::var_os("HORON_HOME").expect("HORON_HOME set");
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
        let horon_agent_home = std::env::var_os("HORON_HOME").expect("HORON_HOME set");
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

use super::*;
mod catalog_memo;
mod tool_tiers;

fn with_clean_provider_test_env<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("HORON_HOME");
    let mut profile_env_keys = vec![
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "KIMI_API_KEY",
        "HORON_AGENT_OPENROUTER_API_BASE",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_OPENROUTER_ENV_FILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
        "HORON_AGENT_OPENROUTER_PROVIDER_FEATURES",
        "HORON_AGENT_OPENROUTER_TRANSPORT_STATE",
        "HORON_AGENT_OPENROUTER_ALLOW_NO_AUTH",
        "HORON_AGENT_OPENROUTER_MODEL_CATALOG",
        "HORON_AGENT_OPENROUTER_MODEL",
        "HORON_AGENT_OPENROUTER_STATIC_MODELS",
        "HORON_AGENT_OPENAI_COMPAT_API_BASE",
        "HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME",
        "HORON_AGENT_OPENAI_COMPAT_ENV_FILE",
        "HORON_AGENT_OPENAI_COMPAT_DEFAULT_MODEL",
        "HORON_AGENT_OPENAI_COMPAT_LOCAL_ENABLED",
        "OPENAI_COMPAT_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "HORON_AGENT_RUNTIME_PROVIDER",
        "HORON_AGENT_ACTIVE_PROVIDER",
        "HORON_AGENT_INITIAL_PROVIDER_EXPLICIT",
        "HORON_AGENT_OPENAI_MODEL",
        "HORON_AGENT_SAME_PROVIDER_ACCOUNT_FAILOVER",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
    ];
    for profile in crate::provider_catalog::openai_compatible_profiles() {
        if !profile_env_keys.contains(&profile.api_key_env) {
            profile_env_keys.push(profile.api_key_env);
        }
    }
    let saved_profile_env = profile_env_keys
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect::<Vec<_>>();
    crate::env::set_var("HORON_HOME", temp.path());
    for (key, _) in &saved_profile_env {
        crate::env::remove_var(key);
    }
    // The in-memory model catalog services are process-global; earlier tests
    // may have hydrated scopes (fixture models) that would corrupt this test's
    // known_*_model_ids() validation, and vice versa. Reset on entry and exit
    // so neither direction leaks.
    crate::provider::models::reset_model_catalog_services_for_tests();

    let result = f();

    crate::provider::models::reset_model_catalog_services_for_tests();
    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
    for (key, value) in saved_profile_env {
        if let Some(value) = value {
            crate::env::set_var(key, value);
        } else {
            crate::env::remove_var(key);
        }
    }
    result
}

fn test_multi_provider_with_openai() -> MultiProvider {
    crate::env::set_var("OPENAI_API_KEY", "sk-test-openai-api-key");
    MultiProvider {
        openai: RwLock::new(Some(test_openai_runtime() as Arc<dyn Provider>)),
        openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
        active_openai_compatible_profile: RwLock::new(None),
        active: RwLock::new(ActiveProvider::OpenAI),
        startup_notices: RwLock::new(Vec::new()),
        initial_provider: None,
        catalog_scope: next_catalog_scope(),
        routes_memo: std::sync::Mutex::new(None),
        post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }
}

fn enter_test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

#[test]
fn openai_compatible_profile_catalog_cache_is_fresh_before_soft_refresh_boundary() {
    assert!(!openai_compatible_profile_catalog_cache_is_stale(
        1_000,
        1_000 + OPENAI_COMPATIBLE_PROFILE_CATALOG_SOFT_REFRESH_SECS - 1,
    ));
}

#[test]
fn openai_compatible_profile_catalog_cache_is_stale_at_soft_refresh_boundary() {
    assert!(openai_compatible_profile_catalog_cache_is_stale(
        1_000,
        1_000 + OPENAI_COMPATIBLE_PROFILE_CATALOG_SOFT_REFRESH_SECS,
    ));
    assert!(!openai_compatible_profile_catalog_cache_is_stale(2_000, 1_000));
}

fn with_env_var<T>(key: &str, value: &str, f: impl FnOnce() -> T) -> T {
    let prev = std::env::var_os(key);
    crate::env::set_var(key, value);
    let result = f();
    if let Some(prev) = prev {
        crate::env::set_var(key, prev);
    } else {
        crate::env::remove_var(key);
    }
    result
}
/// The [OI]-compatible profile setup/restart tests and their
/// `assert_openai_compatible_route_available` helper were removed with the
/// non-[OI] provider runtimes: selecting an [OI]-compatible profile now
/// returns "not available in this build", so there is no route to assert.
/// Parameterized test stand-in for provider runtimes that live downstream
/// (horon-agent-provider-{gemini,cursor,antigravity}-runtime) and therefore cannot
/// be constructed from base tests. Mirrors each runtime's catalog surface
/// (static model list plus `ModelRoute`s) so routing/fallback tests stay
/// meaningful.
struct StubExternalRuntime {
    name: &'static str,
    provider_label: &'static str,
    api_method: &'static str,
    models: &'static [&'static str],
    model: std::sync::RwLock<String>,
    credential_mode: std::sync::RwLock<horon_agent_provider_core::CredentialMode>,
    oauth_account: std::sync::RwLock<Option<String>>,
}

impl StubExternalRuntime {
    fn new(
        name: &'static str,
        provider_label: &'static str,
        api_method: &'static str,
        models: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            provider_label,
            api_method,
            models,
            model: std::sync::RwLock::new(models[0].to_string()),
            credential_mode: std::sync::RwLock::new(horon_agent_provider_core::CredentialMode::Auto),
            oauth_account: std::sync::RwLock::new(None),
        }
    }

    fn openai() -> Self {
        Self::new("openai", "OpenAI", "https", ALL_OPENAI_MODELS)
    }
}

#[async_trait::async_trait]
impl Provider for StubExternalRuntime {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("stub {} runtime does not stream", self.name)
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn model(&self) -> String {
        self.model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            anyhow::bail!("{} model cannot be empty", self.provider_label);
        }
        // Mirror the real runtimes' family validation: the registry is
        // process-global, so hot-init can hand this stub to tests that expect
        // cross-provider models to be rejected (e.g. a Claude model under a
        // forced-OpenAI selection).
        if !self.models.contains(&trimmed) {
            anyhow::bail!(
                "Unsupported {} model '{}'. Use /model to choose from the models available to your account.",
                self.provider_label,
                trimmed,
            );
        }
        *self.model.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = trimmed.to_string();
        Ok(())
    }
    fn available_models(&self) -> Vec<&'static str> {
        self.models.to_vec()
    }
    fn available_models_display(&self) -> Vec<String> {
        self.models.iter().map(|model| model.to_string()).collect()
    }
    fn available_models_for_switching(&self) -> Vec<String> {
        self.available_models_display()
    }
    fn model_routes(&self) -> Vec<ModelRoute> {
        self.available_models_display()
            .into_iter()
            .map(|model| ModelRoute {
                model,
                provider: self.provider_label.to_string(),
                api_method: self.api_method.to_string(),
                available: true,
                detail: String::new(),
                usage: None,
                cheapness: None,
            })
            .collect()
    }
    fn credential_mode(&self) -> horon_agent_provider_core::CredentialMode {
        *self
            .credential_mode
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn set_credential_mode(&self, mode: horon_agent_provider_core::CredentialMode) -> anyhow::Result<()> {
        *self
            .credential_mode
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = mode;
        Ok(())
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubExternalRuntime::new(
            self.name,
            self.provider_label,
            self.api_method,
            self.models,
        ))
    }

    fn set_oauth_account(&self, label: Option<&str>) -> anyhow::Result<()> {
        *self.oauth_account.write().unwrap() = label.map(str::to_owned);
        Ok(())
    }

    fn oauth_account_label(&self) -> Option<String> {
        self.oauth_account.read().unwrap().clone()
    }
}

fn test_openai_runtime() -> Arc<StubExternalRuntime> {
    Arc::new(StubExternalRuntime::openai())
}

include!("tests/model_resolution.rs");
include!("tests/fallback_failover.rs");

/// Rendering the route catalog must never schedule network work.
///
/// The scheduler's staleness predicate must treat a missing or mismatched
/// cache as needing a refresh, so the sweeper actually populates cold caches.
#[test]
fn profile_catalog_cache_needs_refresh_for_missing_cache() {
    with_clean_provider_test_env(|| {
        let profile = crate::provider_catalog::openai_compatible_profiles()
            .first()
            .copied()
            .expect("at least one OpenAI-compatible profile is defined");
        assert!(
            super::catalog_scheduler::profile_catalog_cache_needs_refresh(profile),
            "a missing catalog cache must be reported as needing a refresh"
        );
    });
}

fn write_provider_config(contents: &str) {
    let home = crate::storage::horon_agent_dir().expect("horon-agent home");
    std::fs::create_dir_all(home.join("config")).expect("create config dir");
    std::fs::write(home.join("config.toml"), contents).expect("write config.toml");
    crate::config::Config::invalidate_cache();
}

/// A `[providers.<name>] type = "openai-compatible"` profile is resolved into
/// the direct Chat Completions runtime, and the structured
/// `RuntimeKey::OpenAiCompatible` route selects that same runtime.
#[test]
fn openai_compatible_profile_builds_direct_runtime() {
    with_clean_provider_test_env(|| {
        write_provider_config(
            r#"
[providers.qa]
type = "openai-compatible"
base_url = "http://127.0.0.1:18080/v1"
auth = "none"
default_model = "qa-model"

[[providers.qa.models]]
id = "qa-model"
"#,
        );

        let provider = MultiProvider::from_named_provider_profile("qa").expect("build the profile runtime");
        let runtime = provider.execution_provider().expect("the profile runtime is installed");
        assert_eq!(runtime.name(), horon_agent_provider_openai::PROVIDER_NAME);
        assert_eq!(runtime.model(), "qa-model");
        assert_eq!(provider.model(), "qa-model");
        assert_eq!(provider.active_provider(), ActiveProvider::OpenAI);

        provider
            .set_route_selection(&horon_agent_provider_core::RouteSelection {
                model: "qa-model".to_string(),
                runtime_key: horon_agent_provider_core::RuntimeKey::OpenAiCompatible {
                    profile_id: Some("qa".to_string()),
                },
                api_method: "openai-compatible:qa".to_string(),
                provider_label: "qa".to_string(),
                detail: String::new(),
            })
            .expect("a compatible route selection reaches the direct runtime");
        assert_eq!(provider.model(), "qa-model");
        assert_eq!(provider.active_provider(), ActiveProvider::OpenAI);
    });
}

/// A profile that requires a key but finds none reports the env var name, never
/// the credential value that happens to be in the env file.
#[test]
fn openai_compatible_missing_key_names_env_not_value() {
    with_clean_provider_test_env(|| {
        let profile = crate::provider_catalog::openai_compatible_profiles()
            .iter()
            .copied()
            .find(|profile| profile.requires_api_key && !profile.api_key_env.is_empty() && !profile.env_file.is_empty())
            .expect("a built-in profile that requires an API key");

        let config_dir = crate::storage::app_config_dir().expect("config dir");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::write(config_dir.join(profile.env_file), "UNRELATED_API_KEY=sk-test-secret\n")
            .expect("write env file");

        let provider = MultiProvider::new_fast();
        let error = provider
            .set_model(&format!("{}:test-model", profile.id))
            .expect_err("a missing required key must fail");
        let message = error.to_string();
        assert!(message.contains(profile.api_key_env), "{message}");
        assert!(
            !message.contains("sk-test-secret"),
            "the credential value must not leak into the error: {message}"
        );
    });
}

#[test]
fn pinned_builtin_route_clears_active_compatible_profile() {
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();
        write_provider_config(
            r#"
[providers.qa]
type = "openai-compatible"
base_url = "http://127.0.0.1:18080/v1"
auth = "none"
default_model = "qa-model"

[[providers.qa.models]]
id = "qa-model"
"#,
        );

        let provider = MultiProvider::from_named_provider_profile("qa").expect("build the profile runtime");
        assert_eq!(
            provider.execution_provider().expect("profile runtime").name(),
            horon_agent_provider_openai::PROVIDER_NAME
        );

        // Give the process a built-in OpenAI runtime, then pin an API-key route.
        *provider.openai.write().unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(test_openai_runtime() as Arc<dyn Provider>);
        let model = known_openai_model_ids()
            .first()
            .cloned()
            .expect("at least one OpenAI model");

        provider
            .set_model(&format!("openai-api:{model}"))
            .expect("pin the built-in OpenAI API-key route");
        assert_eq!(
            provider.execution_provider().expect("built-in runtime").name(),
            "openai",
            "a pinned built-in route must not keep the stale profile as the execution target"
        );
        assert_eq!(provider.model(), model);
        assert_eq!(provider.display_name(), "OpenAI");

        let forked = provider.fork();
        assert_eq!(forked.model(), model);
        assert_eq!(
            forked.display_name(),
            "OpenAI",
            "a fork of the pinned built-in state must target the built-in runtime"
        );

        // Selecting a profile route installs it as the active execution target again.
        provider.set_model("qa:qa-model").expect("select the profile route");
        assert_eq!(
            provider.execution_provider().expect("profile runtime").name(),
            horon_agent_provider_openai::PROVIDER_NAME
        );
        assert_eq!(provider.model(), "qa-model");
    });
}

/// An explicit `openai:` prefix names the built-in [OI] runtime, so it must
/// never be absorbed by the active [OI]-compatible profile. This build ships
/// only the direct runtime, so the switch has to fail and name both the
/// missing built-in runtime and the profile the request would have leaked to.
#[test]
fn explicit_openai_prefix_never_falls_through_to_active_profile() {
    with_clean_provider_test_env(|| {
        write_provider_config(
            r#"
[providers.qa]
type = "openai-compatible"
base_url = "http://127.0.0.1:18080/v1"
auth = "none"
default_model = "qa-model"

[[providers.qa.models]]
id = "qa-model"
"#,
        );

        let provider = MultiProvider::from_named_provider_profile("qa").expect("build the profile runtime");
        assert!(
            provider.openai_provider().is_none(),
            "the scenario under test has no built-in [OI] runtime"
        );
        assert_eq!(provider.model(), "qa-model");
        assert_eq!(provider.display_name(), "qa");

        let error = provider
            .set_model("openai:gpt-5")
            .expect_err("an explicit [OI] route must not land on the active profile");
        let message = error.to_string();
        assert!(
            message.contains("'qa'"),
            "the error must name the active profile the route was kept off: {message}"
        );
        assert!(
            message.contains("built-in OpenAI runtime is not available"),
            "the error must name the missing built-in [OI] runtime: {message}"
        );
        assert!(
            !message.contains("No OpenAI-compatible provider is configured"),
            "{message}"
        );

        assert_eq!(provider.model(), "qa-model", "the profile model must not change");
        assert_eq!(
            provider.display_name(),
            "qa",
            "the profile must stay the active runtime"
        );
        assert_eq!(
            provider.execution_provider().expect("profile runtime").name(),
            horon_agent_provider_openai::PROVIDER_NAME,
            "the profile runtime must still execute requests"
        );

        // The pre-existing selection semantics survive: a profile-prefixed spec
        // and a bare model both stay on the active profile.
        provider.set_model("qa:qa-model").expect("select the profile route");
        assert_eq!(provider.model(), "qa-model");
        provider
            .set_model("qa-model")
            .expect("a bare model stays on the active profile");
        assert_eq!(provider.model(), "qa-model");
        assert_eq!(provider.display_name(), "qa");
    });
}

/// A structured picker selection of the built-in [OI] API-key route is the
/// same explicit directive as `openai-api:<model>`. With no built-in runtime
/// the error has to explain that the active profile is an [OI]-compatible
/// profile rather than claiming no [OI]-compatible provider exists.
#[test]
fn pinned_builtin_route_selection_names_the_active_profile() {
    with_clean_provider_test_env(|| {
        write_provider_config(
            r#"
[providers.qa]
type = "openai-compatible"
base_url = "http://127.0.0.1:18080/v1"
auth = "none"
default_model = "qa-model"

[[providers.qa.models]]
id = "qa-model"
"#,
        );

        let provider = MultiProvider::from_named_provider_profile("qa").expect("build the profile runtime");
        assert!(
            provider.openai_provider().is_none(),
            "the scenario under test has no built-in [OI] runtime"
        );

        let error = provider
            .set_route_selection(&horon_agent_provider_core::RouteSelection {
                model: "gpt-5".to_string(),
                runtime_key: horon_agent_provider_core::RuntimeKey::OpenAIApiKey,
                api_method: "openai-api-key".to_string(),
                provider_label: "openai".to_string(),
                detail: String::new(),
            })
            .expect_err("a pinned built-in [OI] route has no runtime in this build");
        let message = error.to_string();
        assert!(
            message.contains("'qa'"),
            "the error must name the active profile: {message}"
        );
        assert!(
            !message.contains("No OpenAI-compatible provider is configured"),
            "a configured [OI]-compatible profile must not be reported as missing: {message}"
        );
        assert_eq!(provider.model(), "qa-model");
        assert_eq!(provider.display_name(), "qa");
    });
}

#[test]
fn named_profile_without_base_url_names_the_profile() {
    with_clean_provider_test_env(|| {
        write_provider_config(
            r#"
[providers.broken]
type = "openai-compatible"
base_url = ""
auth = "none"
default_model = "broken-model"
"#,
        );

        let error = match MultiProvider::from_named_provider_profile("broken") {
            Ok(_) => panic!("an empty base_url must be a named configuration error"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("broken"), "{message}");
        assert!(message.contains("base_url"), "{message}");
    });
}

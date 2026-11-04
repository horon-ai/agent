#[test]
fn test_provider_for_model_claude() {
    assert_eq!(provider_for_model("claude-opus-4-6"), Some("claude"));
    assert_eq!(provider_for_model("claude-opus-4-6[1m]"), Some("claude"));
    assert_eq!(provider_for_model("claude-sonnet-4-6"), Some("claude"));
}

#[test]
fn test_provider_for_model_openai() {
    assert_eq!(provider_for_model("gpt-5.2-codex"), Some("openai"));
    assert_eq!(provider_for_model("gpt-5.5"), Some("openai"));
    assert_eq!(provider_for_model("gpt-5.4"), Some("openai"));
    assert_eq!(provider_for_model("gpt-5.4[1m]"), Some("openai"));
    assert_eq!(provider_for_model("gpt-5.4-pro"), Some("openai"));
}

#[test]
fn test_provider_for_model_gemini() {
    assert_eq!(provider_for_model("gemini-2.5-pro"), Some("gemini"));
    assert_eq!(provider_for_model("gemini-2.5-flash"), Some("gemini"));
    assert_eq!(provider_for_model("gemini-3-pro-preview"), Some("gemini"));
}

#[test]
fn test_provider_for_model_openrouter() {
    // OpenRouter uses provider/model format
    assert_eq!(
        provider_for_model("anthropic/claude-sonnet-4"),
        Some("openrouter")
    );
    assert_eq!(provider_for_model("openai/gpt-4o"), Some("openrouter"));
    assert_eq!(
        provider_for_model("google/gemini-2.0-flash"),
        Some("openrouter")
    );
    assert_eq!(
        provider_for_model("meta-llama/llama-3.1-405b"),
        Some("openrouter")
    );
}

#[test]
fn test_configured_direct_compatible_profiles_are_listed_without_openrouter_key() {
    with_clean_provider_test_env(|| {
        with_env_var("DEEPSEEK_API_KEY", "test-deepseek-key", || {
            with_env_var("KIMI_API_KEY", "test-kimi-key", || {
                let provider = MultiProvider {
                    openai: RwLock::new(None),
                    openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
                    active_openai_compatible_profile: RwLock::new(None),
                    active: RwLock::new(ActiveProvider::OpenAI),
                    startup_notices: RwLock::new(Vec::new()),
                    initial_provider: None,
                    catalog_scope: next_catalog_scope(),
                    routes_memo: std::sync::Mutex::new(None),
                    post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                };

                let routes = provider.model_routes();
                assert!(routes.iter().any(|route| {
                    route.model == "deepseek-v4-flash"
                        && route.provider == "DeepSeek"
                        && route.api_method == "openai-compatible:deepseek"
                        && route.available
                }));
                assert!(routes.iter().any(|route| {
                    route.model == "deepseek-v4-pro"
                        && route.provider == "DeepSeek"
                        && route.api_method == "openai-compatible:deepseek"
                        && route.available
                }));
                assert!(routes.iter().any(|route| {
                    route.model == "kimi-for-coding"
                        && route.provider == "Kimi Code"
                        && route.api_method == "openai-compatible:kimi"
                        && route.available
                }));
                assert!(
                    !routes
                        .iter()
                        .any(|route| route.model == "openrouter models")
                );
            })
        })
    });
}

#[test]
fn test_openai_auth_mode_prefixed_model_switch_changes_credentials() {
    with_clean_provider_test_env(|| {
        let prev_runtime = std::env::var_os("HORON_AGENT_RUNTIME_PROVIDER");
        crate::env::remove_var("HORON_AGENT_RUNTIME_PROVIDER");
        crate::env::set_var("OPENAI_API_KEY", "sk-test-openai-api-key");
        crate::auth::codex::upsert_account_from_tokens(
            "openai-1",
            "oauth-access-token",
            "oauth-refresh-token",
            None,
            None,
        )
        .expect("save OAuth account");

        let openai = test_openai_runtime();
        let provider = MultiProvider {
            openai: RwLock::new(Some(Arc::clone(&openai) as Arc<dyn Provider>)),
            openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
            active_openai_compatible_profile: RwLock::new(None),
            active: RwLock::new(ActiveProvider::OpenAI),
            startup_notices: RwLock::new(Vec::new()),
            initial_provider: None,
            catalog_scope: next_catalog_scope(),
            routes_memo: std::sync::Mutex::new(None),
            post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();

        // Route pinning is MultiProvider's job; per-pin token resolution was
        // covered by the removed OpenAI runtime's tests.
        assert_eq!(
            openai.credential_mode(),
            horon_agent_provider_core::CredentialMode::Auto,
            "default OpenAI credentials stay on the OAuth-first Auto pin"
        );

        provider
            .set_model("openai-api:gpt-5.5")
            .expect("API-key route should select the OpenAI API credentials");
        assert_eq!(
            openai.credential_mode(),
            horon_agent_provider_core::CredentialMode::ApiKey
        );

        provider
            .set_model("openai-oauth:gpt-5.5")
            .expect("OAuth route should switch back to Codex OAuth credentials");
        assert_eq!(
            openai.credential_mode(),
            horon_agent_provider_core::CredentialMode::OAuth
        );

        if let Some(prev_runtime) = prev_runtime {
            crate::env::set_var("HORON_AGENT_RUNTIME_PROVIDER", prev_runtime);
        } else {
            crate::env::remove_var("HORON_AGENT_RUNTIME_PROVIDER");
        }
    });
}

#[path = "model_metadata.rs"]
mod model_metadata;

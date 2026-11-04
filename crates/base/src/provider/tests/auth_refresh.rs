#[derive(Clone)]
struct SetModelAuthRefreshMockProvider {
    refreshed: Arc<std::sync::atomic::AtomicBool>,
    attempts: Arc<std::sync::atomic::AtomicUsize>,
    selected_model: Arc<std::sync::Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl Provider for SetModelAuthRefreshMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        unimplemented!("SetModelAuthRefreshMockProvider")
    }

    fn name(&self) -> &str {
        "set-model-auth-refresh-mock"
    }

    fn model(&self) -> String {
        self.selected_model
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "gpt-5.4".to_string())
    }

    fn set_model(&self, model: &str) -> Result<()> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if !self.refreshed.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("Claude credentials not available");
        }
        *self.selected_model.lock().unwrap() = Some(model.to_string());
        Ok(())
    }

    fn on_auth_changed(&self) {
        self.refreshed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[test]
fn test_set_model_with_auth_refresh_reloads_auth_and_retries_once() {
    let provider = SetModelAuthRefreshMockProvider {
        refreshed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        attempts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        selected_model: Arc::new(std::sync::Mutex::new(None)),
    };

    set_model_with_auth_refresh(&provider, "claude-opus-4-6").expect("auth refresh retry succeeds");

    assert!(provider.refreshed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        provider.attempts.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "restore should try once, reload auth, then retry once"
    );
    assert_eq!(provider.model(), "claude-opus-4-6");
}

#[test]
fn test_on_auth_changed_hot_initializes_openai_and_marks_routes_available() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        let _enter = runtime.enter();

        let provider = MultiProvider {
            openai: RwLock::new(None),
            openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
            active_openai_compatible_profile: RwLock::new(None),
            active: RwLock::new(ActiveProvider::OpenAI),
            startup_notices: RwLock::new(Vec::new()),
            initial_provider: Some(ActiveProvider::OpenAI),
            catalog_scope: next_catalog_scope(),
            routes_memo: std::sync::Mutex::new(None),
            post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };

        crate::auth::codex::upsert_account_from_tokens(
            "openai-1",
            "test-access-token",
            "test-refresh-token",
            None,
            None,
        )
        .expect("save test OpenAI auth");

        provider.on_auth_changed();

        assert!(provider.openai_provider().is_some());
        assert!(provider.model_routes().iter().any(|route| {
            route.provider == "OpenAI"
                && route.api_method_kind() == ModelRouteApiMethod::OpenAIOAuth
                && horon_agent_provider_core::oauth_account_label(&route.api_method)
                    == crate::auth::codex::active_account_label().as_deref()
                && route.available
        }));
    });
}

#[test]
fn test_on_auth_changed_refreshes_existing_openai_provider_credentials() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        let _enter = runtime.enter();

        crate::auth::codex::upsert_account_from_tokens(
            "openai-1",
            "stale-access-token",
            "test-refresh-token",
            None,
            None,
        )
        .expect("save stale test OpenAI auth");

        // The concrete OpenAI runtime is the removed OpenAI runtime; token
        // refresh itself was covered by its own tests. Here we assert the
        // MultiProvider wiring: on_auth_changed must call reload_credentials()
        // on an existing OpenAI runtime instead of replacing it.
        let existing = test_openai_runtime();

        crate::auth::codex::upsert_account_from_tokens(
            "openai-1",
            "fresh-access-token",
            "test-refresh-token",
            None,
            None,
        )
        .expect("save fresh test OpenAI auth");

        let provider = MultiProvider {
            openai: RwLock::new(Some(Arc::clone(&existing) as Arc<dyn Provider>)),
            openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
            active_openai_compatible_profile: RwLock::new(None),
            active: RwLock::new(ActiveProvider::OpenAI),
            startup_notices: RwLock::new(Vec::new()),
            initial_provider: Some(ActiveProvider::OpenAI),
            catalog_scope: next_catalog_scope(),
            routes_memo: std::sync::Mutex::new(None),
            post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };

        provider.on_auth_changed();

        let openai = provider
            .openai_provider()
            .expect("existing openai provider");
        assert!(
            Arc::ptr_eq(&openai, &(existing as Arc<dyn Provider>)),
            "on_auth_changed must reload credentials in place, not replace the runtime"
        );
    });
}

#[test]
fn test_summarize_model_catalog_refresh_ignores_display_only_age_suffix_changes() {
    let summary = summarize_model_catalog_refresh(
        vec!["anthropic/claude-sonnet-4".to_string()],
        vec!["anthropic/claude-sonnet-4".to_string()],
        vec![ModelRoute {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: "Fireworks".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: "fast, 5m ago".to_string(),
            usage: None,
            cheapness: None,
        }],
        vec![ModelRoute {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: "Fireworks".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: "fast, 6m ago".to_string(),
            usage: None,
            cheapness: None,
        }],
    );

    assert_eq!(
        summary.routes_changed, 0,
        "age-only detail churn should be ignored"
    );
}

#[test]
fn test_summarize_model_catalog_refresh_still_counts_meaningful_detail_changes() {
    let summary = summarize_model_catalog_refresh(
        vec!["anthropic/claude-sonnet-4".to_string()],
        vec!["anthropic/claude-sonnet-4".to_string()],
        vec![ModelRoute {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: "Fireworks".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: "fast, 5m ago".to_string(),
            usage: None,
            cheapness: None,
        }],
        vec![ModelRoute {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: "Fireworks".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: "cached, 6m ago".to_string(),
            usage: None,
            cheapness: None,
        }],
    );

    assert_eq!(summary.routes_changed, 1);
}

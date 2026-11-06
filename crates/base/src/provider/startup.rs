use super::*;

impl MultiProvider {
    pub(super) fn spawn_post_auth_model_refresh(&self, provider: Arc<dyn Provider>, provider_label: &'static str) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            crate::logging::auth_event(
                "post_auth_model_refresh_skipped",
                provider_label,
                &[("reason", "no_tokio_runtime")],
            );
            return;
        };

        self.post_auth_refreshes_pending
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let pending = Arc::clone(&self.post_auth_refreshes_pending);

        handle.spawn(async move {
            struct PendingGuard(Arc<std::sync::atomic::AtomicUsize>);
            impl Drop for PendingGuard {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                }
            }
            let _pending_guard = PendingGuard(pending);
            let refresh_started = std::time::Instant::now();
            crate::logging::auth_event("post_auth_model_refresh_started", provider_label, &[]);
            provider.invalidate_credentials().await;
            match provider.prefetch_models().await {
                Ok(()) => {
                    let duration_ms = refresh_started.elapsed().as_millis().to_string();
                    crate::logging::auth_event(
                        "post_auth_model_refresh_completed",
                        provider_label,
                        &[("duration_ms", duration_ms.as_str())],
                    );
                    crate::bus::Bus::global().publish_models_updated();
                }
                Err(err) => {
                    let reason = err.to_string();
                    let duration_ms = refresh_started.elapsed().as_millis().to_string();
                    crate::logging::auth_event(
                        "post_auth_model_refresh_failed",
                        provider_label,
                        &[("reason", reason.as_str()), ("duration_ms", duration_ms.as_str())],
                    );
                    crate::logging::info(&format!(
                        "Failed to refresh {} models after auth change: {}",
                        provider_label, err
                    ));
                }
            }
        });
    }

    pub(super) fn new_with_auth_status(auth_status: auth::AuthStatus) -> Self {
        // Earliest point every entry point (run, serve, acp) passes through.
        // Fetches the models.dev catalog before startup when the operator
        // enabled it and no catalog exists yet; a no-op otherwise and in test
        // builds.
        crate::model_pricing::ensure_fresh_at_startup();
        let provider_init_start = std::time::Instant::now();
        let cfg = crate::config::config();
        let provider_state = ProviderState::from_parts(cfg, &auth_status);
        let initial_provider = Self::initial_provider_from_env();
        if initial_provider.is_none()
            && std::env::var_os("HORON_AGENT_PROVIDER_PROFILE_ACTIVE").is_none()
            && std::env::var_os("HORON_AGENT_NAMED_PROVIDER_PROFILE").is_none()
            && let Some(pref) = provider_state.default_provider_key()
        {
            if let Some(profile) = crate::provider_catalog::resolve_openai_compatible_profile_selection(pref) {
                crate::provider_catalog::apply_openai_compatible_profile_env(Some(profile));
            } else if cfg.providers.contains_key(pref) {
                match crate::provider_catalog::apply_named_provider_profile_env_from_config(pref, cfg) {
                    Ok(profile_name) => {
                        crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_NAME", &profile_name);
                        crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE", "1");
                    }
                    Err(err) => {
                        crate::logging::warn(&format!("Failed to apply default provider profile '{}': {}", pref, err))
                    }
                }
            }
        }

        // Only direct OpenAI-compatible profile runtimes exist in this build;
        // the built-in OpenAI slot stays empty.
        let openai: Option<Arc<dyn Provider>> = None;

        let availability = ProviderAvailability {
            openai: openai.is_some(),
        };
        let mut active = Self::auto_default_provider(availability);

        if let Some(initial) = initial_provider {
            active = initial;
            let is_configured = availability.is_configured(initial);
            if is_configured {
                let display = if matches!(initial, ActiveProvider::OpenAI) {
                    crate::provider_catalog::active_openai_compatible_display_name()
                        .unwrap_or_else(|| Self::provider_key(initial).to_string())
                } else {
                    Self::provider_key(initial).to_string()
                };
                crate::logging::info(&format!("Using initial provider '{}' from CLI/environment", display));
            } else {
                crate::logging::warn(&format!(
                    "Initial provider '{}' is not configured; requests will fail until credentials are available or another model is selected",
                    Self::provider_key(initial)
                ));
            }
        } else if let Some(pref) = provider_state.default_provider_key() {
            if let Some(selection) = provider_state.default_provider_selection() {
                let preferred = selection.active_provider();
                let is_configured = provider_state
                    .preferred_provider_is_configured(availability)
                    .unwrap_or(false);
                if is_configured {
                    active = preferred;
                    crate::logging::info(&format!(
                        "Using preferred provider '{}' from config via {}",
                        pref,
                        provider_state
                            .preferred_provider_display_label()
                            .unwrap_or_else(|| selection.display_label())
                    ));
                } else {
                    crate::logging::warn(&format!(
                        "Preferred provider '{}' is not configured, using auto-detected default",
                        pref
                    ));
                }
            } else {
                crate::logging::warn(&format!(
                    "Unknown default_provider '{}' in config (expected: claude|openai|copilot|antigravity|gemini|cursor|bedrock|openrouter or an OpenAI-compatible profile such as deepseek|comtegra|zai|openai-compatible)",
                    pref
                ));
            }
        }

        let result = Self {
            openai: RwLock::new(openai),
            openai_compatible_profiles: RwLock::new(HashMap::new()),
            active_openai_compatible_profile: RwLock::new(None),
            active: RwLock::new(active),
            startup_notices: RwLock::new(Vec::new()),
            initial_provider,
            catalog_scope: next_catalog_scope(),
            routes_memo: Mutex::new(None),
            post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };

        // An explicit CLI/environment provider selection owns startup routing.
        // Applying the configured default model here can reactivate its configured
        // provider/profile before the caller pins a dual-auth credential mode.
        if result.initial_provider.is_none()
            && let Some(model) = provider_state.default_model()
        {
            if let Err(e) = result.set_config_default_model(model, provider_state.default_provider_key()) {
                crate::logging::warn(&format!("Failed to apply default_model '{}' from config: {}", model, e));
            } else {
                crate::logging::info(&format!("Applied default model '{}' from config", model));
            }
        }

        result.spawn_openai_catalog_refresh_if_needed();
        crate::logging::info(&format!(
            "[TIMING] provider_init: openai={}, total={}ms",
            result
                .openai
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some(),
            provider_init_start.elapsed().as_millis()
        ));
        result
    }

    pub(super) fn spawn_openai_catalog_refresh_if_needed(&self) {
        let Some(provider) = self.openai_provider() else {
            return;
        };
        if !begin_openai_model_catalog_refresh() {
            return;
        }

        tokio::spawn(async move {
            if let Err(err) = provider.prefetch_models().await {
                crate::logging::info(&format!(
                    "Failed to refresh OpenAI model catalog from provider bootstrap: {}",
                    err
                ));
            }
            finish_openai_model_catalog_refresh();
        });
    }

    /// Create a new MultiProvider, detecting available credentials
    pub fn new() -> Self {
        Self::new_with_auth_status(auth::AuthStatus::check())
    }

    /// Create a startup-optimized MultiProvider that avoids expensive auth probes.
    pub fn new_fast() -> Self {
        Self::new_with_auth_status(auth::AuthStatus::check_fast())
    }

    pub fn from_auth_status(auth_status: auth::AuthStatus) -> Self {
        Self::new_with_auth_status(auth_status)
    }

    /// Build a `MultiProvider` whose active runtime is the named
    /// `[providers.<name>]` profile from config.toml.
    ///
    /// The profile is resolved and installed immediately, so the returned
    /// provider is ready to serve `/chat/completions` without any environment
    /// projection step. A missing profile, an unsupported `type`, an invalid
    /// `base_url`, or a required API key that is absent is a named error.
    pub fn from_named_provider_profile(profile_name: &str) -> Result<Self> {
        let provider = Self::new_fast();
        provider.install_named_provider_profile(profile_name)?;
        Ok(provider)
    }

    /// Build a `MultiProvider` whose active runtime is one built-in
    /// `OpenAiCompatibleProfile`.
    pub fn from_openai_compatible_profile(profile: crate::provider_catalog::OpenAiCompatibleProfile) -> Result<Self> {
        let provider = Self::new_fast();
        provider.install_openai_compatible_profile(profile)?;
        Ok(provider)
    }

    fn install_named_provider_profile(&self, profile_name: &str) -> Result<()> {
        let config = crate::config::config()
            .providers
            .get(profile_name)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown provider profile '{}'. Add [providers.{}] to config.toml.",
                    profile_name,
                    profile_name
                )
            })?;
        let runtime = external::build_named_openai_compatible_runtime(profile_name, &config)?;
        let registry = ProviderRegistry::new(self);
        registry.install_compatible_profile(profile_name.to_string(), Arc::clone(&runtime));
        registry.set_active_compatible_profile(profile_name.to_string());
        self.set_active_provider(ActiveProvider::OpenAI);
        Ok(())
    }

    fn install_openai_compatible_profile(
        &self,
        profile: crate::provider_catalog::OpenAiCompatibleProfile,
    ) -> Result<()> {
        let runtime = external::build_openai_compatible_profile_runtime(profile)?;
        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
        let registry = ProviderRegistry::new(self);
        registry.install_compatible_profile(resolved.id.clone(), Arc::clone(&runtime));
        registry.set_active_compatible_profile(resolved.id);
        self.set_active_provider(ActiveProvider::OpenAI);
        Ok(())
    }

    /// Create with explicit initial provider preference
    pub fn with_preference(prefer_openai: bool) -> Self {
        let provider = Self::new();
        if provider.initial_provider.is_none() && prefer_openai && provider.openai_provider().is_some() {
            *provider.active.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = ActiveProvider::OpenAI;
        }
        provider
    }

    pub fn with_preference_fast(prefer_openai: bool) -> Self {
        let provider = Self::new_fast();
        if provider.initial_provider.is_none() && prefer_openai && provider.openai_provider().is_some() {
            *provider.active.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = ActiveProvider::OpenAI;
        }
        provider
    }

    pub(super) fn active_provider(&self) -> ActiveProvider {
        *self.active.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

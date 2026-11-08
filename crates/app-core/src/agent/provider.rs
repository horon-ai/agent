use super::*;

impl Agent {
    pub(super) fn capture_compaction_route(&mut self) {
        if self.session.route_api_method.is_some() {
            return;
        }
        if let Some(route) = self.compaction_route_from_provider() {
            self.session.route_api_method = Some(route);
            return;
        }
        if self.provider.name().eq_ignore_ascii_case("openai") {
            self.session.route_api_method = match self.provider.active_resolved_credential() {
                Some(horon_agent_provider_core::ResolvedCredential::ApiKey) => Some("openai-api".to_owned()),
                _ => None,
            };
        }
    }

    pub fn set_premium_mode(&self, mode: crate::provider::PremiumMode) {
        self.provider.set_premium_mode(mode);
    }

    pub fn premium_mode(&self) -> crate::provider::PremiumMode {
        self.provider.premium_mode()
    }

    pub fn provider_fork(&self) -> Arc<dyn Provider> {
        self.provider.fork()
    }

    pub fn provider_handle(&self) -> Arc<dyn Provider> {
        Arc::clone(&self.provider)
    }

    pub fn available_models(&self) -> Vec<&'static str> {
        self.provider.available_models()
    }

    pub fn available_models_for_switching(&self) -> Vec<String> {
        self.provider.available_models_for_switching()
    }

    pub fn available_models_display(&self) -> Vec<String> {
        self.provider.available_models_display()
    }

    pub fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        let mut routes = self.provider.model_routes();
        crate::model_usage::enrich_routes(&mut routes);
        routes
    }

    pub(super) fn begin_model_usage_turn(&mut self, message_id: &str) {
        self.session.model_usage_turn_id = Some(format!("{}:{}", self.session.id, message_id));
    }

    pub(super) fn model_usage_turn_id(&mut self) -> String {
        if let Some(id) = &self.session.model_usage_turn_id {
            return id.clone();
        }
        // Old sessions and direct loop callers have no durable anchor yet.
        // Internal reminders and tool-result rows do not start a logical turn.
        let message_id = self
            .session
            .visible_conversation_messages()
            .into_iter()
            .rev()
            .find(|message| {
                message.role == Role::User
                    && message.content.iter().any(|block| {
                        matches!(block, ContentBlock::Text { text, .. }
                    if !text.trim().is_empty() && !text.starts_with("[System reminder:"))
                            || matches!(block, ContentBlock::Image { .. } | ContentBlock::Audio { .. })
                    })
            })
            .map(|message| message.id.clone())
            .unwrap_or_else(|| "initial".to_string());
        self.begin_model_usage_turn(&message_id);
        self.session.model_usage_turn_id.clone().unwrap()
    }

    pub(super) fn record_failed_credential_route(&mut self, error: &str) {
        if !horon_agent_provider_core::classify_failover_error_message(error).should_mark_provider_unavailable() {
            return;
        }
        let provider = self.provider.name().to_ascii_lowercase();
        let (key, label) = match (provider.as_str(), self.provider.active_resolved_credential()) {
            ("openai", Some(horon_agent_provider_core::ResolvedCredential::ApiKey)) => ("openai-api", None),
            ("claude" | "anthropic", Some(horon_agent_provider_core::ResolvedCredential::ApiKey)) => {
                ("anthropic-api", None)
            }
            _ => return,
        };
        if crate::provider::provider_unavailability_detail_for_account_label(key, label).is_none() {
            crate::provider::record_provider_unavailable_for_account_label(key, label, error);
        }
    }

    pub(super) fn record_model_turn_usage(&mut self, turn_id: &str) {
        if self.session.is_debug {
            return;
        }
        let Some(mut route) =
            crate::model_usage::serving_route(self.provider.as_ref(), self.session.route_api_method.as_deref())
        else {
            return;
        };
        match crate::model_usage::record_turn(turn_id, &route) {
            Ok(usage) => {
                route.usage = Some(usage);
                Bus::global().publish(BusEvent::ModelUsageUpdated(route));
            }
            Err(error) => logging::warn(&format!("Could not record model turn usage: {error}")),
        }
    }

    pub fn model_catalog_snapshot(&self) -> horon_agent_provider_core::ModelCatalogSnapshot {
        horon_agent_provider_core::ModelCatalogSnapshot::new(
            Some(self.provider_name()),
            Some(self.provider_model()),
            self.available_models_display(),
            self.model_routes(),
        )
    }

    /// The session's registry, with a FRESH `CompactionManager`.
    ///
    /// `Clone` deliberately replaces the manager so a new subagent session owns
    /// an independent budget; `Agent::new_with_session` then seeds that manager
    /// from the child's session and its own context window. A caller that only
    /// reads skills or config may use this. A caller that EXECUTES a tool on
    /// behalf of this same session must use
    /// [`Self::registry_sharing_compaction`] instead, or the context-overflow
    /// guard in `Registry::execute` compares the default budget against the
    /// 18k overhead floor instead of the session's real numbers.
    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }

    /// The session's registry for a caller that will EXECUTE a tool on behalf
    /// of THIS session, such as a client running a tool by hand or a runtime
    /// command that spawns a worker.
    ///
    /// Identical to [`Self::registry`] except that the `CompactionManager` is
    /// shared, so the overflow guard sees the session's real budget and token
    /// count. Do NOT use this to build a subagent: a subagent needs its own
    /// budget, which is what [`Self::registry`] gives it.
    pub(crate) fn registry_sharing_compaction(&self) -> Registry {
        self.registry.clone_sharing_compaction()
    }

    /// Widen this session's tool surface by one tool installed after the
    /// session was built.
    ///
    /// A schema-spawned child receives its `yield` tool once the spawn's
    /// `output_schema` arrives, which is after both the tool allow-list and the
    /// load tiers were snapshotted. Without this the child would be told to call
    /// a tool that is filtered out of its advertised list and refused by the
    /// dispatch gate. The tool is forced eager: it exists to be called at
    /// completion, and a discoverable tool nobody was told to discover is never
    /// called.
    pub(crate) fn allow_tool(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.load_tiers.force_essential(name);
        if let Some(allowed) = self.allowed_tools.as_mut() {
            allowed.insert(name.to_string());
        }
        self.disabled_tools.remove(name);
        // The dispatcher reads the policy's frozen tier snapshot, so it has to
        // see the same eager entry the definition list now uses.
        crate::tool::register_session_load_tiers(&self.session.id, self.load_tiers.clone());
        crate::tool::allow_session_tool(&self.session.id, name);
    }

    pub async fn compaction_mode(&self) -> crate::config::CompactionMode {
        self.registry.compaction().read().await.mode()
    }

    pub async fn set_compaction_mode(&self, mode: crate::config::CompactionMode) -> Result<()> {
        let compaction = self.registry.compaction();
        let mut manager = compaction.write().await;
        manager.set_mode(mode);
        Ok(())
    }

    /// Compaction strategy the active provider reports as owner, if any.
    ///
    /// `None` means the provider leaves compaction to horon, so the configured
    /// strategy decides. Anything else means the provider compacts natively and
    /// horon's summarizer must stay out of the way for the same turn.
    pub(super) fn provider_owned_compaction_strategy(&self) -> Option<crate::config::CompactionStrategy> {
        use crate::config::CompactionStrategy;
        match self.provider.native_compaction_mode()?.as_str() {
            "auto" => Some(CompactionStrategy::OpenAiAuto),
            "explicit" => Some(CompactionStrategy::OpenAiExplicit),
            _ => None,
        }
    }

    fn refresh_compaction_budget(&self) {
        let compaction = self.registry.compaction();
        let provider_name = self.provider.name().to_string();
        let model = self.provider.model();
        let owned = self.provider_owned_compaction_strategy();
        let route = self.session.route_api_method.clone();
        match compaction.try_write() {
            Ok(mut manager) => {
                manager.set_budget(self.provider.context_window());
                manager.reconcile_strategy(owned);
                manager.observe_route_identity_with_route(&provider_name, &model, route.as_deref());
            }
            Err(_) => crate::logging::warn(
                "Could not refresh compaction token budget after provider change: compaction manager is busy",
            ),
        }
    }

    /// The context window the server resolves for the active route.
    ///
    /// Sent to clients on `ModelChanged` so a remote panel does not have to
    /// derive it from its own inert provider, which carries no model catalog and
    /// therefore always answers the generic default.
    pub fn provider_context_window(&self) -> usize {
        self.provider.context_window()
    }

    #[cfg(test)]
    pub(crate) async fn compaction_token_budget(&self) -> usize {
        self.registry.compaction().read().await.token_budget()
    }

    pub fn provider_messages(&mut self) -> Vec<Message> {
        self.session.messages_for_provider()
    }

    pub fn set_model(&mut self, model: &str) -> Result<()> {
        self.set_model_from_provider_state_event(model, crate::provider::ProviderModelSelectionSource::User)
    }

    pub fn set_route_selection(&mut self, selection: &crate::provider::RouteSelection) -> Result<()> {
        self.set_route_selection_from_provider_state_event(
            selection,
            crate::provider::ProviderModelSelectionSource::User,
        )
    }

    pub(crate) fn set_route_selection_from_auth(&mut self, selection: &crate::provider::RouteSelection) -> Result<()> {
        self.set_route_selection_from_provider_state_event(
            selection,
            crate::provider::ProviderModelSelectionSource::Auth,
        )
    }

    fn set_route_selection_from_provider_state_event(
        &mut self,
        selection: &crate::provider::RouteSelection,
        source: crate::provider::ProviderModelSelectionSource,
    ) -> Result<()> {
        self.provider.set_route_selection(selection)?;
        self.session.invalidate_event_generation()?;
        let resolved_model = self.provider.model();
        self.session.provider_key = Some(selection.runtime_key.stable_id());
        self.session.route_api_method = Some(selection.api_method.clone());
        self.session.model = Some(self.provider_model());
        let event = crate::provider::ProviderStateEvent::selected_model(source, resolved_model);
        self.provider_runtime_state.apply(event);
        self.refresh_compaction_budget();
        self.persist_session_best_effort("route selection");
        self.log_env_snapshot("set_route_selection");
        Ok(())
    }

    pub(crate) fn set_model_from_auth(&mut self, model: &str) -> Result<()> {
        self.set_model_from_provider_state_event(model, crate::provider::ProviderModelSelectionSource::Auth)
    }

    fn set_model_from_provider_state_event(
        &mut self,
        model: &str,
        source: crate::provider::ProviderModelSelectionSource,
    ) -> Result<()> {
        crate::provider::set_model_with_auth_refresh(self.provider.as_ref(), model)?;
        self.session.invalidate_event_generation()?;
        let resolved_model = self.provider.model();
        self.session.provider_key = crate::provider::MultiProvider::session_provider_key_after_model_switch(
            model,
            self.provider.name(),
            self.session.provider_key.as_deref(),
        );
        // A saved OAuth account route cannot survive a model switch: this
        // build has no OAuth credentials, so drop the stale label.
        if self
            .session
            .route_api_method
            .as_deref()
            .is_some_and(|method| horon_agent_provider_core::oauth_account_label(method).is_some())
        {
            self.session.route_api_method = None;
        }
        self.session.model = Some(self.provider_model());
        let event = crate::provider::ProviderStateEvent::selected_model(source, resolved_model);
        self.provider_runtime_state.apply(event);
        self.refresh_compaction_budget();
        self.persist_session_best_effort("model selection");
        self.log_env_snapshot("set_model");
        Ok(())
    }

    pub(crate) fn provider_model_selection_generation(&self) -> u64 {
        self.provider_runtime_state.selection_generation()
    }

    pub(crate) fn user_selected_provider_model_after(&self, generation: u64) -> bool {
        self.provider_runtime_state.user_selected_after(generation)
    }

    pub fn restore_reasoning_effort_from_session(&mut self) {
        if let Some(effort) = self.session.reasoning_effort.clone() {
            if let Err(e) = self.provider.set_reasoning_effort(&effort) {
                crate::logging::error(&format!(
                    "Failed to restore session reasoning effort '{}': {}",
                    effort, e
                ));
            }
        } else {
            self.session.reasoning_effort = self.provider.reasoning_effort();
        }
        // Mirror the effort into the deadlock-free side-table so server handlers
        // (e.g. the swarm seed handler) can learn this session's effort without
        // taking the agent lock.
        crate::session_effort::record_session_effort(&self.session.id, self.session.reasoning_effort.as_deref());
    }

    pub fn set_reasoning_effort(&mut self, effort: &str) -> Result<Option<String>> {
        self.provider.set_reasoning_effort(effort)?;
        self.session.invalidate_event_generation()?;
        let current = self.provider.reasoning_effort();
        self.session.reasoning_effort = current.clone();
        // Keep the side-table in sync (see `restore_reasoning_effort_from_session`).
        crate::session_effort::record_session_effort(&self.session.id, current.as_deref());
        self.log_env_snapshot("set_reasoning_effort");
        self.save_session()?;
        Ok(current)
    }

    pub fn set_agent_model_override(&mut self, target: &str, model: Option<String>) -> Result<()> {
        self.session.save_prepared()?;
        self.session.set_agent_model_override(target, model)
    }

    pub fn subagent_model(&self) -> Option<String> {
        self.session.subagent_model.clone()
    }

    pub fn set_subagent_model(&mut self, model: Option<String>) -> Result<()> {
        self.session.subagent_model = model;
        self.log_env_snapshot("set_subagent_model");
        self.save_session()?;
        Ok(())
    }

    pub fn session_provider_key(&self) -> Option<String> {
        self.session.provider_key.clone()
    }

    /// API method/runtime route used to select the active model (e.g.
    /// "openai-api", "claude-oauth", "openai-compatible:nvidia-nim"). Spawned
    /// swarm agents inherit this so they reconstruct the coordinator's exact
    /// auth route instead of falling back to the config default.
    pub fn session_route_api_method(&self) -> Option<String> {
        self.session.route_api_method.clone()
    }

    /// The credential the active provider will use for the next request, when
    /// the provider distinguishes OAuth (subscription) from API key (cost).
    /// Resolved authoritatively here so remote clients can render billing/usage
    /// without re-deriving it from the provider name.
    pub fn active_resolved_credential(&self) -> Option<horon_agent_provider_core::ResolvedCredential> {
        self.provider.active_resolved_credential()
    }

    pub fn set_session_provider_key(&mut self, provider_key: Option<String>) {
        self.session.provider_key = provider_key;
    }

    /// Bookmark or unbookmark the session, returning the effective label.
    pub fn set_session_saved(&mut self, saved: bool, label: Option<String>) -> Result<Option<String>> {
        if saved {
            self.session.mark_saved(label);
        } else {
            self.session.unmark_saved();
        }
        self.save_session()?;
        Ok(self.session.save_label.clone())
    }

    pub fn record_archive(&mut self, archived: bool, reason: &str) -> Result<bool> {
        self.session.record_archive(archived, reason)
    }

    pub fn archived_state(&self) -> Result<Option<bool>> {
        self.session.archived_state()
    }

    pub fn rename_session_title(&mut self, title: Option<String>) -> Result<String> {
        self.session.rename_title(title);
        self.log_env_snapshot("rename_session");
        self.save_session()?;
        Ok(self.session.display_title_or_name().to_string())
    }

    pub fn session_display_title_or_name(&self) -> String {
        self.session.display_title_or_name().to_string()
    }

    pub fn autoreview_enabled(&self) -> Option<bool> {
        self.session.autoreview_enabled
    }

    pub fn set_autoreview_enabled(&mut self, enabled: bool) -> Result<()> {
        self.session.autoreview_enabled = Some(enabled);
        self.log_env_snapshot("set_autoreview_enabled");
        self.save_session()?;
        Ok(())
    }

    pub fn autojudge_enabled(&self) -> Option<bool> {
        self.session.autojudge_enabled
    }

    pub fn set_autojudge_enabled(&mut self, enabled: bool) -> Result<()> {
        self.session.autojudge_enabled = Some(enabled);
        self.log_env_snapshot("set_autojudge_enabled");
        self.save_session()?;
        Ok(())
    }

    /// Set the working directory for this session
    pub fn set_working_dir(&mut self, dir: &str) {
        if self.session.working_dir.as_deref() == Some(dir) {
            return;
        }
        self.session.working_dir = Some(dir.to_string());
        self.refresh_agents_md_snapshot();
        self.session.refresh_initial_session_context_message();
        self.log_env_snapshot("working_dir");
    }

    /// Get the working directory for this session
    pub fn working_dir(&self) -> Option<&str> {
        self.session.working_dir.as_deref()
    }

    /// Record the extra directories the attached client advertised.
    ///
    /// The first list is folded into the immutable session context; a later
    /// change is queued as a one-time reminder for the next prompt.
    pub fn set_additional_dirs(&mut self, directories: Vec<String>) {
        if !self.session.set_additional_dirs(directories) {
            return;
        }
        self.persist_session_best_effort("additional directories");
    }

    /// Get the stored messages (for transcript export)
    pub fn messages(&self) -> &[StoredMessage] {
        self.session.active_context()
    }
}

/// The route vocabulary entry the active direct OpenAI-compatible profile
/// persists, if the provider is that runtime.
///
/// The profile's own model route carries `openai-compatible:<profile>`, which
/// session restore resolves back to the same runtime. A named
/// `[providers.<name>]` profile without a loaded catalog still yields the same
/// entry from its key.
impl Agent {
    fn compaction_route_from_provider(&self) -> Option<String> {
        let model = self.provider.model();
        let from_routes = self
            .provider
            .model_routes()
            .into_iter()
            .find(|route| route.model == model && route.api_method.starts_with("openai-compatible:"))
            .map(|route| route.api_method);
        from_routes.or_else(|| named_profile_route(self.session.provider_key.as_deref()))
    }
}

fn named_profile_route(provider_key: Option<&str>) -> Option<String> {
    let profile = provider_key.map(str::trim).filter(|profile| !profile.is_empty())?;
    crate::config::config()
        .providers
        .contains_key(profile)
        .then(|| format!("openai-compatible:{profile}"))
}

#[cfg(test)]
mod compaction_route_tests {
    use super::*;
    use crate::provider::EventStream;
    use async_trait::async_trait;
    use horon_agent_provider_core::ResolvedCredential;

    struct RouteProvider {
        name: &'static str,
        model: &'static str,
        routes: Vec<crate::provider::ModelRoute>,
        credential: Option<ResolvedCredential>,
    }

    #[async_trait]
    impl Provider for RouteProvider {
        async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }

        fn name(&self) -> &str {
            self.name
        }

        fn model(&self) -> String {
            self.model.to_string()
        }

        fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
            self.routes.clone()
        }

        fn active_resolved_credential(&self) -> Option<ResolvedCredential> {
            self.credential
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(RouteProvider {
                name: self.name,
                model: self.model,
                routes: self.routes.clone(),
                credential: self.credential,
            })
        }
    }

    fn route(api_method: &str) -> crate::provider::ModelRoute {
        crate::provider::ModelRoute {
            model: "qa-model".to_string(),
            provider: "QA".to_string(),
            api_method: api_method.to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
            usage: None,
        }
    }

    /// `capture_compaction_route` records the direct OpenAI-compatible route,
    /// and the route vocabulary resolves it back to the profile runtime.
    #[test]
    fn capture_compaction_route_names_the_direct_openai_compatible_profile() {
        let provider = Arc::new(RouteProvider {
            name: "OpenAI",
            model: "qa-model",
            routes: vec![route("openai-compatible:qa")],
            credential: None,
        });
        let mut agent = Agent::new(provider, Registry::empty());
        agent.capture_compaction_route();
        let recorded = agent
            .session
            .route_api_method
            .clone()
            .expect("the direct compatible runtime records a route");
        assert_eq!(recorded, "openai-compatible:qa");
        let parsed = horon_agent_provider_core::ModelRouteApiMethod::parse(&recorded);
        assert_eq!(parsed.profile_id(), Some("qa"));
        assert_eq!(
            horon_agent_provider_core::RuntimeKey::from_api_method(&parsed, ""),
            horon_agent_provider_core::RuntimeKey::OpenAiCompatible {
                profile_id: Some("qa".to_string())
            }
        );
    }

    /// The OpenAI route keeps its established credential-derived behaviour.
    #[test]
    fn capture_compaction_route_leaves_openai_to_the_credential_route() {
        let provider = Arc::new(RouteProvider {
            name: "OpenAI",
            model: "gpt-5",
            routes: vec![route("openai-api")],
            credential: Some(ResolvedCredential::ApiKey),
        });
        let mut agent = Agent::new(provider, Registry::empty());
        agent.capture_compaction_route();
        assert_eq!(agent.session.route_api_method.as_deref(), Some("openai-api"));
    }
}

#[cfg(test)]
mod registry_handles_tests {
    use super::*;
    use crate::provider::EventStream;
    use async_trait::async_trait;

    /// Answers nothing; only its context window matters here.
    struct WindowProvider(usize);

    #[async_trait]
    impl Provider for WindowProvider {
        async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }

        fn name(&self) -> &str {
            "window-test"
        }

        fn supports_compaction(&self) -> bool {
            false
        }

        fn context_window(&self) -> usize {
            self.0
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(WindowProvider(self.0))
        }
    }

    async fn budget_and_count(registry: &Registry) -> (usize, usize) {
        let manager = registry.compaction();
        let guard = manager.read().await;
        (guard.token_budget(), guard.effective_token_count())
    }

    /// The two handles differ in exactly one way, and the difference is what
    /// decides whether the overflow guard sees the session or a fresh 18k
    /// floor. A caller that EXECUTES a tool for this session must take the
    /// sharing handle.
    #[tokio::test]
    async fn session_registry_handle_shares_the_budget_and_the_plain_one_does_not() {
        let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        const SESSION_WINDOW: usize = 60_000;
        let agent = Agent::new(Arc::new(WindowProvider(SESSION_WINDOW)), Registry::empty());

        let (session_budget, session_count) = budget_and_count(&agent.registry_sharing_compaction()).await;
        let (plain_budget, plain_count) = budget_and_count(&agent.registry()).await;
        println!("QA sharing handle budget={session_budget} count={session_count}");
        println!("QA plain handle   budget={plain_budget} count={plain_count}");

        assert_eq!(
            session_budget, SESSION_WINDOW,
            "the sharing handle is the session's own manager"
        );
        assert_eq!(plain_budget, crate::compaction::DEFAULT_TOKEN_BUDGET);
        assert_eq!(plain_count, crate::compaction::SYSTEM_OVERHEAD_TOKENS);

        agent.registry().compaction().write().await.set_budget(11_000);
        assert_eq!(
            budget_and_count(&agent.registry_sharing_compaction()).await.0,
            SESSION_WINDOW,
            "a caller that keeps the plain handle cannot move the session's budget"
        );
    }
}

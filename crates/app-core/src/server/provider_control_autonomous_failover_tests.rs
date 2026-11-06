#![allow(clippy::await_holding_lock)]
use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::EventStream;
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;

/// Isolated `HORON_AGENT_HOME`: the account pool, session store and settings
/// file all resolve under it, so tests never touch the real machine.
struct IsolatedHome {
    previous: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
}

impl IsolatedHome {
    fn new() -> Self {
        let home = tempfile::TempDir::new().expect("isolated home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", home.path());
        crate::config::Config::invalidate_cache();
        Self { previous, _home: home }
    }

    fn path(&self) -> &std::path::Path {
        self._home.path()
    }

    /// Every test states the failover switch instead of inheriting the default.
    fn write_provider_settings(&self, autonomous_failover: bool) {
        std::fs::write(
            self.path().join("config.toml"),
            format!("[provider]\nautonomous_failover = {autonomous_failover}\n"),
        )
        .expect("write settings file");
        crate::config::Config::invalidate_cache();
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => crate::env::set_var("HORON_AGENT_HOME", previous),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn api_key_route(model: &str) -> ModelRoute {
    ModelRoute {
        model: model.to_string(),
        provider: "OpenAI".to_string(),
        api_method: "openai-api".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
        usage: None,
    }
}

fn failover_prompt(target_route: Option<&str>) -> ProviderFailoverPrompt {
    ProviderFailoverPrompt {
        from_provider: "claude".to_string(),
        from_label: "Anthropic (work)".to_string(),
        to_provider: "openai".to_string(),
        to_label: "OpenAI API key".to_string(),
        target_route: target_route.map(str::to_string),
        reason: "429 Too Many Requests".to_string(),
        estimated_input_chars: 1200,
        estimated_input_tokens: 300,
    }
}

struct FailoverTestProvider {
    model: StdMutex<String>,
    routes: Vec<ModelRoute>,
    applied: StdMutex<Vec<RouteSelection>>,
}

impl FailoverTestProvider {
    fn new(model: &str, routes: Vec<ModelRoute>) -> Self {
        Self {
            model: StdMutex::new(model.to_string()),
            routes,
            applied: StdMutex::new(Vec::new()),
        }
    }

    fn applied_routes(&self) -> Vec<RouteSelection> {
        self.applied.lock().expect("applied lock").clone()
    }

    fn set_model_string(&self, model: &str) {
        *self.model.lock().expect("model lock") = model.to_string();
    }
}

#[async_trait]
impl Provider for FailoverTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        panic!("autonomous failover must not send a request")
    }

    fn name(&self) -> &str {
        "FailoverTest"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        self.set_model_string(model);
        Ok(())
    }

    /// Overridden to record the structured selection, not only the model string.
    fn set_route_selection(&self, selection: &RouteSelection) -> anyhow::Result<()> {
        self.applied.lock().expect("applied lock").push(selection.clone());
        self.set_model_string(&selection.model);
        Ok(())
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        self.routes.clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: StdMutex::new(self.model()),
            routes: self.routes.clone(),
            applied: StdMutex::new(Vec::new()),
        })
    }
}

fn test_agent(model: &str, routes: Vec<ModelRoute>) -> (Arc<FailoverTestProvider>, Agent) {
    let provider = Arc::new(FailoverTestProvider::new(model, routes));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let session = crate::session::Session::create_with_id("session_autonomous_failover".to_string(), None, None);
    let agent = Agent::new_with_session(provider_dyn, crate::tool::Registry::empty(), session, None);
    (provider, agent)
}

/// Failover switched on, catalog supplied.
fn eligible_setup(routes: Vec<ModelRoute>) -> (IsolatedHome, Arc<FailoverTestProvider>, Agent) {
    let home = IsolatedHome::new();
    home.write_provider_settings(true);
    let (provider, agent) = test_agent("test-model-a", routes);
    (home, provider, agent)
}

#[tokio::test]
async fn autonomous_failover_disabled_never_switches_routes() {
    let _guard = crate::storage::lock_test_env();
    let home = IsolatedHome::new();
    home.write_provider_settings(false);
    let (provider, mut agent) = test_agent("test-model-a", vec![api_key_route("gpt-5.4")]);

    let error = apply_autonomous_failover(&failover_prompt(Some("openai-api:gpt-5.4")), &mut agent)
        .await
        .expect_err("a disabled switch must refuse even a fully eligible prompt");

    assert!(error.to_string().contains("disabled"), "{error}");
    assert_eq!(agent.provider_model(), "test-model-a", "the route must not move");
    assert!(provider.applied_routes().is_empty(), "no route was applied");
}

#[tokio::test]
async fn eligible_prompt_switches_the_route_to_the_catalog_selection() {
    let _guard = crate::storage::lock_test_env();
    let (_home, provider, mut agent) = eligible_setup(vec![api_key_route("gpt-5.4")]);
    assert_eq!(agent.provider_model(), "test-model-a");

    apply_autonomous_failover(&failover_prompt(Some("openai-api:gpt-5.4")), &mut agent)
        .await
        .expect("an eligible prompt switches the route");

    assert_eq!(agent.provider_model(), "gpt-5.4");
    let applied = provider.applied_routes();
    assert_eq!(applied.len(), 1, "exactly one route switch");
    assert_eq!(applied[0].routed_model_spec(), "openai-api:gpt-5.4");
    assert_eq!(applied[0].api_method, "openai-api");
    assert_eq!(applied[0].provider_label, "OpenAI");
}

#[tokio::test]
async fn a_cold_catalog_still_applies_the_self_describing_route() {
    let _guard = crate::storage::lock_test_env();
    // No catalog routes: an in-flight refresh, so the credential prefix is the
    // only description of the route.
    let (_home, provider, mut agent) = eligible_setup(Vec::new());

    apply_autonomous_failover(&failover_prompt(Some("openai-api:gpt-5.4")), &mut agent)
        .await
        .expect("a self-describing route applies without a catalog");

    assert_eq!(agent.provider_model(), "gpt-5.4");
    let applied = provider.applied_routes();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].routed_model_spec(), "openai-api:gpt-5.4");
}

#[tokio::test]
async fn prompt_without_a_target_route_requires_human_consent() {
    let _guard = crate::storage::lock_test_env();
    let (_home, provider, mut agent) = eligible_setup(vec![api_key_route("gpt-5.4")]);

    for target_route in [None, Some("   ")] {
        let error = apply_autonomous_failover(&failover_prompt(target_route), &mut agent)
            .await
            .expect_err("a legacy prompt must not switch routes on its own");
        assert!(error.to_string().contains("manual consent"), "{error}");
    }

    assert_eq!(agent.provider_model(), "test-model-a");
    assert!(provider.applied_routes().is_empty());
}

#[tokio::test]
async fn unknown_target_route_is_refused_instead_of_applied_approximately() {
    let _guard = crate::storage::lock_test_env();
    let (_home, provider, mut agent) = eligible_setup(vec![api_key_route("gpt-5.4")]);

    let error = apply_autonomous_failover(&failover_prompt(Some("not-a-route:gpt-5.4")), &mut agent)
        .await
        .expect_err("an unknown spec must be refused");

    assert!(
        error.to_string().contains("not a route this session can apply"),
        "{error}"
    );
    assert_eq!(agent.provider_model(), "test-model-a");
    assert!(provider.applied_routes().is_empty());
}

#[tokio::test]
async fn a_settings_file_without_the_failover_fields_reads_back_the_defaults() {
    let _guard = crate::storage::lock_test_env();
    let home = IsolatedHome::new();
    std::fs::write(
        home.path().join("config.toml"),
        "[provider]\nanthropic_cache_ttl_1h = false\n",
    )
    .expect("write settings file");
    crate::config::Config::invalidate_cache();

    let provider = crate::config::Config::load().provider;
    assert!(
        !provider.autonomous_failover,
        "unattended failover stays off until configured"
    );
    assert_eq!(
        provider.autonomous_failover_max_attempts, 3,
        "an omitted attempt budget defaults to 3, never to 0"
    );
    assert!(
        !provider.anthropic_cache_ttl_1h,
        "the rest of the settings file was actually read"
    );
}

#[tokio::test]
async fn applying_a_route_drops_a_stale_provider_session() {
    let _guard = crate::storage::lock_test_env();
    let (_home, provider, mut agent) = eligible_setup(vec![api_key_route("gpt-5.4")]);

    let mut stale =
        crate::session::Session::create_with_id("session_autonomous_failover_stale".to_string(), None, None);
    stale.provider_session_id = Some("upstream-session-from-the-old-route".to_string());
    // `save` skips a session with no visible message yet, so write the fixture
    // through `save_prepared`.
    stale.save_prepared().expect("save stale session");
    agent
        .restore_session("session_autonomous_failover_stale")
        .expect("restore stale session");
    assert_eq!(
        agent.debug_info()["provider_session_id"],
        serde_json::json!("upstream-session-from-the-old-route")
    );

    apply_autonomous_failover(&failover_prompt(Some("openai-api:gpt-5.4")), &mut agent)
        .await
        .expect("an eligible prompt switches the route");

    assert_eq!(agent.provider_model(), "gpt-5.4");
    assert_eq!(
        agent.debug_info()["provider_session_id"],
        serde_json::Value::Null,
        "the upstream session id of the old route must not survive the switch"
    );
    assert_eq!(provider.applied_routes().len(), 1);
}

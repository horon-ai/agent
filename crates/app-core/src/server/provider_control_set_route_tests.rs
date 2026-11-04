#![allow(clippy::await_holding_lock)]
use super::*;
use crate::auth::test_sandbox::AuthTestSandbox;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, ModelRoute, Provider, RouteSelection};
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;

const START_MODEL: &str = "test-model-a";
const REFUSAL: &str = "Auto-switch canceled: target account is no longer eligible in your auto-switch group.";

fn write_provider_settings(sandbox: &AuthTestSandbox, cross_provider_failover: &str) {
    std::fs::write(
        sandbox.root().join("config.toml"),
        format!("[provider]\ncross_provider_failover = \"{cross_provider_failover}\"\n"),
    )
    .expect("write settings file");
    crate::config::Config::invalidate_cache();
}

fn save_account_pool(members: &[&str]) {
    let pool = crate::auth::account_pool::AccountPool {
        members: members.iter().map(|member| (*member).to_string()).collect(),
        excluded: Vec::new(),
    };
    pool.save().expect("save account pool");
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

fn account_route(model: &str, label: &str) -> ModelRoute {
    ModelRoute {
        model: model.to_string(),
        provider: "OpenAI".to_string(),
        api_method: format!("openai-oauth@{label}"),
        available: true,
        detail: String::new(),
        cheapness: None,
        usage: None,
    }
}

struct SetRouteProvider {
    name: &'static str,
    model: StdMutex<String>,
    routes: Vec<ModelRoute>,
    applied: StdMutex<Vec<RouteSelection>>,
}

impl SetRouteProvider {
    fn applied_routes(&self) -> Vec<RouteSelection> {
        self.applied.lock().expect("applied lock").clone()
    }
}

#[async_trait]
impl Provider for SetRouteProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        panic!("a route switch must not send a request")
    }

    fn name(&self) -> &str {
        self.name
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    /// The source-eligibility check reads the credential kind, not only the name.
    fn active_resolved_credential(&self) -> Option<horon_agent_provider_core::ResolvedCredential> {
        Some(horon_agent_provider_core::ResolvedCredential::ApiKey)
    }

    fn set_route_selection(&self, selection: &RouteSelection) -> anyhow::Result<()> {
        self.applied.lock().expect("applied lock").push(selection.clone());
        *self.model.lock().expect("model lock") = selection.model.clone();
        Ok(())
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        self.routes.clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            name: self.name,
            model: StdMutex::new(self.model()),
            routes: self.routes.clone(),
            applied: StdMutex::new(Vec::new()),
        })
    }
}

fn test_agent(
    name: &'static str,
    routes: Vec<ModelRoute>,
) -> (
    Arc<SetRouteProvider>,
    Agent,
    mpsc::UnboundedSender<ServerEvent>,
    mpsc::UnboundedReceiver<ServerEvent>,
) {
    let provider = Arc::new(SetRouteProvider {
        name,
        model: StdMutex::new(START_MODEL.to_string()),
        routes,
        applied: StdMutex::new(Vec::new()),
    });
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let agent = Agent::new(provider_dyn, crate::tool::Registry::empty());
    let (tx, rx) = mpsc::unbounded_channel();
    (provider, agent, tx, rx)
}

fn assert_refused(rx: &mut mpsc::UnboundedReceiver<ServerEvent>, id: u64) {
    match rx.try_recv().expect("a refusal is reported to the client") {
        ServerEvent::ModelChanged {
            id: event_id,
            model,
            error,
            ..
        } => {
            assert_eq!(event_id, id);
            assert_eq!(error.as_deref(), Some(REFUSAL), "the refusal names its reason");
            assert_eq!(model, START_MODEL, "the fallback is the model still in force");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "one request produces exactly one event");
}

#[tokio::test]
async fn automatic_failover_refuses_a_target_while_the_mode_is_manual() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, "manual");
    save_account_pool(&["openai-api"]);
    let (provider, mut agent, tx, mut rx) = test_agent("openai", vec![api_key_route("gpt-5.4")]);
    let selection = RouteSelection::from_model_route(&api_key_route("gpt-5.4"));
    assert!(
        automatic_failover_source_is_pool_member(&agent),
        "precondition: the source account is eligible, so only the mode refuses"
    );
    assert!(
        crate::provider::MultiProvider::is_failover_route_pool_approved(&selection.routed_model_spec()),
        "precondition: the target route is eligible"
    );
    assert_eq!(
        crate::config::Config::load().provider.cross_provider_failover,
        crate::config::CrossProviderFailoverMode::Manual,
        "precondition: automatic cross-provider switching is off"
    );

    apply_set_route(11, selection, true, &mut agent, &tx);

    assert_refused(&mut rx, 11);
    assert_eq!(agent.provider_model(), START_MODEL, "the route must not move");
    assert!(provider.applied_routes().is_empty(), "no route was applied");
}

#[tokio::test]
async fn automatic_failover_refuses_when_the_source_account_left_the_pool() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, "countdown");
    save_account_pool(&["openai-api"]);
    // A metered Claude API key is not in the pool, so the current route is not
    // one automatic switching may leave from.
    let (provider, mut agent, tx, mut rx) = test_agent("claude", vec![api_key_route("gpt-5.4")]);
    let selection = RouteSelection::from_model_route(&api_key_route("gpt-5.4"));
    assert!(
        !automatic_failover_source_is_pool_member(&agent),
        "precondition: the source account is outside the auto-switch pool"
    );
    assert!(
        crate::provider::MultiProvider::is_failover_route_pool_approved(&selection.routed_model_spec()),
        "precondition: the target route is eligible"
    );

    apply_set_route(12, selection, true, &mut agent, &tx);

    assert_refused(&mut rx, 12);
    assert_eq!(agent.provider_model(), START_MODEL, "the route must not move");
    assert!(provider.applied_routes().is_empty(), "no route was applied");
}

#[tokio::test]
async fn automatic_failover_refuses_a_target_route_outside_the_pool() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, "countdown");
    save_account_pool(&["openai-api"]);
    // `work` is not a saved login, so this exact route is not pool-approved.
    let route = account_route("gpt-5.4", "work");
    let (provider, mut agent, tx, mut rx) = test_agent("openai", vec![route.clone()]);
    let selection = RouteSelection::from_model_route(&route);
    assert!(
        automatic_failover_source_is_pool_member(&agent),
        "precondition: the source account is eligible"
    );
    assert!(
        !crate::provider::MultiProvider::is_failover_route_pool_approved(&selection.routed_model_spec()),
        "precondition: only the target route is ineligible"
    );

    apply_set_route(13, selection, true, &mut agent, &tx);

    assert_refused(&mut rx, 13);
    assert_eq!(agent.provider_model(), START_MODEL, "the route must not move");
    assert!(provider.applied_routes().is_empty(), "no route was applied");
}

#[tokio::test]
async fn a_manual_switch_applies_a_route_the_automatic_gate_would_refuse() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, "manual");
    // Nothing is enrolled: every one of the three automatic conditions refuses.
    save_account_pool(&[]);
    let route = account_route("gpt-5.4", "work");
    let (provider, mut agent, tx, mut rx) = test_agent("openai", vec![route.clone()]);
    let selection = RouteSelection::from_model_route(&route);
    let expected_spec = selection.routed_model_spec();
    assert!(!automatic_failover_source_is_pool_member(&agent));
    assert!(!crate::provider::MultiProvider::is_failover_route_pool_approved(
        &selection.routed_model_spec()
    ));

    apply_set_route(14, selection, false, &mut agent, &tx);

    match rx.try_recv().expect("the switch is reported to the client") {
        ServerEvent::ModelChanged {
            id: 14, model, error, ..
        } => {
            assert_eq!(error, None, "a manual switch is never gated by the pool");
            assert_eq!(model, "gpt-5.4");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert_eq!(agent.provider_model(), "gpt-5.4", "the route actually moved");
    let applied = provider.applied_routes();
    assert_eq!(applied.len(), 1, "exactly one route switch");
    assert_eq!(applied[0].routed_model_spec(), expected_spec);
}

use super::super::turn_failover::{FailoverAttempt, ProviderErrorAction};
use super::*;
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::auth::test_sandbox::AuthTestSandbox;
use crate::provider::{EventStream, ModelRoute, ProviderFailoverPrompt, RouteSelection};

const TARGET_ROUTE: &str = "openai-api:gpt-5.4";
const START_MODEL: &str = "test-model-a";

fn write_provider_settings(
    sandbox: &AuthTestSandbox,
    autonomous_failover: bool,
    max_attempts: u32,
    cap_secs: u64,
    max_retries: u32,
) {
    std::fs::write(
        sandbox.root().join("config.toml"),
        format!(
            "[provider]\nautonomous_failover = {autonomous_failover}\nautonomous_failover_max_attempts = {max_attempts}\nretry_backoff_cap_secs = {cap_secs}\nmax_retries = {max_retries}\n"
        ),
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
        provider: "[OI]".to_string(),
        api_method: "openai-api".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
        usage: None,
    }
}

fn failover_prompt() -> ProviderFailoverPrompt {
    ProviderFailoverPrompt {
        from_provider: "claude".to_string(),
        from_label: "Claude (work)".to_string(),
        to_provider: "openai".to_string(),
        to_label: "[OI] API key".to_string(),
        target_route: Some(TARGET_ROUTE.to_string()),
        reason: "429 Too Many Requests".to_string(),
        estimated_input_chars: 1200,
        estimated_input_tokens: 300,
    }
}

fn failover_error() -> anyhow::Error {
    anyhow::Error::new(crate::provider::FailoverRequired::new(failover_prompt()))
}

fn text_stream(text: &str) -> EventStream {
    Box::pin(futures::stream::iter(vec![
        Ok(StreamEvent::TextDelta(text.to_string())),
        Ok(StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        }),
    ]))
}

#[derive(Clone, Copy)]
enum Behavior {
    FailOnceThenSucceed,
    AlwaysFail,
    RateLimitedThenSucceed { retry_after_secs: u64 },
    RateLimitedWithoutHeader,
}

struct FailoverProvider {
    model: StdMutex<String>,
    routes: Vec<ModelRoute>,
    applied: StdMutex<Vec<RouteSelection>>,
    calls: AtomicUsize,
    behavior: Behavior,
}

impl FailoverProvider {
    fn new(behavior: Behavior) -> Self {
        Self {
            model: StdMutex::new(START_MODEL.to_string()),
            routes: vec![api_key_route("gpt-5.4")],
            applied: StdMutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            behavior,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn applied_routes(&self) -> Vec<RouteSelection> {
        self.applied.lock().expect("applied lock").clone()
    }
}

#[async_trait]
impl Provider for FailoverProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behavior {
            Behavior::AlwaysFail => Err(failover_error()),
            Behavior::FailOnceThenSucceed if call == 0 => Err(failover_error()),
            Behavior::FailOnceThenSucceed => Ok(text_stream("done")),
            Behavior::RateLimitedThenSucceed { retry_after_secs } if call == 0 => Err(anyhow::Error::new(
                StreamError::new("429 Too Many Requests".to_string(), Some(retry_after_secs)),
            )),
            Behavior::RateLimitedThenSucceed { .. } => Ok(text_stream("done")),
            Behavior::RateLimitedWithoutHeader => Err(anyhow::Error::new(StreamError::new(
                "429 Too Many Requests".to_string(),
                None,
            ))),
        }
    }

    fn name(&self) -> &str {
        "FailoverTest"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
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
            model: StdMutex::new(self.model()),
            routes: self.routes.clone(),
            applied: StdMutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            behavior: self.behavior,
        })
    }
}

fn failover_agent(behavior: Behavior) -> (Arc<FailoverProvider>, Agent) {
    let provider = Arc::new(FailoverProvider::new(behavior));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let agent = Agent::new(provider_dyn, Registry::empty());
    (provider, agent)
}

fn eligible_setup(
    behavior: Behavior,
    max_attempts: u32,
    cap_secs: u64,
    max_retries: u32,
) -> (
    AuthTestSandbox,
    Arc<FailoverProvider>,
    Agent,
    mpsc::UnboundedReceiver<ServerEvent>,
) {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, true, max_attempts, cap_secs, max_retries);
    save_account_pool(&["openai-api"]);
    let (provider, agent) = failover_agent(behavior);
    let (tx, rx) = mpsc::unbounded_channel();
    crate::server::register_session_event_sink(&agent.session.id, tx);
    (sandbox, provider, agent, rx)
}

fn failover_switches(events: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
    let mut switches = Vec::new();
    while let Ok(event) = events.try_recv() {
        if matches!(event, ServerEvent::FailoverSwitched { .. }) {
            switches.push(event);
        }
    }
    switches
}

#[tokio::test]
async fn blocking_turn_resends_once_on_another_route_without_a_client() {
    let (_home, provider, mut agent, mut events) = eligible_setup(Behavior::FailOnceThenSucceed, 3, 30, 8);
    assert!(
        crate::provider::MultiProvider::is_failover_route_pool_approved(TARGET_ROUTE),
        "precondition: the pool approves the target route"
    );
    assert_eq!(agent.provider_model(), START_MODEL);

    let text = agent.run_turn(false).await.expect("the resent turn completes");

    assert_eq!(text, "done");
    assert_eq!(provider.calls(), 2, "one failed request and exactly one resend");
    assert_eq!(agent.provider_model(), "gpt-5.4", "the route actually changed");
    let applied = provider.applied_routes();
    assert_eq!(applied.len(), 1, "exactly one route switch");
    assert_eq!(applied[0].routed_model_spec(), TARGET_ROUTE);

    let switches = failover_switches(&mut events);
    assert_eq!(switches.len(), 1, "FailoverSwitched is emitted exactly once");
    match &switches[0] {
        ServerEvent::FailoverSwitched {
            chain_id,
            from,
            to,
            reason,
            attempt,
            max,
        } => {
            assert!(!chain_id.is_empty(), "the event carries its budget chain");
            assert_eq!(from, START_MODEL);
            assert_eq!(to, "gpt-5.4");
            assert_eq!(reason, "429 Too Many Requests");
            assert_eq!(*attempt, 1, "1-based attempt number");
            assert_eq!(*max, 3);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn blocking_turn_stops_after_the_attempt_cap_with_a_clear_error() {
    let (_home, provider, mut agent, mut events) = eligible_setup(Behavior::AlwaysFail, 3, 30, 8);

    let error = agent
        .run_turn(false)
        .await
        .expect_err("an exhausted chain must end the turn");

    let message = error.to_string();
    assert!(
        message.contains("failover attempts exhausted"),
        "the final error must name the exhausted budget: {message}"
    );
    assert_eq!(provider.calls(), 4, "the initial request plus exactly 3 resends");
    assert_eq!(provider.applied_routes().len(), 3, "one switch per spent attempt");
    assert_eq!(failover_switches(&mut events).len(), 3);
}

#[tokio::test]
async fn attached_client_keeps_the_original_error_and_never_switches() {
    let (_home, provider, mut agent, mut events) = eligible_setup(Behavior::AlwaysFail, 3, 30, 8);
    let error = failover_error();

    let outcome = agent.attempt_autonomous_failover(&error, true).await;

    assert!(
        matches!(outcome, FailoverAttempt::NotApplicable),
        "a client is attached, so the countdown path stays in charge"
    );
    assert_eq!(
        error.to_string(),
        failover_prompt().to_error_message(),
        "the original error reaches the client unchanged"
    );
    assert_eq!(agent.provider_model(), START_MODEL, "the route must not move");
    assert!(provider.applied_routes().is_empty());
    assert_eq!(agent.failover_attempts(), 0, "no attempt may be spent");
    assert!(failover_switches(&mut events).is_empty());
    assert_eq!(provider.calls(), 0, "the interception sends no request");
}

#[tokio::test]
async fn disabled_setting_returns_the_original_error_unchanged() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, false, 3, 30, 8);
    save_account_pool(&["openai-api"]);
    let (provider, mut agent) = failover_agent(Behavior::AlwaysFail);
    let (tx, mut events) = mpsc::unbounded_channel();
    crate::server::register_session_event_sink(&agent.session.id, tx);

    let error = agent
        .run_turn(false)
        .await
        .expect_err("a disabled switch must not rescue the turn");

    assert_eq!(error.to_string(), failover_prompt().to_error_message());
    assert_eq!(provider.calls(), 1, "the turn is not resent");
    assert_eq!(agent.provider_model(), START_MODEL);
    assert!(provider.applied_routes().is_empty());
    assert!(failover_switches(&mut events).is_empty());
    assert_eq!(agent.failover_attempts(), 0);
}

#[tokio::test]
async fn a_refused_switch_ends_the_unattended_turn_instead_of_looping() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, true, 3, 30, 8);
    // No pool membership: the target route fails the eligibility gate, so the
    // switch is refused and the run must not resend on its own.
    save_account_pool(&[]);
    let (provider, mut agent) = failover_agent(Behavior::AlwaysFail);
    let (tx, mut events) = mpsc::unbounded_channel();
    crate::server::register_session_event_sink(&agent.session.id, tx);
    assert!(
        !crate::server::session_has_attached_client(&agent.session.id),
        "precondition: nobody is attached, so the unattended path runs"
    );
    assert!(
        !crate::provider::MultiProvider::is_failover_route_pool_approved(TARGET_ROUTE),
        "precondition: only the target eligibility check stands between the prompt and a switch"
    );

    let error = agent
        .run_turn(false)
        .await
        .expect_err("a refused switch must end the turn with the provider error");

    assert_eq!(error.to_string(), failover_prompt().to_error_message());
    assert_eq!(
        provider.calls(),
        1,
        "the refused switch must not enter the turn loop again"
    );
    assert_eq!(agent.provider_model(), START_MODEL, "the route must not move");
    assert!(provider.applied_routes().is_empty());
    assert!(failover_switches(&mut events).is_empty(), "no switch is announced");
    assert_eq!(
        agent.failover_attempts(),
        1,
        "the attempt is spent before the switch is applied"
    );
}

#[tokio::test]
async fn a_retry_after_above_the_cap_switches_routes_instead_of_waiting() {
    let (_home, provider, mut agent, mut events) = eligible_setup(Behavior::FailOnceThenSucceed, 3, 30, 8);
    let error: anyhow::Error = anyhow::Error::new(StreamError::new("429 Too Many Requests".to_string(), Some(120)))
        .context(crate::provider::FailoverRequired::new(failover_prompt()));

    assert_eq!(
        Agent::rate_limit_decision(&error),
        Some(crate::network_retry::RetryAfterDecision::FailoverInstead),
        "120 seconds above a 30 second cap switches rather than waits"
    );

    let outcome = agent.handle_provider_error(&error, false).await;

    assert!(
        matches!(outcome, ProviderErrorAction::Resend { attempt: 1, max: 3 }),
        "the switch spends the first failover attempt"
    );
    assert_eq!(agent.provider_model(), "gpt-5.4");
    assert_eq!(failover_switches(&mut events).len(), 1, "the switch is announced once");
    assert_eq!(provider.applied_routes().len(), 1);
}

#[tokio::test]
async fn a_retry_after_above_the_cap_waits_exactly_the_cap_without_failover() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, false, 3, 1, 8);
    save_account_pool(&["openai-api"]);
    let (provider, mut agent) = failover_agent(Behavior::RateLimitedThenSucceed { retry_after_secs: 120 });
    let (tx, _events) = mpsc::unbounded_channel();
    crate::server::register_session_event_sink(&agent.session.id, tx);

    let error: anyhow::Error = anyhow::Error::new(StreamError::new("429 Too Many Requests".to_string(), Some(120)));
    assert_eq!(
        Agent::rate_limit_decision(&error),
        Some(crate::network_retry::RetryAfterDecision::Wait(Duration::from_secs(1))),
        "the wait is the cap, never the requested 120 seconds"
    );

    let text = agent.run_turn(false).await.expect("the retried turn completes");

    assert_eq!(text, "done");
    assert_eq!(provider.calls(), 2, "one rate-limited request and exactly one retry");
    assert_eq!(agent.provider_model(), START_MODEL, "a wait never switches routes");
    assert!(provider.applied_routes().is_empty());
}

#[tokio::test]
async fn a_rate_limit_without_a_header_keeps_the_existing_error_path() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, false, 3, 30, 8);
    save_account_pool(&["openai-api"]);
    let (provider, mut agent) = failover_agent(Behavior::RateLimitedWithoutHeader);
    let (tx, _events) = mpsc::unbounded_channel();
    crate::server::register_session_event_sink(&agent.session.id, tx);

    let error: anyhow::Error = anyhow::Error::new(StreamError::new("429 Too Many Requests".to_string(), None));
    assert_eq!(
        Agent::rate_limit_decision(&error),
        Some(crate::network_retry::RetryAfterDecision::Backoff),
        "no usable header falls back to the existing backoff"
    );

    let turn_error = agent
        .run_turn(false)
        .await
        .expect_err("without a hint the turn keeps today's failure");
    assert!(turn_error.to_string().contains("429 Too Many Requests"), "{turn_error}");
    assert_eq!(provider.calls(), 1, "no retry without a Retry-After hint");
}

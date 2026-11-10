use super::super::*;
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;

use crate::auth::test_sandbox::AuthTestSandbox;
use crate::provider::{EventStream, RouteSelection};

const START_MODEL: &str = "m1";
const FALLBACK_MODEL: &str = "m2";
const PROFILE: &str = "fallback-gw";

fn write_config(sandbox: &AuthTestSandbox, fallback_models: Option<&str>, autonomous_failover: bool) {
    let fallbacks = fallback_models
        .map(|list| format!("fallback_models = {list}\n"))
        .unwrap_or_default();
    std::fs::write(
        sandbox.root().join("config.toml"),
        format!(
            "[provider]\nautonomous_failover = {autonomous_failover}\nautonomous_failover_max_attempts = 3\nretry_backoff_cap_secs = 30\nmax_retries = 8\n\n[providers.{PROFILE}]\ndefault_model = \"{START_MODEL}\"\n{fallbacks}"
        ),
    )
    .expect("write config file");
    crate::config::Config::invalidate_cache();
    crate::env::set_var("HORON_AGENT_NAMED_PROVIDER_PROFILE", PROFILE);
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
    /// Every model answers `503`, so the list is spent and the turn fails.
    AlwaysUnavailable,
    /// `503` on the first request only, so the second model answers.
    UnavailableOnce,
    /// A credential failure must never spend the list.
    Unauthorized,
    /// A `429` whose `Retry-After` exceeds the cap, with no typed prompt.
    LongRateLimit,
}

struct FallbackProvider {
    model: StdMutex<String>,
    applied: StdMutex<Vec<RouteSelection>>,
    calls: StdMutex<Vec<String>>,
    behavior: Behavior,
}

impl FallbackProvider {
    fn new(behavior: Behavior) -> Self {
        Self {
            model: StdMutex::new(START_MODEL.to_string()),
            applied: StdMutex::new(Vec::new()),
            calls: StdMutex::new(Vec::new()),
            behavior,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn applied_routes(&self) -> Vec<RouteSelection> {
        self.applied.lock().expect("applied lock").clone()
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let model = self.model.lock().expect("model lock").clone();
        let call = {
            let mut calls = self.calls.lock().expect("calls lock");
            calls.push(model);
            calls.len()
        };
        match self.behavior {
            Behavior::AlwaysUnavailable => Err(StreamError::new("503 Service Unavailable".to_string(), None).into()),
            Behavior::UnavailableOnce if call == 1 => {
                Err(StreamError::new("503 Service Unavailable".to_string(), None).into())
            }
            Behavior::UnavailableOnce => Ok(text_stream("done")),
            Behavior::Unauthorized => Err(StreamError::new("401 Unauthorized".to_string(), None).into()),
            Behavior::LongRateLimit if call == 1 => {
                Err(StreamError::new("429 Too Many Requests".to_string(), Some(120)).into())
            }
            Behavior::LongRateLimit => Ok(text_stream("done")),
        }
    }

    fn name(&self) -> &str {
        "FallbackTest"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        *self.model.lock().expect("model lock") = model.to_string();
        Ok(())
    }

    fn set_route_selection(&self, selection: &RouteSelection) -> anyhow::Result<()> {
        self.applied.lock().expect("applied lock").push(selection.clone());
        *self.model.lock().expect("model lock") = selection.model.clone();
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: StdMutex::new(self.model()),
            applied: StdMutex::new(Vec::new()),
            calls: StdMutex::new(Vec::new()),
            behavior: self.behavior,
        })
    }
}

fn setup(
    behavior: Behavior,
    fallback_models: Option<&str>,
    autonomous_failover: bool,
) -> (AuthTestSandbox, Arc<FallbackProvider>, Agent) {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_config(&sandbox, fallback_models, autonomous_failover);
    let provider = Arc::new(FallbackProvider::new(behavior));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let agent = Agent::new(provider_dyn, Registry::empty());
    (sandbox, provider, agent)
}

#[tokio::test]
async fn model_fallback_completes_on_the_second_model_after_a_503() {
    let (_home, provider, mut agent) = setup(Behavior::UnavailableOnce, Some("[\"m2\"]"), false);
    assert_eq!(agent.provider_model(), START_MODEL);

    let text = agent.run_turn(false).await.expect("the resent turn completes");

    assert_eq!(text, "done");
    assert_eq!(
        provider.calls(),
        vec![START_MODEL.to_string(), FALLBACK_MODEL.to_string()],
        "one failed request on the first model, one resend on the second"
    );
}

#[tokio::test]
async fn model_fallback_never_spends_the_list_on_a_credential_error() {
    let (_home, provider, mut agent) = setup(Behavior::Unauthorized, Some("[\"m2\"]"), false);

    let error = agent.run_turn(false).await.expect_err("a 401 must end the turn");

    assert!(
        error.to_string().contains("401"),
        "the original error survives: {error}"
    );
    assert_eq!(provider.calls(), vec![START_MODEL.to_string()], "the list is untouched");
}

#[tokio::test]
async fn model_fallback_restores_the_session_model_on_the_next_turn() {
    let (_home, provider, mut agent) = setup(Behavior::UnavailableOnce, Some("[\"m2\"]"), false);

    agent
        .run_turn(false)
        .await
        .expect("the first turn completes on the fallback");
    assert_eq!(agent.provider_model(), FALLBACK_MODEL, "the turn ran on the fallback");
    assert_eq!(
        agent.session.model.as_deref(),
        Some(START_MODEL),
        "the session's persisted selection never moves"
    );

    agent.run_turn(false).await.expect("the next turn completes");

    assert_eq!(
        provider.calls(),
        vec![
            START_MODEL.to_string(),
            FALLBACK_MODEL.to_string(),
            START_MODEL.to_string()
        ],
        "the next turn requests the session's own model again"
    );
}

#[tokio::test]
async fn model_fallback_without_a_configured_list_keeps_the_original_failure() {
    let (_home, provider, mut agent) = setup(Behavior::UnavailableOnce, None, false);

    let error = agent
        .run_turn(false)
        .await
        .expect_err("without a list the turn fails as before");

    assert!(
        error.to_string().contains("503"),
        "the original error survives: {error}"
    );
    assert_eq!(provider.calls(), vec![START_MODEL.to_string()]);
    assert_eq!(agent.provider_model(), START_MODEL);
}

#[tokio::test]
async fn model_fallback_comes_before_any_account_or_provider_switch() {
    let (_home, provider, mut agent) = setup(Behavior::LongRateLimit, Some("[\"m2\"]"), true);

    let text = agent
        .run_turn(false)
        .await
        .expect("the model step rescues the turn without waiting out the cap");

    assert_eq!(text, "done");
    assert_eq!(
        provider.calls(),
        vec![START_MODEL.to_string(), FALLBACK_MODEL.to_string()]
    );
    assert!(
        provider.applied_routes().is_empty(),
        "no route selection is applied: the model step runs first"
    );
    assert_eq!(
        agent.failover_attempts(),
        0,
        "the account/provider chain never spends an attempt"
    );
}

#[tokio::test]
async fn model_fallback_restores_the_session_model_when_the_turn_fails() {
    let (_home, provider, mut agent) = setup(Behavior::AlwaysUnavailable, Some("[\"m2\"]"), false);

    let error = agent.run_turn(false).await.expect_err("both models failed");

    assert!(
        error.to_string().contains("503"),
        "the last failure reaches the caller: {error}"
    );
    assert_eq!(
        provider.calls(),
        vec![START_MODEL.to_string(), FALLBACK_MODEL.to_string()],
        "each entry is tried once per turn"
    );
    assert_eq!(
        agent.provider_model(),
        START_MODEL,
        "a failed turn puts the session's model back"
    );
}

#[tokio::test]
async fn runtime_info_reports_the_model_fallback_list_and_the_serving_model() {
    let (_home, _provider, mut agent) = setup(Behavior::UnavailableOnce, Some("[\"m2\"]"), false);
    assert_eq!(agent.fallback_models(), vec![FALLBACK_MODEL.to_string()]);
    assert_eq!(agent.last_serving_model(), None, "nothing has answered yet");

    agent.run_turn(false).await.expect("the turn completes on the fallback");

    assert_eq!(
        agent.fallback_models(),
        vec![FALLBACK_MODEL.to_string()],
        "the configured list is reported as configured"
    );
    assert_eq!(
        agent.last_serving_model().as_deref(),
        Some(FALLBACK_MODEL),
        "runtime info names the model that answered, not the restored one"
    );
}

#[tokio::test]
async fn runtime_info_omits_the_model_fallback_list_when_none_is_configured() {
    let (_home, _provider, agent) = setup(Behavior::UnavailableOnce, None, false);

    assert!(agent.fallback_models().is_empty(), "no list, no reported entries");
    assert_eq!(agent.last_serving_model(), None);
}

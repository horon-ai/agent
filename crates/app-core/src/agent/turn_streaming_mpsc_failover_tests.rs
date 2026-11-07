use super::*;
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::auth::test_sandbox::AuthTestSandbox;
use crate::provider::{EventStream, ModelRoute, ProviderFailoverPrompt, RouteSelection};

const TARGET_ROUTE: &str = "openai-api:gpt-5.4";
const START_MODEL: &str = "test-model-a";

fn write_provider_settings(sandbox: &AuthTestSandbox, autonomous_failover: bool, max_attempts: u32) {
    std::fs::write(
        sandbox.root().join("config.toml"),
        format!(
            "[provider]\nautonomous_failover = {autonomous_failover}\nautonomous_failover_max_attempts = {max_attempts}\n"
        ),
    )
    .expect("write settings file");
    crate::config::Config::invalidate_cache();
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behavior {
    PartialThenFailOnce,
    AlwaysFail,
}

struct StreamingFailoverProvider {
    model: StdMutex<String>,
    routes: Vec<ModelRoute>,
    applied: StdMutex<Vec<RouteSelection>>,
    calls: AtomicUsize,
    behavior: Behavior,
}

impl StreamingFailoverProvider {
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
}

#[async_trait]
impl Provider for StreamingFailoverProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match (self.behavior, call) {
            (Behavior::AlwaysFail, _) => Err(failover_error()),
            (Behavior::PartialThenFailOnce, 0) => {
                let events: Vec<anyhow::Result<StreamEvent>> = vec![
                    Ok(StreamEvent::TextDelta("partial answer".to_string())),
                    Err(failover_error()),
                ];
                Ok(Box::pin(futures::stream::iter(events)))
            }
            (Behavior::PartialThenFailOnce, _) => Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta("partial answer".to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }),
            ]))),
        }
    }

    fn name(&self) -> &str {
        "StreamingFailoverTest"
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

fn setup(
    behavior: Behavior,
    autonomous_failover: bool,
) -> (
    AuthTestSandbox,
    Arc<StreamingFailoverProvider>,
    Agent,
    mpsc::UnboundedReceiver<ServerEvent>,
) {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_provider_settings(&sandbox, autonomous_failover, 3);
    let provider = Arc::new(StreamingFailoverProvider::new(behavior));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut agent = Agent::new(provider_dyn, Registry::empty());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "answer me".to_string(),
            cache_control: None,
        }],
    );
    let (tx, rx) = mpsc::unbounded_channel();
    crate::server::register_session_event_sink(&agent.session.id, tx);
    (sandbox, provider, agent, rx)
}

fn count_matching(events: &mut mpsc::UnboundedReceiver<ServerEvent>, matches: fn(&ServerEvent) -> bool) -> usize {
    let mut count = 0;
    while let Ok(event) = events.try_recv() {
        if matches(&event) {
            count += 1;
        }
    }
    count
}

#[tokio::test]
async fn streaming_turn_rolls_back_partial_output_and_resends_on_the_new_route() {
    let (_home, provider, mut agent, mut events) = setup(Behavior::PartialThenFailOnce, true);
    let (event_tx, mut turn_events) = mpsc::unbounded_channel();

    agent
        .run_turn_streaming_mpsc(event_tx)
        .await
        .expect("the resent turn completes");

    assert_eq!(provider.calls(), 2, "one failed stream and exactly one resend");
    assert_eq!(agent.provider_model(), "gpt-5.4", "the route actually changed");

    let rollbacks = count_matching(&mut turn_events, |event| {
        matches!(event, ServerEvent::RetryRollback { .. })
    });
    assert_eq!(rollbacks, 1, "partial output is rolled back exactly once");
    let switches = count_matching(&mut events, |event| {
        matches!(event, ServerEvent::FailoverSwitched { .. })
    });
    assert_eq!(switches, 1, "FailoverSwitched reaches the session sink once");

    let assistant_text: Vec<String> = agent
        .session
        .active_context()
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistant_text,
        vec!["partial answer".to_string()],
        "the resent text appears once, never duplicated"
    );
}

#[tokio::test]
async fn streaming_turn_stops_after_the_attempt_cap_with_a_clear_error() {
    let (_home, provider, mut agent, mut events) = setup(Behavior::AlwaysFail, true);
    let (event_tx, _turn_events) = mpsc::unbounded_channel();

    let error = agent
        .run_turn_streaming_mpsc(event_tx)
        .await
        .expect_err("an exhausted chain must end the turn");

    let message = error.to_string();
    assert!(
        message.contains("failover attempts exhausted"),
        "the final error must name the exhausted budget: {message}"
    );
    assert_eq!(provider.calls(), 4, "the initial request plus exactly 3 resends");
    let switches = count_matching(&mut events, |event| {
        matches!(event, ServerEvent::FailoverSwitched { .. })
    });
    assert_eq!(switches, 3, "one announced switch per spent attempt");
}

#[tokio::test]
async fn streaming_turn_without_the_setting_returns_the_original_error() {
    let (_home, provider, mut agent, mut events) = setup(Behavior::AlwaysFail, false);
    let (event_tx, _turn_events) = mpsc::unbounded_channel();

    let error = agent
        .run_turn_streaming_mpsc(event_tx)
        .await
        .expect_err("a disabled switch must not rescue the turn");

    assert_eq!(error.to_string(), failover_prompt().to_error_message());
    assert_eq!(provider.calls(), 1, "the turn is not resent");
    assert_eq!(agent.provider_model(), START_MODEL);
    let switches = count_matching(&mut events, |event| {
        matches!(event, ServerEvent::FailoverSwitched { .. })
    });
    assert_eq!(switches, 0);
}

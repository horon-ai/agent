#![allow(clippy::await_holding_lock)]
use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::EventStream;
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;
use tokio::time::{Duration, timeout};

struct IsolatedRuntimeDir {
    _prev_runtime: Option<std::ffi::OsString>,
    _temp: tempfile::TempDir,
}

impl IsolatedRuntimeDir {
    fn new() -> Self {
        let temp = tempfile::TempDir::new().expect("runtime dir");
        let prev_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", temp.path());
        Self {
            _prev_runtime: prev_runtime,
            _temp: temp,
        }
    }
}

impl Drop for IsolatedRuntimeDir {
    fn drop(&mut self) {
        if let Some(prev_runtime) = self._prev_runtime.take() {
            crate::env::set_var("HORON_AGENT_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("HORON_AGENT_RUNTIME_DIR");
        }
    }
}

#[derive(Default)]
struct TestEffortProvider {
    model: StdMutex<Option<String>>,
    effort: StdMutex<Option<String>>,
    service_tier: StdMutex<Option<String>>,
    transport: StdMutex<Option<String>>,
}

#[async_trait]
impl Provider for TestEffortProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        panic!("complete should not run in provider control test")
    }

    fn name(&self) -> &str {
        "test-effort"
    }

    fn model(&self) -> String {
        self.model
            .lock()
            .expect("model lock")
            .clone()
            .unwrap_or_else(|| "test-model-a".to_string())
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        *self.model.lock().expect("model lock") = Some(model.to_string());
        Ok(())
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        vec!["test-model-a".to_string(), "test-model-b".to_string()]
    }

    fn context_window(&self) -> usize {
        if self.model() == "test-model-b" { 32_000 } else { 16_000 }
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.effort.lock().expect("effort lock").clone()
    }

    fn set_reasoning_effort(&self, effort: &str) -> anyhow::Result<()> {
        *self.effort.lock().expect("effort lock") = Some(effort.to_string());
        Ok(())
    }

    fn service_tier(&self) -> Option<String> {
        self.service_tier.lock().expect("service lock").clone()
    }

    fn set_service_tier(&self, service_tier: &str) -> anyhow::Result<()> {
        *self.service_tier.lock().expect("service lock") = Some(service_tier.to_string());
        Ok(())
    }

    fn transport(&self) -> Option<String> {
        self.transport.lock().expect("transport lock").clone()
    }

    fn set_transport(&self, transport: &str) -> anyhow::Result<()> {
        *self.transport.lock().expect("transport lock") = Some(transport.to_string());
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: StdMutex::new(Some(self.model())),
            effort: StdMutex::new(self.reasoning_effort()),
            service_tier: StdMutex::new(self.service_tier()),
            transport: StdMutex::new(self.transport()),
        })
    }
}

async fn test_agent(
    session_id: &str,
) -> (
    Arc<TestEffortProvider>,
    Arc<Mutex<Agent>>,
    mpsc::UnboundedSender<ServerEvent>,
    mpsc::UnboundedReceiver<ServerEvent>,
) {
    let provider = Arc::new(TestEffortProvider::default());
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let registry = crate::tool::Registry::new(Arc::clone(&provider_dyn)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some(provider.model());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider_dyn),
        registry,
        session,
        None,
    )));
    let (client_event_tx, client_event_rx) = mpsc::unbounded_channel();
    (provider, agent, client_event_tx, client_event_rx)
}

#[tokio::test]
async fn set_reasoning_effort_does_not_wait_for_busy_agent_lock() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();

    let (provider, agent, client_event_tx, mut client_event_rx) = test_agent("session_busy_reasoning_effort").await;
    let busy_agent_lock = agent.lock().await;

    timeout(
        Duration::from_millis(100),
        handle_set_reasoning_effort(7, "low".to_string(), &agent, &client_event_tx),
    )
    .await
    .expect("reasoning effort changes must not wait for a busy agent mutex");

    assert!(client_event_rx.try_recv().is_err());

    drop(busy_agent_lock);

    let event = timeout(Duration::from_secs(1), client_event_rx.recv())
        .await
        .expect("deferred reasoning effort change should finish after agent is idle");
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    assert!(matches!(
        event,
        Some(ServerEvent::ReasoningEffortChanged {
            id: 7,
            effort: Some(effort),
            error: None,
        }) if effort == "low"
    ));
}

#[tokio::test]
async fn set_model_does_not_wait_for_busy_agent_lock() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();

    let (provider, agent, client_event_tx, mut client_event_rx) = test_agent("session_busy_set_model").await;
    let busy_agent_lock = agent.lock().await;

    timeout(
        Duration::from_millis(100),
        handle_set_model(8, "test-model-b".to_string(), &agent, &client_event_tx),
    )
    .await
    .expect("model changes must not wait for a busy agent mutex");

    assert!(client_event_rx.try_recv().is_err());

    drop(busy_agent_lock);

    let event = timeout(Duration::from_secs(1), client_event_rx.recv())
        .await
        .expect("deferred model change should finish after agent is idle");
    assert_eq!(provider.model(), "test-model-b");
    assert_eq!(agent.lock().await.compaction_token_budget().await, 32_000);
    assert!(matches!(
        event,
        Some(ServerEvent::ModelChanged {
            id: 8,
            model,
            provider_name: Some(provider_name),
            error: None,
            ..
        }) if model == "test-model-b" && provider_name == "test-effort"
    ));
}

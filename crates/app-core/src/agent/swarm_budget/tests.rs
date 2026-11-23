use super::*;
use crate::auth::test_sandbox::AuthTestSandbox;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone)]
struct BudgetFixture {
    requests: Arc<AtomicUsize>,
    stall: bool,
}

#[async_trait]
impl Provider for BudgetFixture {
    fn name(&self) -> &str {
        "swarm-budget-fixture"
    }

    fn model(&self) -> String {
        "fixture".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let first = futures::stream::iter([Ok(StreamEvent::TextDelta("findings".into()))]);
        use futures::StreamExt;
        if self.stall {
            Ok(Box::pin(first.chain(futures::stream::pending())))
        } else {
            Ok(Box::pin(first.chain(futures::stream::iter([Ok(
                StreamEvent::MessageEnd {
                    stop_reason: Some("stop".into()),
                },
            )]))))
        }
    }
}

fn sandbox(settings: &str) -> AuthTestSandbox {
    let sandbox = AuthTestSandbox::new().unwrap();
    std::fs::write(sandbox.root().join("config.toml"), format!("[agents]\n{settings}\n")).unwrap();
    crate::config::Config::invalidate_cache();
    sandbox
}

fn agent_fixture(stall: bool, parent: Option<String>) -> (Agent, Arc<BudgetFixture>) {
    let provider = Arc::new(BudgetFixture {
        requests: Arc::new(AtomicUsize::new(0)),
        stall,
    });
    let agent = Agent::new_with_parent_and_initial_working_dir(provider.clone(), Registry::empty(), None, parent, None);
    (agent, provider)
}

#[tokio::test]
async fn request_limit_reports_the_cutoff_and_leaves_sibling_and_parent_running() {
    let _sandbox = sandbox("swarm_child_request_limit = 1");
    let (mut parent, parent_provider) = agent_fixture(false, None);
    let (mut limited, provider) = agent_fixture(false, Some(parent.session_id().to_owned()));
    let (mut sibling, sibling_provider) = agent_fixture(false, Some(parent.session_id().to_owned()));
    let (tx, mut events) = mpsc::unbounded_channel();
    limited
        .run_once_streaming_mpsc("first", vec![], None, tx.clone())
        .await
        .unwrap();
    let error = limited
        .run_once_streaming_mpsc("another", vec![], None, tx)
        .await
        .expect_err("a second model request exceeds this child's budget");
    assert!(error.to_string().contains("model request limit reached"));
    let events: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(events.iter().any(|event| matches!(
        event,
        ServerEvent::TurnStopped {
            reason: TurnStopReason::LimitReached,
            ..
        }
    )));
    assert_eq!(
        events
            .iter()
            .filter(
                |event| matches!(event, ServerEvent::StatusDetail { detail } if detail.contains("last model request"))
            )
            .count(),
        1
    );
    assert_eq!(provider.requests.load(Ordering::Relaxed), 1);

    let (tx, _events) = mpsc::unbounded_channel();
    sibling
        .run_once_streaming_mpsc("finish", vec![], None, tx.clone())
        .await
        .unwrap();
    parent
        .run_once_streaming_mpsc("first", vec![], None, tx.clone())
        .await
        .unwrap();
    parent
        .run_once_streaming_mpsc("second", vec![], None, tx)
        .await
        .unwrap();
    assert_eq!(sibling_provider.requests.load(Ordering::Relaxed), 1);
    assert_eq!(parent_provider.requests.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn runtime_bound_cancels_a_stalled_stream_without_stopping_its_sibling() {
    let _sandbox = sandbox("swarm_child_runtime_secs = 1");
    let (parent, _) = agent_fixture(false, None);
    let (mut stalled, _) = agent_fixture(true, Some(parent.session_id().to_owned()));
    let (mut sibling, _) = agent_fixture(false, Some(parent.session_id().to_owned()));
    let (tx, mut events) = mpsc::unbounded_channel();
    let (stalled, sibling) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            stalled.run_once_streaming_mpsc("stall after text", vec![], None, tx),
            sibling.run_once_streaming_mpsc("finish", vec![], None, mpsc::unbounded_channel().0)
        )
    })
    .await
    .expect("the runtime bound allows both turns to settle");
    assert!(
        stalled
            .expect_err("the stalled child reached its deadline")
            .to_string()
            .contains("runtime limit reached")
    );
    assert!(sibling.is_ok());
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(event, ServerEvent::TurnStopped { reason: TurnStopReason::LimitReached, message, .. } if message.contains("runtime limit reached"))));
}

#[test]
fn spawn_effort_respects_the_cap_and_the_models_supported_levels() {
    use horon_agent_swarm_core::clamp_spawn_effort;

    let model = ["low", "medium", "xhigh"];
    assert_eq!(
        clamp_spawn_effort(Some("xhigh"), "high", &model).unwrap(),
        Some("medium")
    );
    assert_eq!(clamp_spawn_effort(Some("low"), "high", &model).unwrap(), Some("low"));
    assert!(clamp_spawn_effort(Some("xhigh"), "minimal", &model).is_err());
}

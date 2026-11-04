use super::*;

use crate::agent::turn_recovery_wiring::TURN_RECOVERY_EXHAUSTED_PREFIX;
use horon_agent_agent_runtime::turn_recovery::MAX_AUTO_CONTINUATIONS;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::provider::EventStream;
use crate::tool::Registry;

#[derive(Clone)]
struct TurnStateProbeProvider {
    rounds: Arc<Mutex<VecDeque<Vec<StreamEvent>>>>,
    failing: Arc<AtomicBool>,
}

impl TurnStateProbeProvider {
    fn scripted(rounds: Vec<Vec<StreamEvent>>) -> (Arc<dyn Provider>, Arc<AtomicBool>) {
        let failing = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn Provider> = Arc::new(Self {
            rounds: Arc::new(Mutex::new(rounds.into())),
            failing: Arc::clone(&failing),
        });
        (provider, failing)
    }
}

#[async_trait::async_trait]
impl Provider for TurnStateProbeProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!("probe provider failure"));
        }
        let events = self
            .rounds
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::MessageEnd { stop_reason: None }]);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "turn-state-probe"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn truncated_round() -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta("partial".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("max_tokens".into()),
        },
    ]
}

fn truncated_rounds(count: usize) -> Vec<Vec<StreamEvent>> {
    (0..count).map(|_| truncated_round()).collect()
}

fn completed_round() -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta("done".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]
}

fn probe_agent(provider: Arc<dyn Provider>) -> Agent {
    let mut agent = Agent::new(provider, Registry::empty());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "go".into(),
            cache_control: None,
        }],
    );
    agent
}

fn assert_normal_turn_owns_its_state(agent: &Agent) {
    assert_eq!(
        agent.turn_state().outcome(),
        Some(TurnOutcome::Stop),
        "a completed turn records Stop"
    );
    assert_eq!(agent.turn_state().final_text(), "done");
    assert_eq!(
        agent.turn_state().auto_continuations(),
        1,
        "the truncated first response spent exactly one automatic continuation"
    );
    assert_eq!(
        agent.turn_state().incomplete_continuations(),
        0,
        "turn recovery owns the budget-truncation class, so the older incomplete path must not count it twice"
    );
}

fn assert_failed_turn_owns_its_state(agent: &Agent) {
    assert_eq!(
        agent.turn_state().outcome(),
        Some(TurnOutcome::Error),
        "a provider failure records Error"
    );
    assert!(
        agent.turn_state().final_text().is_empty(),
        "a failed turn carries no final text, got {:?}",
        agent.turn_state().final_text()
    );
}

fn assert_truncated_turn_owns_its_state(agent: &Agent) {
    assert_eq!(
        agent.turn_state().outcome(),
        Some(TurnOutcome::Length),
        "an unrecovered truncation records Length"
    );
    let final_text = agent.turn_state().final_text();
    assert!(
        final_text.starts_with("partial"),
        "the partial output survives the exhaustion, got {final_text:?}"
    );
    assert!(
        final_text.contains(TURN_RECOVERY_EXHAUSTED_PREFIX),
        "the terminal notice is delivered with the partial output, got {final_text:?}"
    );
    assert_eq!(
        agent.turn_state().auto_continuations(),
        MAX_AUTO_CONTINUATIONS,
        "the automatic-continuation budget is spent exactly once"
    );
}

fn assert_cancelled_turn_owns_its_state(agent: &Agent) {
    assert_eq!(
        agent.turn_state().outcome(),
        Some(TurnOutcome::Aborted),
        "a cancel observed by the loop records Aborted"
    );
}

#[tokio::test]
async fn turn_state_owns_blocking_transitions() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, failing) = TurnStateProbeProvider::scripted(vec![truncated_round(), completed_round()]);
    let mut agent = probe_agent(provider);

    let text = agent.run_turn(false).await.expect("the turn completes");
    assert_eq!(text, "done");
    assert_normal_turn_owns_its_state(&agent);

    let (truncated_provider, _truncated_failing) = TurnStateProbeProvider::scripted(truncated_rounds(
        Agent::MAX_INCOMPLETE_CONTINUATION_ATTEMPTS as usize + 1,
    ));
    let mut truncated = probe_agent(truncated_provider);
    let partial = truncated
        .run_turn(false)
        .await
        .expect("the turn ends with partial output");
    assert!(
        partial.starts_with("partial"),
        "the partial output survives the exhaustion, got {partial:?}"
    );
    assert!(
        partial.contains(TURN_RECOVERY_EXHAUSTED_PREFIX),
        "the terminal notice is returned with the partial output, got {partial:?}"
    );
    assert_truncated_turn_owns_its_state(&truncated);

    failing.store(true, Ordering::SeqCst);
    let error = agent.run_turn(false).await.expect_err("the provider fails");
    assert!(error.to_string().contains("probe provider failure"), "got {error}");
    assert_failed_turn_owns_its_state(&agent);

    let (cancelled_provider, _cancelled_failing) = TurnStateProbeProvider::scripted(vec![completed_round()]);
    let mut cancelled = probe_agent(cancelled_provider);
    cancelled.request_graceful_shutdown();
    cancelled
        .run_turn(false)
        .await
        .expect("a cancelled turn returns without an error");
    assert_cancelled_turn_owns_its_state(&cancelled);
}

#[tokio::test]
async fn turn_state_owns_streaming_transitions() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (provider, failing) = TurnStateProbeProvider::scripted(vec![truncated_round(), completed_round()]);
    let mut agent = probe_agent(provider);

    let (tx, _rx) = mpsc::unbounded_channel();
    agent
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("the streaming turn completes");
    assert_normal_turn_owns_its_state(&agent);

    let (truncated_provider, _truncated_failing) = TurnStateProbeProvider::scripted(truncated_rounds(
        Agent::MAX_INCOMPLETE_CONTINUATION_ATTEMPTS as usize + 1,
    ));
    let mut truncated = probe_agent(truncated_provider);
    let (tx, _rx) = mpsc::unbounded_channel();
    truncated
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("the streaming turn ends with partial output");
    assert_truncated_turn_owns_its_state(&truncated);

    failing.store(true, Ordering::SeqCst);
    let (tx, _rx) = mpsc::unbounded_channel();
    let error = agent.run_turn_streaming_mpsc(tx).await.expect_err("the provider fails");
    assert!(error.to_string().contains("probe provider failure"), "got {error}");
    assert_failed_turn_owns_its_state(&agent);

    let (cancelled_provider, _cancelled_failing) = TurnStateProbeProvider::scripted(vec![completed_round()]);
    let mut cancelled = probe_agent(cancelled_provider);
    cancelled.request_graceful_shutdown();
    let (tx, _rx) = mpsc::unbounded_channel();
    cancelled
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("a cancelled turn returns without an error");
    assert_cancelled_turn_owns_its_state(&cancelled);
}

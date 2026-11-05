use super::*;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use horon_agent_agent_runtime::{InterruptSignal, SoftInterruptMessage, SoftInterruptSource};

use crate::provider::EventStream;
use crate::tool::{ConcurrencyClass, Registry, Tool, ToolOutput};
use futures::FutureExt;

fn message(content: &str) -> SoftInterruptMessage {
    SoftInterruptMessage {
        content: content.to_string(),
        images: Vec::new(),
        urgent: false,
        source: SoftInterruptSource::User,
        client_id: None,
        input_id: None,
    }
}

fn stored_contents(session_id: &str) -> Vec<String> {
    crate::soft_interrupt_store::load(session_id)
        .expect("soft interrupt store")
        .into_iter()
        .map(|message| message.content)
        .collect()
}

struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "soft interrupt commit test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        ConcurrencyClass::Ordered
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new("ok"))
    }
}

enum Round {
    Stream(Vec<StreamEvent>),
    Fail(String),
    Panic,
    /// Fires the turn's cancel signal before answering.
    CancelThen(Vec<StreamEvent>),
    /// Queues a message before answering.
    QueueThen(String, Vec<StreamEvent>),
}

#[derive(Clone)]
struct ScriptedProvider {
    rounds: Arc<Mutex<VecDeque<Round>>>,
    cancel: Arc<Mutex<Option<InterruptSignal>>>,
    queue: Arc<Mutex<Option<horon_agent_agent_runtime::SoftInterruptQueue>>>,
}

impl ScriptedProvider {
    fn new(rounds: Vec<Round>) -> Self {
        Self {
            rounds: Arc::new(Mutex::new(rounds.into())),
            cancel: Arc::new(Mutex::new(None)),
            queue: Arc::new(Mutex::new(None)),
        }
    }

    fn next_events(&self) -> Result<EventStream> {
        let round = self.rounds.lock().unwrap().pop_front().unwrap_or(Round::Stream(vec![
            StreamEvent::TextDelta("done".into()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            },
        ]));
        let events = match round {
            Round::Stream(events) => events,
            Round::Fail(error) => return Err(anyhow::anyhow!(error)),
            Round::Panic => panic!("scripted provider panicked inside the turn loop"),
            Round::CancelThen(events) => {
                if let Some(signal) = self.cancel.lock().unwrap().as_ref() {
                    signal.fire();
                }
                events
            }
            Round::QueueThen(content, events) => {
                if let Some(queue) = self.queue.lock().unwrap().as_ref() {
                    queue.lock().expect("queue lock").push(message(&content));
                }
                events
            }
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

#[async_trait::async_trait]
impl Provider for ScriptedProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        self.next_events()
    }

    async fn complete_split(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.next_events()
    }

    fn name(&self) -> &str {
        "soft-interrupt-commit-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn round_end() -> StreamEvent {
    StreamEvent::MessageEnd {
        stop_reason: Some("end_turn".into()),
    }
}

fn text_round() -> Vec<StreamEvent> {
    vec![StreamEvent::TextDelta("done".into()), round_end()]
}

fn tool_round(id: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolUseStart {
            id: id.into(),
            name: "read".into(),
        },
        StreamEvent::ToolInputDeltaFor {
            id: id.into(),
            delta: "{\"file_path\":\"a\"}".into(),
        },
        StreamEvent::ToolUseEndFor { id: id.into() },
        round_end(),
    ]
}

async fn agent_with(rounds: Vec<Round>) -> (Agent, Arc<ScriptedProvider>) {
    let provider = Arc::new(ScriptedProvider::new(rounds));
    let registry = Registry::empty();
    registry
        .register("read".to_string(), Arc::new(EchoTool) as Arc<dyn Tool>)
        .await;
    let mut agent = Agent::new(provider.clone(), registry);
    *provider.cancel.lock().unwrap() = Some(agent.graceful_shutdown_signal());
    *provider.queue.lock().unwrap() = Some(agent.soft_interrupt_queue());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "go".into(),
            cache_control: None,
        }],
    );
    (agent, provider)
}

fn transcript_occurrences(agent: &Agent, needle: &str) -> usize {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| message.content.iter())
        .filter(|block| match block {
            ContentBlock::Text { text, .. } => text.contains(needle),
            _ => false,
        })
        .count()
}

#[tokio::test]
async fn commit_after_successful_turn_clears_disk() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) =
        agent_with(vec![Round::Stream(tool_round("call-a")), Round::Stream(text_round())]).await;
    let session_id = agent.session_id().to_string();
    agent.queue_soft_interrupt("keep me once".into(), Vec::new(), false, SoftInterruptSource::User);
    crate::soft_interrupt_store::append(&session_id, message("keep me once")).expect("seed the disk copy");
    assert_eq!(stored_contents(&session_id), vec!["keep me once"]);

    let injected = agent.inject_soft_interrupts(None).await;
    assert_eq!(injected.len(), 1);
    assert_eq!(
        agent.soft_interrupt_queue().in_flight_len(),
        1,
        "an injected message is staged as in flight until the turn settles"
    );
    assert_eq!(agent.soft_interrupt_queue().pending_len(), 0);

    agent.run_turn(false).await.expect("the turn completes");

    assert_eq!(
        agent.soft_interrupt_queue().in_flight_len(),
        0,
        "a successful turn commits the messages it injected"
    );
    assert!(
        stored_contents(&session_id).is_empty(),
        "the disk copy is cleared once the turn committed, got {:?}",
        stored_contents(&session_id)
    );
    assert_eq!(
        transcript_occurrences(&agent, "keep me once"),
        1,
        "the injected message stays in the transcript exactly once"
    );

    let again = agent.inject_soft_interrupts(None).await;
    assert!(again.is_empty(), "a committed message is never injected a second time");
}

#[tokio::test]
async fn restore_after_error_turn_keeps_disk() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = agent_with(vec![
        Round::Stream(tool_round("call-a")),
        Round::Fail("provider exploded".into()),
    ])
    .await;
    let session_id = agent.session_id().to_string();
    agent.queue_soft_interrupt("retry me".into(), Vec::new(), false, SoftInterruptSource::User);
    crate::soft_interrupt_store::append(&session_id, message("retry me")).expect("seed the disk copy");

    let error = agent.run_turn(false).await.expect_err("the provider fails");
    assert!(error.to_string().contains("provider exploded"), "got {error}");

    assert_eq!(
        agent.soft_interrupt_queue().in_flight_len(),
        0,
        "a failed turn restores"
    );
    assert_eq!(
        agent.soft_interrupt_queue().pending_len(),
        1,
        "the message goes back to the front of the queue"
    );
    assert_eq!(
        stored_contents(&session_id),
        vec!["retry me"],
        "the disk copy is left alone so a restart can inject it again"
    );
}

#[tokio::test]
async fn restore_after_cancel_turn_keeps_disk() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = agent_with(vec![
        Round::Stream(tool_round("call-a")),
        Round::CancelThen(text_round()),
    ])
    .await;
    let session_id = agent.session_id().to_string();
    agent.queue_soft_interrupt("cancelled".into(), Vec::new(), false, SoftInterruptSource::User);
    crate::soft_interrupt_store::append(&session_id, message("cancelled")).expect("seed the disk copy");

    agent
        .run_turn(false)
        .await
        .expect("a cancelled turn returns without an error");

    assert_eq!(
        agent.turn_state().outcome(),
        Some(TurnOutcome::Aborted),
        "the turn records the cancel it ended on"
    );
    assert_eq!(
        agent.soft_interrupt_queue().in_flight_len(),
        0,
        "a cancelled turn restores"
    );
    assert_eq!(agent.soft_interrupt_queue().pending_len(), 1);
    assert_eq!(stored_contents(&session_id), vec!["cancelled"]);
}

#[tokio::test]
async fn restore_after_panic_turn_keeps_disk() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = agent_with(vec![Round::Stream(tool_round("call-a")), Round::Panic]).await;
    let session_id = agent.session_id().to_string();
    agent.queue_soft_interrupt("panic survivor".into(), Vec::new(), false, SoftInterruptSource::User);
    crate::soft_interrupt_store::append(&session_id, message("panic survivor")).expect("seed the disk copy");

    let result = std::panic::AssertUnwindSafe(agent.run_turn(false)).catch_unwind().await;
    assert!(result.is_err(), "the scripted provider panics inside the turn loop");

    assert_eq!(
        agent.soft_interrupt_queue().in_flight_len(),
        0,
        "the guard's drop restores when the turn unwinds without settling"
    );
    assert_eq!(agent.soft_interrupt_queue().pending_len(), 1);
    assert_eq!(stored_contents(&session_id), vec!["panic survivor"]);
}

#[tokio::test]
async fn retry_after_restore_injects_once() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = agent_with(vec![
        Round::Stream(tool_round("call-a")),
        Round::Fail("provider exploded".into()),
    ])
    .await;
    let session_id = agent.session_id().to_string();
    agent.queue_soft_interrupt("one shot".into(), Vec::new(), false, SoftInterruptSource::User);
    crate::soft_interrupt_store::append(&session_id, message("one shot")).expect("seed the disk copy");

    agent.run_turn(false).await.expect_err("the provider fails");
    assert_eq!(agent.soft_interrupt_queue().pending_len(), 1);
    assert_eq!(
        agent.soft_interrupt_queue().in_flight_len(),
        0,
        "a failed turn leaves exactly one copy in the queue, not two"
    );
    assert_eq!(
        transcript_occurrences(&agent, "one shot"),
        0,
        "the failed turn's injected copy is rolled back out of the transcript"
    );

    let retried = agent.inject_soft_interrupts(None).await;
    assert_eq!(retried.len(), 1, "the restored message is injected again");
    assert_eq!(transcript_occurrences(&agent, "one shot"), 1);
    assert_eq!(agent.soft_interrupt_queue().pending_len(), 0);

    let after = agent.inject_soft_interrupts(None).await;
    assert!(after.is_empty(), "the retry injects the message exactly once");
    assert_eq!(transcript_occurrences(&agent, "one shot"), 1);
}

#[tokio::test]
async fn restore_emits_log_line() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    crate::logging::init();
    let log_path = crate::logging::log_path().expect("log path under the sandboxed home");
    let (mut agent, _provider) = agent_with(vec![
        Round::Stream(tool_round("call-a")),
        Round::Fail("provider exploded".into()),
    ])
    .await;
    let session_id = agent.session_id().to_string();
    agent.queue_soft_interrupt("logged".into(), Vec::new(), false, SoftInterruptSource::User);
    crate::soft_interrupt_store::append(&session_id, message("logged")).expect("seed the disk copy");

    agent.run_turn(false).await.expect_err("the provider fails");

    let log = std::fs::read_to_string(&log_path).expect("the logger writes under the sandboxed home");
    let restores: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("AGENT_SOFT_INTERRUPT_RESTORE") && line.contains(&session_id))
        .collect();
    assert_eq!(restores.len(), 1, "a restore leaves one log line, got {restores:?}");
    assert!(restores[0].contains("restored=1"), "got {}", restores[0]);
}

#[cfg(unix)]
#[tokio::test]
async fn commit_failure_keeps_in_flight() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = agent_with(vec![
        Round::Stream(tool_round("call-a")),
        Round::QueueThen("second".into(), text_round()),
    ])
    .await;
    let session_id = agent.session_id().to_string();
    agent.queue_soft_interrupt("first".into(), Vec::new(), false, SoftInterruptSource::User);
    crate::soft_interrupt_store::append(&session_id, message("first")).expect("seed the disk copy");

    let store_dir = sandbox.root().join("pending-soft-interrupts");
    assert!(store_dir.is_dir(), "the store directory exists after seeding");
    let mut permissions = std::fs::metadata(&store_dir).expect("store dir").permissions();
    let original = permissions.clone();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o555);
    std::fs::set_permissions(&store_dir, permissions).expect("make the store read-only");

    agent.run_turn(false).await.expect("the turn completes");

    std::fs::set_permissions(&store_dir, original).expect("restore store permissions");

    assert_eq!(
        agent.soft_interrupt_queue().in_flight_len(),
        1,
        "a failed disk write keeps the injected message staged instead of losing it"
    );
    assert_eq!(
        stored_contents(&session_id),
        vec!["first"],
        "the disk copy survives the failure"
    );

    let retried = agent.inject_soft_interrupts(None).await;
    assert_eq!(
        retried.iter().map(|item| item.content.as_str()).collect::<Vec<_>>(),
        vec!["second"],
        "the retry injects only what is still pending, never the staged message again"
    );
    assert_eq!(transcript_occurrences(&agent, "first"), 1);

    agent.commit_soft_interrupt_turn().await;
    assert_eq!(agent.soft_interrupt_queue().in_flight_len(), 0);
    assert!(
        stored_contents(&session_id).is_empty(),
        "the retried commit clears the disk copy"
    );
}

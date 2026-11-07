//! Per-tool-call cancellation inside a turn: only the named call stops.
//!
//! The daemon-level request path (`cancel_tool_call` → `ToolCallCancelResult`)
//! is covered in `server::client_lifecycle_tests::acp_tool_cancel`; these tests
//! drive the turn itself, so they can hold two members of one concurrent group
//! open and cancel exactly one of them.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc};

use super::*;
use crate::provider::EventStream;
use crate::tool::{ConcurrencyClass, Tool, ToolEffects, ToolOutput};

/// Answers the first request from the script and every later one with text, so
/// one turn runs exactly one tool round and then finishes on its own.
struct RoundProvider(Arc<Mutex<Option<mpsc::Receiver<Result<StreamEvent>>>>>);

#[async_trait::async_trait]
impl Provider for RoundProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        if let Some(rx) = self.0.lock().unwrap().take() {
            Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
        } else {
            Ok(Box::pin(futures::stream::iter([
                Ok(StreamEvent::TextDelta("second request".into())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                }),
            ])))
        }
    }

    fn name(&self) -> &str {
        "tool-cancel-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self(Arc::clone(&self.0)))
    }
}

#[derive(Default)]
struct GateState {
    released: HashMap<String, bool>,
    waiters: HashMap<String, Arc<Notify>>,
}

/// Holds named calls open so the test decides who is still running when it
/// cancels one of them.
struct Gate {
    started_tx: mpsc::UnboundedSender<String>,
    state: Mutex<GateState>,
}

impl Gate {
    fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        let (started_tx, started_rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                started_tx,
                state: Mutex::new(GateState::default()),
            }),
            started_rx,
        )
    }

    async fn park(&self, key: &str) {
        let _ = self.started_tx.send(key.to_string());
        let notify = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            Arc::clone(state.waiters.entry(key.to_string()).or_default())
        };
        let mut notified = std::pin::pin!(notify.notified());
        notified.as_mut().enable();
        let released = {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.released.get(key).copied().unwrap_or(false)
        };
        if !released {
            notified.await;
        }
    }

    fn release(&self, key: &str) {
        let notify = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.released.insert(key.to_string(), true);
            state.waiters.get(key).cloned()
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

struct GatedTool {
    name: &'static str,
    gate: Arc<Gate>,
}

#[async_trait::async_trait]
impl Tool for GatedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "tool-cancel test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        ConcurrencyClass::Concurrent
    }

    /// The scheduler groups by declared effects, so both doubles must report
    /// read so they land in one concurrent group.
    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    async fn execute(&self, input: Value, _: ToolContext) -> Result<ToolOutput> {
        let key = input["key"].as_str().unwrap_or_default().to_string();
        self.gate.park(&key).await;
        Ok(ToolOutput::new(format!("{}:{}", self.name, key)))
    }
}

fn provider_pair() -> (Arc<RoundProvider>, mpsc::Sender<Result<StreamEvent>>) {
    let (tx, rx) = mpsc::channel(64);
    (Arc::new(RoundProvider(Arc::new(Mutex::new(Some(rx))))), tx)
}

fn tool_use_events(id: &str, name: &str, input: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolUseStart {
            id: id.into(),
            name: name.into(),
        },
        StreamEvent::ToolInputDeltaFor {
            id: id.into(),
            delta: input.into(),
        },
        StreamEvent::ToolUseEndFor { id: id.into() },
    ]
}

fn round_end() -> StreamEvent {
    StreamEvent::MessageEnd {
        stop_reason: Some("tool_use".into()),
    }
}

async fn recv_or_panic(rx: &mut mpsc::UnboundedReceiver<String>, what: &str) -> String {
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap_or_else(|| panic!("gate channel closed while waiting for {what}"))
}

/// Wait until `call_id` is registered as cancellable, giving the scheduler a
/// bounded number of handoffs. `yield_now` is a handoff, not a sleep.
async fn await_registration(session_id: &str, call_id: &str) -> bool {
    for _ in 0..4_000 {
        if crate::tool_cancel_registry::running_call_ids(session_id)
            .iter()
            .any(|id| id == call_id)
        {
            return true;
        }
        tokio::task::yield_now().await;
    }
    false
}

fn tool_results(agent: &Agent) -> Vec<(String, String, Option<bool>)> {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((tool_use_id.clone(), content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

fn tool_done(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<(String, Option<String>)> {
    let mut done = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::ToolDone { id, error, .. } = event {
            done.push((id, error));
        }
    }
    done
}

fn cancelled_events(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<String> {
    let mut ids = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::ToolCancelled { id } = event {
            ids.push(id);
        }
    }
    ids
}

#[tokio::test]
async fn cancelling_one_member_of_a_parallel_group_leaves_its_sibling_alone() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (gate, mut started) = Gate::new();
    let registry = Registry::empty();
    registry
        .register(
            "read".to_string(),
            Arc::new(GatedTool {
                name: "read",
                gate: Arc::clone(&gate),
            }) as Arc<dyn Tool>,
        )
        .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read two files".into(),
            cache_control: None,
        }],
    );
    for event in tool_use_events("call-a", "read", "{\"key\":\"a\"}")
        .into_iter()
        .chain(tool_use_events("call-b", "read", "{\"key\":\"b\"}"))
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    // A v2 owner, so `tool_cancelled` — which only a v2 connection may see —
    // has somewhere to go.
    let (acp_tx, mut acp_rx) = mpsc::unbounded_channel();
    let acp_token = crate::tool::acp_session::install_with_version(&session_id, "conn-acp", acp_tx, Some(2));

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    let mut starts = vec![
        recv_or_panic(&mut started, "first member start").await,
        recv_or_panic(&mut started, "second member start").await,
    ];
    starts.sort();
    assert_eq!(starts, vec!["a", "b"], "both members must be in flight");

    assert!(
        await_registration(&session_id, "call-a").await,
        "the first member must be cancellable by its own id"
    );
    assert!(
        await_registration(&session_id, "call-b").await,
        "the second member must be cancellable by its own id"
    );

    assert!(
        crate::tool_cancel_registry::cancel(&session_id, "call-a"),
        "the named member was running, so the cancel is accepted"
    );

    // Only the sibling is released: the cancelled member never finishes on its
    // own, so the turn completing proves its work was aborted.
    gate.release("b");
    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the cancel")
        .unwrap()
        .unwrap();

    let results = tool_results(&agent);
    let by_id: HashMap<&str, (&str, Option<bool>)> = results
        .iter()
        .map(|(id, content, error)| (id.as_str(), (content.as_str(), *error)))
        .collect();
    assert_eq!(
        by_id.get("call-a"),
        Some(&(super::CANCELLED_TOOL_RESULT, Some(true))),
        "the cancelled member records the cancellation text: {results:?}"
    );
    assert_eq!(
        by_id.get("call-b"),
        Some(&("read:b", None)),
        "the sibling must produce its normal result: {results:?}"
    );

    let mut done = tool_done(&mut rx);
    done.sort();
    assert_eq!(
        done,
        vec![
            ("call-a".to_string(), Some(super::CANCELLED_TOOL_RESULT.to_string())),
            ("call-b".to_string(), None),
        ]
    );
    assert_eq!(
        cancelled_events(&mut acp_rx),
        vec!["call-a".to_string()],
        "only the cancelled call reports tool_cancelled, and only to its v2 owner"
    );
    crate::tool::acp_session::clear(&session_id, acp_token);
}

#[tokio::test]
async fn cancelling_a_sequential_call_continues_the_same_turn() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (gate, mut started) = Gate::new();
    let registry = Registry::empty();
    registry
        .register(
            "read".to_string(),
            Arc::new(GatedTool {
                name: "read",
                gate: Arc::clone(&gate),
            }) as Arc<dyn Tool>,
        )
        .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read one file".into(),
            cache_control: None,
        }],
    );
    for event in tool_use_events("call-one", "read", "{\"key\":\"one\"}")
        .into_iter()
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    assert_eq!(recv_or_panic(&mut started, "the only call's start").await, "one");
    assert!(await_registration(&session_id, "call-one").await);
    assert!(crate::tool_cancel_registry::cancel(&session_id, "call-one"));

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the cancel")
        .unwrap()
        .unwrap();

    let results = tool_results(&agent);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].0, "call-one");
    assert_eq!(results[0].1, super::CANCELLED_TOOL_RESULT);
    assert_eq!(results[0].2, Some(true));

    let mut saw_follow_up = false;
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::TextDelta { text } = event {
            saw_follow_up |= text == "second request";
        }
    }
    assert!(saw_follow_up, "the turn asks the model again after a cancelled call");
    assert_eq!(
        crate::tool_cancel_registry::running_call_ids(&session_id),
        Vec::<String>::new(),
        "a finished call leaves no cancel registration behind"
    );
}

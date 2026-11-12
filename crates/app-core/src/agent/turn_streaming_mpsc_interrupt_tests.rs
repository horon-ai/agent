//! What a client observes while a concurrent group is in flight: an urgent
//! interrupt, and a background request.
//!
//! The streaming loop used to check for an urgent interrupt once per GROUP, so
//! a message that asked for several paid network calls ran every one of them
//! before the interrupt was noticed; `dev` checked once per call. These tests
//! pin the per-call reaction on the concurrent path: the group stops as soon as
//! the interrupt is queued, completed members keep their results, every
//! `tool_use` still owns exactly one `tool_result`, and results stay in request
//! order.
//!
//! The same file pins the other half of what a client sees during a group: a
//! background request raised then is refused visibly. A multi-member group
//! registers nothing in `running_tool_registry` and the background signal
//! carries no target identity, so the answer is `moved: false` with no call
//! named, while a lone `Ordered` call such as `bash` still moves and names
//! itself.
//!
//! No test here asserts wall-clock timing. The members are parked on a gate the
//! test releases by name, and the interrupt or the background request is raised
//! while the members are still parked, so the round can only return if the
//! event was observed mid-group.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use horon_agent_agent_runtime::{SoftInterruptMessage, SoftInterruptSource};

use crate::provider::EventStream;
use crate::server::{SessionControlHandle, move_tool_to_background};
use crate::tool::{ConcurrencyClass, Tool, ToolEffects, ToolOutput};

use super::*;

/// Provider whose first request is a scripted stream and whose later requests
/// answer with plain text, so one turn runs exactly one tool round.
#[derive(Clone)]
struct InterruptProvider(Arc<Mutex<Option<mpsc::Receiver<Result<StreamEvent>>>>>);

#[async_trait::async_trait]
impl Provider for InterruptProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        if let Some(rx) = self.0.lock().unwrap().take() {
            Ok(Box::pin(ReceiverStream::new(rx)))
        } else {
            Ok(Box::pin(futures::stream::iter([
                Ok(StreamEvent::TextDelta("done".into())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                }),
            ])))
        }
    }

    fn name(&self) -> &str {
        "streaming-interrupt-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[derive(Default)]
struct GateState {
    released: HashSet<String>,
    waiters: HashMap<String, Arc<Notify>>,
}

/// Holds named members of a round open so the test controls who finishes when.
struct Gate {
    started_tx: mpsc::UnboundedSender<String>,
    finished_tx: mpsc::UnboundedSender<String>,
    state: Mutex<GateState>,
}

impl Gate {
    fn new() -> (
        Arc<Self>,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let (started_tx, started_rx) = mpsc::unbounded_channel();
        let (finished_tx, finished_rx) = mpsc::unbounded_channel();
        let gate = Arc::new(Self {
            started_tx,
            finished_tx,
            state: Mutex::new(GateState::default()),
        });
        (gate, started_rx, finished_rx)
    }

    async fn park(&self, key: &str) {
        let _ = self.started_tx.send(key.to_string());
        let notify = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            Arc::clone(
                state
                    .waiters
                    .entry(key.to_string())
                    .or_insert_with(|| Arc::new(Notify::new())),
            )
        };
        let mut notified = std::pin::pin!(notify.notified());
        notified.as_mut().enable();
        let already_released = {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.released.contains(key)
        };
        if !already_released {
            notified.await;
        }
        let _ = self.finished_tx.send(key.to_string());
    }

    fn release(&self, key: &str) {
        let notify = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.released.insert(key.to_string());
            state.waiters.get(key).cloned()
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

struct RoundTool {
    name: &'static str,
    class: ConcurrencyClass,
    gate: Arc<Gate>,
}

#[async_trait::async_trait]
impl Tool for RoundTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "streaming interrupt test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        self.class
    }

    /// The round scheduler groups by declared effects, so the double reports
    /// the effects its class stands for. The trait default is a write barrier,
    /// which would serialize every call the test means to overlap.
    fn effects(&self) -> ToolEffects {
        match self.class {
            ConcurrencyClass::Concurrent => ToolEffects::read(),
            ConcurrencyClass::Ordered => ToolEffects::write(),
        }
    }

    async fn execute(&self, input: Value, _: ToolContext) -> Result<ToolOutput> {
        let key = input["key"].as_str().unwrap_or_default().to_string();
        self.gate.park(&key).await;
        Ok(ToolOutput::new(format!("{}:{}", self.name, key)))
    }
}

async fn registry_with(tools: Vec<RoundTool>) -> Registry {
    let registry = Registry::empty();
    for tool in tools {
        registry
            .register(tool.name.to_string(), Arc::new(tool) as Arc<dyn Tool>)
            .await;
    }
    registry
}

fn provider_pair() -> (Arc<InterruptProvider>, mpsc::Sender<Result<StreamEvent>>) {
    let (tx, rx) = mpsc::channel(64);
    let provider = Arc::new(InterruptProvider(Arc::new(Mutex::new(Some(rx)))));
    (provider, tx)
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

/// Sets `[features] parallel_tools` for one test, and restores the default
/// before the sandbox drops. The sandbox holds the process-wide env lock, so no
/// other test can read the config while this value is in force.
struct ParallelToolsFlag;

impl ParallelToolsFlag {
    fn set(root: &std::path::Path, enabled: bool) -> Self {
        write_parallel_tools(root, enabled);
        Self
    }
}

impl Drop for ParallelToolsFlag {
    fn drop(&mut self) {
        if let Ok(dir) = std::env::var("HORON_HOME") {
            write_parallel_tools(std::path::Path::new(&dir), true);
        }
    }
}

fn write_parallel_tools(root: &std::path::Path, enabled: bool) {
    std::fs::write(
        root.join("config.toml"),
        format!("[features]\nparallel_tools = {enabled}\n"),
    )
    .expect("the config is written");
    crate::config::invalidate_config_cache();
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
                ..
            } => Some((tool_use_id.clone(), content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

fn tool_use_ids(agent: &Agent) -> Vec<String> {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

fn transcript_texts(agent: &Agent) -> Vec<String> {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn drain_events(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn tool_done_ids(events: &[ServerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::ToolDone { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

fn base_context() -> ToolContext {
    ToolContext {
        session_id: "streaming-interrupt-tests".to_string(),
        message_id: "message".to_string(),
        tool_call_id: "round".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn call(id: &str, name: &str, key: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        input: json!({ "key": key }),
        intent: None,
        thought_signature: None,
    }
}

/// The daemon's own interruption path: the user's message is queued into the
/// session's shared queue, exactly as `SessionControlHandle` does it.
fn queue_urgent(queue: &horon_agent_agent_runtime::SoftInterruptQueue, content: &str) {
    queue.lock().expect("soft interrupt queue").push(SoftInterruptMessage {
        content: content.to_string(),
        images: Vec::new(),
        urgent: true,
        source: SoftInterruptSource::User,
        client_id: None,
        input_id: None,
    });
}

#[tokio::test]
async fn an_urgent_interrupt_stops_a_multi_member_group_mid_flight() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _flag = ParallelToolsFlag::set(sandbox.root(), true);
    assert!(
        crate::config::config().features.parallel_tools,
        "the test needs the concurrent path"
    );

    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![RoundTool {
        name: "read",
        class: ConcurrencyClass::Concurrent,
        gate: Arc::clone(&gate),
    }])
    .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let queue = agent.soft_interrupt_queue();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read three files".into(),
            cache_control: None,
        }],
    );

    for event in tool_use_events("call-a", "read", "{\"key\":\"a\"}")
        .into_iter()
        .chain(tool_use_events("call-b", "read", "{\"key\":\"b\"}"))
        .chain(tool_use_events("call-c", "read", "{\"key\":\"c\"}"))
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    // All three members are in flight together: that overlap IS the group.
    let mut starts = vec![
        recv_or_panic(&mut started, "first read start").await,
        recv_or_panic(&mut started, "second read start").await,
        recv_or_panic(&mut started, "third read start").await,
    ];
    starts.sort();
    assert_eq!(starts, vec!["a", "b", "c"], "all members must be in flight together");

    // One member completes before the interrupt.
    gate.release("a");
    assert_eq!(recv_or_panic(&mut finished, "a finish").await, "a");

    // The user's message lands while b and c are still parked. Nothing releases
    // them, so only an interrupt observed mid-group can end this round.
    queue_urgent(&queue, "stop, do this instead");

    let agent = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the interrupt must end the round while its members are in flight")
        .unwrap()
        .unwrap();

    assert!(
        finished.try_recv().is_err(),
        "the parked members must be cancelled, not released"
    );

    let results = tool_results(&agent);
    let contents: Vec<(&str, &str)> = results
        .iter()
        .map(|(id, content, _)| (id.as_str(), content.as_str()))
        .collect();
    assert_eq!(
        contents,
        vec![
            ("call-a", "read:a"),
            ("call-b", USER_INTERRUPT_SKIP),
            ("call-c", USER_INTERRUPT_SKIP),
        ],
        "one result per call, in request order, keeping completed work: {results:?}"
    );

    let mut use_ids = tool_use_ids(&agent);
    let mut result_ids: Vec<String> = results.iter().map(|(id, _, _)| id.clone()).collect();
    use_ids.sort();
    result_ids.sort();
    assert_eq!(
        use_ids,
        vec!["call-a".to_string(), "call-b".to_string(), "call-c".to_string()]
    );
    assert_eq!(result_ids, use_ids, "no tool_use may be left without its tool_result");

    let events = drain_events(&mut rx);
    assert_eq!(
        tool_done_ids(&events),
        vec!["call-a", "call-b", "call-c"],
        "the cancelled members still owe one terminal event each, in request order"
    );

    let injected: Vec<(&str, Option<usize>)> = events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::SoftInterruptInjected {
                content,
                point,
                tools_skipped,
                ..
            } if point == "C" => Some((content.as_str(), *tools_skipped)),
            _ => None,
        })
        .collect();
    assert_eq!(
        injected,
        vec![("stop, do this instead", Some(2))],
        "the interrupt is injected at point C with the two skipped calls"
    );
    let texts = transcript_texts(&agent);
    assert!(
        texts.iter().any(|text| text.contains("stop, do this instead")),
        "the user's message must reach the transcript: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("[User interrupted: 2 remaining tool(s) skipped]")),
        "the model must be told how many calls it lost: {texts:?}"
    );
}

#[tokio::test]
async fn an_interrupt_queued_before_dispatch_still_runs_the_first_call_only() {
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![RoundTool {
        name: "read",
        class: ConcurrencyClass::Concurrent,
        gate: Arc::clone(&gate),
    }])
    .await;
    let calls = vec![
        call("call-a", "read", "a"),
        call("call-b", "read", "b"),
        call("call-c", "read", "c"),
    ];
    let cancel = super::tool_round::GroupCancel::new(InterruptSignal::new()).watching_interrupt(Arc::new(|| true));
    let ctx = base_context();

    let group = super::tool_round::run_group(registry, &calls, &ctx, Some(cancel));
    let driver = async {
        assert_eq!(recv_or_panic(&mut started, "first call start").await, "a");
        assert!(
            started.try_recv().is_err(),
            "an interrupt already queued must not start the paid calls after the first"
        );
        gate.release("a");
        assert_eq!(recv_or_panic(&mut finished, "a finish").await, "a");
    };

    let (outcome, ()) = tokio::join!(group, driver);

    assert_eq!(
        outcome.stopped,
        Some(super::tool_round::GroupStop::Interrupt),
        "the group must report why it stopped"
    );
    assert_eq!(outcome.results.len(), 1, "{:?}", outcome.results);
    assert_eq!(outcome.results[0].0, 0);
    assert_eq!(outcome.unstarted, vec![1, 2]);
}

/// Raise the background request exactly as a client's `BackgroundTool`
/// request does: through the daemon's own handler, with a control handle
/// carrying the agent's signals. Returns what the client would be told:
/// `(moved, tool_call_id, tool_name)` from the `BackgroundToolResult` event.
fn request_background(control: &SessionControlHandle, id: u64) -> Option<(bool, Option<String>, Option<String>)> {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    move_tool_to_background(id, control, &event_tx);
    drop(event_tx);
    let mut answer = None;
    while let Ok(event) = event_rx.try_recv() {
        if let ServerEvent::BackgroundToolResult {
            id: event_id,
            moved,
            tool_call_id,
            tool_name,
        } = event
        {
            assert_eq!(event_id, id, "the answer must carry the request's id");
            eprintln!(
                "BackgroundToolResult id={event_id} moved={moved} tool_call_id={tool_call_id:?} tool_name={tool_name:?}"
            );
            answer = Some((moved, tool_call_id, tool_name));
        }
    }
    answer
}

/// Wait for the one-member path to register the call it is awaiting. The
/// member is parked on the gate, so the entry can only appear; the deadline
/// is a failure guard, not the synchronization.
async fn wait_for_registered_call(session_id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while crate::running_tool_registry::current(session_id).is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the awaited call never registered itself"
        );
        tokio::task::yield_now().await;
    }
}

/// IS-13, visibility half: a background request raised while a multi-member
/// group is in flight must reach the client as an explicit refusal.
///
/// The registry side is pinned elsewhere; what a client actually renders comes
/// from the `BackgroundToolResult` event. The group registers no call, so the
/// answer is `moved: false` with nothing named, and the request must not
/// disturb the group it landed in.
#[tokio::test]
async fn a_background_request_during_a_group_is_refused_visibly() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _flag = ParallelToolsFlag::set(sandbox.root(), true);
    assert!(
        crate::config::config().features.parallel_tools,
        "the test needs the concurrent path"
    );

    let (gate, mut started, _finished) = Gate::new();
    let registry = registry_with(vec![RoundTool {
        name: "read",
        class: ConcurrencyClass::Concurrent,
        gate: Arc::clone(&gate),
    }])
    .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session_id().to_string();
    let control = SessionControlHandle::new(
        session_id.clone(),
        agent.soft_interrupt_queue(),
        agent.background_tool_signal(),
        InterruptSignal::new(),
    );
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

    let (out, _rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    // Both members are in flight together: that overlap IS the group, and the
    // gate keeps it open until the test releases them.
    let mut starts = vec![
        recv_or_panic(&mut started, "first read start").await,
        recv_or_panic(&mut started, "second read start").await,
    ];
    starts.sort();
    assert_eq!(starts, vec!["a", "b"], "both members must be in flight together");
    assert_eq!(
        crate::running_tool_registry::current(&session_id),
        None,
        "a multi-member group must register nothing to move"
    );

    let answer = request_background(&control, 9001);
    assert_eq!(
        answer,
        Some((false, None, None)),
        "the client must see the refusal, and no call may be named"
    );

    // A refused request must not disturb the group it landed in: both members
    // still land, in request order.
    gate.release("a");
    gate.release("b");
    let agent = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the group must finish after the refused request")
        .unwrap()
        .unwrap();
    let results = tool_results(&agent);
    let contents: Vec<(&str, &str)> = results
        .iter()
        .map(|(id, content, _)| (id.as_str(), content.as_str()))
        .collect();
    assert_eq!(contents, vec![("call-a", "read:a"), ("call-b", "read:b")]);
    assert!(
        !agent.background_tool_signal().is_set(),
        "the fire during the group must be cleared, not carried into the next call"
    );
}

/// IS-13, other half: a lone `Ordered` call is still detachable. The same
/// request that the group refuses must move a single awaited `bash` and name
/// it, and the stored result must be the background notice rather than the
/// tool's output.
#[tokio::test]
async fn a_lone_ordered_bash_is_still_detachable() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _flag = ParallelToolsFlag::set(sandbox.root(), true);

    let (gate, mut started, _finished) = Gate::new();
    let registry = registry_with(vec![RoundTool {
        name: "bash",
        class: ConcurrencyClass::Ordered,
        gate: Arc::clone(&gate),
    }])
    .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session_id().to_string();
    let control = SessionControlHandle::new(
        session_id.clone(),
        agent.soft_interrupt_queue(),
        agent.background_tool_signal(),
        InterruptSignal::new(),
    );
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "run the shell".into(),
            cache_control: None,
        }],
    );

    for event in tool_use_events("call-c", "bash", "{\"key\":\"c\"}")
        .into_iter()
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    let (out, _rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    assert_eq!(recv_or_panic(&mut started, "bash start").await, "c");
    wait_for_registered_call(&session_id).await;
    let running = crate::running_tool_registry::current(&session_id).expect("the lone call registers itself");
    assert_eq!((running.call_id.as_str(), running.name.as_str()), ("call-c", "bash"));

    let answer = request_background(&control, 9002);
    assert_eq!(
        answer,
        Some((true, Some("call-c".to_string()), Some("bash".to_string()))),
        "a lone ordered call must still be moved and named"
    );

    gate.release("c");
    let agent = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the moved call must not keep the turn waiting")
        .unwrap()
        .unwrap();
    let results = tool_results(&agent);
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(
        results[0].1.contains("was moved to background by the user (task_id:"),
        "the stored result must be the background notice: {}",
        results[0].1
    );
}

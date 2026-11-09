use std::collections::{HashMap, HashSet};

use std::sync::{Arc, Mutex};

use anyhow::Result;

use serde_json::{Value, json};

use tokio::sync::{Notify, mpsc};

use tokio_stream::wrappers::ReceiverStream;

use crate::provider::EventStream;

use crate::tool::{ConcurrencyClass, Tool, ToolEffects, ToolOutput};

use super::*;

/// Provider whose first request is a scripted stream and whose later requests
/// answer with plain text, so one turn runs exactly one tool round.
#[derive(Clone)]
struct RoundProvider(Arc<Mutex<Option<mpsc::Receiver<Result<StreamEvent>>>>>);

#[async_trait::async_trait]
impl Provider for RoundProvider {
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
        "streaming-round-test"
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

enum RoundBehavior {
    Gate(Arc<Gate>),
}

struct RoundTool {
    name: &'static str,
    class: ConcurrencyClass,
    behavior: RoundBehavior,
}

#[async_trait::async_trait]
impl Tool for RoundTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "streaming scheduler test double"
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
        let RoundBehavior::Gate(gate) = &self.behavior;
        gate.park(&key).await;
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

fn provider_pair() -> (Arc<RoundProvider>, mpsc::Sender<Result<StreamEvent>>) {
    let (tx, rx) = mpsc::channel(64);
    let provider = Arc::new(RoundProvider(Arc::new(Mutex::new(Some(rx)))));
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

/// The call `running_tool_registry` names for this session, giving the turn a
/// bounded number of scheduler handoffs to register it first.
///
/// `yield_now` is a handoff, not a sleep: the turn task registers a one-member
/// group immediately before awaiting it, so a caller that arrives first only
/// needs the scheduler to run. A group member is never registered, so its
/// caller exhausts the bound and answers `None`.
async fn registered_call_id(session_id: &str) -> Option<String> {
    for _ in 0..2_000 {
        if let Some(tool) = crate::running_tool_registry::current(session_id) {
            return Some(tool.call_id);
        }
        tokio::task::yield_now().await;
    }
    None
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

fn tool_done_ids(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<String> {
    let mut ids = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::ToolDone { id, .. } = event {
            ids.push(id);
        }
    }
    ids
}

#[tokio::test]
async fn two_concurrent_reads_emit_two_ordered_tool_done_events() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![RoundTool {
        name: "read",
        class: ConcurrencyClass::Concurrent,
        behavior: RoundBehavior::Gate(Arc::clone(&gate)),
    }])
    .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
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

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    // Both members start before either is released: that overlap IS the group.
    let mut starts = vec![
        recv_or_panic(&mut started, "first read start").await,
        recv_or_panic(&mut started, "second read start").await,
    ];
    starts.sort();
    assert_eq!(starts, vec!["a", "b"], "both members must be in flight together");

    gate.release("b");
    assert_eq!(recv_or_panic(&mut finished, "b finish").await, "b");
    gate.release("a");
    assert_eq!(recv_or_panic(&mut finished, "a finish").await, "a");

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the group")
        .unwrap()
        .unwrap();

    assert_eq!(
        tool_done_ids(&mut rx),
        vec!["call-a", "call-b"],
        "terminal events must follow request order, not completion order"
    );

    let results = tool_results(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["call-a", "call-b"], "got {results:?}");
    let contents: Vec<&str> = results.iter().map(|(_, content, _)| content.as_str()).collect();
    assert_eq!(contents, vec!["read:a", "read:b"]);
    assert_eq!(tool_use_ids(&agent).len(), results.len());
}

#[tokio::test]
async fn shutdown_mid_group_keeps_one_tool_result_per_tool_use() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![RoundTool {
        name: "read",
        class: ConcurrencyClass::Concurrent,
        behavior: RoundBehavior::Gate(Arc::clone(&gate)),
    }])
    .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let shutdown = agent.graceful_shutdown_signal();
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

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    // The interrupt lands after one member has already completed. The other
    // member needs no start confirmation: the group dispatches both whatever
    // the scheduler does, and the invariant must hold for either outcome.
    assert_eq!(recv_or_panic(&mut started, "first read start").await, "a");
    gate.release("a");
    assert_eq!(recv_or_panic(&mut finished, "a finish").await, "a");
    shutdown.fire();

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn returns once the reload cancels the group")
        .unwrap()
        .unwrap();

    let results = tool_results(&agent);
    let by_id: HashMap<&str, &str> = results
        .iter()
        .map(|(id, content, _)| (id.as_str(), content.as_str()))
        .collect();
    assert_eq!(
        by_id.get("call-a"),
        Some(&"read:a"),
        "completed work is kept: {results:?}"
    );
    assert_eq!(
        by_id.get("call-b"),
        Some(&"[Skipped - server reloading]"),
        "{results:?}"
    );
    let mut result_ids: Vec<String> = results.iter().map(|(id, _, _)| id.clone()).collect();
    let mut use_ids = tool_use_ids(&agent);
    result_ids.sort();
    use_ids.sort();
    assert_eq!(use_ids, vec!["call-a".to_string(), "call-b".to_string()], "{use_ids:?}");
    assert_eq!(
        result_ids, use_ids,
        "the reload must leave no tool_use without its tool_result"
    );
    assert_eq!(
        tool_done_ids(&mut rx),
        vec!["call-a", "call-b"],
        "the skipped member still owes a terminal event"
    );
}

#[tokio::test]
async fn ordered_bash_call_still_registers_and_detaches_to_background() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![RoundTool {
        name: "bash",
        class: ConcurrencyClass::Ordered,
        behavior: RoundBehavior::Gate(Arc::clone(&gate)),
    }])
    .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    let background = agent.background_tool_signal();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "run the shell".into(),
            cache_control: None,
        }],
    );

    for event in tool_use_events("call-bash", "bash", "{\"key\":\"c\"}")
        .into_iter()
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    assert_eq!(recv_or_panic(&mut started, "bash start").await, "c");
    assert_eq!(
        registered_call_id(&session_id).await.as_deref(),
        Some("call-bash"),
        "the only registration site in the tree must survive for an Ordered call"
    );

    background.fire();

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the detach")
        .unwrap()
        .unwrap();

    let results = tool_results(&agent);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].0, "call-bash");
    assert!(
        results[0].1.contains("moved to background"),
        "an Ordered call must still be detachable, got {:?}",
        results[0].1
    );
    let done = tool_done_ids(&mut rx);
    assert_eq!(done, vec!["call-bash"], "got {done:?}");

    // The adopted call keeps running in the background manager, so let the test
    // double finish instead of leaving it parked for the whole runtime.
    gate.release("c");
    assert_eq!(recv_or_panic(&mut finished, "bash finish").await, "c");
}

#[tokio::test]
async fn background_request_during_a_group_reports_untargeted_and_leaves_the_next_call_alone() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![
        RoundTool {
            name: "read",
            class: ConcurrencyClass::Concurrent,
            behavior: RoundBehavior::Gate(Arc::clone(&gate)),
        },
        RoundTool {
            name: "bash",
            class: ConcurrencyClass::Ordered,
            behavior: RoundBehavior::Gate(Arc::clone(&gate)),
        },
    ])
    .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    let background = agent.background_tool_signal();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read two files, then run the shell".into(),
            cache_control: None,
        }],
    );

    for event in tool_use_events("call-a", "read", "{\"key\":\"a\"}")
        .into_iter()
        .chain(tool_use_events("call-b", "read", "{\"key\":\"b\"}"))
        .chain(tool_use_events("call-c", "bash", "{\"key\":\"c\"}"))
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    let (out, _rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    let mut starts = vec![
        recv_or_panic(&mut started, "first read start").await,
        recv_or_panic(&mut started, "second read start").await,
    ];
    starts.sort();
    assert_eq!(starts, vec!["a", "b"]);

    // No registration while a group runs, so the server's `moved` answer is
    // false and a client can state the limitation instead of guessing a target.
    assert_eq!(
        registered_call_id(&session_id).await,
        None,
        "a concurrent group must register no running-tool entry"
    );

    // The request lands mid-group and cannot be targeted, so it must be dropped
    // here rather than carried into the next call.
    background.fire();

    gate.release("a");
    assert_eq!(recv_or_panic(&mut finished, "a finish").await, "a");
    gate.release("b");
    assert_eq!(recv_or_panic(&mut finished, "b finish").await, "b");

    // Positive control for the same mechanism: the following Ordered call does
    // register, which is what makes the None above meaningful.
    assert_eq!(recv_or_panic(&mut started, "bash start").await, "c");
    assert_eq!(
        registered_call_id(&session_id).await.as_deref(),
        Some("call-c"),
        "the Ordered call after the group must register"
    );
    gate.release("c");
    assert_eq!(recv_or_panic(&mut finished, "bash finish").await, "c");

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes")
        .unwrap()
        .unwrap();

    let results = tool_results(&agent);
    let by_id: HashMap<&str, &str> = results
        .iter()
        .map(|(id, content, _)| (id.as_str(), content.as_str()))
        .collect();
    assert_eq!(by_id.get("call-a"), Some(&"read:a"), "{results:?}");
    assert_eq!(by_id.get("call-b"), Some(&"read:b"), "{results:?}");
    assert_eq!(
        by_id.get("call-c"),
        Some(&"bash:c"),
        "the untargeted background request must not move the next call: {results:?}"
    );
    assert!(
        !agent.background_tool_signal().is_set(),
        "the signal must be cleared so it cannot move a later call"
    );
}

#[tokio::test]
async fn disabled_parallel_tools_keeps_calls_in_separate_groups_and_preserves_order() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _flag = ParallelToolsFlag::set(sandbox.root(), false);
    assert!(
        !crate::config::config().features.parallel_tools,
        "the test needs the disabled path"
    );

    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![RoundTool {
        name: "read",
        class: ConcurrencyClass::Concurrent,
        behavior: RoundBehavior::Gate(Arc::clone(&gate)),
    }])
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

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    assert_eq!(recv_or_panic(&mut started, "first read start").await, "a");
    // With the flag on these two reads form one group and register nothing, so
    // this registration is the observed group size rather than a timing claim.
    assert_eq!(
        registered_call_id(&session_id).await.as_deref(),
        Some("call-a"),
        "with parallel_tools off every call is its own registered group"
    );
    assert!(
        started.try_recv().is_err(),
        "the second read must not start while the first is still running"
    );

    gate.release("a");
    assert_eq!(recv_or_panic(&mut finished, "a finish").await, "a");
    assert_eq!(
        recv_or_panic(&mut started, "second read start").await,
        "b",
        "the second read starts only after the first finished"
    );
    gate.release("b");
    assert_eq!(recv_or_panic(&mut finished, "b finish").await, "b");

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes")
        .unwrap()
        .unwrap();

    assert_eq!(tool_done_ids(&mut rx), vec!["call-a", "call-b"]);
    let results = tool_results(&agent);
    let contents: Vec<&str> = results.iter().map(|(_, content, _)| content.as_str()).collect();
    assert_eq!(contents, vec!["read:a", "read:b"]);
}

use std::sync::atomic::{AtomicUsize, Ordering};

/// Small enough that the guard's overhead floor stays out of the arithmetic:
/// `estimate_compaction_tokens_from_chars` adds it only from half the default
/// budget up, so a message's character count is the whole seed.
const ROUND_BUDGET: usize = 20_000;

const SEED_TARGET: usize = 15_000;

/// The round seeds AFTER the assistant message holding this round's `tool_use`
/// blocks is recorded, so a seed measured before the turn is about thirty
/// tokens lighter than the round's own. Every boundary below leaves this much
/// slack, which absorbs that gap and any accounting drift around it.
const MARGIN_TOKENS: usize = 500;

/// Provider reporting a small context window, so the round's ceiling is
/// reachable from a short transcript without hundreds of small turns.
#[derive(Clone)]
struct WindowRoundProvider {
    inner: RoundProvider,
    window: usize,
}

#[async_trait::async_trait]
impl Provider for WindowRoundProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        tool_choice: Option<&str>,
    ) -> Result<EventStream> {
        self.inner.complete(messages, tools, system, tool_choice).await
    }

    fn name(&self) -> &str {
        "streaming-round-window-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn context_window(&self) -> usize {
        self.window
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// Test double whose output size the test sets after measuring the seed, so
/// where the ceiling falls is arithmetic rather than a guess.
struct SizedRoundTool {
    name: &'static str,
    class: ConcurrencyClass,
    chars: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for SizedRoundTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "streaming round budget test double with a settable output size"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        self.class
    }

    /// See `RoundTool::effects`: the scheduler groups by declared effects.
    fn effects(&self) -> ToolEffects {
        match self.class {
            ConcurrencyClass::Concurrent => ToolEffects::read(),
            ConcurrencyClass::Ordered => ToolEffects::write(),
        }
    }

    async fn execute(&self, _: Value, _: ToolContext) -> Result<ToolOutput> {
        let chars = self.chars.load(Ordering::SeqCst);
        Ok(ToolOutput::new("x".repeat(chars)))
    }
}

fn sized_tool(name: &'static str, class: ConcurrencyClass) -> (SizedRoundTool, Arc<AtomicUsize>) {
    let chars = Arc::new(AtomicUsize::new(0));
    let tool = SizedRoundTool {
        name,
        class,
        chars: Arc::clone(&chars),
    };
    (tool, chars)
}

async fn sized_registry(tools: Vec<SizedRoundTool>) -> Registry {
    let registry = Registry::empty();
    for tool in tools {
        let name = tool.name.to_string();
        registry.register(name, Arc::new(tool) as Arc<dyn Tool>).await;
    }
    registry
}

fn round_threshold() -> usize {
    Registry::context_guard_threshold_tokens(ROUND_BUDGET)
}

/// Characters that price at about `tokens` tokens. The estimator covers 4.87
/// ASCII characters per token rather than four.
const fn chars_in(tokens: usize) -> usize {
    (tokens * 487).div_ceil(100)
}

/// The budget and the seed the round itself takes: this agent's compaction
/// manager over the session's provider transcript.
async fn round_seed_and_budget(agent: &mut Agent) -> (usize, usize) {
    let messages = agent.session.messages_for_provider();
    let manager = agent.registry.compaction();
    let guard = manager.read().await;
    (guard.token_budget(), guard.effective_token_count_with(&messages))
}

/// Agent on [`ROUND_BUDGET`] whose transcript carries about `seed_target`
/// tokens before the round.
async fn seeded_agent(registry: Registry, seed_target: usize) -> (Agent, mpsc::Sender<Result<StreamEvent>>) {
    let (tx, rx) = mpsc::channel(64);
    let provider = Arc::new(WindowRoundProvider {
        inner: RoundProvider(Arc::new(Mutex::new(Some(rx)))),
        window: ROUND_BUDGET,
    });
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "go".into(),
            cache_control: None,
        }],
    );
    let (_, base) = round_seed_and_budget(&mut agent).await;
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "f".repeat(chars_in(seed_target.saturating_sub(base))),
            cache_control: None,
        }],
    );
    (agent, tx)
}

/// Per-output dump of what the round decided, printed for the QA artifact and
/// reconstructed from the transcript the round produced: the seed the round was
/// handed, then each applied result added the way the round adds it.
fn dump_decisions(seed: usize, results: &[(String, String, Option<bool>)]) {
    let threshold = round_threshold();
    std::println!("QA streaming round-budget budget={ROUND_BUDGET} threshold={threshold} seed={seed}");
    let mut running = seed;
    for (index, (id, content, _)) in results.iter().enumerate() {
        let refused = content.contains("CONTEXT LIMIT REACHED");
        running += crate::util::estimate_tokens(content);
        std::println!(
            "QA streaming round-budget output[{index}] id={id} refused={refused} running_total={running} \
             threshold={threshold} budget={ROUND_BUDGET}"
        );
    }
}

async fn run_round(mut agent: Agent, tx: mpsc::Sender<Result<StreamEvent>>, calls: &[(&str, &str, &str)]) -> Agent {
    for (id, name, key) in calls {
        for event in tool_use_events(id, name, &format!("{{\"key\":\"{key}\"}}")) {
            tx.send(Ok(event)).await.unwrap();
        }
    }
    tx.send(Ok(round_end())).await.unwrap();
    drop(tx);

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });
    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the streamed turn completes")
        .unwrap()
        .unwrap();
    std::println!("QA streaming tool-done ids={:?}", tool_done_ids(&mut rx));
    agent
}

#[tokio::test]
async fn round_budget_refuses_the_second_group_member_that_crosses_the_ceiling() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (read, read_chars) = sized_tool("read", ConcurrencyClass::Concurrent);
    let (ls, ls_chars) = sized_tool("ls", ConcurrencyClass::Concurrent);
    let registry = sized_registry(vec![read, ls]).await;

    let (mut agent, tx) = seeded_agent(registry, SEED_TARGET).await;
    let (budget, seed) = round_seed_and_budget(&mut agent).await;
    let threshold = round_threshold();
    assert_eq!(budget, ROUND_BUDGET, "the window is the one the provider reports");
    assert!(
        seed < threshold - MARGIN_TOKENS - 1_000,
        "the seed must leave room to spend, got {seed} against a {threshold} ceiling"
    );

    // Both members run through the group runner. Each alone clears the tool
    // layer's per-output guard; their sum is what cannot fit in one round.
    read_chars.store(chars_in(threshold - seed - MARGIN_TOKENS), Ordering::SeqCst);
    ls_chars.store(chars_in(1_000), Ordering::SeqCst);

    let agent = run_round(agent, tx, &[("call-a", "read", "a"), ("call-b", "ls", "b")]).await;

    let results = tool_results(&agent);
    dump_decisions(seed, &results);
    assert_eq!(results.len(), 2, "got {results:?}");
    assert_eq!(results[0].0, "call-a", "request order survives a refusal");
    assert!(
        !results[0].1.contains("CONTEXT LIMIT REACHED") && !results[0].1.contains("OUTPUT WITHHELD"),
        "the first member fits and must be admitted, got {:?}",
        results[0].1.chars().take(90).collect::<String>()
    );
    assert!(
        results[1].1.contains("CONTEXT LIMIT REACHED"),
        "the second member crosses the round's ceiling and must be refused, got {:?}",
        results[1].1.chars().take(120).collect::<String>()
    );
    assert!(
        results[1].1.contains("20k"),
        "the refusal names the same budget the tool layer prints, got {:?}",
        results[1].1
    );
    assert!(
        results[1].1.contains("/compact"),
        "the refusal carries the tool layer's remedy, got {:?}",
        results[1].1
    );
}

#[tokio::test]
async fn round_budget_refuses_every_member_when_the_seed_is_already_spent() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (read, read_chars) = sized_tool("read", ConcurrencyClass::Concurrent);
    let (ls, _) = sized_tool("ls", ConcurrencyClass::Concurrent);
    let registry = sized_registry(vec![read, ls]).await;

    let (mut agent, tx) = seeded_agent(registry, ROUND_BUDGET).await;
    let (_, seed) = round_seed_and_budget(&mut agent).await;
    let threshold = round_threshold();
    assert!(
        seed >= threshold,
        "the seed must already sit past the ceiling, got {seed} against {threshold}"
    );
    read_chars.store(40, Ordering::SeqCst);

    let agent = run_round(agent, tx, &[("call-a", "read", "a"), ("call-b", "ls", "b")]).await;

    let results = tool_results(&agent);
    dump_decisions(seed, &results);
    assert_eq!(results.len(), 2, "got {results:?}");
    for (id, content, _) in &results {
        assert!(
            content.contains("CONTEXT LIMIT REACHED"),
            "a spent seed refuses every output of the round, {id} got {:?}",
            content.chars().take(90).collect::<String>()
        );
    }
}

#[tokio::test]
async fn round_budget_carries_across_the_groups_of_one_round() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (read, read_chars) = sized_tool("read", ConcurrencyClass::Concurrent);
    let (ls, ls_chars) = sized_tool("ls", ConcurrencyClass::Concurrent);
    let (bash, bash_chars) = sized_tool("bash", ConcurrencyClass::Ordered);
    let registry = sized_registry(vec![read, ls, bash]).await;

    let (mut agent, tx) = seeded_agent(registry, SEED_TARGET).await;
    let (_, seed) = round_seed_and_budget(&mut agent).await;
    let threshold = round_threshold();
    assert!(
        seed < threshold - MARGIN_TOKENS - 1_000,
        "the seed must leave room, got {seed}"
    );

    // Group one is the concurrent pair (the group runner); group two is the
    // ordered shell call (the inline one-member path). One budget covers both,
    // so the dataless second member and the whole third call are refused.
    read_chars.store(chars_in(threshold - seed - MARGIN_TOKENS), Ordering::SeqCst);
    ls_chars.store(chars_in(1_000), Ordering::SeqCst);
    bash_chars.store(chars_in(1_000), Ordering::SeqCst);

    let agent = run_round(
        agent,
        tx,
        &[("call-a", "read", "a"), ("call-b", "ls", "b"), ("call-c", "bash", "c")],
    )
    .await;

    let results = tool_results(&agent);
    dump_decisions(seed, &results);
    assert_eq!(results.len(), 3, "got {results:?}");
    let refused: Vec<bool> = results
        .iter()
        .map(|(_, content, _)| content.contains("CONTEXT LIMIT REACHED"))
        .collect();
    assert_eq!(
        refused,
        vec![false, true, true],
        "the first member fits, the second crosses, and the next group inherits the total: {results:?}"
    );
}

#[tokio::test]
async fn a_one_member_streamed_round_under_the_ceiling_is_untouched() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (bash, bash_chars) = sized_tool("bash", ConcurrencyClass::Ordered);
    let registry = sized_registry(vec![bash]).await;

    let (mut agent, tx) = seeded_agent(registry, SEED_TARGET).await;
    let (_, seed) = round_seed_and_budget(&mut agent).await;
    let threshold = round_threshold();
    assert!(seed < threshold, "the seed leaves room, got {seed} against {threshold}");
    bash_chars.store(400, Ordering::SeqCst);

    let agent = run_round(agent, tx, &[("call-bash", "bash", "c")]).await;

    let results = tool_results(&agent);
    dump_decisions(seed, &results);
    assert_eq!(results.len(), 1, "got {results:?}");
    assert_eq!(
        results[0].1,
        "x".repeat(400),
        "a one-member round under the ceiling keeps its output byte for byte"
    );
    assert_eq!(results[0].2, None);
}

#[derive(Clone)]
struct ScriptedProvider {
    rounds: Arc<Mutex<std::collections::VecDeque<Vec<StreamEvent>>>>,
    dynamic_prompts: Arc<Mutex<Vec<String>>>,
}

impl ScriptedProvider {
    fn next_events(&self) -> Vec<Result<StreamEvent>> {
        match self.rounds.lock().unwrap().pop_front() {
            Some(events) => events.into_iter().map(Ok).collect(),
            None => vec![
                Ok(StreamEvent::TextDelta("done".into())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                }),
            ],
        }
    }
}

#[async_trait::async_trait]
impl Provider for ScriptedProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::iter(self.next_events())))
    }

    async fn complete_split(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        system_dynamic: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.dynamic_prompts.lock().unwrap().push(system_dynamic.to_string());
        Ok(Box::pin(futures::stream::iter(self.next_events())))
    }

    fn name(&self) -> &str {
        "streaming-scripted-loop-guard-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn scripted_provider(rounds: Vec<Vec<StreamEvent>>) -> (Arc<ScriptedProvider>, Arc<Mutex<Vec<String>>>) {
    let dynamic_prompts = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ScriptedProvider {
        rounds: Arc::new(Mutex::new(rounds.into())),
        dynamic_prompts: Arc::clone(&dynamic_prompts),
    });
    (provider, dynamic_prompts)
}

fn identical_round(prefix: &str, tool: &str, count: usize, input: &Value) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    for index in 0..count {
        events.extend(tool_use_events(&format!("{prefix}-{index}"), tool, &input.to_string()));
    }
    events.push(round_end());
    events
}

struct CountingReadTool {
    name: &'static str,
    class: ConcurrencyClass,
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for CountingReadTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "loop-guard test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        self.class
    }

    /// See `RoundTool::effects`: the scheduler groups by declared effects.
    fn effects(&self) -> ToolEffects {
        match self.class {
            ConcurrencyClass::Concurrent => ToolEffects::read(),
            ConcurrencyClass::Ordered => ToolEffects::write(),
        }
    }

    async fn execute(&self, input: Value, _: ToolContext) -> Result<ToolOutput> {
        let count = self.executions.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Ok(ToolOutput::new(format!(
            "{}#{count}:{}",
            self.name,
            input["file_path"].as_str().unwrap_or_default()
        )))
    }
}

async fn counting_registry(
    name: &'static str,
    class: ConcurrencyClass,
) -> (Registry, Arc<std::sync::atomic::AtomicUsize>) {
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = Registry::empty();
    registry
        .register(
            name.to_string(),
            Arc::new(CountingReadTool {
                name,
                class,
                executions: Arc::clone(&executions),
            }) as Arc<dyn Tool>,
        )
        .await;
    (registry, executions)
}

/// Sets `[features] loop_guard` for one test and restores the default before the
/// sandbox drops, because the config cache is process-global.
struct LoopGuardFlag;

impl LoopGuardFlag {
    fn set(root: &std::path::Path, enabled: bool) -> Self {
        write_loop_guard(root, enabled);
        Self
    }
}

impl Drop for LoopGuardFlag {
    fn drop(&mut self) {
        if let Ok(dir) = std::env::var("HORON_HOME") {
            write_loop_guard(std::path::Path::new(&dir), true);
        }
    }
}

fn write_loop_guard(root: &std::path::Path, enabled: bool) {
    std::fs::write(
        root.join("config.toml"),
        format!("[features]\nloop_guard = {enabled}\nparallel_tools = true\n"),
    )
    .expect("the config is written");
    crate::config::invalidate_config_cache();
}

fn stored_loop_guard_notice(agent: &Agent) -> Option<String> {
    agent.session.active_context().iter().find_map(|message| {
        if message.display_role != Some(crate::session::StoredDisplayRole::System) {
            return None;
        }
        message.content.iter().find_map(|block| match block {
            ContentBlock::Text { text, .. } if text.contains("Loop guard ended the turn") => Some(text.clone()),
            _ => None,
        })
    })
}

fn tool_done_by_id(events: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<(String, bool)> {
    let mut done = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let ServerEvent::ToolDone { id, error, .. } = event {
            done.push((id, error.is_some()));
        }
    }
    done
}

#[tokio::test]
async fn five_identical_reads_across_two_rounds_warn_refuse_then_stop() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry("read", ConcurrencyClass::Concurrent).await;
    let input = json!({ "file_path": "src/lib.rs" });
    let (provider, dynamic_prompts) = scripted_provider(vec![
        identical_round("first", "read", 3, &input),
        identical_round("second", "read", 2, &input),
    ]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the same file five times".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    let results = tool_results(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["first-0", "first-1", "first-2", "second-0", "second-1"],
        "{results:?}"
    );
    for (index, (_, content, is_error)) in results.iter().take(3).enumerate() {
        assert_eq!(*is_error, None, "call {index} must execute, got {content:?}");
        assert!(content.starts_with("read#"), "call {index} must have run: {content:?}");
    }
    assert_eq!(results[3].2, Some(true), "{results:?}");
    assert!(
        results[3].1.contains("Loop guard refused"),
        "the fourth identical call must be refused, got {:?}",
        results[3].1
    );
    assert_eq!(results[4].2, Some(true), "{results:?}");
    assert!(
        results[4].1.contains("Loop guard ended the turn"),
        "the second refusal must stop the turn, got {:?}",
        results[4].1
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);

    assert!(
        stored_loop_guard_notice(&agent).is_some(),
        "the stop must leave a notice"
    );

    let mut stopped = Vec::new();
    let mut done = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ServerEvent::TurnStopped { message, .. } => stopped.push(message),
            ServerEvent::ToolDone { id, error, .. } => done.push((id, error.is_some())),
            _ => {}
        }
    }
    assert_eq!(
        done,
        vec![
            ("first-0".to_string(), false),
            ("first-1".to_string(), false),
            ("first-2".to_string(), false),
            ("second-0".to_string(), true),
            ("second-1".to_string(), true),
        ],
        "every call ends exactly once at its own index"
    );
    assert_eq!(stopped.len(), 1, "one stop event: {stopped:?}");
    assert!(stopped[0].contains("ended the turn"), "{stopped:?}");

    let prompts = dynamic_prompts.lock().unwrap();
    assert_eq!(prompts.len(), 2, "the turn stops before a third request: {prompts:?}");
    assert!(prompts[1].contains("Loop guard"), "{:?}", prompts[1]);
}

#[tokio::test]
async fn ordered_calls_are_judged_one_dispatch_at_a_time() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry("bash", ConcurrencyClass::Ordered).await;
    let input = json!({ "file_path": "src/lib.rs" });
    let (provider, _prompts) = scripted_provider(vec![
        identical_round("first", "bash", 3, &input),
        identical_round("second", "bash", 2, &input),
    ]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "run the same command five times".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    let results = tool_results(&agent);
    assert_eq!(results.len(), 5, "{results:?}");
    assert!(
        results[..3].iter().all(|(_, _, is_error)| is_error.is_none()),
        "the first three ordered calls run: {results:?}"
    );
    assert!(results[3].1.contains("Loop guard refused"), "{:?}", results[3].1);
    assert!(results[4].1.contains("Loop guard ended the turn"), "{:?}", results[4].1);
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(stored_loop_guard_notice(&agent).is_some());

    let done = tool_done_by_id(&mut rx);
    assert_eq!(
        done,
        vec![
            ("first-0".to_string(), false),
            ("first-1".to_string(), false),
            ("first-2".to_string(), false),
            ("second-0".to_string(), true),
            ("second-1".to_string(), true),
        ],
        "ordered calls each end exactly once"
    );
}

#[tokio::test]
async fn loop_guard_off_executes_all_five_identical_reads() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _flag = LoopGuardFlag::set(sandbox.root(), false);
    assert!(!crate::config::config().features.loop_guard);

    let (registry, executions) = counting_registry("read", ConcurrencyClass::Concurrent).await;
    let input = json!({ "file_path": "src/lib.rs" });
    let (provider, _prompts) = scripted_provider(vec![
        identical_round("first", "read", 3, &input),
        identical_round("second", "read", 2, &input),
    ]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the same file five times".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    let results = tool_results(&agent);
    assert_eq!(results.len(), 5, "{results:?}");
    assert!(
        results.iter().all(|(_, _, is_error)| is_error.is_none()),
        "with the guard off every call executes: {results:?}"
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert!(stored_loop_guard_notice(&agent).is_none());

    let done = tool_done_by_id(&mut rx);
    assert_eq!(done.len(), 5, "{done:?}");
    assert!(done.iter().all(|(_, is_error)| !*is_error), "{done:?}");
}

#[tokio::test]
async fn refused_call_produces_one_result_and_one_terminal_event_at_its_index() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry("read", ConcurrencyClass::Concurrent).await;
    let input = json!({ "file_path": "src/lib.rs" });
    let (provider, _prompts) = scripted_provider(vec![
        identical_round("first", "read", 3, &input),
        identical_round("second", "read", 1, &input),
    ]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the same file four times".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    let results = tool_results(&agent);
    assert_eq!(results.len(), 4, "{results:?}");
    assert_eq!(results[3].0, "second-0");
    assert_eq!(results[3].2, Some(true));
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);

    let done = tool_done_by_id(&mut rx);
    assert_eq!(
        done,
        vec![
            ("first-0".to_string(), false),
            ("first-1".to_string(), false),
            ("first-2".to_string(), false),
            ("second-0".to_string(), true),
        ],
        "each call ends exactly once at its own index"
    );
}

#[tokio::test]
async fn paginated_reads_and_alternating_files_execute_in_full() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry("read", ConcurrencyClass::Concurrent).await;
    let page = |offset: u64| json!({ "file_path": "big.rs", "offset": offset, "limit": 100 });
    let mut first = Vec::new();
    for index in 0..3u64 {
        first.extend(tool_use_events(
            &format!("page-a-{index}"),
            "read",
            &page(index * 100).to_string(),
        ));
    }
    first.push(round_end());
    let mut second = Vec::new();
    for index in 3..5u64 {
        second.extend(tool_use_events(
            &format!("page-b-{index}"),
            "read",
            &page(index * 100).to_string(),
        ));
    }
    second.push(round_end());
    let mut third = Vec::new();
    for index in 0..6 {
        let path = if index % 2 == 0 { "left.rs" } else { "right.rs" };
        third.extend(tool_use_events(
            &format!("compare-{index}"),
            "read",
            &json!({ "file_path": path }).to_string(),
        ));
    }
    third.push(round_end());

    let (provider, _prompts) = scripted_provider(vec![first, second, third]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "page through one file, then compare two".into(),
            cache_control: None,
        }],
    );

    let (out, _rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    let results = tool_results(&agent);
    assert_eq!(results.len(), 11, "{results:?}");
    assert!(
        results.iter().all(|(_, _, is_error)| is_error.is_none()),
        "pagination and A/B comparison are ordinary work: {results:?}"
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 11);
    assert!(stored_loop_guard_notice(&agent).is_none());
}

// ---------------------------------------------------------------------------
// End-to-end coverage through the real `run_turn_streaming_mpsc` entry point
// (plan todo 14). The provider, the scripted message and the tool doubles come
// from `agent_tests.rs`, shared with the non-streaming loop's module, so both
// loops are driven by the same script instead of two near-copies. Every
// assertion here is structural: request order in the transcript, one `ToolExec`
// and one `ToolDone` per call, and observed group size. Timing appears once, as
// a ratio between two members that each wait 400 ms.
// ---------------------------------------------------------------------------

use crate::agent::tests::{
    ParallelToolsFlag, SharedGatedRound, SharedTimedRound, SharedToolRun, shared_dump_content_pair,
    shared_dump_transcript, shared_echo_registry, shared_gated_round, shared_mixed_round, shared_recv_or_panic,
    shared_round_end, shared_round_provider, shared_round_results, shared_timed_round, shared_tool_use,
};

/// Every `ToolExec` and `ToolDone` the turn emitted, in emission order.
fn tool_event_pairs(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> (Vec<String>, Vec<(String, bool)>) {
    let mut execs = Vec::new();
    let mut dones = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ServerEvent::ToolExec { id, .. } => execs.push(id),
            ServerEvent::ToolDone { id, error, .. } => dones.push((id, error.is_some())),
            _ => {}
        }
    }
    (execs, dones)
}

fn qa_event_order(execs: &[String], dones: &[(String, bool)]) {
    for (index, id) in execs.iter().enumerate() {
        std::println!("QA stream exec[{index}] id={id}");
    }
    for (index, (id, is_error)) in dones.iter().enumerate() {
        std::println!("QA stream done[{index}] id={id} error={is_error}");
    }
}

#[tokio::test]
async fn end_to_end_group_keeps_request_order_and_emits_one_event_pair_per_call() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    crate::config::invalidate_config_cache();
    assert!(
        crate::config::config().features.parallel_tools,
        "the default path must group the two reads"
    );

    let SharedGatedRound {
        provider,
        registry,
        gate,
        mut started,
        mut finished,
        executions,
    } = shared_gated_round(vec![shared_mixed_round()]).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read two files, then run the shell".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    let mut starts = vec![
        shared_recv_or_panic(&mut started, "first read start").await,
        shared_recv_or_panic(&mut started, "second read start").await,
    ];
    starts.sort();
    assert_eq!(starts, vec!["a".to_string(), "b".to_string()]);

    // Completion order is deliberately the reverse of request order.
    gate.release("b");
    assert_eq!(shared_recv_or_panic(&mut finished, "b finish").await, "b");
    gate.release("a");
    assert_eq!(shared_recv_or_panic(&mut finished, "a finish").await, "a");

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the gated group")
        .unwrap()
        .unwrap();

    let (execs, dones) = tool_event_pairs(&mut rx);
    qa_event_order(&execs, &dones);
    assert_eq!(
        execs,
        vec!["call-a", "call-b", "call-c"],
        "one start event per call, in request order"
    );
    assert_eq!(
        dones,
        vec![
            ("call-a".to_string(), false),
            ("call-b".to_string(), false),
            ("call-c".to_string(), false),
        ],
        "one terminal event per call, in request order, not completion order"
    );

    let results = tool_results(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["call-a", "call-b", "call-c"],
        "results must follow request order even when b finished first, got {results:?}"
    );
    let contents: Vec<&str> = results.iter().map(|(_, content, _)| content.as_str()).collect();
    assert_eq!(contents, vec!["read:a", "read:b", "bash:c"]);
    assert_eq!(tool_use_ids(&agent).len(), results.len(), "one result per use");
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);
    shared_dump_transcript("stream", &agent);
}

#[tokio::test]
async fn end_to_end_two_400ms_members_finish_well_under_700ms() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    crate::config::invalidate_config_cache();
    assert!(crate::config::config().features.parallel_tools);

    let SharedTimedRound {
        provider,
        registry,
        runs,
        executions,
    } = shared_timed_round(vec![shared_mixed_round()], Duration::from_millis(400)).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read two files, then run the shell".into(),
            cache_control: None,
        }],
    );

    let (out, _rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    let runs = runs.lock().unwrap().clone();
    let concurrent: Vec<&SharedToolRun> = runs.iter().filter(|run| run.key == "a" || run.key == "b").collect();
    assert_eq!(concurrent.len(), 2, "both concurrent members must have run");
    for run in &concurrent {
        let duration = run.finished.duration_since(run.started);
        std::println!("QA stream overlap key={} duration_ms={}", run.key, duration.as_millis());
        assert!(
            duration >= Duration::from_millis(395),
            "each member must really wait its 400 ms, got {duration:?} for {}",
            run.key
        );
    }
    let span = concurrent
        .iter()
        .map(|run| run.finished)
        .max()
        .unwrap()
        .duration_since(concurrent.iter().map(|run| run.started).min().unwrap());
    std::println!("QA stream overlap span_ms={}", span.as_millis());
    // A ratio, not an absolute timing: one sleep for the group, at least 800 ms
    // for the same two members one after another.
    assert!(
        span < Duration::from_millis(700),
        "two 400 ms members must overlap; sequential execution would need at least 800 ms, got {span:?}"
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);

    let results = tool_results(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["call-a", "call-b", "call-c"], "got {results:?}");
}

#[tokio::test]
async fn end_to_end_interrupt_mid_group_leaves_one_result_per_call() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    crate::config::invalidate_config_cache();

    let SharedGatedRound {
        provider,
        registry,
        gate,
        mut started,
        mut finished,
        ..
    } = shared_gated_round(vec![shared_mixed_round()]).await;
    let mut agent = Agent::new(provider, registry);
    let shutdown = agent.graceful_shutdown_signal();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read two files, then run the shell".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    // The interrupt lands after one member has completed and while the other is
    // still in flight.
    assert_eq!(shared_recv_or_panic(&mut started, "first read start").await, "a");
    gate.release("a");
    assert_eq!(shared_recv_or_panic(&mut finished, "a finish").await, "a");
    shutdown.fire();

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn returns once the reload cancels the group")
        .unwrap()
        .unwrap();

    let results = tool_results(&agent);
    let uses = tool_use_ids(&agent);
    std::println!(
        "QA stream interrupt tool_use={} tool_result={}",
        uses.len(),
        results.len()
    );
    assert_eq!(uses.len(), 3, "the round asked for three calls");
    assert_eq!(
        results.len(),
        uses.len(),
        "the interrupt must not drop a result: {results:?}"
    );
    let by_id: HashMap<&str, &str> = results
        .iter()
        .map(|(id, content, _)| (id.as_str(), content.as_str()))
        .collect();
    assert_eq!(
        by_id.get("call-a"),
        Some(&"read:a"),
        "completed work is kept: {results:?}"
    );
    assert_eq!(
        by_id.get("call-b"),
        Some(&"[Skipped - server reloading]"),
        "{results:?}"
    );
    assert_eq!(
        by_id.get("call-c"),
        Some(&"[Skipped - server reloading]"),
        "a call after the cancelled group still owes its result: {results:?}"
    );

    // The two members the cancelled group dispatched each still end exactly
    // once. A call AFTER the cancelled group is written to the transcript only
    // (the turn is already returning), so it owes a result but no event.
    let (_, dones) = tool_event_pairs(&mut rx);
    qa_event_order(&[], &dones);
    assert_eq!(
        dones,
        vec![("call-a".to_string(), false), ("call-b".to_string(), true)],
        "every dispatched member still ends exactly once: {dones:?}"
    );
    shared_dump_transcript("stream-interrupt", &agent);
}

#[tokio::test]
async fn end_to_end_disabled_parallel_tools_observes_group_size_one() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _flag = ParallelToolsFlag::set(sandbox.root(), false);
    assert!(
        !crate::config::config().features.parallel_tools,
        "the test needs the disabled path"
    );

    let SharedGatedRound {
        provider,
        registry,
        gate,
        mut started,
        mut finished,
        executions,
    } = shared_gated_round(vec![shared_mixed_round()]).await;
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read two files, then run the shell".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    assert_eq!(shared_recv_or_panic(&mut started, "first read start").await, "a");
    // Observed group size 1, by two structural signals: the one-member group is
    // registered (a multi-member group never is), and the second member has not
    // started while the first is still in flight. No timing is involved.
    assert_eq!(
        registered_call_id(&session_id).await.as_deref(),
        Some("call-a"),
        "with parallel_tools off every call is its own registered group"
    );
    assert!(
        started.try_recv().is_err(),
        "with parallel_tools off the second call must wait for the first"
    );
    gate.release("a");
    assert_eq!(shared_recv_or_panic(&mut finished, "a finish").await, "a");
    assert_eq!(shared_recv_or_panic(&mut started, "second read start").await, "b");
    gate.release("b");
    assert_eq!(shared_recv_or_panic(&mut finished, "b finish").await, "b");

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes")
        .unwrap()
        .unwrap();

    let (execs, dones) = tool_event_pairs(&mut rx);
    qa_event_order(&execs, &dones);
    assert_eq!(execs, vec!["call-a", "call-b", "call-c"]);
    assert_eq!(
        dones,
        vec![
            ("call-a".to_string(), false),
            ("call-b".to_string(), false),
            ("call-c".to_string(), false),
        ]
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);
    let results = tool_results(&agent);
    let contents: Vec<&str> = results.iter().map(|(_, content, _)| content.as_str()).collect();
    assert_eq!(contents, vec!["read:a", "read:b", "bash:c"]);
    std::println!("QA stream observed group sizes=[1, 1, 1] (one registration per call)");
    shared_dump_transcript("stream-disabled", &agent);
}

#[tokio::test]
async fn end_to_end_loop_guard_stops_the_turn() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    crate::config::invalidate_config_cache();
    assert!(crate::config::config().features.loop_guard);

    let (registry, executions) = shared_echo_registry("read", ConcurrencyClass::Concurrent).await;
    let input = json!({ "file_path": "src/lib.rs" });
    let mut first = Vec::new();
    for index in 0..3 {
        first.extend(shared_tool_use(&format!("first-{index}"), "read", &input));
    }
    first.push(shared_round_end());
    let mut second = Vec::new();
    for index in 0..2 {
        second.extend(shared_tool_use(&format!("second-{index}"), "read", &input));
    }
    second.push(shared_round_end());

    let (provider, prompts) = shared_round_provider(vec![first, second]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the same file five times".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    let results = tool_results(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["first-0", "first-1", "first-2", "second-0", "second-1"],
        "{results:?}"
    );
    for (index, (_, content, is_error)) in results.iter().take(3).enumerate() {
        assert_eq!(*is_error, None, "call {index} must execute, got {content:?}");
        assert!(content.starts_with("read:"), "call {index} must have run: {content:?}");
    }
    assert_eq!(results[3].2, Some(true), "{results:?}");
    assert!(
        results[3].1.contains("Loop guard refused"),
        "the fourth identical call must be refused, got {:?}",
        results[3].1
    );
    assert_eq!(results[4].2, Some(true), "{results:?}");
    assert!(
        results[4].1.contains("Loop guard ended the turn"),
        "the second refusal must stop the turn, got {:?}",
        results[4].1
    );
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "only the three allowed calls may reach the tool"
    );
    assert!(
        stored_loop_guard_notice(&agent).is_some(),
        "the stop must leave a stored notice"
    );
    {
        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2, "the turn stops before a third request: {prompts:?}");
        assert!(
            prompts[1].contains("Loop guard"),
            "the warning must reach the next request, got: {:?}",
            prompts[1]
        );
    }

    let (execs, dones) = tool_event_pairs(&mut rx);
    qa_event_order(&execs, &dones);
    assert_eq!(
        execs,
        vec!["first-0", "first-1", "first-2", "second-0", "second-1"],
        "one start event per call, in request order"
    );
    assert_eq!(
        dones,
        vec![
            ("first-0".to_string(), false),
            ("first-1".to_string(), false),
            ("first-2".to_string(), false),
            ("second-0".to_string(), true),
            ("second-1".to_string(), true),
        ],
        "every call ends exactly once at its own index"
    );
}

/// IS-2's content half on the streaming entry point: the same scripted round
/// driven twice through `run_turn_streaming_mpsc` must store the same
/// `(tool_use_id, content)` pairs whether the two concurrent reads overlap or
/// run one at a time.
///
/// Order is pinned by other tests in this file; this one compares CONTENT, so a
/// payload that was truncated, reordered, or paired with the wrong id under
/// concurrency fails here even while every ordering assertion still holds.
#[tokio::test]
async fn parallel_and_sequential_streaming_rounds_store_the_same_content() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();

    let parallel = {
        let _flag = ParallelToolsFlag::set(sandbox.root(), true);
        assert!(
            crate::config::config().features.parallel_tools,
            "the first run needs the parallel path"
        );
        shared_round_results(true, vec![shared_mixed_round()], |mut agent| async move {
            let (out, mut rx) = mpsc::unbounded_channel();
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            agent
                .run_turn_streaming_mpsc(out)
                .await
                .expect("the streaming turn runs");
            drain.await.expect("the event drain ends with the turn");
            agent
        })
        .await
    };

    let sequential = {
        let _flag = ParallelToolsFlag::set(sandbox.root(), false);
        assert!(
            !crate::config::config().features.parallel_tools,
            "the second run needs the sequential path"
        );
        shared_round_results(false, vec![shared_mixed_round()], |mut agent| async move {
            let (out, mut rx) = mpsc::unbounded_channel();
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            agent
                .run_turn_streaming_mpsc(out)
                .await
                .expect("the streaming turn runs");
            drain.await.expect("the event drain ends with the turn");
            agent
        })
        .await
    };

    shared_dump_content_pair(&parallel, &sequential);

    let expected_ids = ["call-a", "call-b", "call-c"];
    let parallel_ids: Vec<&str> = parallel.iter().map(|(id, _, _)| id.as_str()).collect();
    let sequential_ids: Vec<&str> = sequential.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        parallel_ids, expected_ids,
        "the parallel round must store one result per call: {parallel:?}"
    );
    assert_eq!(
        sequential_ids, expected_ids,
        "the sequential round must store one result per call: {sequential:?}"
    );
    assert_eq!(
        parallel, sequential,
        "a parallel round must store the same content as a sequential one"
    );
}

// ---------------------------------------------------------------------------
// Plan todo 35: SDK-answered results take the round's budget on this loop too.
// The streaming loop applies them in two places - the ordered branch and the
// concurrent group - and both must reach the same decision as a tool result.
// ---------------------------------------------------------------------------

/// One streamed round whose call is answered by the provider instead of a tool.
/// `sdk_payloads` are the results the provider streams back, one per call, so a
/// multi-member case can give its members different sizes.
async fn run_sdk_round(mut agent: Agent, tx: mpsc::Sender<Result<StreamEvent>>, sdk_payloads: &[usize]) -> Agent {
    for (index, chars) in sdk_payloads.iter().enumerate() {
        let id = format!("sdk-{index}");
        for event in tool_use_events(&id, "remote_job", "{}") {
            tx.send(Ok(event)).await.unwrap();
        }
        tx.send(Ok(StreamEvent::ToolResult {
            tool_use_id: id,
            content: "x".repeat(*chars),
            is_error: false,
        }))
        .await
        .unwrap();
    }
    tx.send(Ok(round_end())).await.unwrap();
    drop(tx);

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });
    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the streamed turn completes")
        .unwrap()
        .unwrap();
    std::println!("QA streaming sdk tool-done ids={:?}", tool_done_ids(&mut rx));
    agent
}

/// The ordered branch: one SDK-answered call in its own round.
#[tokio::test]
async fn an_ordered_sdk_result_is_admitted_by_the_round_budget() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (job, job_chars) = sized_tool("remote_job", ConcurrencyClass::Ordered);
    job_chars.store(0, Ordering::SeqCst);
    let registry = sized_registry(vec![job]).await;

    let (mut agent, tx) = seeded_agent(registry, SEED_TARGET).await;
    let (_, seed) = round_seed_and_budget(&mut agent).await;
    std::println!(
        "QA streaming sdk ordered budget={ROUND_BUDGET} seed={seed} threshold={} payload_chars={}",
        round_threshold(),
        chars_in(10_000)
    );

    let agent = run_sdk_round(agent, tx, &[chars_in(10_000)]).await;

    let results = tool_results(&agent);
    assert_eq!(results.len(), 1, "got {results:?}");
    let content = &results[0].1;
    std::println!(
        "QA streaming sdk ordered result chars={} head={:?}",
        content.chars().count(),
        content.chars().take(110).collect::<String>()
    );
    assert!(
        content.contains("CONTEXT LIMIT REACHED"),
        "an SDK result that does not fit is withheld like a tool result, got {} chars",
        content.chars().count()
    );
    assert!(
        content.chars().count() < chars_in(10_000),
        "the oversized SDK result must not reach history whole"
    );
}

/// The concurrent group branch: two SDK-answered members of one group, applied
/// in request order, each taking the round's running total.
#[tokio::test]
async fn a_grouped_sdk_result_is_admitted_in_request_order() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (job, job_chars) = sized_tool("remote_job", ConcurrencyClass::Concurrent);
    job_chars.store(0, Ordering::SeqCst);
    let registry = sized_registry(vec![job]).await;

    let (mut agent, tx) = seeded_agent(registry, SEED_TARGET).await;
    let (_, seed) = round_seed_and_budget(&mut agent).await;
    std::println!(
        "QA streaming sdk group budget={ROUND_BUDGET} seed={seed} threshold={} first_chars={} second_chars={}",
        round_threshold(),
        chars_in(1_000),
        chars_in(10_000)
    );

    // The first fits the ~3k tokens that remain; the second does not, and the
    // round must not let it through because it arrived in the same group.
    let agent = run_sdk_round(agent, tx, &[chars_in(1_000), chars_in(10_000)]).await;

    let results = tool_results(&agent);
    assert_eq!(results.len(), 2, "got {results:?}");
    assert_eq!(results[0].0, "sdk-0", "request order survives a refusal");
    assert_eq!(results[1].0, "sdk-1");
    std::println!(
        "QA streaming sdk group first_fits={} second_refused={}",
        !results[0].1.contains("CONTEXT LIMIT REACHED"),
        results[1].1.contains("CONTEXT LIMIT REACHED")
    );
    assert!(
        !results[0].1.contains("CONTEXT LIMIT REACHED"),
        "the first member fits and must be admitted, got {:?}",
        results[0].1.chars().take(90).collect::<String>()
    );
    assert!(
        results[1].1.contains("CONTEXT LIMIT REACHED"),
        "the second member crosses the round's ceiling and must be refused, got {:?}",
        results[1].1.chars().take(120).collect::<String>()
    );
}

// ---------------------------------------------------------------------------
// Plan todo 33 on the streaming loop: the daemon's entry point bounds a turn
// that only polls.
// ---------------------------------------------------------------------------

/// The cap is consulted once per dispatch decision, so it applies to the
/// streaming loop exactly as to the blocking one. An unattended daemon runs
/// this loop, which is why the bound is asserted here and not only in the unit
/// tests: a turn that only polls must stop instead of paying for another
/// request.
#[tokio::test]
async fn a_streamed_turn_that_only_polls_is_bounded_by_the_cap() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry("bg", ConcurrencyClass::Concurrent).await;
    let input = json!({ "action": "status", "task_id": "task-7" });
    let rounds: Vec<Vec<StreamEvent>> = (0..30)
        .map(|index| {
            let mut events = tool_use_events(&format!("poll-{index}"), "bg", &input.to_string());
            events.push(round_end());
            events
        })
        .collect();
    let (provider, _prompts) = scripted_provider(rounds);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "wait until the job finishes".into(),
            cache_control: None,
        }],
    );

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    let results = tool_results(&agent);
    assert_eq!(
        results.len(),
        25,
        "24 polls run and the 25th is the cap, so no later round is requested: {results:?}"
    );
    assert!(
        results[..24].iter().all(|(_, _, is_error)| is_error.is_none()),
        "todo 29 holds on this loop too: every poll up to the cap runs"
    );
    assert_eq!(results[24].2, Some(true), "{results:?}");
    assert!(
        results[24].1.contains("cap is 24"),
        "the capped poll carries the notice naming the cap, got {:?}",
        results[24].1
    );
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        24,
        "the poll that hits the cap never reaches the tool"
    );

    let notice = stored_loop_guard_notice(&agent).expect("the cap must leave a stored notice");
    std::println!("QA streaming polling cap, stored notice:\n{notice}");

    let mut stopped = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::TurnStopped { message, .. } = event {
            stopped.push(message);
        }
    }
    assert_eq!(stopped.len(), 1, "the client is told the turn stopped once");
    assert!(
        stopped[0].contains("cap is 24"),
        "the stop event carries the same notice, got {:?}",
        stopped[0]
    );
}

/// A concurrent tool that reports one file diff.
///
/// Every real write tool is a barrier, so the scheduler never puts two of them in
/// one group. Registering this double twice is the only way to exercise the
/// group path's diff with more than one member.
struct DiffTool {
    name: &'static str,
}

#[async_trait::async_trait]
impl Tool for DiffTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "file diff test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        ConcurrencyClass::Concurrent
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    async fn execute(&self, input: Value, _: ToolContext) -> Result<ToolOutput> {
        let key = input["key"].as_str().unwrap_or_default();
        Ok(ToolOutput::new(format!("edited {key}")).with_metadata(json!({
            "file_diffs": [{
                "path": format!("/work/{key}.txt"),
                "old_text": "old",
                "new_text": format!("new {key}"),
            }]
        })))
    }
}

/// Every `ToolDiff` a turn queued on the ACP connection, in order.
fn acp_diffs(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<(String, Vec<crate::protocol::FileDiffWire>)> {
    let mut diffs = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ServerEvent::ToolDiff { id, diffs: files } => diffs.push((id, files)),
            other => panic!("the ACP connection received an unexpected event: {other:?}"),
        }
    }
    diffs
}

#[tokio::test]
async fn edit_emits_tool_diff_before_tool_done() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "first line\n").unwrap();

    let (provider, tx) = provider_pair();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    let (acp_tx, mut acp_rx) = mpsc::unbounded_channel();
    let acp_token = crate::tool::acp_session::install(&session_id, "conn-acp", acp_tx);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "edit the note".into(),
            cache_control: None,
        }],
    );
    let input = json!({
        "file_path": file.display().to_string(),
        "old_string": "first line",
        "new_string": "second line",
    });
    for event in tool_use_events("call-edit", "edit", &input.to_string())
        .into_iter()
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });

    // The diff has to be on the editor's connection by the time the terminal
    // event lands, and it has to describe the file the edit produced.
    let mut done_output = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("the turn must reach a tool result")
    {
        if let ServerEvent::ToolDone { id, output, .. } = event {
            assert_eq!(id, "call-edit");
            let diffs = acp_diffs(&mut acp_rx);
            assert_eq!(diffs.len(), 1, "one diff for one edited file");
            assert_eq!(diffs[0].0, "call-edit");
            assert_eq!(diffs[0].1.len(), 1);
            assert_eq!(diffs[0].1[0].path, file.display().to_string());
            assert_eq!(diffs[0].1[0].old_text.as_deref(), Some("first line\n"));
            assert_eq!(diffs[0].1[0].new_text, "second line\n");
            done_output = Some(output);
            break;
        }
    }
    assert!(done_output.is_some(), "the turn reported no tool result");

    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the group")
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "second line\n");
    crate::tool::acp_session::clear(&session_id, acp_token);
}

#[tokio::test]
async fn group_edit_emits_tool_diff() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let registry = Registry::empty();
    for tool in [DiffTool { name: "read" }, DiffTool { name: "diff-write" }] {
        registry
            .register(tool.name.to_string(), Arc::new(tool) as Arc<dyn Tool>)
            .await;
    }
    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    let (acp_tx, mut acp_rx) = mpsc::unbounded_channel();
    let acp_token = crate::tool::acp_session::install(&session_id, "conn-acp", acp_tx);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "write two files".into(),
            cache_control: None,
        }],
    );
    for event in tool_use_events("call-a", "read", "{\"key\":\"a\"}")
        .into_iter()
        .chain(tool_use_events("call-b", "diff-write", "{\"key\":\"b\"}"))
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });
    let mut done = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("the group must report both members")
    {
        if let ServerEvent::ToolDone { id, .. } = event {
            done.push(id);
            if done.len() == 2 {
                break;
            }
        }
    }
    assert_eq!(done, vec!["call-a", "call-b"], "request order, not completion order");

    let diffs = acp_diffs(&mut acp_rx);
    let ids: Vec<&str> = diffs.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["call-a", "call-b"]);
    assert_eq!(diffs[0].1[0].path, "/work/a.txt");
    assert_eq!(diffs[1].1[0].path, "/work/b.txt");
    assert_eq!(diffs[1].1[0].new_text, "new b");

    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the group")
        .unwrap()
        .unwrap();
    crate::tool::acp_session::clear(&session_id, acp_token);
}

#[tokio::test]
async fn large_edit_omits_diff() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("big.txt");
    let mut original: String = (0..9_000)
        .map(|line| format!("{line:05} filler filler filler filler filler\n"))
        .collect();
    // 256 KiB is the budget above which a call reports its unified text diff
    // only (`file_diff::MAX_FILE_DIFF_BYTES`).
    const DIFF_BUDGET: usize = 262_144;
    assert!(
        original.len() > DIFF_BUDGET,
        "the fixture has to exceed the diff budget"
    );
    original.push_str("marker\n");
    std::fs::write(&file, &original).unwrap();

    let (provider, tx) = provider_pair();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    let (acp_tx, mut acp_rx) = mpsc::unbounded_channel();
    let acp_token = crate::tool::acp_session::install(&session_id, "conn-acp", acp_tx);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "edit the big file".into(),
            cache_control: None,
        }],
    );
    let input = json!({
        "file_path": file.display().to_string(),
        "old_string": "marker\n",
        "new_string": "replacement\n",
    });
    for event in tool_use_events("call-big", "edit", &input.to_string())
        .into_iter()
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    let (out, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(out).await.map(|_| agent) });
    let mut saw_done = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("the turn must reach a tool result")
    {
        if matches!(event, ServerEvent::ToolDone { .. }) {
            saw_done = true;
            break;
        }
    }
    assert!(saw_done, "the edit still completes");
    assert!(
        acp_diffs(&mut acp_rx).is_empty(),
        "a rewrite over the budget reports no inline diff"
    );

    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the group")
        .unwrap()
        .unwrap();
    assert!(std::fs::read_to_string(&file).unwrap().contains("replacement\n"));
    crate::tool::acp_session::clear(&session_id, acp_token);
}

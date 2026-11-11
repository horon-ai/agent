use std::collections::{HashMap, HashSet};

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use tokio::sync::{Notify, mpsc};

use tokio_stream::wrappers::ReceiverStream;

use crate::provider::EventStream;

use crate::tool::{ConcurrencyClass, Tool, ToolEffects, ToolOutput};

use crate::agent::tests::{ParallelToolsFlag, shared_dump_content_pair, shared_round_results};

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
        "round-scheduler-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

enum RoundBehavior {
    Echo,
    Gate(Arc<RoundGate>),
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
        "turn_loops scheduler test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        self.class
    }

    /// The scheduler groups by declared effects, so this double has to report
    /// the effects its class stands for; the default (a write barrier) would
    /// serialize every call it is meant to run beside another.
    fn effects(&self) -> ToolEffects {
        match self.class {
            ConcurrencyClass::Concurrent => ToolEffects::read(),
            ConcurrencyClass::Ordered => ToolEffects::write(),
        }
    }

    async fn execute(&self, input: Value, _: ToolContext) -> Result<ToolOutput> {
        let key = input["key"].as_str().unwrap_or_default();
        if let RoundBehavior::Gate(gate) = &self.behavior {
            gate.park(key).await;
        }
        Ok(ToolOutput::new(format!("{}:{}", self.name, key)))
    }
}

#[derive(Default)]
struct GateState {
    released: HashSet<String>,
    waiters: HashMap<String, Arc<Notify>>,
}

/// Lets the test hold one member of a concurrent group open so completion
/// order and request order can be made to differ on purpose.
struct RoundGate {
    started_tx: mpsc::UnboundedSender<String>,
    finished_tx: mpsc::UnboundedSender<String>,
    state: Mutex<GateState>,
}

impl RoundGate {
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

async fn recv_or_panic(rx: &mut mpsc::UnboundedReceiver<String>, what: &str) -> String {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap_or_else(|| panic!("gate channel closed while waiting for {what}"))
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

fn call(id: &str, name: &str, key: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        input: json!({ "key": key }),
        intent: None,
        thought_signature: None,
    }
}

fn tool_result_sequence(agent: &Agent) -> Vec<(String, String, Option<bool>)> {
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

#[tokio::test]
async fn concurrent_group_applies_results_in_request_order_and_announces_all_rows_first() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (gate, mut started, mut finished) = RoundGate::new();
    let registry = registry_with(vec![
        RoundTool {
            name: "read",
            class: ConcurrencyClass::Concurrent,
            behavior: RoundBehavior::Gate(Arc::clone(&gate)),
        },
        RoundTool {
            name: "bash",
            class: ConcurrencyClass::Ordered,
            behavior: RoundBehavior::Echo,
        },
    ])
    .await;

    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
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

    let mut updates = Bus::global().subscribe();
    let task = tokio::spawn(async move { agent.run_turn(false).await.map(|_| agent) });

    // Both concurrent members must start before either finishes, which is the
    // overlap itself; no timing assertion is involved.
    let mut starts = vec![
        recv_or_panic(&mut started, "first read start").await,
        recv_or_panic(&mut started, "second read start").await,
    ];
    starts.sort();
    assert_eq!(starts, vec!["a", "b"]);

    // Deliberately reverse completion order relative to request order.
    gate.release("b");
    assert_eq!(recv_or_panic(&mut finished, "b finish").await, "b");
    gate.release("a");
    assert_eq!(recv_or_panic(&mut finished, "a finish").await, "a");

    let agent = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("turn completes after the gated group")
        .unwrap()
        .unwrap();

    let results = tool_result_sequence(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["call-a", "call-b", "call-c"],
        "tool results must stay in request order, got {results:?}"
    );
    let contents: Vec<&str> = results.iter().map(|(_, content, _)| content.as_str()).collect();
    assert_eq!(contents, vec!["read:a", "read:b", "bash:c"]);

    let mut events: Vec<(String, &'static str)> = Vec::new();
    while let Ok(event) = updates.try_recv() {
        if let BusEvent::ToolUpdated(update) = event
            && update.session_id == session_id
        {
            events.push((update.tool_call_id, update.status.as_str()));
        }
    }
    assert_eq!(
        events,
        vec![
            ("call-a".to_string(), "running"),
            ("call-b".to_string(), "running"),
            ("call-a".to_string(), "completed"),
            ("call-b".to_string(), "completed"),
            ("call-c".to_string(), "running"),
            ("call-c".to_string(), "completed"),
        ],
        "a group must announce every row before its first terminal event"
    );
}

#[tokio::test]
async fn disabled_parallel_tools_plans_one_member_groups() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let registry = registry_with(vec![
        RoundTool {
            name: "read",
            class: ConcurrencyClass::Concurrent,
            behavior: RoundBehavior::Echo,
        },
        RoundTool {
            name: "bash",
            class: ConcurrencyClass::Ordered,
            behavior: RoundBehavior::Echo,
        },
    ])
    .await;
    let (provider, _tx) = provider_pair();
    let agent = Agent::new(provider, registry);

    let calls = vec![
        call("call-a", "read", "a"),
        call("call-b", "read", "b"),
        call("call-c", "bash", "c"),
    ];

    let enabled = agent.plan_tool_round_groups(&calls, true).await;
    let sizes: Vec<usize> = enabled.iter().map(|group| group.end - group.start).collect();
    assert_eq!(sizes, vec![2, 1], "concurrent reads must share one group when enabled");

    let disabled = agent.plan_tool_round_groups(&calls, false).await;
    let sizes: Vec<usize> = disabled.iter().map(|group| group.end - group.start).collect();
    assert_eq!(
        sizes,
        vec![1, 1, 1],
        "every call must be its own group when parallel_tools is off"
    );
}

#[tokio::test]
async fn malformed_call_keeps_its_own_error_result_and_its_sibling_still_runs() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let registry = registry_with(vec![RoundTool {
        name: "read",
        class: ConcurrencyClass::Concurrent,
        behavior: RoundBehavior::Echo,
    }])
    .await;
    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read one file and one broken call".into(),
            cache_control: None,
        }],
    );

    for event in tool_use_events("call-a", "read", "{\"key\":\"a\"}")
        .into_iter()
        .chain(tool_use_events("call-bad", "read", "null"))
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["call-a", "call-bad"], "got {results:?}");
    assert_eq!(results[0].1, "read:a");
    assert_eq!(results[0].2, None);
    assert!(
        results[1].1.contains("arguments must be a JSON object"),
        "malformed call must report its own validation error, got {:?}",
        results[1].1
    );
    assert_eq!(results[1].2, Some(true));
}

/// The session budget this test seeds, far from `DEFAULT_TOKEN_BUDGET`. The
/// 200 KB result is condensed to the 48 KiB generic budget (~8k tokens), which
/// is still above the ~7.2k single-output ceiling of a 24k window.
const SESSION_WINDOW: usize = 24_000;

/// Returns a result far larger than the session budget, so the
/// context-overflow guard inside `Registry::execute` has something to refuse.
struct HugeRoundTool;

#[async_trait::async_trait]
impl Tool for HugeRoundTool {
    fn name(&self) -> &str {
        "huge"
    }

    fn description(&self) -> &str {
        "turn_loops round test double that returns an oversized result"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _: Value, _: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new("x".repeat(200_000)))
    }
}

/// `RoundProvider` that reports a distinctive context window, so the budget the
/// session seeds can be told apart from a fresh manager's default.
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
        "round-window-test"
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

async fn budget_and_count(registry: &Registry) -> (usize, usize) {
    let manager = registry.compaction();
    let guard = manager.read().await;
    (guard.token_budget(), guard.effective_token_count())
}

#[tokio::test]
async fn round_guard_observes_the_session_budget_not_a_clone_floor() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let registry = Registry::empty();
    registry
        .register("huge".to_string(), Arc::new(HugeRoundTool) as Arc<dyn Tool>)
        .await;

    let (tx, rx) = mpsc::channel(64);
    let provider = Arc::new(WindowRoundProvider {
        inner: RoundProvider(Arc::new(Mutex::new(Some(rx)))),
        window: SESSION_WINDOW,
    });
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "return the oversized result".into(),
            cache_control: None,
        }],
    );

    for event in tool_use_events("call-huge", "huge", "{}")
        .into_iter()
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    agent.run_turn(false).await.unwrap();

    let (session_budget, session_count) = budget_and_count(&agent.registry).await;
    let (plain_budget, plain_count) = budget_and_count(&agent.registry.clone()).await;
    let (sharing_budget, sharing_count) = budget_and_count(&agent.registry.clone_sharing_compaction()).await;
    std::println!("QA session budget={session_budget} count={session_count}");
    std::println!("QA plain   budget={plain_budget} count={plain_count}");
    std::println!("QA sharing budget={sharing_budget} count={sharing_count}");

    assert_eq!(session_budget, SESSION_WINDOW);
    assert_eq!(
        (plain_budget, plain_count),
        (
            crate::compaction::DEFAULT_TOKEN_BUDGET,
            crate::compaction::SYSTEM_OVERHEAD_TOKENS
        ),
        "a plain clone must keep its fresh manager, which is what the round must not use"
    );
    assert_eq!((sharing_budget, sharing_count), (session_budget, session_count));

    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 1, "got {results:?}");
    std::println!(
        "QA tool_result_prefix={}",
        results[0].1.chars().take(90).collect::<String>()
    );
    assert!(
        results[0].1.contains("OUTPUT WITHHELD"),
        "the round's registry must refuse against the session budget, got: {}",
        results[0].1
    );
    assert!(
        results[0].1.contains("24k"),
        "the refusal must name the session budget, got: {}",
        results[0].1
    );
}

use std::sync::atomic::{AtomicUsize, Ordering};

/// Small enough that the guard's overhead floor stays out of the arithmetic:
/// `estimate_compaction_tokens_from_chars` adds it only from half the default
/// budget up, so a message's character count is the whole seed.
const ROUND_BUDGET: usize = 20_000;

const SEED_TARGET: usize = 15_000;

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
        "round budget test double with a settable output size"
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
/// tokens before the round, so the round's own ceiling is reachable within a
/// single round instead of hundreds of small ones.
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

async fn run_round(mut agent: Agent, tx: mpsc::Sender<Result<StreamEvent>>, calls: &[(&str, &str, &str)]) -> Agent {
    for (id, name, key) in calls {
        for event in tool_use_events(id, name, &format!("{{\"key\":\"{key}\"}}")) {
            tx.send(Ok(event)).await.unwrap();
        }
    }
    tx.send(Ok(round_end())).await.unwrap();
    drop(tx);
    agent.run_turn(false).await.unwrap();
    agent
}

/// Per-output dump of what the round decided, printed for the QA artifact and
/// reconstructed from the transcript the round produced: the seed the round was
/// handed, then each applied result added the way the round adds it.
fn dump_decisions(seed: usize, results: &[(String, String, Option<bool>)]) {
    let threshold = round_threshold();
    std::println!("QA round-budget budget={ROUND_BUDGET} threshold={threshold} seed={seed}");
    let mut running = seed;
    for (index, (id, content, _)) in results.iter().enumerate() {
        let refused = content.contains("CONTEXT LIMIT REACHED");
        running += crate::util::estimate_tokens(content);
        std::println!(
            "QA round-budget output[{index}] id={id} refused={refused} running_total={running} \
             threshold={threshold} budget={ROUND_BUDGET}"
        );
    }
}

/// The round seeds AFTER the assistant message holding this round's `tool_use`
/// blocks is recorded, so a seed measured before the turn is about thirty
/// tokens lighter than the round's own. Every boundary below leaves 500 tokens
/// of slack, which absorbs that gap and any accounting drift around it.
const MARGIN_TOKENS: usize = 500;

#[tokio::test]
async fn round_budget_refuses_the_second_output_that_crosses_the_ceiling() {
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

    // Each output alone clears the tool layer's per-output guard; their sum is
    // what cannot fit in one round. The first leaves 500 tokens to spare, the
    // second spends 1,000 more and must be refused.
    read_chars.store(chars_in(threshold - seed - MARGIN_TOKENS), Ordering::SeqCst);
    ls_chars.store(chars_in(1_000), Ordering::SeqCst);

    let agent = run_round(agent, tx, &[("call-a", "read", "a"), ("call-b", "ls", "b")]).await;

    let results = tool_result_sequence(&agent);
    dump_decisions(seed, &results);
    assert_eq!(results.len(), 2, "got {results:?}");
    assert_eq!(results[0].0, "call-a", "request order survives a refusal");
    assert!(
        !results[0].1.contains("CONTEXT LIMIT REACHED") && !results[0].1.contains("OUTPUT WITHHELD"),
        "the first output fits and must be admitted, got {:?}",
        results[0].1.chars().take(90).collect::<String>()
    );
    assert!(
        results[1].1.contains("CONTEXT LIMIT REACHED"),
        "the second output crosses the round's ceiling and must be refused, got {:?}",
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
async fn round_budget_refuses_every_output_when_the_seed_is_already_spent() {
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

    let results = tool_result_sequence(&agent);
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
    let (bash, bash_chars) = sized_tool("bash", ConcurrencyClass::Ordered);
    let registry = sized_registry(vec![read, bash]).await;

    let (mut agent, tx) = seeded_agent(registry, SEED_TARGET).await;
    let (_, seed) = round_seed_and_budget(&mut agent).await;
    let threshold = round_threshold();
    assert!(seed < threshold - MARGIN_TOKENS, "the seed must leave room, got {seed}");

    // Group one spends the round down to within 500 tokens of the ceiling;
    // group two must inherit that total rather than start again from the seed.
    read_chars.store(chars_in(threshold - seed - MARGIN_TOKENS), Ordering::SeqCst);
    bash_chars.store(chars_in(1_000), Ordering::SeqCst);

    let agent = run_round(agent, tx, &[("call-a", "read", "a"), ("call-b", "bash", "b")]).await;

    let results = tool_result_sequence(&agent);
    dump_decisions(seed, &results);
    assert_eq!(results.len(), 2, "got {results:?}");
    assert!(
        !results[0].1.contains("CONTEXT LIMIT REACHED"),
        "the first group's output is the last one that fits, got {:?}",
        results[0].1.chars().take(90).collect::<String>()
    );
    assert!(
        results[1].1.contains("CONTEXT LIMIT REACHED"),
        "the second group starts from the first group's total, so its output is refused, got {:?}",
        results[1].1.chars().take(120).collect::<String>()
    );
}

#[tokio::test]
async fn a_one_member_round_under_the_ceiling_is_untouched() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (bash, bash_chars) = sized_tool("bash", ConcurrencyClass::Ordered);
    let registry = sized_registry(vec![bash]).await;

    let (mut agent, tx) = seeded_agent(registry, SEED_TARGET).await;
    let (_, seed) = round_seed_and_budget(&mut agent).await;
    let threshold = round_threshold();
    assert!(seed < threshold, "the seed leaves room, got {seed} against {threshold}");
    bash_chars.store(400, Ordering::SeqCst);

    let agent = run_round(agent, tx, &[("call-bash", "bash", "c")]).await;

    let results = tool_result_sequence(&agent);
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
        "scripted-loop-guard-test"
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
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for CountingReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "loop-guard test double for read"
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
        let count = self.executions.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Ok(ToolOutput::new(format!(
            "read#{count}:{}",
            input["file_path"].as_str().unwrap_or_default()
        )))
    }
}

async fn counting_registry() -> (Registry, Arc<std::sync::atomic::AtomicUsize>) {
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = Registry::empty();
    registry
        .register(
            "read".to_string(),
            Arc::new(CountingReadTool {
                executions: Arc::clone(&executions),
            }) as Arc<dyn Tool>,
        )
        .await;
    (registry, executions)
}

/// Sets `[features] loop_guard` for one test and restores the default before the
/// sandbox drops. `AuthTestSandbox` holds a global lock, so the write cannot
/// race another test's config read.
struct LoopGuardFlag;

impl LoopGuardFlag {
    fn set(root: &std::path::Path, enabled: bool) -> Self {
        write_features(root, enabled);
        Self
    }
}

impl Drop for LoopGuardFlag {
    fn drop(&mut self) {
        if let Ok(dir) = std::env::var("HORON_HOME") {
            write_features(std::path::Path::new(&dir), true);
        }
    }
}

fn write_features(root: &std::path::Path, loop_guard: bool) {
    std::fs::write(
        root.join("config.toml"),
        format!("[features]\nloop_guard = {loop_guard}\nparallel_tools = true\n"),
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

#[tokio::test]
async fn five_identical_reads_across_two_rounds_warn_refuse_then_stop() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry().await;
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

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["first-0", "first-1", "first-2", "second-0", "second-1"],
        "every call keeps its own result at its own index: {results:?}"
    );

    // The first three execute: the third is the warning, not a refusal.
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
    assert!(results[3].1.contains("4 times"), "{:?}", results[3].1);

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

    let notice = stored_loop_guard_notice(&agent).expect("the stop must leave a stored notice");
    assert_eq!(notice.lines().count(), 1, "the notice is one line: {notice}");

    let prompts = dynamic_prompts.lock().unwrap();
    assert_eq!(
        prompts.len(),
        2,
        "the turn must stop before a third request: {prompts:?}"
    );
    assert!(
        prompts[1].contains("Loop guard"),
        "the warning must reach the next request, got: {:?}",
        prompts[1]
    );
}

#[tokio::test]
async fn loop_guard_off_executes_all_five_identical_reads() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _flag = LoopGuardFlag::set(sandbox.root(), false);
    assert!(
        !crate::config::config().features.loop_guard,
        "the test needs the disabled path"
    );

    let (registry, executions) = counting_registry().await;
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

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 5, "{results:?}");
    assert!(
        results.iter().all(|(_, _, is_error)| is_error.is_none()),
        "with the guard off every call executes: {results:?}"
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert!(
        stored_loop_guard_notice(&agent).is_none(),
        "the guard must not stop anything when it is off"
    );
    assert!(
        dynamic_prompts
            .lock()
            .unwrap()
            .iter()
            .all(|prompt| !prompt.contains("Loop guard")),
        "no warning may be raised when the guard is off"
    );
}

#[tokio::test]
async fn warning_does_not_survive_into_the_next_turn_on_the_non_streaming_path() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, _executions) = counting_registry().await;
    let input = json!({ "file_path": "src/lib.rs" });
    let (provider, dynamic_prompts) = scripted_provider(vec![identical_round("first", "read", 3, &input)]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the same file three times".into(),
            cache_control: None,
        }],
    );

    agent.run_turn(false).await.unwrap();
    {
        let prompts = dynamic_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2, "one request per round: {prompts:?}");
        assert!(
            prompts[1].contains("Loop guard"),
            "the warning must reach the request after the warning: {:?}",
            prompts[1]
        );
    }

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "now do something else".into(),
            cache_control: None,
        }],
    );
    agent.run_turn(false).await.unwrap();

    let prompts = dynamic_prompts.lock().unwrap();
    assert_eq!(prompts.len(), 3, "the second turn makes its own request: {prompts:?}");
    assert!(
        !prompts[2].contains("Loop guard"),
        "a warning must not leak into the next turn, got: {:?}",
        prompts[2]
    );
    assert!(
        !agent.system_prompt_for_debug().contains("Loop guard"),
        "the reminder must be cleared when the turn ends"
    );
}

#[tokio::test]
async fn refused_call_produces_one_result_and_one_terminal_event_at_its_index() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry().await;
    let input = json!({ "file_path": "src/lib.rs" });
    let (provider, _prompts) = scripted_provider(vec![
        identical_round("first", "read", 3, &input),
        identical_round("second", "read", 1, &input),
    ]);
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the same file four times".into(),
            cache_control: None,
        }],
    );

    let mut updates = Bus::global().subscribe();
    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 4, "{results:?}");
    assert_eq!(results[3].0, "second-0", "the refusal lands at its own index");
    assert_eq!(results[3].2, Some(true));

    let mut terminal: Vec<(String, &'static str)> = Vec::new();
    while let Ok(event) = updates.try_recv() {
        if let BusEvent::ToolUpdated(update) = event
            && update.session_id == session_id
            && update.status.as_str() != "running"
        {
            terminal.push((update.tool_call_id, update.status.as_str()));
        }
    }
    assert_eq!(
        terminal,
        vec![
            ("first-0".to_string(), "completed"),
            ("first-1".to_string(), "completed"),
            ("first-2".to_string(), "completed"),
            ("second-0".to_string(), "error"),
        ],
        "each call, refused or not, ends exactly once at its own index"
    );
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "a refused call never reaches the tool"
    );
}

#[tokio::test]
async fn paginated_reads_of_one_file_execute_in_full() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry().await;
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

    let (provider, _prompts) = scripted_provider(vec![first, second]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "page through one file".into(),
            cache_control: None,
        }],
    );

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 5, "{results:?}");
    assert!(
        results.iter().all(|(_, _, is_error)| is_error.is_none()),
        "pagination is ordinary work and must never be refused: {results:?}"
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert!(stored_loop_guard_notice(&agent).is_none());
}

#[tokio::test]
async fn alternating_reads_of_two_files_execute_in_full() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = counting_registry().await;
    let mut round = Vec::new();
    for index in 0..6 {
        let path = if index % 2 == 0 { "left.rs" } else { "right.rs" };
        round.extend(tool_use_events(
            &format!("compare-{index}"),
            "read",
            &json!({ "file_path": path }).to_string(),
        ));
    }
    round.push(round_end());

    let (provider, _prompts) = scripted_provider(vec![round]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "compare two files".into(),
            cache_control: None,
        }],
    );

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 6, "{results:?}");
    assert!(
        results.iter().all(|(_, _, is_error)| is_error.is_none()),
        "alternating two files is comparison, not a loop: {results:?}"
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 6);
    assert!(stored_loop_guard_notice(&agent).is_none());
}

// ---------------------------------------------------------------------------
// End-to-end coverage through the real `run_turn` entry point (plan todo 14).
//
// The provider, the scripted message and the tool doubles come from
// `agent_tests.rs`, shared with the streaming loop's module, so both loops are
// driven by the same script instead of two near-copies. Every assertion here
// is structural: request order in the transcript, one start and one terminal
// event per call, and observed group size. Timing appears once, as a ratio
// between two members that each wait 400 ms.
// ---------------------------------------------------------------------------

use crate::agent::tests::{
    SharedGatedRound, SharedTimedRound, shared_dump_transcript, shared_echo_registry, shared_gated_round,
    shared_mixed_round, shared_recv_or_panic, shared_round_end, shared_round_provider, shared_timed_round,
    shared_tool_use,
};

fn tool_use_count(agent: &Agent) -> usize {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
        .count()
}

fn qa_tool_events(events: &[(String, &'static str)]) {
    for (index, (id, status)) in events.iter().enumerate() {
        std::println!("QA turn event[{index}] id={id} status={status}");
    }
}

#[tokio::test]
async fn end_to_end_group_keeps_request_order_and_announces_each_call_once() {
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
    let session_id = agent.session.id.clone();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read two files, then run the shell".into(),
            cache_control: None,
        }],
    );

    let mut updates = Bus::global().subscribe();
    let task = tokio::spawn(async move { agent.run_turn(false).await.map(|_| agent) });

    // Both concurrent members must be in flight together: that overlap IS the
    // group, and it needs no clock to observe.
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

    let results = tool_result_sequence(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["call-a", "call-b", "call-c"],
        "results must follow request order even when b finished first, got {results:?}"
    );
    let contents: Vec<&str> = results.iter().map(|(_, content, _)| content.as_str()).collect();
    assert_eq!(contents, vec!["read:a", "read:b", "bash:c"]);
    assert_eq!(tool_use_count(&agent), results.len(), "one result per use");
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);
    shared_dump_transcript("turn", &agent);

    let mut events: Vec<(String, &'static str)> = Vec::new();
    while let Ok(event) = updates.try_recv() {
        if let BusEvent::ToolUpdated(update) = event
            && update.session_id == session_id
        {
            events.push((update.tool_call_id, update.status.as_str()));
        }
    }
    qa_tool_events(&events);
    assert_eq!(
        events,
        vec![
            ("call-a".to_string(), "running"),
            ("call-b".to_string(), "running"),
            ("call-a".to_string(), "completed"),
            ("call-b".to_string(), "completed"),
            ("call-c".to_string(), "running"),
            ("call-c".to_string(), "completed"),
        ],
        "every group member starts before the first terminal, and terminals follow request order"
    );
    for id in ["call-a", "call-b", "call-c"] {
        let starts = events
            .iter()
            .filter(|(event_id, status)| event_id.as_str() == id && *status == "running")
            .count();
        let terminals = events
            .iter()
            .filter(|(event_id, status)| event_id.as_str() == id && *status != "running")
            .count();
        assert_eq!(starts, 1, "{id} must announce exactly one start");
        assert_eq!(terminals, 1, "{id} must announce exactly one terminal event");
    }
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

    agent.run_turn(false).await.unwrap();

    let runs = runs.lock().unwrap().clone();
    let concurrent: Vec<&crate::agent::tests::SharedToolRun> =
        runs.iter().filter(|run| run.key == "a" || run.key == "b").collect();
    assert_eq!(concurrent.len(), 2, "both concurrent members must have run");
    for run in &concurrent {
        let duration = run.finished.duration_since(run.started);
        std::println!("QA overlap key={} duration_ms={}", run.key, duration.as_millis());
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
    std::println!("QA overlap span_ms={}", span.as_millis());
    // A ratio, not an absolute timing: two 400 ms members in one group span
    // one sleep, while running them one after another would need at least
    // 800 ms. 700 ms sits between those two, so the assertion fails exactly
    // when the group is not concurrent.
    assert!(
        span < Duration::from_millis(700),
        "two 400 ms members must overlap; sequential execution would need at least 800 ms, got {span:?}"
    );
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);

    let results = tool_result_sequence(&agent);
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

    let task = tokio::spawn(async move { agent.run_turn(false).await.map(|_| agent) });

    let mut starts = vec![
        shared_recv_or_panic(&mut started, "first read start").await,
        shared_recv_or_panic(&mut started, "second read start").await,
    ];
    starts.sort();
    assert_eq!(starts, vec!["a".to_string(), "b".to_string()]);

    // The interrupt lands while the group is still in flight. This loop has no
    // mid-round cancel by design (the round is awaited to completion), so the
    // invariant under test is the transcript one: no `tool_use` may be left
    // without its `tool_result`.
    gate.release("a");
    assert_eq!(shared_recv_or_panic(&mut finished, "a finish").await, "a");
    shutdown.fire();
    gate.release("b");
    assert_eq!(shared_recv_or_panic(&mut finished, "b finish").await, "b");

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn returns after the interrupt")
        .unwrap()
        .unwrap();

    let results = tool_result_sequence(&agent);
    let uses = tool_use_count(&agent);
    std::println!("QA interrupt tool_use={uses} tool_result={}", results.len());
    assert_eq!(uses, 3, "the round asked for three calls");
    assert_eq!(results.len(), uses, "the interrupt must not drop a result: {results:?}");
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["call-a", "call-b", "call-c"], "{results:?}");
    shared_dump_transcript("turn-interrupt", &agent);
}

/// Sets `[features] parallel_tools = false` for one test and restores both
/// toggles before the sandbox drops.
struct DisabledParallelTools;

impl DisabledParallelTools {
    fn set(root: &std::path::Path) -> Self {
        std::fs::write(
            root.join("config.toml"),
            "[features]\nparallel_tools = false\nloop_guard = true\n",
        )
        .expect("the config is written");
        crate::config::invalidate_config_cache();
        Self
    }
}

impl Drop for DisabledParallelTools {
    fn drop(&mut self) {
        if let Ok(dir) = std::env::var("HORON_HOME") {
            write_features(std::path::Path::new(&dir), true);
        }
    }
}

#[tokio::test]
async fn end_to_end_disabled_parallel_tools_observes_group_size_one() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _flag = DisabledParallelTools::set(sandbox.root());
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

    let mut updates = Bus::global().subscribe();
    let task = tokio::spawn(async move { agent.run_turn(false).await.map(|_| agent) });

    assert_eq!(shared_recv_or_panic(&mut started, "first read start").await, "a");
    // Observed group size 1: the second Concurrent call must not be in flight
    // while the first one is. This is event order, never timing.
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

    let mut events: Vec<(String, &'static str)> = Vec::new();
    while let Ok(event) = updates.try_recv() {
        if let BusEvent::ToolUpdated(update) = event
            && update.session_id == session_id
        {
            events.push((update.tool_call_id, update.status.as_str()));
        }
    }
    qa_tool_events(&events);
    assert_eq!(
        events,
        vec![
            ("call-a".to_string(), "running"),
            ("call-a".to_string(), "completed"),
            ("call-b".to_string(), "running"),
            ("call-b".to_string(), "completed"),
            ("call-c".to_string(), "running"),
            ("call-c".to_string(), "completed"),
        ],
        "with parallel_tools off every call is its own group of size 1"
    );
    // The observed group size, derived from the event order alone.
    let mut groups = Vec::new();
    let mut open = 0usize;
    for (_, status) in &events {
        if *status == "running" {
            open += 1;
        } else if open > 0 {
            groups.push(open);
            open = 0;
        }
    }
    std::println!("QA observed group sizes={groups:?}");
    assert_eq!(groups, vec![1, 1, 1], "observed group size must be 1 for every call");
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 3);
    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 3, "{results:?}");
    shared_dump_transcript("turn-disabled", &agent);
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
    let session_id = agent.session.id.clone();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the same file five times".into(),
            cache_control: None,
        }],
    );

    let mut updates = Bus::global().subscribe();
    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["first-0", "first-1", "first-2", "second-0", "second-1"],
        "every call keeps its own result at its own index: {results:?}"
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

    let mut terminal: Vec<(String, &'static str)> = Vec::new();
    while let Ok(event) = updates.try_recv() {
        if let BusEvent::ToolUpdated(update) = event
            && update.session_id == session_id
            && update.status.as_str() != "running"
        {
            terminal.push((update.tool_call_id, update.status.as_str()));
        }
    }
    qa_tool_events(&terminal);
    assert_eq!(
        terminal,
        vec![
            ("first-0".to_string(), "completed"),
            ("first-1".to_string(), "completed"),
            ("first-2".to_string(), "completed"),
            ("second-0".to_string(), "error"),
            ("second-1".to_string(), "error"),
        ],
        "every call, refused or not, ends exactly once at its own index"
    );
}

/// IS-2's content half on the blocking entry point: the same scripted round
/// driven twice through `run_turn` must store the same `(tool_use_id, content)`
/// pairs whether the two concurrent reads overlap or run one at a time.
///
/// Order is pinned by other tests in this file; this one compares CONTENT, so a
/// payload that was truncated, reordered, or paired with the wrong id under
/// concurrency fails here even while every ordering assertion still holds.
#[tokio::test]
async fn parallel_and_sequential_rounds_store_the_same_content() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();

    let parallel = {
        let _flag = ParallelToolsFlag::set(sandbox.root(), true);
        assert!(
            crate::config::config().features.parallel_tools,
            "the first run needs the parallel path"
        );
        shared_round_results(true, vec![shared_mixed_round()], |mut agent| async move {
            agent.run_turn(false).await.expect("the blocking turn runs");
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
            agent.run_turn(false).await.expect("the blocking turn runs");
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
// Plan todos 33 and 35: the second net for polling, and the round budget for
// SDK-answered results.
// ---------------------------------------------------------------------------

/// A status-style tool double that counts how often it actually ran, so a
/// refusal can be told apart from an execution by evidence rather than by
/// reading the transcript alone.
struct CountingStatusTool {
    name: &'static str,
    executions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for CountingStatusTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "status test double that reports how many times it ran"
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

    async fn execute(&self, _: Value, _: ToolContext) -> Result<ToolOutput> {
        let count = self.executions.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ToolOutput::new(format!("status#{count}: running")))
    }
}

async fn status_registry(name: &'static str) -> (Registry, Arc<AtomicUsize>) {
    let executions = Arc::new(AtomicUsize::new(0));
    let registry = Registry::empty();
    registry
        .register(
            name.to_string(),
            Arc::new(CountingStatusTool {
                name,
                executions: Arc::clone(&executions),
            }) as Arc<dyn Tool>,
        )
        .await;
    (registry, executions)
}

/// One tool round per poll: the shape an unattended daemon falls into when it
/// keeps asking whether a background task has finished.
fn poll_rounds(prefix: &str, count: usize, input: &Value) -> Vec<Vec<StreamEvent>> {
    (0..count)
        .map(|index| {
            let mut events = tool_use_events(&format!("{prefix}-{index}"), "bg", &input.to_string());
            events.push(round_end());
            events
        })
        .collect()
}

/// Todo 33 end to end: a turn that only polls is bounded. Todo 29's trade is
/// untouched - every poll up to the cap executes - and the cap then ends the
/// turn with a notice instead of paying for another request.
#[tokio::test]
async fn a_turn_that_only_polls_is_bounded_by_the_cap() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = status_registry("bg").await;
    let input = json!({ "action": "status", "task_id": "task-7" });
    let (provider, dynamic_prompts) = scripted_provider(poll_rounds("poll", 30, &input));
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "wait until the job finishes".into(),
            cache_control: None,
        }],
    );

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    assert_eq!(
        results.len(),
        25,
        "24 polls run and the 25th is the cap, so no later round is requested: {results:?}"
    );
    for (index, (_, content, is_error)) in results[..24].iter().enumerate() {
        assert_eq!(*is_error, None, "poll {index} must execute: {content:?}");
        assert!(
            content.starts_with("status#"),
            "poll {index} must reach the tool: {content:?}"
        );
    }
    assert_eq!(results[24].2, Some(true), "{results:?}");
    assert!(
        results[24].1.contains("cap is 24"),
        "the capped poll carries the notice naming the cap, got {:?}",
        results[24].1
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        24,
        "the poll that hits the cap never reaches the tool"
    );

    let notice = stored_loop_guard_notice(&agent).expect("the cap must leave a stored notice");
    std::println!("QA polling cap, stored notice:\n{notice}");
    assert!(notice.contains("cap is 24"), "{notice}");
    assert_eq!(
        dynamic_prompts.lock().unwrap().len(),
        25,
        "one request per round, and none after the cap"
    );
}

/// The happy half of the same trade: a normal turn that polls a few times is
/// unaffected. Four polls run, the single reminder still rides the next
/// request, and nothing ends the turn.
#[tokio::test]
async fn a_short_poll_sequence_is_unaffected_by_the_cap() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = status_registry("bg").await;
    let input = json!({ "action": "status", "task_id": "task-7" });
    let (provider, dynamic_prompts) = scripted_provider(poll_rounds("short", 4, &input));
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "check the job a few times".into(),
            cache_control: None,
        }],
    );

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 4, "{results:?}");
    assert!(
        results.iter().all(|(_, _, is_error)| is_error.is_none()),
        "a short wait never escalates: {results:?}"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 4);
    assert!(
        stored_loop_guard_notice(&agent).is_none(),
        "the cap must not fire on a short sequence"
    );
    let prompts = dynamic_prompts.lock().unwrap();
    assert_eq!(prompts.len(), 5, "four poll rounds and the closing answer");
    assert!(
        prompts[3].contains("Loop guard"),
        "the one reminder still reaches the request after the third poll: {:?}",
        prompts[3]
    );
}

/// Todo 35: an SDK-answered result is an applied output of this round, so it
/// takes the round's own decision instead of reaching history whole. The round
/// here has ~3k tokens of room; the payload is ~10k.
#[tokio::test]
async fn an_sdk_answered_result_is_admitted_by_the_round_budget() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (registry, executions) = status_registry("remote_job").await;

    let (mut agent, tx) = seeded_agent(registry, SEED_TARGET).await;
    let (_, seed) = round_seed_and_budget(&mut agent).await;
    std::println!(
        "QA sdk round budget={ROUND_BUDGET} seed={seed} threshold={} payload_chars={}",
        round_threshold(),
        chars_in(10_000)
    );

    for event in tool_use_events("sdk-0", "remote_job", "{}") {
        tx.send(Ok(event)).await.unwrap();
    }
    tx.send(Ok(StreamEvent::ToolResult {
        tool_use_id: "sdk-0".into(),
        content: "x".repeat(chars_in(10_000)),
        is_error: false,
    }))
    .await
    .unwrap();
    tx.send(Ok(round_end())).await.unwrap();
    drop(tx);

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 1, "{results:?}");
    let content = &results[0].1;
    std::println!(
        "QA sdk result chars={} head={:?}",
        content.chars().count(),
        content.chars().take(120).collect::<String>()
    );
    assert!(
        content.contains("CONTEXT LIMIT REACHED"),
        "an SDK result that does not fit must be withheld like any other output, got {} chars: {:?}",
        content.chars().count(),
        content.chars().take(200).collect::<String>()
    );
    assert!(
        content.chars().count() < chars_in(10_000),
        "the oversized SDK result must not reach history whole"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "the SDK answered the call, so the tool itself never ran"
    );
}

/// A tool-scoped `[[stream_rules]]` entry refuses the call before it runs and
/// reports the rule, while a turn-repeat rule leaves no ledger entry behind.
#[tokio::test]
async fn tool_scope_stream_rule_blocks_a_write_call() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    std::fs::write(
        sandbox.root().join("config.toml"),
        r#"
[features]
stream_rules = true

[[stream_rules]]
id = "no-unwrap"
pattern = "unwrap\\(\\)"
body = "Use the ? operator instead of unwrap()."
scope = ["tool:write"]
"#,
    )
    .expect("config");
    crate::config::invalidate_config_cache();

    let registry = registry_with(vec![RoundTool {
        name: "write",
        class: ConcurrencyClass::Ordered,
        behavior: RoundBehavior::Echo,
    }])
    .await;
    let (provider, tx) = provider_pair();
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "write a file containing unwrap()".into(),
            cache_control: None,
        }],
    );

    for event in tool_use_events("call-a", "write", "{\"key\":\"x.unwrap()\"}")
        .into_iter()
        .chain([round_end()])
    {
        tx.send(Ok(event)).await.unwrap();
    }
    drop(tx);

    agent.run_turn(false).await.unwrap();

    let results = tool_result_sequence(&agent);
    assert_eq!(results.len(), 1, "the refused call still owes one result: {results:?}");
    assert_eq!(results[0].0, "call-a");
    assert!(
        results[0]
            .1
            .starts_with("Blocked by rule \"no-unwrap\": Use the ? operator instead of unwrap()."),
        "got {:?}",
        results[0].1
    );
    assert_eq!(results[0].2, Some(true), "a blocked call is an error result");
    assert!(!results[0].1.starts_with("write:"), "the tool must not have executed");
    assert!(
        agent.session.stream_rule_ledger.is_empty(),
        "a turn-repeat rule records no ledger entry"
    );
}

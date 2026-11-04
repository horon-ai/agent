use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::sync::mpsc;

use super::InterruptSignal;
use super::tool_round::{
    GroupCancel, MAX_GROUP_MEMBERS, MAX_NETWORK_MEMBERS, ToolGroup, classify_call, plan_groups, plan_round, run_group,
};
use crate::message::ToolCall;
use crate::protocol::{SessionToolConfig, SessionToolDefinition};
use crate::tool::{Registry, Tool, ToolContext, ToolEffects, ToolExecutionMode, ToolOutput};

enum Behavior {
    EchoCallId,
    Gate(Arc<Gate>),
    Panic,
}

struct FakeTool {
    name: &'static str,
    effects: ToolEffects,
    behavior: Behavior,
}

#[async_trait::async_trait]
impl Tool for FakeTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "tool_round test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn effects(&self) -> ToolEffects {
        self.effects
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        match &self.behavior {
            Behavior::EchoCallId => Ok(ToolOutput::new(ctx.tool_call_id)),
            Behavior::Gate(gate) => {
                gate.park(input["key"].as_str().unwrap_or_default()).await;
                Ok(ToolOutput::new(ctx.tool_call_id))
            }
            Behavior::Panic => panic!("fake tool panic"),
        }
    }
}

#[derive(Default)]
struct GateState {
    released: HashSet<String>,
    waiters: HashMap<String, Arc<Notify>>,
}

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

async fn registry_with(tools: Vec<FakeTool>) -> Registry {
    let registry = Registry::empty();
    for tool in tools {
        registry
            .register(tool.name.to_string(), Arc::new(tool) as Arc<dyn Tool>)
            .await;
    }
    registry
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

fn base_context() -> ToolContext {
    ToolContext {
        session_id: "tool-round-tests".to_string(),
        message_id: "message".to_string(),
        tool_call_id: "round".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn sizes(groups: &[ToolGroup]) -> Vec<usize> {
    groups.iter().map(ToolGroup::len).collect()
}

#[test]
fn grouping_splits_concurrent_runs_around_ordered_calls() {
    let effects = vec![
        ToolEffects::read(),
        ToolEffects::read(),
        ToolEffects::write().union(ToolEffects::process()),
        ToolEffects::read(),
        ToolEffects::read(),
        ToolEffects::read(),
    ];

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![2, 1, 3]);
    assert_eq!(
        groups,
        vec![
            ToolGroup { start: 0, end: 2 },
            ToolGroup { start: 2, end: 3 },
            ToolGroup { start: 3, end: 6 },
        ]
    );
}

#[test]
fn grouping_caps_a_concurrent_run_at_ten_members() {
    let effects = vec![ToolEffects::read(); 11];

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![MAX_GROUP_MEMBERS, 1]);
    assert_eq!(groups[0], ToolGroup { start: 0, end: 10 });
    assert_eq!(groups[1], ToolGroup { start: 10, end: 11 });
}

#[test]
fn grouping_caps_network_calls_at_four_members() {
    let effects = vec![ToolEffects::read().union(ToolEffects::network()); 5];

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![MAX_NETWORK_MEMBERS, 1]);
    assert_eq!(groups[0], ToolGroup { start: 0, end: 4 });
    assert_eq!(groups[1], ToolGroup { start: 4, end: 5 });
}

#[test]
fn a_mixed_group_never_carries_more_than_four_network_members() {
    let mut effects = vec![ToolEffects::read()];
    effects.extend(vec![ToolEffects::read().union(ToolEffects::network()); 5]);

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![5, 1]);
    assert_eq!(groups[0], ToolGroup { start: 0, end: 5 });
    for group in &groups {
        let network = group.range().filter(|index| effects[*index].networks()).count();
        assert!(
            network <= MAX_NETWORK_MEMBERS,
            "group {:?} carries {network} network members",
            group
        );
    }
}

#[tokio::test]
async fn an_unclassified_call_is_treated_as_ordered() {
    // A name the registry cannot resolve has no declared effects, so every such
    // call is a barrier of its own and nothing in that round overlaps.
    let registry = registry_with(vec![FakeTool {
        name: "read",
        effects: ToolEffects::read(),
        behavior: Behavior::EchoCallId,
    }])
    .await;
    let calls = vec![
        call("a", "read", "a"),
        call("b", "definitely_not_a_tool", "b"),
        call("c", "also_not_a_tool", "c"),
    ];

    let mut effects = Vec::new();
    for round_call in &calls {
        effects.push(classify_call(&registry, "tool-round-unknown-session", round_call).await);
    }

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![1, 1, 1]);
}

#[tokio::test]
async fn results_come_back_in_request_order_when_the_first_member_finishes_last() {
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![FakeTool {
        name: "gated",
        effects: ToolEffects::read(),
        behavior: Behavior::Gate(Arc::clone(&gate)),
    }])
    .await;
    let calls = vec![
        call("call-a", "gated", "a"),
        call("call-b", "gated", "b"),
        call("call-c", "gated", "c"),
    ];
    let ctx = base_context();

    let driver = async {
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(started.recv().await.expect("every member starts"));
        }
        seen.sort();
        assert_eq!(seen, vec!["a", "b", "c"]);

        for key in ["b", "c"] {
            gate.release(key);
            assert_eq!(finished.recv().await.as_deref(), Some(key));
        }
        gate.release("a");
        assert_eq!(finished.recv().await.as_deref(), Some("a"));
    };

    let (outcome, ()) = tokio::join!(run_group(registry, &calls, &ctx, None), driver);

    assert_eq!(outcome.unstarted, Vec::<usize>::new());
    let order: Vec<usize> = outcome.results.iter().map(|(index, _)| *index).collect();
    assert_eq!(order, vec![0, 1, 2]);
    let outputs: Vec<String> = outcome
        .results
        .iter()
        .map(|(_, result)| result.as_ref().unwrap().output.clone())
        .collect();
    assert_eq!(outputs, vec!["call-a", "call-b", "call-c"]);
}

#[tokio::test]
async fn a_panicking_member_becomes_one_error_and_its_siblings_still_return() {
    let registry = registry_with(vec![
        FakeTool {
            name: "boom",
            effects: ToolEffects::read(),
            behavior: Behavior::Panic,
        },
        FakeTool {
            name: "echo",
            effects: ToolEffects::read(),
            behavior: Behavior::EchoCallId,
        },
    ])
    .await;
    let calls = vec![call("call-boom", "boom", "x"), call("call-echo", "echo", "y")];

    let outcome = run_group(registry, &calls, &base_context(), None).await;

    assert_eq!(outcome.results.len(), 2);
    assert_eq!(outcome.results[0].0, 0);
    let error = outcome.results[0]
        .1
        .as_ref()
        .expect_err("a panicking member must be that call's error");
    assert!(error.to_string().contains("panicked"), "{error}");
    assert_eq!(outcome.results[1].0, 1);
    assert_eq!(outcome.results[1].1.as_ref().unwrap().output, "call-echo");
    assert_eq!(outcome.unstarted, Vec::<usize>::new());
}

#[tokio::test]
async fn cancellation_returns_partial_results_and_the_unstarted_indexes() {
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![FakeTool {
        name: "gated",
        effects: ToolEffects::read(),
        behavior: Behavior::Gate(Arc::clone(&gate)),
    }])
    .await;
    let calls = vec![
        call("call-a", "gated", "a"),
        call("call-b", "gated", "b"),
        call("call-c", "gated", "c"),
    ];
    let ctx = base_context();
    let signal = InterruptSignal::new();

    let driver = async {
        for _ in 0..3 {
            started.recv().await.expect("every member starts");
        }
        gate.release("b");
        assert_eq!(finished.recv().await.as_deref(), Some("b"));
        signal.fire();
    };

    let (outcome, ()) = tokio::join!(
        run_group(registry, &calls, &ctx, Some(GroupCancel::new(signal.clone()))),
        driver
    );

    assert_eq!(outcome.results.len(), 1, "{:?}", outcome.results);
    assert_eq!(outcome.results[0].0, 1);
    assert_eq!(outcome.results[0].1.as_ref().unwrap().output, "call-b");
    assert_eq!(outcome.unstarted, vec![0, 2]);
}

#[tokio::test]
async fn every_result_keeps_its_original_call_id() {
    let registry = registry_with(vec![FakeTool {
        name: "echo",
        effects: ToolEffects::read(),
        behavior: Behavior::EchoCallId,
    }])
    .await;
    let calls = vec![
        call("toolu_01", "echo", "a"),
        call("toolu_02", "echo", "b"),
        call("toolu_03", "echo", "c"),
    ];

    let outcome = run_group(registry, &calls, &base_context(), None).await;

    assert_eq!(outcome.results.len(), 3);
    for (index, result) in &outcome.results {
        assert_eq!(result.as_ref().unwrap().output, calls[*index].id);
    }
}

#[tokio::test]
async fn a_session_custom_tool_is_ordered_even_when_it_shadows_a_builtin() {
    let registry = registry_with(vec![FakeTool {
        name: "read",
        effects: ToolEffects::read(),
        behavior: Behavior::EchoCallId,
    }])
    .await;
    let read_call = call("call-read", "read", "a");
    assert_eq!(
        classify_call(&registry, "tool-round-plain-session", &read_call).await,
        ToolEffects::read()
    );

    let session = "tool-round-sdk-custom";
    let (sender, _receiver) = mpsc::unbounded_channel();
    crate::tool::sdk::configure(
        session,
        "tool-round-tests",
        SessionToolConfig {
            enabled: None,
            disabled: Vec::new(),
            custom: vec![SessionToolDefinition {
                name: "read".to_string(),
                description: "client callback".to_string(),
                parameters: json!({"type": "object"}),
            }],
        },
        sender,
    )
    .expect("configure session tools");

    let effects = classify_call(&registry, session, &read_call).await;
    crate::tool::sdk::remove_session(session);

    assert_eq!(effects, ToolEffects::write());
}

#[test]
fn grouping_fills_a_group_to_ten_before_it_starts_another() {
    // Exactly ten concurrent calls stay in one group: the flush happens when
    // the eleventh arrives, not at nine or ten. This pins the boundary the
    // eleven-call test only implies.
    let ten = vec![ToolEffects::read(); MAX_GROUP_MEMBERS];

    let groups = plan_groups(&ten);

    assert_eq!(
        groups,
        vec![ToolGroup {
            start: 0,
            end: MAX_GROUP_MEMBERS,
        }]
    );

    // A round longer than two groups flushes again at every ten, so the cap is
    // a rolling split rather than a single one-shot cut.
    let twenty = vec![ToolEffects::read(); MAX_GROUP_MEMBERS * 2];

    let groups = plan_groups(&twenty);

    assert_eq!(sizes(&groups), vec![MAX_GROUP_MEMBERS, MAX_GROUP_MEMBERS]);
    assert_eq!(groups[0], ToolGroup { start: 0, end: 10 });
    assert_eq!(groups[1], ToolGroup { start: 10, end: 20 });
}

#[tokio::test]
async fn the_network_cap_counts_aliased_network_names() {
    // A provider alias resolves to the same paid network tool, so five aliased
    // calls must split at four exactly like five canonical ones.
    let registry = registry_with(vec![FakeTool {
        name: "websearch",
        effects: ToolEffects::read().union(ToolEffects::network()),
        behavior: Behavior::EchoCallId,
    }])
    .await;
    let calls: Vec<ToolCall> = (0..5)
        .map(|index| call(&format!("c{index}"), "functions.websearch", "k"))
        .collect();

    let mut effects = Vec::new();
    for round_call in &calls {
        effects.push(classify_call(&registry, "tool-round-alias-session", round_call).await);
    }

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![MAX_NETWORK_MEMBERS, 1]);
    assert_eq!(groups[0], ToolGroup { start: 0, end: 4 });
    assert_eq!(groups[1], ToolGroup { start: 4, end: 5 });
}

#[test]
fn network_effect_limits_a_group_without_a_name_list() {
    // Nothing here names a tool: five calls whose declared effects spend a
    // network request split at four because the effects say so.
    let effects = vec![ToolEffects::read().union(ToolEffects::network()); 5];

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![MAX_NETWORK_MEMBERS, 1]);
    assert_eq!(groups[0], ToolGroup { start: 0, end: 4 });
    assert_eq!(groups[1], ToolGroup { start: 4, end: 5 });
}

#[test]
fn write_effect_closes_the_open_group() {
    let effects = vec![
        ToolEffects::read(),
        ToolEffects::read(),
        ToolEffects::write(),
        ToolEffects::read(),
    ];

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![2, 1, 1]);
    assert_eq!(
        groups,
        vec![
            ToolGroup { start: 0, end: 2 },
            ToolGroup { start: 2, end: 3 },
            ToolGroup { start: 3, end: 4 },
        ]
    );
}

#[test]
fn read_and_network_share_a_group_until_the_network_cap() {
    // Four network members is the cap, so this exercises the boundary itself:
    // reads fill the group alongside network calls right up to
    // MAX_NETWORK_MEMBERS, and the fifth network call has to start a new group.
    let mut effects = vec![ToolEffects::read(), ToolEffects::read(), ToolEffects::read()];
    effects.extend(vec![
        ToolEffects::read().union(ToolEffects::network());
        MAX_NETWORK_MEMBERS
    ]);
    effects.push(ToolEffects::read().union(ToolEffects::network()));

    let groups = plan_groups(&effects);

    assert_eq!(MAX_NETWORK_MEMBERS, 4, "the boundary this test asserts");
    assert_eq!(
        sizes(&groups),
        vec![3 + MAX_NETWORK_MEMBERS, 1],
        "the group fills to the network cap and the next network call starts another"
    );
    assert_eq!(groups[0], ToolGroup { start: 0, end: 7 });
    assert_eq!(groups[1], ToolGroup { start: 7, end: 8 });

    let filled = groups[0].range().filter(|index| effects[*index].networks()).count();
    assert_eq!(
        filled, MAX_NETWORK_MEMBERS,
        "the first group has to actually reach the cap, not stop short of it"
    );
    assert_eq!(
        groups[0].range().filter(|index| !effects[*index].networks()).count(),
        3,
        "plain reads share the same group as network calls"
    );
}

#[tokio::test]
async fn unknown_tool_effect_is_a_barrier() {
    // The registry knows `read`; the middle call names nothing. A barrier for
    // the unknown call is what keeps the two reads on either side apart.
    let registry = registry_with(vec![FakeTool {
        name: "read",
        effects: ToolEffects::read(),
        behavior: Behavior::EchoCallId,
    }])
    .await;
    let calls = vec![
        call("a", "read", "a"),
        call("b", "read", "b"),
        call("c", "no_such_tool", "c"),
        call("d", "read", "d"),
    ];

    let mut effects = Vec::new();
    for round_call in &calls {
        effects.push(classify_call(&registry, "tool-round-unknown-barrier", round_call).await);
    }
    assert_eq!(effects[2], ToolEffects::write());

    let groups = plan_groups(&effects);

    assert_eq!(sizes(&groups), vec![2, 1, 1]);
}

/// A tool whose reach depends on its input, like `read` of a remote path.
struct InputEffectsTool {
    name: &'static str,
}

#[async_trait::async_trait]
impl Tool for InputEffectsTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "per-input effects test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    fn effects_for_input(&self, input: &Value) -> ToolEffects {
        if input["remote"].as_bool().unwrap_or(false) {
            ToolEffects::read().union(ToolEffects::network())
        } else {
            ToolEffects::read()
        }
    }

    async fn execute(&self, _input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new(ctx.tool_call_id))
    }
}

#[tokio::test]
async fn plan_round_reads_the_effects_of_each_call_from_its_input() {
    // The tool is one `read`; only the input decides which calls spend a network
    // request. `plan_round` must therefore consult `effects_for_input` per call
    // rather than the tool's static effects, or all five would share one group.
    let registry = registry_of(vec![Arc::new(InputEffectsTool { name: "read" })]).await;
    let calls: Vec<ToolCall> = (0..5)
        .map(|index| ToolCall {
            id: format!("call-{index}"),
            name: "read".to_string(),
            input: json!({ "remote": true }),
            intent: None,
            thought_signature: None,
        })
        .collect();

    let groups = plan_round(&registry, "tool-round-per-input", &calls).await;

    assert_eq!(sizes(&groups), vec![MAX_NETWORK_MEMBERS, 1]);
    assert_eq!(groups[0], ToolGroup { start: 0, end: 4 });
    assert_eq!(groups[1], ToolGroup { start: 4, end: 5 });

    // A local read declares no network effect, so it joins the open group.
    let mixed = vec![
        ToolCall {
            id: "local-0".to_string(),
            name: "read".to_string(),
            input: json!({ "remote": false }),
            intent: None,
            thought_signature: None,
        },
        ToolCall {
            id: "remote-0".to_string(),
            name: "read".to_string(),
            input: json!({"remote": true}),
            intent: None,
            thought_signature: None,
        },
    ];
    let groups = plan_round(&registry, "tool-round-per-input", &mixed).await;
    assert_eq!(groups, vec![ToolGroup { start: 0, end: 2 }]);
}

#[tokio::test]
async fn a_round_cancelled_before_dispatch_reports_every_index_unstarted() {
    let (gate, mut started, _finished) = Gate::new();
    let registry = registry_with(vec![FakeTool {
        name: "gated",
        effects: ToolEffects::read(),
        behavior: Behavior::Gate(Arc::clone(&gate)),
    }])
    .await;
    let calls = vec![call("call-a", "gated", "a"), call("call-b", "gated", "b")];
    let signal = InterruptSignal::new();
    signal.fire();

    let outcome = run_group(registry, &calls, &base_context(), Some(GroupCancel::new(signal))).await;

    assert!(outcome.results.is_empty(), "{:?}", outcome.results);
    assert_eq!(outcome.unstarted, vec![0, 1]);
    assert!(
        started.try_recv().is_err(),
        "a round already cancelled must not dispatch any member"
    );
}

#[tokio::test]
async fn a_panicking_member_keeps_the_siblings_on_both_sides() {
    let registry = registry_with(vec![
        FakeTool {
            name: "echo",
            effects: ToolEffects::read(),
            behavior: Behavior::EchoCallId,
        },
        FakeTool {
            name: "boom",
            effects: ToolEffects::read(),
            behavior: Behavior::Panic,
        },
    ])
    .await;
    let calls = vec![
        call("call-0", "echo", "a"),
        call("call-1", "boom", "b"),
        call("call-2", "echo", "c"),
    ];

    let outcome = run_group(registry, &calls, &base_context(), None).await;

    let indexes: Vec<usize> = outcome.results.iter().map(|(index, _)| *index).collect();
    assert_eq!(indexes, vec![0, 1, 2]);
    assert_eq!(outcome.results[0].1.as_ref().unwrap().output, "call-0");
    assert!(
        outcome.results[1].1.is_err(),
        "the panicking member must be that call's error"
    );
    assert_eq!(outcome.results[2].1.as_ref().unwrap().output, "call-2");
    assert_eq!(outcome.unstarted, Vec::<usize>::new());
}

/// The interval one timed tool actually ran in, recorded inside the tool.
#[derive(Debug, Clone, Copy)]
struct ToolWindow {
    name: &'static str,
    started: Instant,
    finished: Instant,
}

/// A `Concurrent` tool that sleeps for `delay` and records its own interval.
struct TimedTool {
    name: &'static str,
    delay: Duration,
    windows: Arc<Mutex<Vec<ToolWindow>>>,
}

#[async_trait::async_trait]
impl Tool for TimedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "timed tool_round test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let started = Instant::now();
        tokio::time::sleep(self.delay).await;
        let finished = Instant::now();
        self.windows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(ToolWindow {
                name: self.name,
                started,
                finished,
            });
        Ok(ToolOutput::new(self.name))
    }
}

async fn registry_of(tools: Vec<Arc<dyn Tool>>) -> Registry {
    let registry = Registry::empty();
    for tool in tools {
        registry.register(tool.name().to_string(), tool).await;
    }
    registry
}

#[tokio::test]
async fn concurrent_group_overlaps_in_time() {
    // Grouping alone proves nothing about wall-clock behavior: two members of
    // one group must actually run at the same time. The proof is the recorded
    // intervals themselves, not a stopwatch on the whole round.
    //
    // A round-level wall-clock bound is load-dependent: this test measured
    // 370.9ms for two 200ms members while four other worktrees were building,
    // because spawning the members is what the scheduler delays. Overlap of
    // the members' own intervals is invariant under that delay, and a
    // serialized group cannot produce any overlap at all, so that is what this
    // asserts. The round bound below is only a hang guard.
    let member_delay = Duration::from_millis(50);
    let windows: Arc<Mutex<Vec<ToolWindow>>> = Arc::new(Mutex::new(Vec::new()));
    let registry = registry_of(vec![
        Arc::new(TimedTool {
            name: "slow-a",
            delay: member_delay,
            windows: Arc::clone(&windows),
        }),
        Arc::new(TimedTool {
            name: "slow-b",
            delay: member_delay,
            windows: Arc::clone(&windows),
        }),
    ])
    .await;
    let calls = vec![call("call-a", "slow-a", "a"), call("call-b", "slow-b", "b")];

    let round_started = Instant::now();
    let outcome = run_group(registry, &calls, &base_context(), None).await;
    let round_elapsed = round_started.elapsed();
    assert!(
        round_elapsed < Duration::from_secs(10),
        "the round took {round_elapsed:?}"
    );

    assert_eq!(outcome.unstarted, Vec::<usize>::new());
    assert!(outcome.results.iter().all(|(_, result)| result.is_ok()));

    let recorded = windows.lock().unwrap_or_else(|error| error.into_inner()).clone();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    let a = recorded
        .iter()
        .find(|window| window.name == "slow-a")
        .expect("slow-a ran");
    let b = recorded
        .iter()
        .find(|window| window.name == "slow-b")
        .expect("slow-b ran");

    // (i) The two intervals overlap in time. This is the load-invariant form
    // of the claim: a serialized group leaves `b.started >= a.finished`, so the
    // overlap is exactly zero, while a concurrent group always leaves a
    // positive overlap however late the scheduler starts the second member.
    // A percentage floor would not be load-invariant: under load the second
    // spawn was delayed by 41ms of a 50ms member, leaving 11ms of overlap.
    assert!(
        a.started < b.finished && b.started < a.finished,
        "the two tool intervals did not overlap: a={a:?} b={b:?}"
    );
    let overlap_start = a.started.max(b.started);
    let overlap_end = a.finished.min(b.finished);
    let overlap = overlap_end.saturating_duration_since(overlap_start);
    assert!(
        overlap > Duration::ZERO,
        "the two members did not share any instant: a={a:?} b={b:?}"
    );
}

#[tokio::test]
async fn tool_results_keep_request_order() {
    // The result slice is indexed by the request, so it must stay in request
    // order even when completion order disagrees. Completion order is forced
    // with the gate rather than with two delays: a 10ms member does not
    // reliably beat a 50ms one on a loaded machine, and a test whose premise
    // depends on a race proves nothing when the race does not happen.
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![FakeTool {
        name: "gated",
        effects: ToolEffects::read(),
        behavior: Behavior::Gate(Arc::clone(&gate)),
    }])
    .await;
    let calls = vec![
        call("call-first", "gated", "first"),
        call("call-second", "gated", "second"),
    ];
    let ctx = base_context();

    let driver = async {
        // Both members are dispatched before either is released.
        let mut seen = vec![
            started.recv().await.expect("first member starts"),
            started.recv().await.expect("second member starts"),
        ];
        seen.sort();
        assert_eq!(seen, vec!["first", "second"]);

        // Release the second request first, so it finishes first: completion
        // order is now the exact reverse of request order.
        gate.release("second");
        assert_eq!(finished.recv().await.as_deref(), Some("second"));
        gate.release("first");
        assert_eq!(finished.recv().await.as_deref(), Some("first"));
    };

    let (outcome, ()) = tokio::join!(run_group(registry, &calls, &ctx, None), driver);

    let indexes: Vec<usize> = outcome.results.iter().map(|(index, _)| *index).collect();
    assert_eq!(indexes, vec![0, 1], "results must be indexed by the request");
    let outputs: Vec<String> = outcome
        .results
        .iter()
        .map(|(_, result)| result.as_ref().unwrap().output.clone())
        .collect();
    assert_eq!(
        outputs,
        vec!["call-first", "call-second"],
        "the second request finished first, so a completion-ordered slice would read [call-second, call-first]"
    );
    assert_eq!(outcome.unstarted, Vec::<usize>::new());
}

/// The 25 ms interrupt poll is the number the timer-vs-notification decision
/// needs, so it must actually count: a member held open for 150 ms has to
/// record several wakeups. A zero here would make the counter useless while
/// still looking green.
#[tokio::test]
async fn interrupt_poll_records_a_wakeup_per_interval_while_a_member_runs() {
    // The counters are process-wide, so reset-and-read only means anything
    // while no other test is measuring them.
    let _counters = crate::perf::lock_perf_counters().await;
    horon_agent_perf_counters::reset();
    let (gate, mut started, mut finished) = Gate::new();
    let registry = registry_with(vec![FakeTool {
        name: "gated",
        effects: ToolEffects::read(),
        behavior: Behavior::Gate(Arc::clone(&gate)),
    }])
    .await;
    let calls = vec![call("call-a", "gated", "a")];
    let ctx = base_context();

    let driver = async {
        started.recv().await.expect("member starts");
        tokio::time::sleep(Duration::from_millis(150)).await;
        gate.release("a");
        finished.recv().await.expect("member finishes");
    };

    let cancel = GroupCancel::new(InterruptSignal::new()).watching_interrupt(Arc::new(|| false));
    let (outcome, ()) = tokio::join!(run_group(registry, &calls, &ctx, Some(cancel)), driver);

    assert_eq!(outcome.results.len(), 1);
    assert!(
        outcome.stopped.is_none(),
        "no interrupt fired, so nothing stopped the group"
    );
    let wakeups = horon_agent_perf_counters::snapshot().interrupt_poll_wakeups;
    assert!(
        wakeups >= 3,
        "150 ms of a running member against a 25 ms interval must record several wakeups, got {wakeups}"
    );
}

/// Interrupt latency is the number the timer-vs-notification decision rests on:
/// the poll costs one wakeup per 25 ms (measured in the soak), so the only thing
/// a notification could buy is a shorter stop. This measures that stop directly by
/// flipping the watch at a random phase of the interval and timing the group's
/// return, which also yields the mean and worst case a 25 ms tick implies.
#[tokio::test]
async fn interrupt_poll_latency_is_bounded_by_the_interval() {
    // The counters are process-wide, so reset-and-read only means anything
    // while no other test is measuring them.
    let _counters = crate::perf::lock_perf_counters().await;
    horon_agent_perf_counters::reset();
    let mut latencies = Vec::new();
    let mut poll_wakeups_total = 0u64;
    for trial in 0..20u64 {
        let (gate, mut started, _finished) = Gate::new();
        let registry = registry_with(vec![FakeTool {
            name: "gated",
            effects: ToolEffects::read(),
            behavior: Behavior::Gate(Arc::clone(&gate)),
        }])
        .await;
        let calls = vec![call("call-a", "gated", "a")];
        let ctx = base_context();
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watch = Arc::clone(&fired);
        let cancel = GroupCancel::new(InterruptSignal::new())
            .watching_interrupt(Arc::new(move || watch.load(std::sync::atomic::Ordering::SeqCst)));

        // A member that never finishes on its own, so only the interrupt can end
        // the group and the return time is the poll's own latency.
        let driver = async {
            started.recv().await.expect("member starts");
            tokio::time::sleep(Duration::from_micros(1_000 + trial * 1_000)).await;
            fired.store(true, std::sync::atomic::Ordering::SeqCst);
        };
        let began = Instant::now();
        let (outcome, ()) = tokio::join!(run_group(registry, &calls, &ctx, Some(cancel)), driver);
        let elapsed = began.elapsed();
        assert_eq!(outcome.stopped, Some(super::tool_round::GroupStop::Interrupt));
        latencies.push(elapsed);
        poll_wakeups_total = horon_agent_perf_counters::snapshot().interrupt_poll_wakeups;
    }
    latencies.sort();
    let min = latencies.first().copied().unwrap_or_default();
    let max = latencies.last().copied().unwrap_or_default();
    let mean = latencies.iter().map(Duration::as_micros).sum::<u128>() / latencies.len() as u128;
    eprintln!(
        "t16_interrupt_latency={{trials:{},min_us:{},mean_us:{mean},max_us:{},poll_wakeups_total:{poll_wakeups_total},interval_ms:25}}",
        latencies.len(),
        min.as_micros(),
        max.as_micros()
    );
    assert!(
        max < Duration::from_millis(25) * 8,
        "a 25 ms poll must stop the group within a small multiple of its interval, worst was {max:?}"
    );
}

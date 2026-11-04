use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::Value;

use super::*;
use crate::agent::Agent;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::session::{Session, StoredReplayEventKind};
use crate::tool::{LazyToolActivator, Registry, Tool, ToolContext, ToolEffects, ToolExecutionMode, ToolOutput};

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("stub provider must never be called")
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

struct ProbeTool {
    name: &'static str,
    effects: ToolEffects,
    runs: Arc<AtomicUsize>,
}

impl ProbeTool {
    fn new(name: &'static str, effects: ToolEffects) -> Self {
        Self {
            name,
            effects,
            runs: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn executions(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.runs)
    }
}

#[async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "Probe tool that declares the effects its test needs."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type": "object"})
    }

    fn effects(&self) -> ToolEffects {
        self.effects
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new("probe ran"))
    }
}

struct IsolatedHome {
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("isolated agent home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", home.path());
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.previous {
            Some(home) => crate::env::set_var("HORON_AGENT_HOME", home),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn context(session_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "plan-mode-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

async fn registry_with_probes() -> Registry {
    let registry = Registry::empty();
    for (name, effects) in [
        ("probe_write", ToolEffects::write()),
        ("probe_read", ToolEffects::read()),
    ] {
        registry
            .register(name.to_string(), Arc::new(ProbeTool::new(name, effects)))
            .await;
    }
    registry
}

#[test]
fn planning_state_blocks_write_and_process() {
    let session = "plan-mode-blocks";
    clear_state(session);
    set_state(session, PlanState::Planning);

    for effects in [
        ToolEffects::write(),
        ToolEffects::append(),
        ToolEffects::process(),
        ToolEffects::read().union(ToolEffects::write()),
        ToolEffects::network().union(ToolEffects::process()),
    ] {
        assert!(!allows_effects(session, effects), "planning must refuse {effects:?}");
    }

    set_state(session, PlanState::PendingApproval);
    assert!(
        !allows_effects(session, ToolEffects::write()),
        "pending approval is still read-only"
    );
    clear_state(session);
}

#[test]
fn planning_state_allows_reads_and_network() {
    let session = "plan-mode-allows-reads";
    clear_state(session);

    for state in [PlanState::Planning, PlanState::PendingApproval] {
        set_state(session, state);
        assert!(
            allows_effects(session, ToolEffects::read()),
            "{state:?} must allow reads"
        );
        assert!(
            allows_effects(session, ToolEffects::network()),
            "{state:?} must allow network calls"
        );
        assert!(allows_effects(
            session,
            ToolEffects::read().union(ToolEffects::network())
        ));
    }
    clear_state(session);
}

#[test]
fn approved_state_allows_everything() {
    let session = "plan-mode-approved";
    clear_state(session);

    for state in [PlanState::Off, PlanState::Approved] {
        set_state(session, state);
        for effects in [
            ToolEffects::read(),
            ToolEffects::network(),
            ToolEffects::write(),
            ToolEffects::append(),
            ToolEffects::process(),
            ToolEffects::write().union(ToolEffects::process()),
        ] {
            assert!(allows_effects(session, effects), "{state:?} must allow {effects:?}");
        }
    }
    clear_state(session);
}

#[tokio::test]
async fn gate_applies_on_both_dispatch_paths() {
    let session = "plan-mode-both-paths";
    clear_state(session);

    let registry = registry_with_probes().await;
    // Building the agent reads config from the isolated home, so the environment
    // guard is scoped to construction and released before the calls below await.
    let agent = {
        let _env = crate::storage::lock_test_env();
        let _home = IsolatedHome::new();
        let allowed: HashSet<String> = ["probe_write", "probe_read"].iter().map(|n| n.to_string()).collect();
        Agent::new_with_session(
            Arc::new(StubProvider),
            registry.clone(),
            Session::create_with_id(session.to_string(), None, None),
            Some(allowed),
        )
    };

    set_state(session, PlanState::Planning);

    let refusal = registry
        .execute("probe_write", serde_json::json!({}), context(session))
        .await
        .expect_err("the registry path must refuse a write while planning");
    assert_eq!(refusal.to_string(), block_message("probe_write"));

    registry
        .execute("probe_read", serde_json::json!({}), context(session))
        .await
        .expect("the registry path must allow a read while planning");

    assert!(
        agent.validate_tool_allowed_for_test("probe_write").is_err(),
        "the agent path must refuse a write while planning"
    );
    agent
        .validate_tool_allowed_for_test("probe_read")
        .expect("the agent path must allow a read while planning");

    set_state(session, PlanState::Approved);

    agent
        .validate_tool_allowed_for_test("probe_write")
        .expect("an approved plan must reopen writes on the agent path");
    registry
        .execute("probe_write", serde_json::json!({}), context(session))
        .await
        .expect("an approved plan must reopen writes on the registry path");

    clear_state(session);
}

#[tokio::test]
async fn unclassifiable_name_is_a_barrier_while_planning() {
    let session = "plan-mode-unknown-name";
    clear_state(session);
    let registry = registry_with_probes().await;
    set_state(session, PlanState::Planning);

    let refusal = registry
        .execute("probe_absent", serde_json::json!({}), context(session))
        .await
        .expect_err("a name the registry cannot classify must be refused while planning");
    assert_eq!(
        refusal.to_string(),
        block_message("probe_absent"),
        "an unclassifiable name must hit the plan gate, not the unknown-tool error"
    );

    clear_state(session);
}

#[tokio::test]
async fn lazily_inactive_tool_is_a_barrier_while_planning() {
    let session = "plan-mode-lazy-inactive";
    clear_state(session);

    let registry = Registry::empty();
    let activations = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&activations);
    let activator: LazyToolActivator = Arc::new(move |_name: String| {
        let counter = Arc::clone(&counter);
        let answer: BoxFuture<'static, bool> = Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            true
        });
        answer
    });
    registry.register_lazy_tool_activator(activator);

    let tool = ProbeTool::new("probe_lazy_read", ToolEffects::read());
    let runs = tool.executions();
    registry
        .register_inactive("probe_lazy_read".to_string(), Arc::new(tool))
        .await;

    set_state(session, PlanState::Planning);

    let refusal = registry
        .execute("probe_lazy_read", serde_json::json!({}), context(session))
        .await
        .expect_err("an inactive tool declares nothing the gate can see, so planning must refuse it");
    assert_eq!(refusal.to_string(), block_message("probe_lazy_read"));
    assert_eq!(
        activations.load(Ordering::SeqCst),
        0,
        "the plan gate must refuse before the activator runs"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0, "the refused tool must never execute");

    // The agent path classifies the same name synchronously, so it must reach the
    // same verdict without the async map read the registry path uses.
    let agent = {
        let _env = crate::storage::lock_test_env();
        let _home = IsolatedHome::new();
        let allowed: HashSet<String> = ["probe_lazy_read"].iter().map(|n| n.to_string()).collect();
        Agent::new_with_session(
            Arc::new(StubProvider),
            registry.clone(),
            Session::create_with_id(session.to_string(), None, None),
            Some(allowed),
        )
    };
    let agent_refusal = agent
        .validate_tool_allowed_for_test("probe_lazy_read")
        .expect_err("the agent path must refuse an inactive tool while planning");
    assert_eq!(agent_refusal.to_string(), block_message("probe_lazy_read"));
    assert_eq!(
        activations.load(Ordering::SeqCst),
        0,
        "classifying an inactive tool must never activate it"
    );

    clear_state(session);
}

#[test]
fn plan_state_transitions_are_recorded_as_session_events() {
    let mut session = Session::create_with_id("plan-mode-events".to_string(), None, None);
    assert!(session.replay_events.is_empty());

    session.record_plan_mode_event(PlanState::Planning.as_str().to_string());
    session.record_plan_mode_event(PlanState::Planning.as_str().to_string());
    assert_eq!(
        session.replay_events.len(),
        1,
        "a repeated state must not append a second event"
    );
    assert_eq!(
        session.replay_events[0].kind,
        StoredReplayEventKind::PlanMode {
            mode: "planning".to_string()
        }
    );

    session.record_plan_mode_event(PlanState::Approved.as_str().to_string());
    assert_eq!(session.replay_events.len(), 2);
    assert_eq!(
        session.replay_events[1].kind,
        StoredReplayEventKind::PlanMode {
            mode: "approved".to_string()
        }
    );

    let json = serde_json::to_string(&session.replay_events).expect("replay events serialize");
    assert!(
        json.contains("\"event\":\"plan_mode\""),
        "the event must serialize under its own tag: {json}"
    );
    assert_eq!(
        PlanState::parse("pending_approval"),
        Some(PlanState::PendingApproval),
        "the recorded name must parse back"
    );
}

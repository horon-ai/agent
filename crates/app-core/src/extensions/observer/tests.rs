//! Contracts of package observation.
//!
//! Each test names one property the issue promises: a per-session sequence in
//! delivery order, one `session.start` per session even when a reconnect asks
//! again, tool metadata that cannot carry input, a bounded queue that drops and
//! counts instead of waiting, a revocation that refuses a stale observation, a
//! plan-mode rule that never starts a child just to observe, and a dispatcher
//! that publishes `tool.before`/`tool.after` only for an approved call.
//!
//! The two dispatcher cases drive `Registry::execute`, the entry point a model's
//! call takes, so the approval mode and the plan-mode refusal run where they
//! really run instead of through a helper. The package store is resolved once
//! per session and these tests point it at a throwaway home; the delivery worker
//! therefore resolves no package and starts no child.

#![allow(clippy::await_holding_lock)]

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use horon_agent_extension_types::ObserverEvent;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::extensions::lifecycle::{PackageLifecycleRegistry, RevocationCause};
use crate::plan_mode::{self, PlanState};
use crate::tool::{Registry, Tool, ToolContext, ToolExecutionMode, ToolOutput};

fn drain(receiver: &mut mpsc::Receiver<Notice>) -> Vec<ObserverParams> {
    let mut seen = Vec::new();
    while let Ok(notice) = receiver.try_recv() {
        seen.push(notice.params);
    }
    seen
}

/// Restores the process home when a dispatcher case ends.
struct RestoreHome(Option<OsString>);

impl RestoreHome {
    fn install(home: &Path) -> Self {
        let restore = Self(std::env::var_os("HORON_HOME"));
        crate::env::set_var("HORON_HOME", home);
        crate::config::invalidate_config_cache();
        restore
    }
}

impl Drop for RestoreHome {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echo the text it was given."
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new(input["text"].as_str().unwrap_or_default()))
    }
}

fn context(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_owned(),
        message_id: "observer-message".to_owned(),
        tool_call_id: format!("call-{session}"),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn sequence(state: &Arc<Mutex<SessionObserver>>) -> u64 {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).next_seq
}

/// Capture the package generation every turn entry point captures first.
///
/// The dispatcher only observes a tool call once the session has a package
/// generation, which is exactly the turn boundary this reproduces.
async fn enter_generation(session: &str) {
    let store = crate::packages::PackageStore::for_current_user().expect("a package store");
    let session_state = crate::extensions::package_snapshot::open_for(session, store);
    session_state.enter_turn(None).await.expect("a package generation");
}

#[test]
fn one_session_numbers_its_events_in_delivery_order() {
    let (mut state, mut receiver) = SessionObserver::new();
    state.push(ObserverEvent::TurnStart, None, |seq| json!({"seq": seq}));
    state.push(ObserverEvent::ToolBefore, None, |seq| {
        tool_before_metadata("read", "call-1", seq)
    });
    state.push(ObserverEvent::TurnEnd, None, |seq| json!({"seq": seq}));

    let seen = drain(&mut receiver);
    assert_eq!(
        seen.iter().map(|params| params.event).collect::<Vec<_>>(),
        vec![
            ObserverEvent::TurnStart,
            ObserverEvent::ToolBefore,
            ObserverEvent::TurnEnd
        ]
    );
    assert_eq!(
        seen.iter()
            .map(|params| params.metadata["seq"].clone())
            .collect::<Vec<_>>(),
        vec![json!(1), json!(2), json!(3)]
    );
}

#[test]
fn session_start_reaches_a_session_once_across_a_reconnect() {
    let (mut state, mut receiver) = SessionObserver::new();
    assert!(state.push(ObserverEvent::SessionStart, None, |seq| json!({"seq": seq})));
    // A reconnect builds a new agent for the same session id and fires the
    // event again; the state that outlives the agent refuses the second one.
    assert!(!state.push(ObserverEvent::SessionStart, None, |seq| json!({"seq": seq})));
    assert!(state.push(ObserverEvent::TurnStart, None, |seq| json!({"seq": seq})));

    let seen = drain(&mut receiver);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].event, ObserverEvent::SessionStart);
    assert_eq!(seen[0].metadata["seq"], json!(1));
    assert_eq!(seen[1].event, ObserverEvent::TurnStart);
    assert_eq!(
        seen[1].metadata["seq"],
        json!(2),
        "a refused start must not spend a sequence number"
    );
}

#[test]
fn tool_before_names_the_call_and_its_surface_without_any_input() {
    let before = tool_before_metadata("read", "call-9", 4);
    assert_eq!(
        before,
        json!({"seq": 4, "tool": "read", "call_id": "call-9", "origin": "builtin"})
    );
    assert_eq!(
        tool_before_metadata("ext__demo__greet", "call-1", 1)["origin"],
        json!("package")
    );
    assert_eq!(
        tool_before_metadata("mcp__corp__search", "call-2", 2)["origin"],
        json!("mcp")
    );

    let object = before.as_object().expect("the notice is an object");
    assert_eq!(
        object.len(),
        4,
        "a tool.before notice carries identity and surface only"
    );
}

#[test]
fn tool_after_reports_only_its_outcome_and_duration() {
    let after = tool_after_metadata(false, 1234, 7);
    assert_eq!(after, json!({"seq": 7, "outcome": "error", "duration_ms": 1234}));
    assert_eq!(
        after.as_object().expect("the notice is an object").len(),
        3,
        "a tool.after notice carries the outcome class and duration only"
    );
    assert_eq!(tool_after_metadata(true, 0, 8)["outcome"], json!("ok"));
}

#[test]
fn a_full_queue_drops_and_counts_without_waiting() {
    let (mut state, mut receiver) = SessionObserver::new();
    for _ in 0..MAX_OBSERVATIONS {
        assert!(state.push(ObserverEvent::TurnEnd, None, |seq| json!({"seq": seq})));
    }
    // Nobody drained the queue yet, so this is exactly the bound. A blocking
    // send would hang this test instead of returning.
    assert!(!state.push(ObserverEvent::TurnEnd, None, |seq| json!({"seq": seq})));
    assert_eq!(state.dropped, 1);
    assert_eq!(
        state.next_seq,
        u64::try_from(MAX_OBSERVATIONS).expect("the bound fits"),
        "a dropped event does not advance the sequence"
    );
    assert_eq!(drain(&mut receiver).len(), MAX_OBSERVATIONS);
}

#[test]
fn an_observation_after_a_revocation_is_refused() {
    let registry = PackageLifecycleRegistry::open();
    registry.revoke("demo", 7, RevocationCause::Disabled);

    assert!(
        crate::extensions::invoke::revoked_after(&registry, "demo", 6),
        "a target resolved before the revocation is stale"
    );
    assert!(
        !crate::extensions::invoke::revoked_after(&registry, "demo", 7),
        "a target resolved at the revoking generation is still allowed"
    );
    assert!(!crate::extensions::invoke::revoked_after(&registry, "other", 1));
}

#[test]
fn only_the_four_lifecycle_names_map_onto_observation_events() {
    use crate::hooks::observation_event;

    assert_eq!(observation_event("session_start"), Some(ObserverEvent::SessionStart));
    assert_eq!(observation_event("session_end"), Some(ObserverEvent::SessionEnd));
    assert_eq!(observation_event("turn_start"), Some(ObserverEvent::TurnStart));
    assert_eq!(observation_event("turn_end"), Some(ObserverEvent::TurnEnd));
    // A gate or a feedback hook is never an observation: only the dispatcher
    // publishes the two tool events, after approval.
    assert_eq!(observation_event("pre_tool"), None);
    assert_eq!(observation_event("post_tool"), None);
}

#[test]
fn planning_never_starts_a_child_for_an_observation() {
    let session = "observer-plan-gate";
    assert!(crate::extensions::invoke::observation_may_start_child(session));
    plan_mode::set_state(session, PlanState::Planning);
    assert!(!crate::extensions::invoke::observation_may_start_child(session));
    plan_mode::set_state(session, PlanState::PendingApproval);
    assert!(!crate::extensions::invoke::observation_may_start_child(session));
    plan_mode::set_state(session, PlanState::Approved);
    assert!(crate::extensions::invoke::observation_may_start_child(session));
    plan_mode::clear_state(session);
}

#[tokio::test]
async fn an_approved_call_publishes_before_then_after() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("a temporary home");
    std::fs::create_dir_all(home.path().join("packages")).expect("a package area");
    let _restore = RestoreHome::install(home.path());

    let session = "observer-approved-call";
    enter_generation(session).await;
    let state = state_for(session).expect("an observation state");
    let registry = Registry::empty();
    registry.register("echo".to_owned(), Arc::new(EchoTool)).await;

    let output = registry
        .execute("echo", json!({"text": "never-observed-input"}), context(session))
        .await
        .expect("an auto-approved call runs");
    assert_eq!(output.output, "never-observed-input");
    assert_eq!(
        sequence(&state),
        2,
        "an approved call publishes tool.before and tool.after"
    );

    forget_session(session);
    crate::extensions::package_snapshot::forget_session(session);
}

#[tokio::test]
async fn a_denied_call_publishes_nothing() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("a temporary home");
    std::fs::create_dir_all(home.path().join("packages")).expect("a package area");
    let _restore = RestoreHome::install(home.path());

    let session = "observer-denied-call";
    enter_generation(session).await;
    let state = state_for(session).expect("an observation state");
    plan_mode::set_state(session, PlanState::Planning);
    let registry = Registry::empty();
    registry.register("echo".to_owned(), Arc::new(EchoTool)).await;

    let outcome = registry
        .execute("echo", json!({"text": "blocked-input"}), context(session))
        .await;
    plan_mode::clear_state(session);

    assert!(outcome.is_err(), "plan mode refuses a write-effect call");
    assert_eq!(
        sequence(&state),
        0,
        "a policy-refused call is never observed, so its input cannot leak"
    );

    forget_session(session);
    crate::extensions::package_snapshot::forget_session(session);
}

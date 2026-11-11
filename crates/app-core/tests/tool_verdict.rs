//! A refusal before a tool runs is a typed error, and the text did not move.
//!
//! The turn loop turns a dispatch error into model-visible text with
//! `format!("Error: {}", e)`. Wrapping the refusal message in `ToolRejected`
//! must leave that text byte for byte identical, and must give a caller that
//! holds the error a way to tell "the tool never ran" from "the tool ran and
//! failed" without reading the sentence. Both properties are proven here
//! through the public dispatch surface, which is the surface later stages
//! (harness API, ACP, SDKs) will read the marker from.

use async_trait::async_trait;
use horon_agent_app_core::tool::{Registry, Tool, ToolContext, ToolExecutionMode, ToolOutput, ToolRejected};
use serde_json::{Value, json};
use std::sync::Arc;

/// A tool that runs and then fails, so the error is the tool's own failure
/// rather than a refusal of the dispatcher.
struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "fixture_failing"
    }

    fn description(&self) -> &str {
        "Runs, then fails"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        anyhow::bail!("fixture tool failure")
    }
}

fn context(session_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: format!("{session_id}-message"),
        tool_call_id: format!("{session_id}-call"),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

async fn registry() -> Registry {
    let registry = Registry::empty();
    registry
        .register("fixture_failing".to_string(), Arc::new(FailingTool))
        .await;
    registry
}

/// The refusal for a name that resolves to nothing is typed, and the message
/// the model reads is the same sentence it read before verdicts existed.
#[tokio::test]
async fn an_unknown_tool_is_rejected_with_the_same_text_as_before() {
    let registry = registry().await;
    let error = registry
        .execute("no_such_tool", json!({}), context("verdict-unknown"))
        .await
        .expect_err("an unknown tool must not run");

    let rejected = error
        .downcast_ref::<ToolRejected>()
        .expect("a name that resolves to nothing is a refusal, not a tool failure");
    assert_eq!(rejected.to_string(), error.to_string());
    assert!(
        rejected.0.starts_with("Unknown tool: no_such_tool."),
        "the sentence the model reads is unchanged, got: {}",
        rejected.0
    );
    assert!(
        rejected.0.contains("Available tools:"),
        "the recovery hint must survive, got: {}",
        rejected.0
    );
    // The turn loop builds the model's text from the error's Display.
    assert_eq!(
        format!("Error: {error}"),
        format!("Error: {}", rejected.0),
        "the text handed to the model must not change"
    );
}

/// A tool that ran and failed keeps an ordinary error: the marker means "never
/// ran", so attaching it here would tell the reader the opposite of the truth.
#[tokio::test]
async fn a_tool_that_ran_and_failed_is_not_a_refusal() {
    let registry = registry().await;
    let error = registry
        .execute("fixture_failing", json!({}), context("verdict-failure"))
        .await
        .expect_err("the fixture tool always fails");

    assert!(
        error.downcast_ref::<ToolRejected>().is_none(),
        "a failed call ran, so it must not carry the never-ran marker: {error}"
    );
    assert_eq!(error.to_string(), "fixture tool failure");
    assert_eq!(format!("Error: {error}"), "Error: fixture tool failure");
}

/// Plan mode refuses through the same marker: the call did not run, and the
/// message still says why.
#[tokio::test]
async fn a_plan_mode_refusal_is_rejected() {
    let session = "verdict-plan-mode";
    horon_agent_app_core::plan_mode::clear_state(session);
    horon_agent_app_core::plan_mode::set_state(session, horon_agent_app_core::plan_mode::PlanState::Planning);

    let registry = registry().await;
    let error = registry
        .execute("fixture_failing", json!({}), context(session))
        .await
        .expect_err("plan mode refuses a call that declares a write barrier");

    horon_agent_app_core::plan_mode::clear_state(session);

    let rejected = error
        .downcast_ref::<ToolRejected>()
        .expect("a plan-mode refusal never ran the tool");
    assert_eq!(rejected.to_string(), error.to_string());
    assert!(
        rejected.0.contains("plan mode"),
        "the refusal must still name plan mode, got: {}",
        rejected.0
    );
}

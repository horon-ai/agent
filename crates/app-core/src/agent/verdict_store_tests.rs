//! What the turn loop stores when a call ends in each way it can end.
//!
//! The verdict is stored so a later reader never has to re-derive `ok` or
//! `failed` from the text. These tests drive one real round through each loop
//! and read the stored blocks back: the successful call's own verdict travels,
//! a refusal is `rejected` because it never ran, and an error from a tool that
//! ran is `failed`.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::tool::{ConcurrencyClass, Tool, ToolEffects, ToolOutput, ToolRejected, ToolVerdict};

use super::*;

/// What the double does for one call, selected by the call's input key.
enum Outcome {
    /// The tool ran and reported this verdict itself.
    Reports(ToolVerdict),
    /// Refused before it ran, the way the registry refuses a call.
    Reject,
    /// Ran and failed.
    Fail,
}

/// One `read` double whose answer depends on the requested key, so a single
/// allowed tool name can produce every outcome in one round.
struct VerdictTool;

#[async_trait::async_trait]
impl Tool for VerdictTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "verdict store test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        ConcurrencyClass::Concurrent
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    async fn execute(&self, input: Value, _: ToolContext) -> Result<ToolOutput> {
        let outcome = match input["key"].as_str().unwrap_or_default() {
            "partial" => Outcome::Reports(ToolVerdict::Partial),
            "reject" => Outcome::Reject,
            "fail" => Outcome::Fail,
            _ => Outcome::Reports(ToolVerdict::Ok),
        };
        match outcome {
            Outcome::Reports(verdict) => Ok(ToolOutput::new("payload").with_verdict(verdict)),
            Outcome::Reject => Err(ToolRejected("denied by policy".to_string()).into()),
            Outcome::Fail => Err(anyhow::anyhow!("the tool failed")),
        }
    }
}

async fn registry_with_verdict_tool() -> Registry {
    let registry = Registry::empty();
    registry
        .register("read".to_string(), Arc::new(VerdictTool) as Arc<dyn Tool>)
        .await;
    registry
}

fn one_round() -> Vec<StreamEvent> {
    let mut events = Vec::new();
    for (id, key) in [
        ("call-ok", "ok"),
        ("call-partial", "partial"),
        ("call-rejected", "reject"),
        ("call-failed", "fail"),
    ] {
        events.extend(crate::agent::tests::shared_tool_use(id, "read", &json!({ "key": key })));
    }
    events.push(crate::agent::tests::shared_round_end());
    events
}

/// Stored `(tool_use_id, verdict, is_error)` in transcript order.
fn stored_verdicts(agent: &Agent) -> Vec<(String, Option<ToolVerdict>, Option<bool>)> {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                verdict,
                ..
            } => Some((tool_use_id.clone(), *verdict, *is_error)),
            _ => None,
        })
        .collect()
}

fn assert_every_outcome_is_stored(agent: &Agent) {
    let stored = stored_verdicts(agent);
    let expected = vec![
        ("call-ok".to_string(), Some(ToolVerdict::Ok), None),
        ("call-partial".to_string(), Some(ToolVerdict::Partial), None),
        ("call-rejected".to_string(), Some(ToolVerdict::Rejected), Some(true)),
        ("call-failed".to_string(), Some(ToolVerdict::Failed), Some(true)),
    ];
    assert_eq!(stored, expected, "stored verdicts differ: {stored:?}");
}

#[tokio::test]
async fn a_round_stores_the_verdict_of_every_outcome_it_can_reach() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let registry = registry_with_verdict_tool().await;
    let (provider, _prompts) = crate::agent::tests::shared_round_provider(vec![one_round()]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "run four calls".into(),
            cache_control: None,
        }],
    );

    agent.run_turn(false).await.unwrap();

    assert_every_outcome_is_stored(&agent);
}

#[tokio::test]
async fn the_streaming_round_stores_the_same_verdicts() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let registry = registry_with_verdict_tool().await;
    let (provider, _prompts) = crate::agent::tests::shared_round_provider(vec![one_round()]);
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "run four calls".into(),
            cache_control: None,
        }],
    );

    let (out, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    assert_every_outcome_is_stored(&agent);
}

/// A History row for a tool result reports the verdict stored on that result,
/// to every connection, and a result stored without one keeps the row's bytes.
#[test]
fn a_history_tool_row_reports_the_stored_verdict() {
    use crate::server::foreground_state::history_message;

    let result = |call: &str, verdict: Option<ToolVerdict>| crate::session::StoredMessage {
        id: format!("{call}-result"),
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: call.to_string(),
            content: "text".to_string(),
            is_error: None,
            verdict,
        }],
        display_role: None,
        turn_start: false,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
        original_input: None,
        arg_repairs: None,
        degradations: None,
    };
    let stored = [
        result("call-failed", Some(ToolVerdict::Failed)),
        result("call-plain", None),
    ];
    let row = |index: usize, call: &str, include_message_ids: bool| {
        let rendered = crate::session::RenderedMessage {
            response_stats: None,
            role: "tool".to_string(),
            content: "text".to_string(),
            tool_calls: Vec::new(),
            tool_data: Some(crate::message::ToolCall {
                id: call.to_string(),
                name: "read".to_string(),
                ..Default::default()
            }),
            stored_index: Some(index),
        };
        history_message(rendered, &stored, include_message_ids)
    };

    for include_message_ids in [false, true] {
        let failed = row(0, "call-failed", include_message_ids);
        assert_eq!(failed.verdict, Some(ToolVerdict::Failed));
        let wire = serde_json::to_value(&failed).unwrap();
        assert_eq!(wire["verdict"], "failed", "{wire}");

        let plain = serde_json::to_value(row(1, "call-plain", include_message_ids)).unwrap();
        assert!(plain.get("verdict").is_none(), "no stored verdict, no key: {plain}");
    }
    // A row is matched to its own result, not to whatever the message holds.
    assert_eq!(row(0, "call-other", false).verdict, None);
}

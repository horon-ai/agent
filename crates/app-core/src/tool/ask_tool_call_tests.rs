//! The `ask` tool hands its session and tool call id to the forwarding
//! connection, which is the only place that knows whether the editor speaks
//! protocol v2 and can name the call the question belongs to.
//
// The bridge guard is deliberately held across awaits: it serializes the
// process-wide ask bridge for the whole test, and dropping it early would let
// another test replace the installed answerer mid-run.
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::tool::ToolExecutionMode;
use horon_agent_tool_types::AskAnswer;
use std::time::Duration;

/// The ask bridge is process-global, so the tests that install it take the
/// crate's shared environment lock for their whole run.
fn bridge_guard() -> std::sync::MutexGuard<'static, ()> {
    crate::storage::lock_test_env()
}

fn one_question() -> Value {
    json!({
        "questions": [{
            "id": "scope",
            "question": "Which surface should change?",
            "options": [{"label": "daemon only"}, {"label": "daemon and desktop"}],
            "recommended": 0
        }]
    })
}

fn context(session: &str, tool_call_id: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "message".to_string(),
        tool_call_id: tool_call_id.to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

#[tokio::test]
async fn the_prompt_carries_the_session_and_the_calling_tool_call() {
    let _bridge = bridge_guard();
    ask_bridge::clear_all();
    let session = "ask-tool-call-ids";
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AskPrompt>();
    let token = ask_bridge::install(session, tx);

    let ask = AskTool::new().with_wait(Duration::from_secs(30));
    let answer = tokio::spawn(async move { ask.execute(one_question(), context(session, "call-q")).await });

    let prompt = rx.recv().await.expect("the tool asks its client");
    assert_eq!(prompt.tool_call_id, "call-q");
    assert_eq!(prompt.session_id, session);
    assert_eq!(prompt.questions.len(), 1);

    prompt
        .response_tx
        .send(vec![AskAnswer {
            question_id: "scope".to_string(),
            question: String::new(),
            selected: vec!["daemon only".to_string()],
            auto_resolved: false,
        }])
        .expect("the tool is still waiting for the answer");
    answer
        .await
        .expect("the waiting task finishes")
        .expect("an answered question is a successful call");

    ask_bridge::clear(session, token);
}

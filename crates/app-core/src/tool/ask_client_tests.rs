#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::{AskTool, Tool, ToolContext};
use crate::config::AskPolicy;
use crate::tool::ToolExecutionMode;
use crate::tool::ask_bridge::{self, AskPrompt};
use horon_agent_tool_types::AskAnswer;
use serde_json::{Value, json};
use std::time::Duration;

/// The ask bridge is process-global: `ask_bridge::clear_all` empties the whole
/// registry, including the install a sibling test made a moment earlier, so two
/// of these tests running side by side leave one of them waiting on a channel
/// nobody holds, which panics on "the tool asks". Each test takes the crate's
/// shared test lock instead, the same lock the receipt-ledger tests that share
/// these globals already hold, so the bridge is only ever touched by one test
/// at a time.
fn bridge_guard() -> std::sync::MutexGuard<'static, ()> {
    crate::storage::lock_test_env()
}

fn context(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn two_questions() -> Value {
    json!({
        "questions": [
            {
                "id": "scope",
                "question": "Which surface should change?",
                "options": [
                    {"label": "daemon only"},
                    {"label": "daemon and desktop"},
                    {"label": "every client"}
                ],
                "recommended": 1
            },
            {
                "id": "style",
                "question": "How strict should validation be?",
                "options": [
                    {"label": "lenient"},
                    {"label": "strict"}
                ]
            }
        ]
    })
}

fn answer(question_id: &str, labels: &[&str]) -> AskAnswer {
    AskAnswer {
        question_id: question_id.to_string(),
        question: String::new(),
        selected: labels.iter().map(|label| label.to_string()).collect(),
        auto_resolved: false,
    }
}

#[tokio::test]
async fn without_an_ask_client_the_recommended_policy_is_unchanged() {
    let _bridge = bridge_guard();
    let session = "ask-client-none";
    ask_bridge::clear_all();
    assert!(
        ask_bridge::sender_for(session).is_none(),
        "the fixture must start with no answerer"
    );

    let output = AskTool::with_policy(AskPolicy::Recommended)
        .execute(two_questions(), context(session))
        .await
        .expect("recommended policy resolves");

    let metadata = output.metadata.expect("answers travel in metadata");
    assert_eq!(metadata["policy"], json!("recommended"));
    let answers = metadata["answers"].as_array().expect("answers array");
    assert_eq!(answers[0]["selected"], json!(["daemon and desktop"]));
    assert_eq!(answers[1]["selected"], json!(["lenient"]));
    assert_eq!(answers[0]["auto_resolved"], json!(true));
    assert!(
        output.output.contains("no interactive client is attached"),
        "the text must still explain the automatic choice, got {:?}",
        output.output
    );
}

#[tokio::test]
async fn an_attached_ask_client_answers_and_its_label_reaches_the_result() {
    let _bridge = bridge_guard();
    let session = "ask-client-answers";
    ask_bridge::clear_all();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AskPrompt>();
    let token = ask_bridge::install(session, tx);

    let client = tokio::spawn(async move {
        let prompt = rx.recv().await.expect("the tool asks");
        assert_eq!(prompt.questions.len(), 2);
        assert_eq!(prompt.questions[0].id.as_deref(), Some("scope"));
        prompt
            .response_tx
            .send(vec![answer("scope", &["every client"]), answer("style", &["strict"])])
            .expect("the tool is still waiting");
        prompt.ask_id
    });

    let output = AskTool::with_policy(AskPolicy::Recommended)
        .execute(two_questions(), context(session))
        .await
        .expect("the client answer resolves the tool");

    let ask_id = client.await.expect("client task");
    assert!(ask_id.starts_with("ask"), "the emitted id is the ask id: {ask_id}");

    let metadata = output.metadata.expect("answers travel in metadata");
    let answers = metadata["answers"].as_array().expect("answers array");
    assert_eq!(answers[0]["selected"], json!(["every client"]));
    assert_eq!(answers[1]["selected"], json!(["strict"]));
    assert_eq!(
        answers[0]["auto_resolved"],
        json!(false),
        "a human choice is not an automatic one"
    );
    assert!(output.output.contains("every client"), "{}", output.output);
    assert!(output.output.contains("selected by the person"), "{}", output.output);
    assert!(
        !output.output.contains("automatically"),
        "a human answer must not be described as automatic: {}",
        output.output
    );

    ask_bridge::clear(session, token);
    ask_bridge::clear_all();
}

#[tokio::test]
async fn a_client_that_never_answers_times_out_to_the_recommendation() {
    let _bridge = bridge_guard();
    let session = "ask-client-timeout";
    ask_bridge::clear_all();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AskPrompt>();
    let token = ask_bridge::install(session, tx);
    let asked = tokio::spawn(async move { rx.recv().await.expect("the tool asks") });

    let output = AskTool::with_policy(AskPolicy::Recommended)
        .with_wait(Duration::from_millis(50))
        .execute(two_questions(), context(session))
        .await
        .expect("a timeout resolves rather than failing");

    let prompt = asked.await.expect("ask task");
    assert_eq!(prompt.questions.len(), 2);

    let metadata = output.metadata.expect("answers travel in metadata");
    let answers = metadata["answers"].as_array().expect("answers array");
    assert_eq!(answers[0]["selected"], json!(["daemon and desktop"]));
    assert_eq!(answers[0]["auto_resolved"], json!(true));
    assert!(
        output.output.contains("did not answer within"),
        "the text must say the wait elapsed: {}",
        output.output
    );
    assert!(
        output.output.contains("selected automatically rather than by a person"),
        "the text must say the choice was automatic: {}",
        output.output
    );

    ask_bridge::clear(session, token);
    ask_bridge::clear_all();
}

#[tokio::test]
async fn a_disconnecting_client_resolves_the_turn_instead_of_hanging() {
    let _bridge = bridge_guard();
    let session = "ask-client-disconnect";
    ask_bridge::clear_all();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AskPrompt>();
    let token = ask_bridge::install(session, tx);
    let asked = tokio::spawn(async move {
        let prompt = rx.recv().await.expect("the tool asks");
        drop(prompt);
    });

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        AskTool::with_policy(AskPolicy::Recommended)
            .with_wait(Duration::from_secs(300))
            .execute(two_questions(), context(session)),
    )
    .await
    .expect("a dropped answer channel must end the wait, not park the turn")
    .expect("the turn continues on the recommendation");

    asked.await.expect("ask task");
    assert!(
        output.output.contains("disconnected before replying"),
        "the text must name the disconnect: {}",
        output.output
    );
    assert!(output.output.contains("daemon and desktop"), "{}", output.output);

    ask_bridge::clear(session, token);
    ask_bridge::clear_all();
}

#[tokio::test]
async fn a_label_the_question_never_offered_is_not_used() {
    let _bridge = bridge_guard();
    let session = "ask-client-unusable";
    ask_bridge::clear_all();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AskPrompt>();
    let token = ask_bridge::install(session, tx);
    let client = tokio::spawn(async move {
        let prompt = rx.recv().await.expect("the tool asks");
        prompt
            .response_tx
            .send(vec![answer("scope", &["a label nobody offered"])])
            .expect("the tool is still waiting");
    });

    let output = AskTool::with_policy(AskPolicy::Recommended)
        .execute(two_questions(), context(session))
        .await
        .expect("an unusable answer falls back to the recommendation");

    client.await.expect("client task");
    let metadata = output.metadata.expect("answers travel in metadata");
    let answers = metadata["answers"].as_array().expect("answers array");
    assert_eq!(
        answers[0]["selected"],
        json!(["daemon and desktop"]),
        "an option the question never offered must not reach the model"
    );
    assert_eq!(answers[0]["auto_resolved"], json!(true));
    assert!(!output.output.contains("a label nobody offered"), "{}", output.output);

    ask_bridge::clear(session, token);
    ask_bridge::clear_all();
}

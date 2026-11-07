//! `AskToolCall` names the calling tool call on a v2 connection and must reach
//! that connection immediately before the question it belongs to. A v1
//! connection keeps the single `AskRequested` event it always had.
//
// The test sandbox is deliberately held across awaits: it serializes the
// process environment and the ACP/ask registries for the whole test, and
// dropping it early would let another test rewrite them mid-run.
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::tool::ask_bridge::AskPrompt;
use horon_agent_tool_types::{AskOption, AskQuestion};

fn question(id: &str) -> AskQuestion {
    AskQuestion {
        id: Some(id.to_string()),
        question: format!("question {id}"),
        header: None,
        options: vec![
            AskOption {
                label: "first".to_string(),
                description: None,
            },
            AskOption {
                label: "second".to_string(),
                description: None,
            },
        ],
        recommended: Some(1),
        multi: false,
    }
}

fn prompt(
    ask_id: &str,
    session: &str,
    tool_call_id: &str,
) -> (AskPrompt, tokio::sync::oneshot::Receiver<Vec<AskAnswer>>) {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    (
        AskPrompt {
            ask_id: ask_id.to_string(),
            questions: vec![question("scope")],
            response_tx,
            session_id: session.to_string(),
            tool_call_id: tool_call_id.to_string(),
        },
        response_rx,
    )
}

#[tokio::test]
async fn the_tool_call_id_reaches_a_v2_connection_before_the_question() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let session = "client-ask-tool-call-v2";
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let token = crate::tool::acp_session::install_with_version(session, "conn-ask", tx.clone(), Some(2));
    let (pending, ask_tx, forwarder) = spawn_ask_forwarder(tx.clone());

    let (ask, answer) = prompt("ask-1", session, "call-q");
    ask_tx.send(ask).expect("the forwarder accepts the prompt");

    match rx.recv().await.expect("the connection receives the call id") {
        ServerEvent::AskToolCall {
            request_id,
            tool_call_id,
        } => {
            assert_eq!(request_id, "ask-1");
            assert_eq!(tool_call_id, "call-q");
        }
        other => panic!("expected the call id first, got {other:?}"),
    }
    match rx.recv().await.expect("the connection receives the question") {
        ServerEvent::AskRequested { request_id, questions } => {
            assert_eq!(request_id, "ask-1", "both events name the same request");
            assert_eq!(questions.len(), 1);
        }
        other => panic!("expected the question after the call id, got {other:?}"),
    }

    handle_ask_response(
        7,
        "ask-1".to_string(),
        vec![AskSelection {
            question_id: "scope".to_string(),
            selected: vec!["second".to_string()],
        }],
        &pending,
        &tx,
    )
    .await;
    assert_eq!(
        answer.await.expect("the waiting tool receives the answer").len(),
        1,
        "the added call id must not disturb the answer path"
    );

    forwarder.abort();
    crate::tool::acp_session::clear(session, token);
}

#[tokio::test]
async fn a_v1_connection_receives_only_the_question() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let session = "client-ask-tool-call-v1";
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let token = crate::tool::acp_session::install_with_version(session, "conn-ask", tx.clone(), None);
    let (pending, ask_tx, forwarder) = spawn_ask_forwarder(tx.clone());

    let (ask, _answer) = prompt("ask-2", session, "call-v1");
    ask_tx.send(ask).expect("the forwarder accepts the prompt");

    match rx.recv().await.expect("the connection receives the question") {
        ServerEvent::AskRequested { request_id, .. } => assert_eq!(request_id, "ask-2"),
        other => panic!("a v1 connection must not receive the call id, got {other:?}"),
    }
    assert!(
        rx.try_recv().is_err(),
        "a v1 connection receives exactly one event per question"
    );

    drop(pending);
    forwarder.abort();
    crate::tool::acp_session::clear(session, token);
}

#[tokio::test]
async fn a_connection_that_never_negotiated_v2_receives_only_the_question() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    // No ACP installation at all: the forwarder must still deliver the
    // question and stay silent about a call id nobody asked for.
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let (pending, ask_tx, forwarder) = spawn_ask_forwarder(tx.clone());

    let (ask, _answer) = prompt("ask-3", "session-without-acp", "call-plain");
    ask_tx.send(ask).expect("the forwarder accepts the prompt");

    match rx.recv().await.expect("the connection receives the question") {
        ServerEvent::AskRequested { request_id, .. } => assert_eq!(request_id, "ask-3"),
        other => panic!("expected the question, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "nothing else is emitted");

    drop(pending);
    forwarder.abort();
}

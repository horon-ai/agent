// The env lock is deliberately held across awaits: it serializes the process
// environment for the whole test, and dropping it early would let another test
// rewrite `HORON_HOME` mid-run.
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::protocol::ServerEvent;
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

fn selection(question_id: &str, label: &str) -> AskSelection {
    AskSelection {
        question_id: question_id.to_string(),
        selected: vec![label.to_string()],
    }
}

#[tokio::test]
async fn an_ask_answer_reaches_the_waiting_tool_and_is_acked() {
    let pending: PendingAsks = Arc::new(Mutex::new(HashMap::new()));
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    pending.lock().await.insert(
        "ask-live".to_string(),
        PendingAsk {
            questions: vec![question("scope")],
            response_tx,
        },
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    handle_ask_response(
        7,
        "ask-live".to_string(),
        vec![selection("scope", "second")],
        &pending,
        &tx,
    )
    .await;

    let answers = response_rx.await.expect("the tool receives the answer");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].question_id, "scope");
    assert_eq!(answers[0].selected, vec!["second".to_string()]);
    assert!(!answers[0].auto_resolved);
    assert!(matches!(rx.recv().await, Some(ServerEvent::Ack { id: 7 })));
    assert!(pending.lock().await.is_empty(), "an answered ask is retired");
}

#[tokio::test]
async fn a_late_ask_answer_is_refused_and_changes_nothing() {
    let pending: PendingAsks = Arc::new(Mutex::new(HashMap::new()));
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
    pending.lock().await.insert(
        "ask-newer".to_string(),
        PendingAsk {
            questions: vec![question("scope")],
            response_tx,
        },
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    handle_ask_response(
        9,
        "ask-expired".to_string(),
        vec![selection("scope", "first")],
        &pending,
        &tx,
    )
    .await;

    match rx.recv().await {
        Some(ServerEvent::Error { id, message, .. }) => {
            assert_eq!(id, 9);
            assert!(
                message.contains("no longer pending"),
                "the refusal must say why: {message}"
            );
        }
        other => panic!("a stale answer is an error, got {other:?}"),
    }
    assert!(
        response_rx.try_recv().is_err(),
        "the newer question must still be waiting for its own answer"
    );
    assert_eq!(
        pending.lock().await.len(),
        1,
        "the newer pending ask is untouched by the late reply"
    );
}

#[tokio::test]
async fn an_answer_after_the_tool_stopped_waiting_is_refused_not_acked() {
    let pending: PendingAsks = Arc::new(Mutex::new(HashMap::new()));
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    drop(response_rx);
    pending.lock().await.insert(
        "ask-expired".to_string(),
        PendingAsk {
            questions: vec![question("scope")],
            response_tx,
        },
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    handle_ask_response(
        10,
        "ask-expired".to_string(),
        vec![selection("scope", "second")],
        &pending,
        &tx,
    )
    .await;

    match rx.recv().await {
        Some(ServerEvent::Error { id, message, .. }) => {
            assert_eq!(id, 10);
            assert!(message.contains("stopped waiting"), "{message}");
        }
        other => panic!("an answer nobody waits for must be refused, got {other:?}"),
    }
    assert!(pending.lock().await.is_empty(), "the expired ask is retired");
}

#[tokio::test]
async fn a_permission_decision_is_recorded_where_the_waiting_tool_reads_it() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp home");
    let previous = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", dir.path());

    for (decision, approved) in [
        (PermissionDecision::Allow, true),
        (PermissionDecision::AllowAlways, true),
        (PermissionDecision::Deny, false),
    ] {
        let request_id = format!("req-{decision:?}");
        let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
        handle_permission_decision(11, request_id.clone(), decision, &tx, None);
        // The handler answers synchronously, so the acknowledgement is already
        // queued: reading it without an await keeps the env guard out of an
        // await point.
        assert!(
            matches!(rx.try_recv(), Ok(ServerEvent::Ack { id: 11 })),
            "{decision:?} is acknowledged"
        );
        let recorded = crate::safety::SafetySystem::new()
            .decision_for(&request_id)
            .unwrap_or_else(|| panic!("{decision:?} must be recorded"));
        assert_eq!(recorded.approved, approved, "{decision:?}");
        assert_eq!(recorded.decided_via, "api_client");
    }

    match previous {
        Some(value) => crate::env::set_var("HORON_HOME", value),
        None => crate::env::remove_var("HORON_HOME"),
    }
}

#[tokio::test]
async fn an_empty_permission_request_id_is_refused() {
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    handle_permission_decision(12, String::new(), PermissionDecision::Allow, &tx, None);
    match rx.recv().await {
        Some(ServerEvent::Error { id, message, .. }) => {
            assert_eq!(id, 12);
            assert!(message.contains("request_id"), "{message}");
        }
        other => panic!("an empty id is an error, got {other:?}"),
    }
}

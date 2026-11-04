//! Answer-receipt tests through the client answer path, where the pending check,
//! the digest comparison, and the claim have to agree with each other.

use super::*;
use crate::tool::ask_bridge::{AnswerDecision, ask_receipt_ledger_for_test};
use horon_agent_tool_types::{AskOption, AskQuestion};

struct IsolatedHome {
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("isolated agent home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", dir.path());
        Self {
            _lock: lock,
            dir,
            previous,
        }
    }

    fn journal_root(&self) -> std::path::PathBuf {
        self.dir.path().join(crate::durable_runtime::JOURNAL_DIR_NAME)
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
    }
}

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

async fn pending_with(ask_id: &str) -> (PendingAsks, tokio::sync::oneshot::Receiver<Vec<AskAnswer>>) {
    let pending: PendingAsks = Arc::new(Mutex::new(HashMap::new()));
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    pending.lock().await.insert(
        ask_id.to_string(),
        PendingAsk {
            questions: vec![question("scope")],
            response_tx,
        },
    );
    (pending, response_rx)
}

#[tokio::test]
async fn ask_answer_receipts_deliver_once_and_replay_the_retry() {
    let _home = IsolatedHome::new();
    let session = "ask_receipts_client_replay";
    ask_receipt_ledger_for_test(session);
    let (pending, response_rx) = pending_with("ask-live").await;
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let answers = vec![selection("scope", "second")];

    answer_ask(
        7,
        "ask-live".to_string(),
        answers.clone(),
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-1",
        }),
        &pending,
        &tx,
    )
    .await;

    let delivered = response_rx.await.expect("the tool receives the answer");
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].selected, vec!["second".to_string()]);
    assert!(matches!(rx.recv().await, Some(ServerEvent::Ack { id: 7 })));
    assert!(pending.lock().await.is_empty());

    answer_ask(
        8,
        "ask-live".to_string(),
        answers,
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-1",
        }),
        &pending,
        &tx,
    )
    .await;

    assert!(
        matches!(rx.recv().await, Some(ServerEvent::Ack { id: 8 })),
        "a retry of the same answer is acknowledged, not refused"
    );
    assert!(
        pending.lock().await.is_empty(),
        "the retry must not re-register the question"
    );
}

#[tokio::test]
async fn ask_answer_receipts_conflict_after_the_answer_is_recorded() {
    let _home = IsolatedHome::new();
    let session = "ask_receipts_client_conflict";
    ask_receipt_ledger_for_test(session);
    let (pending, response_rx) = pending_with("ask-live").await;
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    answer_ask(
        11,
        "ask-live".to_string(),
        vec![selection("scope", "second")],
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-1",
        }),
        &pending,
        &tx,
    )
    .await;
    response_rx.await.expect("first answer delivered");
    assert!(matches!(rx.recv().await, Some(ServerEvent::Ack { id: 11 })));

    answer_ask(
        12,
        "ask-live".to_string(),
        vec![selection("scope", "first")],
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-2",
        }),
        &pending,
        &tx,
    )
    .await;
    match rx.recv().await {
        Some(ServerEvent::Error { id, message, .. }) => {
            assert_eq!(id, 12);
            assert!(
                message.contains("already answered"),
                "a second answer is refused as a conflict: {message}"
            );
        }
        other => panic!("a conflicting answer is an error, got {other:?}"),
    }
}

#[tokio::test]
async fn ask_answer_receipts_unknown_question_is_refused_without_a_record() {
    let _home = IsolatedHome::new();
    let session = "ask_receipts_client_unknown";
    let ledger = ask_receipt_ledger_for_test(session);
    let pending: PendingAsks = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    answer_ask(
        13,
        "ask-never-asked".to_string(),
        vec![selection("scope", "first")],
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-1",
        }),
        &pending,
        &tx,
    )
    .await;

    match rx.recv().await {
        Some(ServerEvent::Error { id, message, .. }) => {
            assert_eq!(id, 13);
            assert!(message.contains("no longer pending"), "{message}");
        }
        other => panic!("an unknown question is refused, got {other:?}"),
    }
    let digest = ask_bridge::answer_digest("ask-never-asked", "answer-1", &[selection("scope", "first")]);
    assert!(
        matches!(
            ledger.begin("ask-never-asked", "answer-1", &digest, false).await,
            AnswerDecision::Unknown
        ),
        "a refused answer leaves no claim behind"
    );
}

#[tokio::test]
async fn ask_answer_receipts_timeout_racing_an_answer_does_not_claim_the_question() {
    let _home = IsolatedHome::new();
    let session = "ask_receipts_client_timeout";
    let ledger = ask_receipt_ledger_for_test(session);
    let pending: PendingAsks = Arc::new(Mutex::new(HashMap::new()));
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    drop(response_rx);
    pending.lock().await.insert(
        "ask-timed-out".to_string(),
        PendingAsk {
            questions: vec![question("scope")],
            response_tx,
        },
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let answers = vec![selection("scope", "second")];

    answer_ask(
        14,
        "ask-timed-out".to_string(),
        answers.clone(),
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-1",
        }),
        &pending,
        &tx,
    )
    .await;

    match rx.recv().await {
        Some(ServerEvent::Error { id, message, .. }) => {
            assert_eq!(id, 14);
            assert!(message.contains("stopped waiting"), "{message}");
        }
        other => panic!("an answer to a timed-out question is refused, got {other:?}"),
    }

    // The question resolved itself on timeout, so no answer receipt may claim it.
    // Nothing was recorded, which is what keeps a retry deterministic instead of
    // turning into an indeterminate claim.
    let digest = ask_bridge::answer_digest("ask-timed-out", "answer-1", &answers);
    assert!(
        matches!(
            ledger.begin("ask-timed-out", "answer-1", &digest, false).await,
            AnswerDecision::Unknown
        ),
        "a refused answer leaves no receipt behind"
    );
    assert!(
        pending.lock().await.is_empty(),
        "the timed-out question is retired by the refused answer"
    );
}

#[tokio::test]
async fn ask_answer_receipts_claim_then_kill_is_indeterminate_and_not_redelivered() {
    let _home = IsolatedHome::new();
    let session = "ask_receipts_client_kill";
    let ledger = ask_receipt_ledger_for_test(session);
    let answers = vec![selection("scope", "second")];
    let digest = ask_bridge::answer_digest("ask-killed", "answer-1", &answers);

    assert!(matches!(
        ledger.begin("ask-killed", "answer-1", &digest, true).await,
        AnswerDecision::Claimed
    ));

    // The daemon dies right after the claim and before the terminal row. A fresh
    // process must not hand the answer to the tool again.
    ask_receipt_ledger_for_test(session);
    let (pending, mut response_rx) = pending_with("ask-killed").await;
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    answer_ask(
        15,
        "ask-killed".to_string(),
        answers,
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-1",
        }),
        &pending,
        &tx,
    )
    .await;

    match rx.recv().await {
        Some(ServerEvent::Error { id, message, .. }) => {
            assert_eq!(id, 15);
            assert!(message.contains("indeterminate"), "{message}");
        }
        other => panic!("an interrupted claim is indeterminate, got {other:?}"),
    }
    assert!(
        response_rx.try_recv().is_err(),
        "the answer must not reach the tool after the interrupted claim"
    );
}

#[tokio::test]
async fn ask_answer_receipts_delivered_then_lost_terminal_is_indeterminate() {
    let home = IsolatedHome::new();
    let session = "ask_receipts_delivered_then_lost";
    ask_receipt_ledger_for_test(session);
    let (pending, response_rx) = pending_with("ask-lost").await;
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let answers = vec![selection("scope", "second")];

    answer_ask(
        21,
        "ask-lost".to_string(),
        answers.clone(),
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-1",
        }),
        &pending,
        &tx,
    )
    .await;
    let delivered = response_rx.await.expect("the tool received the answer");
    assert_eq!(delivered[0].selected, vec!["second".to_string()]);
    assert!(matches!(rx.recv().await, Some(ServerEvent::Ack { id: 21 })));

    // Simulated crash between delivery and the durable terminal row: the journal
    // keeps the claim and loses the result. A fresh process must report the
    // answer as indeterminate and must not hand it to the tool a second time.
    let journal = home
        .journal_root()
        .join(session)
        .join(crate::durable_runtime::JOURNAL_FILE_NAME);
    let text = std::fs::read_to_string(&journal).expect("the journal exists after the claim");
    let kept: Vec<&str> = text.lines().filter(|line| !line.contains("control.terminal")).collect();
    assert!(
        kept.len() < text.lines().count(),
        "the fixture must actually drop a terminal row"
    );
    std::fs::write(&journal, format!("{}\n", kept.join("\n"))).expect("the terminal row is dropped");
    assert!(
        !std::fs::read_to_string(&journal)
            .expect("read back")
            .contains("control.terminal"),
        "the terminal row is gone from disk"
    );

    ask_receipt_ledger_for_test(session);
    let (pending, mut response_rx) = pending_with("ask-lost").await;
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    answer_ask(
        22,
        "ask-lost".to_string(),
        answers,
        Some(AnswerReceipt {
            session_id: session,
            answer_id: "answer-1",
        }),
        &pending,
        &tx,
    )
    .await;

    match rx.recv().await {
        Some(ServerEvent::Error { id, message, .. }) => {
            assert_eq!(id, 22);
            assert!(
                message.contains("indeterminate"),
                "a delivered answer with a lost record is indeterminate: {message}"
            );
        }
        other => panic!("the answer must not be re-delivered, got {other:?}"),
    }
    assert!(
        response_rx.try_recv().is_err(),
        "the tool must not receive the same answer twice"
    );
}

#[tokio::test]
async fn ask_answer_receipts_legacy_answer_without_answer_id_is_unchanged() {
    let _home = IsolatedHome::new();
    let session = "ask_receipts_client_legacy";
    let ledger = ask_receipt_ledger_for_test(session);
    let (pending, response_rx) = pending_with("ask-legacy").await;
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    answer_ask(
        16,
        "ask-legacy".to_string(),
        vec![selection("scope", "first")],
        None,
        &pending,
        &tx,
    )
    .await;

    let delivered = response_rx.await.expect("the tool receives the answer");
    assert_eq!(delivered[0].selected, vec!["first".to_string()]);
    assert!(matches!(rx.recv().await, Some(ServerEvent::Ack { id: 16 })));
    let digest = ask_bridge::answer_digest("ask-legacy", "answer-1", &[selection("scope", "first")]);
    assert!(
        matches!(
            ledger.begin("ask-legacy", "answer-1", &digest, false).await,
            AnswerDecision::Unknown
        ),
        "a legacy answer writes no receipt"
    );
}

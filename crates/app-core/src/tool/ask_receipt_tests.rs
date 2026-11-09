//! Receipt-ledger tests: the pending check, the digest comparison, and the claim
//! as one serialized decision against the real durable journal.

use super::*;
use crate::protocol::AskSelection;
use serde_json::json;

struct IsolatedHome {
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("isolated agent home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
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
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

fn selection(question_id: &str, label: &str) -> AskSelection {
    AskSelection {
        question_id: question_id.to_string(),
        selected: vec![label.to_string()],
    }
}

fn restarted_ledger(session_id: &str) -> Arc<AnswerLedger> {
    forget_ledger_for_test(session_id);
    ledger_for(session_id)
}

fn journal_controls(root: &std::path::Path, session_id: &str) -> Vec<(String, Option<serde_json::Value>)> {
    let store = DurableRuntimeStore::open_in(root, session_id).expect("open the journal");
    store
        .state()
        .controls
        .iter()
        .map(|(id, entry)| (id.clone(), entry.result.clone()))
        .collect()
}

#[tokio::test]
async fn ask_answer_receipts_replay_the_same_answer_once() {
    let home = IsolatedHome::new();
    let session = "ask_receipts_replay";
    let ledger = restarted_ledger(session);
    let answers = vec![selection("scope", "second")];
    let digest = answer_digest("ask-1", "answer-1", &answers);

    assert!(matches!(
        ledger.begin("ask-1", "answer-1", &digest, true).await,
        AnswerDecision::Claimed
    ));
    let result = json!({"request_id": "ask-1", "resolved": true});
    ledger
        .complete("ask-1", &digest, result.clone())
        .await
        .expect("the terminal row lands");

    match ledger.begin("ask-1", "answer-1", &digest, false).await {
        AnswerDecision::Replay { result: stored } => assert_eq!(stored, result),
        other => panic!("a retried identical answer is a replay, got {other:?}"),
    }

    let controls = journal_controls(&home.journal_root(), session);
    assert_eq!(controls.len(), 1, "one question owns one control record: {controls:?}");
    assert_eq!(controls[0].1, Some(result), "the stored result is the receipt");
}

#[tokio::test]
async fn ask_answer_receipts_replay_survives_a_restart() {
    let home = IsolatedHome::new();
    let session = "ask_receipts_replay_restart";
    let answers = vec![selection("scope", "second")];
    let digest = answer_digest("ask-1", "answer-1", &answers);

    {
        let ledger = restarted_ledger(session);
        assert!(matches!(
            ledger.begin("ask-1", "answer-1", &digest, true).await,
            AnswerDecision::Claimed
        ));
        ledger
            .complete("ask-1", &digest, json!({"resolved": true}))
            .await
            .expect("terminal");
    }

    let reopened = restarted_ledger(session);
    match reopened.begin("ask-1", "answer-1", &digest, true).await {
        AnswerDecision::Replay { result } => assert_eq!(result["resolved"], json!(true)),
        other => panic!("a recorded answer replays after a restart, got {other:?}"),
    }
    assert_eq!(
        journal_controls(&home.journal_root(), session).len(),
        1,
        "a replay writes nothing"
    );
}

#[tokio::test]
async fn ask_answer_receipts_conflict_on_a_different_answer() {
    let home = IsolatedHome::new();
    let session = "ask_receipts_conflict";
    let ledger = restarted_ledger(session);
    let first = vec![selection("scope", "second")];
    let first_digest = answer_digest("ask-1", "answer-1", &first);
    assert!(matches!(
        ledger.begin("ask-1", "answer-1", &first_digest, true).await,
        AnswerDecision::Claimed
    ));
    ledger
        .complete("ask-1", &first_digest, json!({"resolved": true}))
        .await
        .expect("terminal");

    let different_choices = vec![selection("scope", "first")];
    match ledger
        .begin(
            "ask-1",
            "answer-1",
            &answer_digest("ask-1", "answer-1", &different_choices),
            true,
        )
        .await
    {
        AnswerDecision::Conflict { reason } => assert!(reason.contains("different choices"), "{reason}"),
        other => panic!("different choices under one answer id conflict, got {other:?}"),
    }

    match ledger
        .begin("ask-1", "answer-2", &answer_digest("ask-1", "answer-2", &first), true)
        .await
    {
        AnswerDecision::Conflict { reason } => assert!(reason.contains("different answer id"), "{reason}"),
        other => panic!("a second answer id on an answered question conflicts, got {other:?}"),
    }

    let controls = journal_controls(&home.journal_root(), session);
    assert_eq!(controls.len(), 1, "a conflict writes no second record: {controls:?}");
    assert!(controls[0].1.is_some(), "the first answer keeps its terminal result");
}

#[tokio::test]
async fn ask_answer_receipts_unknown_question_is_not_claimed() {
    let home = IsolatedHome::new();
    let session = "ask_receipts_unknown";
    let ledger = restarted_ledger(session);
    let answers = vec![selection("scope", "first")];
    let digest = answer_digest("ask-missing", "answer-1", &answers);

    assert!(matches!(
        ledger.begin("ask-missing", "answer-1", &digest, false).await,
        AnswerDecision::Unknown
    ));
    assert!(
        journal_controls(&home.journal_root(), session).is_empty(),
        "an unknown question leaves no record behind"
    );

    assert!(
        matches!(
            ledger.begin("ask-missing", "answer-1", &digest, true).await,
            AnswerDecision::Claimed
        ),
        "unknown means nothing is recorded, not that the question can never be answered"
    );
}

#[tokio::test]
async fn ask_answer_receipts_claim_without_terminal_is_indeterminate() {
    let home = IsolatedHome::new();
    let session = "ask_receipts_kill_after_claim";
    let answers = vec![selection("scope", "second")];
    let digest = answer_digest("ask-1", "answer-1", &answers);

    {
        let ledger = restarted_ledger(session);
        assert!(matches!(
            ledger.begin("ask-1", "answer-1", &digest, true).await,
            AnswerDecision::Claimed
        ));
    }

    let reopened = restarted_ledger(session);
    match reopened.begin("ask-1", "answer-1", &digest, true).await {
        AnswerDecision::Indeterminate { reason } => {
            assert!(reason.contains("indeterminate"), "{reason}");
            assert!(reason.contains("not delivered again"), "{reason}");
        }
        other => panic!("a claim without a terminal row is indeterminate, got {other:?}"),
    }
    match reopened
        .begin("ask-1", "answer-9", &answer_digest("ask-1", "answer-9", &answers), true)
        .await
    {
        AnswerDecision::Conflict { .. } => {}
        other => panic!("a different answer on a claimed question conflicts, got {other:?}"),
    }

    let controls = journal_controls(&home.journal_root(), session);
    assert_eq!(
        controls.len(),
        1,
        "the restart must not add a second claim: {controls:?}"
    );
    assert_eq!(controls[0].1, None, "the claim is still unterminated");
}

#[tokio::test]
async fn ask_answer_receipts_completed_receipts_are_bounded() {
    let _home = IsolatedHome::new();
    let session = "ask_receipts_bounded";
    let cap = 4usize;
    let ledger = ask_receipt_ledger_with_cap_for_test(session, cap);
    let answers = vec![selection("scope", "first")];

    for index in 0..(cap + 2) {
        let ask_id = format!("ask-{index}");
        let answer_id = format!("answer-{index}");
        let digest = answer_digest(&ask_id, &answer_id, &answers);
        assert!(matches!(
            ledger.begin(&ask_id, &answer_id, &digest, true).await,
            AnswerDecision::Claimed
        ));
        ledger
            .complete(&ask_id, &digest, json!({"resolved": true}))
            .await
            .expect("terminal");
    }

    let state = ledger.lock();
    assert_eq!(state.completed.len(), cap, "completed receipts are capped");
    assert_eq!(state.entries.len(), cap);
    assert!(!state.entries.contains_key("ask-0"), "the oldest receipt is evicted");
    assert!(
        state.entries.contains_key(&format!("ask-{}", cap + 1)),
        "the newest receipt stays"
    );
    assert_eq!(COMPLETED_RECEIPT_CAP, 1_024, "the production cap is the plan's 1,024");
}

#[tokio::test]
async fn ask_answer_receipts_pending_claims_are_never_evicted() {
    let _home = IsolatedHome::new();
    let session = "ask_receipts_pending_kept";
    let cap = 3usize;
    let ledger = ask_receipt_ledger_with_cap_for_test(session, cap);
    let answers = vec![selection("scope", "first")];

    assert!(matches!(
        ledger
            .begin(
                "ask-pending",
                "answer-pending",
                &answer_digest("ask-pending", "answer-pending", &answers),
                true
            )
            .await,
        AnswerDecision::Claimed
    ));
    for index in 0..(cap + 3) {
        let ask_id = format!("ask-done-{index}");
        let answer_id = format!("answer-done-{index}");
        let digest = answer_digest(&ask_id, &answer_id, &answers);
        assert!(matches!(
            ledger.begin(&ask_id, &answer_id, &digest, true).await,
            AnswerDecision::Claimed
        ));
        ledger
            .complete(&ask_id, &digest, json!({"resolved": true}))
            .await
            .expect("terminal");
    }

    let state = ledger.lock();
    assert!(
        matches!(state.entries.get("ask-pending"), Some(Receipt::Claiming { .. })),
        "a pending claim survives every completed-receipt eviction"
    );
    assert_eq!(state.completed.len(), cap, "the completed table stayed at its cap");
}

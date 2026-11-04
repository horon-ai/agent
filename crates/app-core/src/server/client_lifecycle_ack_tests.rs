//! Inbound side of the approval/failover ack transport, driven through the
//! same handlers the `Request` match arms call.

use super::*;
use crate::ambient::{BlockedApproval, BlockedApprovalStore};
use crate::auth::test_sandbox::AuthTestSandbox;
use crate::server::state::{EmitOutcome, PendingFailoverEvent, emit_session_event};

fn record(request_id: &str) -> BlockedApproval {
    BlockedApproval {
        session_id: "sess-ack".to_string(),
        request_id: request_id.to_string(),
        ..BlockedApproval::default()
    }
}

fn seed_pending_event(store: &BlockedApprovalStore, request_id: &str) {
    let pending = serde_json::json!({"type": "approval_resolved", "request_id": request_id});
    assert!(
        store
            .update_with(request_id, move |stored| stored.pending_event = Some(pending))
            .expect("seed the pending event"),
        "the record exists"
    );
}

fn switch_event(chain_id: &str, from: &str, to: &str, attempt: u32) -> ServerEvent {
    ServerEvent::FailoverSwitched {
        chain_id: chain_id.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        reason: "429 Too Many Requests".to_string(),
        attempt,
        max: 3,
    }
}

fn stored_switches(sandbox: &AuthTestSandbox) -> Vec<PendingFailoverEvent> {
    let path = sandbox.root().join("failover-events.json");
    if !path.exists() {
        return Vec::new();
    }
    crate::storage::read_json::<Vec<PendingFailoverEvent>>(&path).expect("the announcement file is valid JSON")
}

/// Exactly one record's durable notification is retired.
#[test]
fn an_approval_ack_clears_only_its_record_and_replies_ack() {
    let _sandbox = AuthTestSandbox::new().expect("isolated home");
    let store = BlockedApprovalStore::open_default().unwrap();
    assert!(store.add(record("req-acked")).unwrap());
    assert!(store.add(record("req-other")).unwrap());
    seed_pending_event(&store, "req-acked");
    seed_pending_event(&store, "req-other");

    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_approval_ack(7, "req-acked".to_string(), &tx);

    assert!(
        matches!(rx.try_recv(), Ok(ServerEvent::Ack { id: 7 })),
        "the ack is acknowledged"
    );
    assert!(rx.try_recv().is_err(), "one ack produces exactly one reply");

    let records = store.list().unwrap();
    assert_eq!(records.len(), 2, "an ack never creates or removes a record");
    let acked = records
        .iter()
        .find(|stored| stored.request_id == "req-acked")
        .expect("the acked record is still there");
    assert_eq!(acked.pending_event, None, "the acked record owes nothing more");
    let other = records
        .iter()
        .find(|stored| stored.request_id == "req-other")
        .expect("the other record is untouched");
    assert!(
        other.pending_event.is_some(),
        "a different record keeps its pending event"
    );
}

/// A missing record is success, not an error, and nothing changes.
#[test]
fn an_approval_ack_for_a_missing_record_succeeds_and_changes_nothing() {
    let _sandbox = AuthTestSandbox::new().expect("isolated home");
    let store = BlockedApprovalStore::open_default().unwrap();
    assert!(store.add(record("req-kept")).unwrap());
    seed_pending_event(&store, "req-kept");
    let before = store.list().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_approval_ack(8, "req-absent".to_string(), &tx);

    assert!(
        matches!(rx.try_recv(), Ok(ServerEvent::Ack { id: 8 })),
        "a missing record is not an error"
    );
    assert_eq!(store.list().unwrap(), before, "the store is byte-for-byte unchanged");
}

/// A matching ack drops exactly its own announcement.
#[test]
fn a_matching_failover_ack_removes_exactly_its_entry() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-ack-failover";

    for event in [
        switch_event("chain-1", "model-a", "model-b", 1),
        switch_event("chain-1", "model-b", "model-c", 2),
    ] {
        assert_eq!(
            emit_session_event(session_id, event),
            EmitOutcome::NoSink,
            "precondition: both switches are recorded without a live client"
        );
    }
    assert_eq!(stored_switches(&sandbox).len(), 2);

    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_failover_ack(
        9,
        session_id,
        "chain-1".to_string(),
        "model-b".to_string(),
        "model-c".to_string(),
        2,
        &tx,
    );

    assert!(matches!(rx.try_recv(), Ok(ServerEvent::Ack { id: 9 })));
    let remaining = stored_switches(&sandbox);
    assert_eq!(remaining.len(), 1, "exactly one entry was removed");
    assert_eq!(remaining[0].chain_id, "chain-1");
    assert_eq!(remaining[0].attempt, 1, "the unacked attempt is the one left");
    assert_eq!(remaining[0].from, "model-a");
    assert_eq!(remaining[0].to, "model-b");
}

/// A non-matching key removes nothing and still succeeds.
#[test]
fn a_non_matching_failover_ack_removes_nothing() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-ack-mismatch";

    assert_eq!(
        emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1)),
        EmitOutcome::NoSink
    );
    let before = stored_switches(&sandbox);
    assert_eq!(before.len(), 1);

    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_failover_ack(
        10,
        session_id,
        "chain-0".to_string(),
        "model-a".to_string(),
        "model-b".to_string(),
        1,
        &tx,
    );

    assert!(matches!(rx.try_recv(), Ok(ServerEvent::Ack { id: 10 })));
    assert_eq!(
        stored_switches(&sandbox),
        before,
        "a non-matching ack leaves the file untouched"
    );
}

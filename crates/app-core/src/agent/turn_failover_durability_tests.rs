//! Durability of `FailoverSwitched` across a clientless switch and a restart.
//!
//! The behaviour under test lives in `crate::server::state` (the emit chokepoint
//! and the pending-announcement file), so this module is declared from there
//! while the file sits beside `turn_failover.rs`, the switch that emits the
//! event.

use super::*;
use crate::auth::test_sandbox::AuthTestSandbox;
use tokio::sync::mpsc;

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

fn events_path(sandbox: &AuthTestSandbox) -> PathBuf {
    sandbox.root().join("failover-events.json")
}

fn stored(sandbox: &AuthTestSandbox) -> Vec<PendingFailoverEvent> {
    let path = events_path(sandbox);
    if !path.exists() {
        return Vec::new();
    }
    crate::storage::read_json::<Vec<PendingFailoverEvent>>(&path).expect("the announcement file is valid JSON")
}

fn drain(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

/// The durable write must land before the delivery step runs, not after it.
/// The probe reads the file from inside the delivery call, which is the exact
/// instant a client would first see the event.
#[tokio::test]
async fn the_entry_is_on_disk_at_the_moment_delivery_runs() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-durable-ordering";
    let mut observed_chain_ids: Vec<String> = Vec::new();

    let outcome = persist_then_deliver(
        session_id,
        switch_event("chain-ordering", "model-a", "model-b", 1),
        |session_id, _event| {
            observed_chain_ids = stored(&sandbox)
                .into_iter()
                .filter(|entry| entry.session_id == session_id)
                .map(|entry| entry.chain_id)
                .collect();
            EmitOutcome::Delivered
        },
    );

    assert_eq!(outcome, EmitOutcome::Delivered);
    assert_eq!(
        observed_chain_ids,
        vec!["chain-ordering".to_string()],
        "the announcement is already durable when delivery runs"
    );
}

fn switched_keys(events: &[ServerEvent]) -> Vec<(String, String, String, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::FailoverSwitched {
                chain_id,
                from,
                to,
                attempt,
                ..
            } => Some((chain_id.clone(), from.clone(), to.clone(), *attempt)),
            _ => None,
        })
        .collect()
}

/// The entry must exist before anything is handed to a client. Dropping the
/// receiver first makes the hand-off fail, so a file that still has the entry
/// proves the write does not depend on delivery.
#[tokio::test]
async fn a_switch_is_on_disk_before_it_reaches_a_sink() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-durable-before-delivery";

    let (tx, rx) = mpsc::unbounded_channel();
    drop(rx);
    register_session_event_sink(session_id, tx);

    let outcome = emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1));
    assert_eq!(
        outcome,
        EmitOutcome::Closed,
        "the sink is gone, so delivery fails after the announcement is recorded"
    );

    let entries = stored(&sandbox);
    assert_eq!(entries.len(), 1, "the failed delivery still left exactly one entry");
    assert_eq!(entries[0].session_id, session_id);
    assert_eq!(entries[0].chain_id, "chain-1");
    assert_eq!(entries[0].from, "model-a");
    assert_eq!(entries[0].to, "model-b");
    assert_eq!(entries[0].attempt, 1);
    assert_eq!(entries[0].max, 3);

    // No sink at all is the clientless case: still recorded, never delivered.
    let outcome = emit_session_event("sess-durable-no-sink", switch_event("chain-2", "model-a", "model-c", 1));
    assert_eq!(outcome, EmitOutcome::NoSink);
    assert_eq!(stored(&sandbox).len(), 2, "a clientless switch is recorded too");
}

#[tokio::test]
async fn an_ack_removes_only_its_entry() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-durable-ack";

    emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1));
    emit_session_event(session_id, switch_event("chain-1", "model-b", "model-c", 2));
    emit_session_event(session_id, switch_event("chain-2", "model-a", "model-d", 1));
    assert_eq!(stored(&sandbox).len(), 3, "precondition: three pending announcements");

    let removed = clear_acked_failover_event(session_id, "chain-1", "model-b", "model-c", 2);

    assert_eq!(removed, 1, "exactly one entry matches the ack");
    let remaining = stored(&sandbox);
    assert_eq!(remaining.len(), 2, "the other two entries survive");
    assert!(
        remaining
            .iter()
            .any(|entry| entry.chain_id == "chain-1" && entry.attempt == 1)
    );
    assert!(remaining.iter().any(|entry| entry.chain_id == "chain-2"));
    assert!(
        !remaining
            .iter()
            .any(|entry| entry.chain_id == "chain-1" && entry.attempt == 2),
        "the acknowledged entry is gone"
    );
}

#[tokio::test]
async fn an_ack_with_a_non_matching_key_removes_nothing() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-durable-ack-mismatch";

    emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1));
    let before = stored(&sandbox);
    assert_eq!(before.len(), 1);

    assert_eq!(
        clear_acked_failover_event(session_id, "chain-0", "model-a", "model-b", 1),
        0
    );
    assert_eq!(
        clear_acked_failover_event(session_id, "chain-1", "model-a", "model-c", 1),
        0
    );
    assert_eq!(
        clear_acked_failover_event(session_id, "chain-1", "model-a", "model-b", 2),
        0
    );
    assert_eq!(
        clear_acked_failover_event("sess-other", "chain-1", "model-a", "model-b", 1),
        0
    );

    assert_eq!(stored(&sandbox), before, "a non-matching ack leaves the file untouched");
}

#[tokio::test]
async fn re_registering_a_sink_reemits_each_pending_entry_once() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-durable-reemit";

    emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1));
    emit_session_event(session_id, switch_event("chain-1", "model-b", "model-c", 2));
    emit_session_event("sess-durable-other", switch_event("chain-9", "model-x", "model-y", 1));

    let (tx, mut rx) = mpsc::unbounded_channel();
    register_session_event_sink(session_id, tx);
    let first = switched_keys(&drain(&mut rx));
    assert_eq!(
        first,
        vec![
            ("chain-1".to_string(), "model-a".to_string(), "model-b".to_string(), 1),
            ("chain-1".to_string(), "model-b".to_string(), "model-c".to_string(), 2),
        ],
        "every pending entry for this session, once each, and nothing from another session"
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    register_session_event_sink(session_id, tx);
    let second = switched_keys(&drain(&mut rx));
    assert_eq!(
        second, first,
        "a re-registered sink sees each pending entry exactly once"
    );

    assert_eq!(stored(&sandbox).len(), 3, "re-emitting never clears a pending entry");
}

#[tokio::test]
async fn a_corrupt_file_does_not_block_a_switch() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-durable-corrupt";

    std::fs::write(events_path(&sandbox), b"{ this is not json").expect("seed a corrupt announcement file");
    assert!(
        read_pending_failover_events().is_empty(),
        "a malformed file reads as empty instead of failing"
    );

    let outcome = emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1));
    assert_eq!(outcome, EmitOutcome::NoSink, "the switch still goes through");

    let entries = stored(&sandbox);
    assert_eq!(entries.len(), 1, "the append rewrote the file in the current shape");
    assert_eq!(entries[0].chain_id, "chain-1");
}

#[tokio::test]
async fn the_pending_list_is_capped_at_one_hundred() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-durable-cap";

    for index in 0..200 {
        emit_session_event(
            session_id,
            switch_event(&format!("chain-{index:03}"), "model-a", "model-b", 1),
        );
    }

    let entries = stored(&sandbox);
    assert_eq!(entries.len(), MAX_PENDING_FAILOVER_EVENTS, "the file stays bounded");
    assert_eq!(entries.len(), 100);
    assert_eq!(
        entries[0].chain_id, "chain-100",
        "the oldest announcements were dropped"
    );
    assert_eq!(entries[99].chain_id, "chain-199", "the newest announcement is kept");
}

/// A repeat emit of the same switch (a restart re-announce) must not add a
/// second entry for the same ack key.
#[tokio::test]
async fn the_same_ack_key_is_recorded_once() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let session_id = "sess-durable-dedupe";

    emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1));
    emit_session_event(session_id, switch_event("chain-1", "model-a", "model-b", 1));

    assert_eq!(stored(&sandbox).len(), 1, "the ack key dedupes a repeated announcement");
}

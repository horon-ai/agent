//! Send-path tests: sequencing, lane delivery, buffering, and allocations.

use crate::harness_tests::{Mode, direct, from_peer, harness, never};
use crate::message::{IrcError, IrcLimits, WaitOutcome};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn send_orders_same_millisecond_messages_by_seq() {
    let h = harness(IrcLimits::default()).add("a").add("b").mode("b", Mode::Errored);
    for index in 0..5 {
        let body = format!("step-{index}");
        let report = h.send("a", "b", &body).await.expect("send must be accepted");
        assert_eq!(report.delivered(), 0);
    }
    let drained = h.bus.drain("b", None);
    assert_eq!(drained.len(), 5);
    let seqs: Vec<u64> = drained.iter().map(|message| message.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "mailbox order must follow seq");
    for window in seqs.windows(2) {
        assert!(window[0] < window[1], "seq must strictly increase within a sender");
    }
    assert!(
        drained.windows(2).all(|pair| pair[0].id != pair[1].id),
        "each send takes its own message id"
    );
    let bodies: Vec<&str> = drained.iter().map(|message| message.body.as_ref()).collect();
    assert_eq!(bodies, ["step-0", "step-1", "step-2", "step-3", "step-4"]);
}

#[tokio::test]
async fn waiter_receives_before_delivery_and_mailbox_stays_empty() {
    let h = harness(IrcLimits::default()).add("a").add("b").responder("a", true);
    h.send("b", "a", "ask").await.expect("seed send");
    h.clear_calls();
    let waiting = tokio::spawn({
        let bus = Arc::clone(&h.bus);
        async move { bus.wait("b", direct(), Duration::from_secs(5), never()).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.send("a", "b", "reply").await.expect("delivery send");
    let outcome = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .expect("wait must resolve")
        .expect("wait task must not panic");
    match outcome {
        WaitOutcome::Message(message) => assert_eq!(message.body.as_ref(), "reply"),
        _ => panic!("waiter must receive the message directly"),
    }
    assert_eq!(h.bus.unread_count("b"), 0, "an injected message must not also buffer");
    assert!(h.calls().is_empty(), "a parked waiter must win over delivery");
}

#[tokio::test]
async fn reply_arriving_during_wait_setup_is_received() {
    let h = harness(IrcLimits::default())
        .add("a")
        .add("b")
        .responder("b", true)
        .reply_on_pending("b", "a", "late answer");
    h.send("a", "b", "question").await.expect("seed send");
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        h.bus.wait("a", from_peer("b"), Duration::from_secs(5), never()),
    )
    .await
    .expect("wait must not park");
    match outcome {
        WaitOutcome::Message(message) => assert_eq!(message.body.as_ref(), "late answer"),
        _ => panic!("a reply that lands during setup must still be returned"),
    }
    assert_eq!(h.bus.stats().waiters_live, 0);
}

#[tokio::test]
async fn broadcast_shares_one_body_allocation() {
    let h = harness(IrcLimits::default())
        .add("root")
        .add("c1")
        .add("c2")
        .add("c3")
        .broadcast(&["c1", "c2", "c3"]);
    let before = h.bus.stats().body_allocations;
    let report = h.send_broadcast("root", "shared body").await.expect("broadcast");
    assert_eq!(report.receipts.len(), 3);
    assert_eq!(report.delivered(), 3);
    assert_eq!(
        h.bus.stats().body_allocations - before,
        1,
        "one body allocation per send"
    );
    let calls = h.calls();
    assert_eq!(calls.len(), 3);
    let to: Vec<&str> = report.receipts.iter().map(|receipt| receipt.to.as_ref()).collect();
    assert_eq!(to, ["c1", "c2", "c3"], "receipts are sorted by target id");
    // Every leg carried the same Arc, so the allocation count cannot grow per target.
    assert_eq!(h.bus.stats().body_allocations - before, 1);
}

#[tokio::test]
async fn unanswered_sends_clear_on_reply_and_progress_is_promised() {
    let h = harness(IrcLimits::default()).add("a").add("b");
    h.send("a", "b", "question").await.expect("direct send");
    let unanswered = h.bus.unanswered_sends("a");
    assert_eq!(unanswered.len(), 1);
    assert_eq!(unanswered[0].to.as_ref(), "b");
    assert!(!unanswered[0].promised);

    h.send_progress("b", "a", "working on it").await.expect("progress");
    let promised = h.bus.unanswered_sends("a");
    assert_eq!(promised.len(), 1, "progress does not answer");
    assert!(promised[0].promised, "progress marks the send as promised");

    h.send("b", "a", "answer").await.expect("reply");
    assert!(
        h.bus.unanswered_sends("a").is_empty(),
        "a direct reply clears the obligation"
    );
    assert!(
        h.bus.sent_since("b", "a", promised[0].seq),
        "the reply is after the request"
    );
}

#[tokio::test]
async fn mailbox_drops_oldest_beyond_cap() {
    let limits = IrcLimits {
        mailbox_cap: 3,
        ..IrcLimits::default()
    };
    let h = harness(limits).add("a").add("b").mode("b", Mode::Errored);
    for index in 0..5 {
        h.send("a", "b", &format!("m{index}")).await.expect("send");
    }
    assert_eq!(h.bus.unread_count("b"), 3);
    let stats = h.bus.stats();
    assert_eq!(stats.mailbox_dropped, 2);
    assert_eq!(stats.mailbox_high_water, 3);
    let remaining: Vec<String> = h.bus.drain("b", None).into_iter().map(|m| m.body.to_string()).collect();
    assert_eq!(remaining, ["m2", "m3", "m4"], "the oldest messages are dropped first");
}

#[tokio::test]
async fn oversized_body_is_rejected_before_any_delivery() {
    let limits = IrcLimits {
        max_body_bytes: 8,
        ..IrcLimits::default()
    };
    let h = harness(limits).add("a").add("b");
    let before = h.bus.stats().sent;
    let error = h.send("a", "b", "nine-byte").await.expect_err("must reject");
    match error {
        IrcError::BodyTooLarge { bytes, max } => {
            assert_eq!(bytes, 9);
            assert_eq!(max, 8);
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(h.calls().is_empty(), "a rejected body must not reach delivery");
    assert_eq!(h.bus.stats().sent, before);
    assert_eq!(h.send("a", "b", "12345678").await.expect("exact limit").delivered(), 1);
}

#[tokio::test]
async fn self_send_and_progress_broadcast_are_rejected() {
    let h = harness(IrcLimits::default())
        .add("root")
        .add("child")
        .broadcast(&["child"]);
    expect_error(h.send("root", "root", "hi").await, |error| {
        matches!(error, IrcError::SelfSend)
    });
    expect_error(h.send_progress("root", "all", "working").await, |error| {
        matches!(error, IrcError::ProgressBroadcast)
    });
    expect_error(h.send("root", "child", "   ").await, |error| {
        matches!(error, IrcError::EmptyBody)
    });
    assert!(h.calls().is_empty(), "rejected sends must deliver nothing");
}

fn expect_error<T, F>(result: Result<T, IrcError>, accepts: F)
where
    F: FnOnce(&IrcError) -> bool,
{
    match result {
        Err(error) if accepts(&error) => {}
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("expected an error"),
    }
}

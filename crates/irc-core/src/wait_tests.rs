//! Wait-path tests: parking, release conditions, filters, and cleanup.

use crate::harness_tests::{Harness, Mode, describe, direct, from_peer, harness, never};
use crate::message::{IrcLimits, Outcome, WaitFilter, WaitOutcome};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::test]
async fn mutual_wait_returns_unanswerable() {
    let h = two_way().await;
    // Both sides owe each other, so the side that parks second must be released.
    let parked = tokio::spawn({
        let bus = Arc::clone(&h.bus);
        async move { bus.wait("b", from_peer("a"), Duration::from_millis(200), never()).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = Instant::now();
    let outcome = h.bus.wait("a", from_peer("b"), Duration::from_secs(2), never()).await;
    assert!(
        matches!(outcome, WaitOutcome::Unanswerable(_)),
        "the side that arrives second must be released instead of parking: got {}",
        describe(&outcome)
    );
    assert!(started.elapsed() < Duration::from_secs(1), "settling must be immediate");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), parked)
            .await
            .expect("the owed side settles on its own deadline")
            .expect("wait task"),
        WaitOutcome::Timeout
    ));
    assert_eq!(h.bus.stats().waiters_live, 0);
}

/// Two peers that traded direct messages, so only the later sender still owes a reply.
async fn two_way() -> Harness {
    let h = harness(IrcLimits::default())
        .add("a")
        .add("b")
        .responder("a", true)
        .responder("b", true);
    h.send("a", "b", "from a").await.expect("a to b");
    h.send_progress("b", "a", "working on it").await.expect("b progress");
    assert_eq!(h.bus.unanswered_sends("a").len(), 1);
    assert_eq!(h.bus.unanswered_sends("b").len(), 1);
    assert_eq!(h.bus.unread_count("a"), 0, "a promise is delivered, not buffered");
    h
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_mutual_waits_do_not_both_park() {
    let h = two_way().await;

    let first = tokio::spawn({
        let bus = Arc::clone(&h.bus);
        async move { bus.wait("a", from_peer("b"), Duration::from_millis(300), never()).await }
    });
    let second = tokio::spawn({
        let bus = Arc::clone(&h.bus);
        async move { bus.wait("b", from_peer("a"), Duration::from_millis(300), never()).await }
    });

    let (left, right) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(1), first),
        tokio::time::timeout(Duration::from_secs(1), second),
    );
    let settled = [left, right]
        .into_iter()
        .filter(|result| {
            matches!(
                result.as_ref().ok().and_then(|inner| inner.as_ref().ok()),
                Some(WaitOutcome::Unanswerable(_) | WaitOutcome::NothingToWaitFor)
            )
        })
        .count();
    assert!(
        settled >= 1,
        "at least one side must settle within a second instead of both parking"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), async {
            while h.bus.stats().waiters_live > 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok(),
        "no waiter may be left behind"
    );
}

#[tokio::test]
async fn wait_returns_nothing_to_wait_for_when_owed_targets_cannot_respond() {
    let h = harness(IrcLimits::default()).add("a").add("b").responder("b", false);
    h.send("a", "b", "question").await.expect("send");
    let outcome = h.bus.wait("a", direct(), Duration::from_secs(5), never()).await;
    assert!(matches!(outcome, WaitOutcome::NothingToWaitFor));
    assert_eq!(h.bus.stats().waiters_live, 0, "the waiter must not leak");
}

#[tokio::test]
async fn timed_out_and_cancelled_waiters_are_removed() {
    let h = harness(IrcLimits::default()).add("a").add("b").responder("b", true);
    h.send("a", "b", "question").await.expect("send");
    let timed = h.bus.wait("a", direct(), Duration::from_millis(20), never()).await;
    assert!(matches!(timed, WaitOutcome::Timeout));
    assert_eq!(h.bus.stats().waiters_live, 0);

    let cancelled = h
        .bus
        .wait("a", direct(), Duration::from_secs(5), async {
            tokio::time::sleep(Duration::from_millis(20)).await;
        })
        .await;
    assert!(matches!(cancelled, WaitOutcome::Cancelled));
    assert_eq!(h.bus.stats().waiters_live, 0);
}

#[tokio::test]
async fn take_and_drain_respect_from_filter() {
    let h = harness(IrcLimits::default())
        .add("a")
        .add("b")
        .add("c")
        .handle("b", "peer-b")
        .mode("a", Mode::Errored);
    h.send("b", "a", "b1").await.expect("send");
    h.send("c", "a", "c1").await.expect("send");
    h.send("b", "a", "b2").await.expect("send");

    let first = h.bus.take("a", Some("b")).expect("b message");
    assert_eq!(first.body.as_ref(), "b1");
    assert_eq!(h.bus.unread_count("a"), 2);

    let rest: Vec<String> = h.bus.drain("a", None).into_iter().map(|m| m.body.to_string()).collect();
    assert_eq!(rest, ["c1", "b2"], "drain preserves arrival order");

    h.send("peer-b", "a", "via handle").await.expect("handle resolution");
    assert_eq!(h.bus.unread_count("a"), 1);
    h.bus.drain("a", None);
    assert!(h.bus.drain("a", Some("c")).is_empty(), "the mailbox is empty");
}

#[tokio::test]
async fn forget_removes_mailbox_waiters_and_marks() {
    let h = harness(IrcLimits::default())
        .add("a")
        .add("b")
        .add("c")
        .responder("b", true)
        .mode("a", Mode::Errored);
    h.send("c", "a", "buffered").await.expect("send");
    assert_eq!(h.bus.unread_count("a"), 1);
    h.send("a", "b", "question").await.expect("send");
    assert_eq!(h.bus.unanswered_sends("a").len(), 1);

    let parked = tokio::spawn({
        let bus = Arc::clone(&h.bus);
        async move { bus.wait("a", from_peer("b"), Duration::from_secs(5), never()).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.bus.stats().waiters_live, 1);

    h.bus.forget("a");
    assert_eq!(h.bus.unread_count("a"), 0, "forget drops the mailbox");
    assert_eq!(h.bus.stats().waiters_live, 0, "forget drops parked waiters");
    assert!(h.bus.unanswered_sends("a").is_empty(), "forget drops reply marks");
    assert!(!h.bus.sent_since("a", "b", 0));
    assert!(
        tokio::time::timeout(Duration::from_secs(2), parked)
            .await
            .expect("a dropped waiter must settle")
            .is_ok()
    );
}

#[tokio::test]
async fn errored_delivery_is_buffered_unavailable_is_not() {
    let h = harness(IrcLimits::default())
        .add("a")
        .add("b")
        .add("c")
        .mode("b", Mode::Errored)
        .mode("c", Mode::Unavailable);
    let errored = h.send("a", "b", "keep me").await.expect("send");
    assert!(matches!(errored.receipts[0].outcome, Outcome::Failed(_)));
    assert_eq!(h.bus.unread_count("b"), 1, "an errored hand-off stays readable");

    let unavailable = h.send("a", "c", "drop me").await.expect("send");
    assert!(matches!(unavailable.receipts[0].outcome, Outcome::Failed(_)));
    assert_eq!(h.bus.unread_count("c"), 0, "an unavailable target must not buffer");
}

#[tokio::test]
async fn unavailable_delivery_forgets_target_state() {
    let h = harness(IrcLimits::default())
        .add("a")
        .add("b")
        .mode("b", Mode::Unavailable);
    h.send("b", "a", "hello").await.expect("seed");
    assert_eq!(h.bus.unanswered_sends("b").len(), 1);
    h.send("a", "b", "anybody there").await.expect("send");
    assert!(
        h.bus.unanswered_sends("b").is_empty(),
        "a gone session leaves no reply obligation behind"
    );
    assert!(!h.bus.sent_since("b", "a", 0));
}

#[tokio::test]
async fn parent_flag_is_set_only_for_direct_parent() {
    let h = harness(IrcLimits::default())
        .add("parent")
        .add("other")
        .add("child")
        .mode("child", Mode::Injected)
        .parent("child", "parent");
    h.send("parent", "child", "from parent").await.expect("send");
    h.send("other", "child", "from peer").await.expect("send");
    let calls = h.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], ("child".to_string(), true));
    assert_eq!(calls[1], ("child".to_string(), false));
    assert!(
        matches!(h.bus.unread_count("child"), 0),
        "an injected leg does not buffer"
    );
}

#[tokio::test]
async fn wait_returns_pending_input_when_deliver_reports_it() {
    let h = harness(IrcLimits::default())
        .add("a")
        .add("b")
        .responder("b", true)
        .pending("a");
    h.send("a", "b", "question").await.expect("send");
    let outcome = h.bus.wait("a", direct(), Duration::from_secs(5), never()).await;
    assert!(matches!(outcome, WaitOutcome::PendingInput));
    assert_eq!(h.bus.stats().waiters_live, 0);
}

#[tokio::test]
async fn wait_timeout_is_clamped_to_wait_max() {
    let limits = IrcLimits {
        wait_max: Duration::from_millis(50),
        ..IrcLimits::default()
    };
    let h = harness(limits).add("a").add("b").responder("b", true);
    h.send("a", "b", "question").await.expect("send");
    let started = Instant::now();
    let outcome = h.bus.wait("a", direct(), Duration::from_secs(3600), never()).await;
    assert!(matches!(outcome, WaitOutcome::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the configured cap must bound the wait"
    );
}

#[tokio::test]
async fn broadcast_concurrency_never_exceeds_limit() {
    let limits = IrcLimits {
        broadcast_concurrency: 2,
        ..IrcLimits::default()
    };
    let targets = ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"];
    let mut h = harness(limits).add("root");
    for target in targets {
        h = h.add(target);
    }
    let h = h.broadcast(&targets).delay(10);
    let report = h.send_broadcast("root", "fan out").await.expect("broadcast");
    assert_eq!(report.receipts.len(), 8);
    assert_eq!(h.bus.stats().broadcast_inflight_high_water, 2);
}

#[tokio::test]
async fn direct_only_filter_skips_broadcast_and_progress() {
    let h = harness(IrcLimits::default())
        .add("root")
        .add("a")
        .add("b")
        .broadcast(&["a"])
        .mode("a", Mode::Errored);
    h.send_broadcast("root", "everyone").await.expect("broadcast");
    h.send_progress("b", "a", "thinking").await.expect("progress");
    h.send("b", "a", "answer").await.expect("direct");
    assert_eq!(h.bus.unread_count("a"), 3);

    let outcome = h
        .bus
        .wait(
            "a",
            WaitFilter {
                from: None,
                direct_only: true,
            },
            Duration::from_secs(1),
            never(),
        )
        .await;
    match outcome {
        WaitOutcome::Message(message) => assert_eq!(message.body.as_ref(), "answer"),
        _ => panic!("a direct-only wait must skip broadcast and progress notes"),
    }
    let next = h
        .bus
        .wait(
            "a",
            WaitFilter {
                from: None,
                direct_only: true,
            },
            Duration::from_secs(1),
            never(),
        )
        .await;
    assert!(matches!(next, WaitOutcome::NothingToWaitFor));
    assert_eq!(h.bus.unread_count("a"), 2, "skipped messages stay readable");
}

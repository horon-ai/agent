//! Structural gate, section `irc`: the mailbox cap, the dropped count under a
//! flood, one body allocation per broadcast, no waiter left parked after
//! churn, and the broadcast concurrency ceiling. Printed for
//! `scripts/perf/check_structural.py`, bounded by
//! `scripts/perf/structural_budget.json`.

use crate::bus::IrcBus;
use crate::harness_tests::{Mode, describe, direct, harness};
use crate::message::{IrcLimits, SendRequest, WaitOutcome};
use std::sync::Arc;
use std::time::Duration;

const FLOOD: usize = 1000;
const BROADCAST_TARGETS: usize = 64;
const WAITERS: usize = 256;
const MAILBOX_CAP: usize = 100;

/// Park `want` waiters before the test moves time, so an advance cannot miss a
/// waiter that has not reached its `select!` yet.
async fn wait_for_waiters(bus: &IrcBus, want: u64) {
    for _ in 0..10_000 {
        if bus.stats().waiters_live == want {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(bus.stats().waiters_live, want, "waiters never parked");
}

#[tokio::test(start_paused = true)]
async fn irc_bus_stays_bounded() {
    let broadcast_ids: Vec<String> = (0..BROADCAST_TARGETS).map(|index| format!("b{index:02}")).collect();
    let mut builder = harness(IrcLimits::default())
        .add("root")
        .add("sink")
        .mode("sink", Mode::Errored)
        .add("waiter")
        .add("peer")
        .responder("peer", true);
    for id in &broadcast_ids {
        builder = builder.add(id);
    }
    let targets: Vec<&str> = broadcast_ids.iter().map(String::as_str).collect();
    let mut world = builder.broadcast(&targets);

    for _ in 0..FLOOD {
        let report = world
            .send("root", "sink", "flood")
            .await
            .expect("flood send is accepted");
        assert_eq!(report.delivered(), 0, "an errored leg is not delivered");
    }
    let after_flood = world.bus.stats();
    assert_eq!(
        after_flood.mailbox_high_water, MAILBOX_CAP as u64,
        "the mailbox stops at its cap"
    );
    assert_eq!(
        after_flood.mailbox_dropped,
        (FLOOD - MAILBOX_CAP) as u64,
        "every message past the cap is dropped"
    );

    // A slow delivery keeps several broadcast legs in flight at once, which is
    // what the concurrency ceiling is about.
    world = world.delay(1);
    let before = world.bus.stats().body_allocations;
    let bus = Arc::clone(&world.bus);
    let broadcast = tokio::spawn(async move {
        bus.send(SendRequest {
            from: "root".into(),
            to: "all".into(),
            body: "broadcast".into(),
            reply_to: None,
            progress: false,
            wake_relay: false,
        })
        .await
    });
    while !broadcast.is_finished() {
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1)).await;
    }
    let report = broadcast.await.expect("broadcast task").expect("broadcast is accepted");
    assert_eq!(
        report.receipts.len(),
        BROADCAST_TARGETS,
        "one receipt per broadcast target"
    );
    assert_eq!(
        report.delivered(),
        BROADCAST_TARGETS,
        "every broadcast leg is delivered"
    );
    let broadcast_body_allocations = world.bus.stats().body_allocations - before;
    assert_eq!(broadcast_body_allocations, 1, "a broadcast allocates its body once");
    let broadcast_inflight_high_water = world.bus.stats().broadcast_inflight_high_water;
    assert!(
        broadcast_inflight_high_water > 1,
        "the measurement never had more than one leg in flight"
    );
    assert!(
        broadcast_inflight_high_water <= IrcLimits::default().broadcast_concurrency as u64,
        "broadcast concurrency must stay under its ceiling"
    );

    world
        .send("waiter", "peer", "question")
        .await
        .expect("direct send is accepted");
    let mut timeouts = Vec::new();
    for _ in 0..WAITERS / 2 {
        let bus = Arc::clone(&world.bus);
        timeouts.push(tokio::spawn(async move {
            bus.wait("waiter", direct(), Duration::from_secs(1), std::future::pending::<()>())
                .await
        }));
    }
    wait_for_waiters(&world.bus, (WAITERS / 2) as u64).await;
    tokio::time::advance(Duration::from_secs(2)).await;

    let mut timed_out = 0usize;
    for task in timeouts {
        match task.await.expect("timeout waiter") {
            WaitOutcome::Timeout => timed_out += 1,
            other => panic!("a waiter past its deadline must time out, got {}", describe(&other)),
        }
    }
    assert_eq!(
        timed_out,
        WAITERS / 2,
        "every parked waiter whose deadline passed times out"
    );

    let mut cancels = Vec::new();
    let mut cancel_signals = Vec::new();
    for _ in 0..WAITERS / 2 {
        let bus = Arc::clone(&world.bus);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        cancel_signals.push(tx);
        cancels.push(tokio::spawn(async move {
            bus.wait("waiter", direct(), Duration::from_secs(60), async move {
                drop(rx.await);
            })
            .await
        }));
    }
    wait_for_waiters(&world.bus, (WAITERS / 2) as u64).await;
    for tx in cancel_signals {
        assert!(tx.send(()).is_ok(), "the cancel receiver is still awaited");
    }

    let mut cancelled = 0usize;
    for task in cancels {
        match task.await.expect("cancelled waiter") {
            WaitOutcome::Cancelled => cancelled += 1,
            other => panic!("a waiter whose cancel fired must report it, got {}", describe(&other)),
        }
    }
    assert_eq!(
        cancelled,
        WAITERS / 2,
        "every parked waiter whose cancel fired reports it"
    );

    let waiters_live_after_churn = world.bus.stats().waiters_live;
    assert_eq!(waiters_live_after_churn, 0, "churn must not leave a waiter parked");

    println!(
        "STRUCTURAL_PERF section=irc source=irc-core mailbox_high_water={} mailbox_dropped={} broadcast_body_allocations={} waiters_live_after_churn={} broadcast_inflight_high_water={} flood={} broadcast_targets={}",
        after_flood.mailbox_high_water,
        after_flood.mailbox_dropped,
        broadcast_body_allocations,
        waiters_live_after_churn,
        broadcast_inflight_high_water,
        FLOOD,
        BROADCAST_TARGETS,
    );
}

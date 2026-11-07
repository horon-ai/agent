//! Wait latency tests: a parked waiter must not pay a timer round trip per reply.

use crate::IrcBus;
use crate::harness_tests::{Harness, World, describe, from_peer, harness};
use crate::message::{Deliver, DeliverResult, Directory, Endpoint, IrcLimits, IrcMessage, WaitOutcome};
use futures::future::BoxFuture;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Cancel future that signals once, on its first poll, then never resolves.
///
/// `wait` polls the cancel branch only after it has registered the waiter, so the
/// signal means the caller is parked and a reply can no longer miss the waiter.
struct Parked(Option<Arc<Notify>>);

impl Future for Parked {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if let Some(notify) = self.0.take() {
            notify.notify_one();
        }
        Poll::Pending
    }
}

/// Wraps the harness delivery double so a test can count `has_pending_input` calls.
struct CountingDeliver {
    world: Arc<World>,
    pending_calls: AtomicU64,
}

impl Deliver for CountingDeliver {
    fn deliver<'a>(
        &'a self,
        target: &'a Endpoint,
        message: &'a IrcMessage,
        from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult> {
        self.world.deliver(target, message, from_parent)
    }

    fn has_pending_input<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool> {
        self.pending_calls.fetch_add(1, Ordering::Relaxed);
        self.world.has_pending_input(agent)
    }

    fn can_respond<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool> {
        self.world.can_respond(agent)
    }
}

/// Parks `me` until `peer` answers, announcing the moment the waiter is registered.
async fn park(bus: &Arc<IrcBus>, me: &'static str, peer: &'static str, notify: Arc<Notify>) -> WaitOutcome {
    bus.wait(me, from_peer(peer), Duration::from_secs(5), Parked(Some(notify)))
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serial_waiter_receives_2000_replies_within_one_second() {
    let h = harness(IrcLimits::default())
        .add("a")
        .add("b")
        .responder("a", true)
        .responder("b", true);

    let started = Instant::now();
    for round in 0..2000 {
        // A fresh owed reply per round keeps the waiter from returning early.
        h.send("a", "b", "ping").await.expect("a to b");
        let notify = Arc::new(Notify::new());
        let parked = tokio::spawn({
            let bus = Arc::clone(&h.bus);
            let notify = Arc::clone(&notify);
            async move { park(&bus, "a", "b", notify).await }
        });
        notify.notified().await;
        h.send("b", "a", "pong").await.expect("b to a");
        let outcome = parked.await.expect("wait task");
        assert!(
            matches!(outcome, WaitOutcome::Message(_)),
            "round {round} must receive the reply, got {}",
            describe(&outcome)
        );
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "2000 serialized replies took {elapsed:?}, so a wait paid a timer round trip"
    );
}

#[tokio::test(start_paused = true)]
async fn ready_reply_does_not_wait_for_a_recheck_tick() {
    let world = World::new();
    let deliver = Arc::new(CountingDeliver {
        world: Arc::clone(&world),
        pending_calls: AtomicU64::new(0),
    });
    let bus = IrcBus::new(
        IrcLimits::default(),
        Arc::clone(&world) as Arc<dyn Directory>,
        Arc::clone(&deliver) as Arc<dyn Deliver>,
    );
    let h = Harness { world, bus }
        .add("a")
        .add("b")
        .responder("a", true)
        .responder("b", true);

    h.send("a", "b", "ping").await.expect("a to b");
    let notify = Arc::new(Notify::new());
    let parked = tokio::spawn({
        let bus = Arc::clone(&h.bus);
        let notify = Arc::clone(&notify);
        async move { park(&bus, "a", "b", notify).await }
    });
    notify.notified().await;

    let before = tokio::time::Instant::now();
    h.send("b", "a", "pong").await.expect("b to a");
    let outcome = parked.await.expect("wait task");
    let advanced = before.elapsed();
    assert!(
        matches!(outcome, WaitOutcome::Message(_)),
        "the reply must be handed over, got {}",
        describe(&outcome)
    );
    assert_eq!(
        advanced,
        Duration::ZERO,
        "a ready reply must not wait for the recheck timer"
    );
    let pending_calls = deliver.pending_calls.load(Ordering::Relaxed);
    assert!(
        pending_calls <= 1,
        "one wait must not poll for pending input more than once, saw {pending_calls}"
    );
}

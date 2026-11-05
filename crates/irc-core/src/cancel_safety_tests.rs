//! Cancel-safety tests.
//!
//! A `wait` future can be dropped at any await point: a tool task is aborted
//! when the user interrupts a turn. Dropping it must unregister the parked
//! waiter and must not swallow a reply that already reached its receiver.
//!
//! The other race is between `hand_off` and registration: a `send` that finds
//! no waiter goes through the delivery double and may end `Errored`. If a
//! receiver registers while that delivery is in flight, the buffered message
//! must go to the waiter instead of sitting in the mailbox.

use crate::IrcBus;
use crate::message::{
    Deliver, DeliverResult, Directory, Endpoint, IrcLimits, IrcMessage, Outcome, SendRequest, WaitFilter, WaitOutcome,
};
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone, Copy)]
enum Mode {
    Woken,
    Errored,
}

struct Inner {
    agents: Vec<Endpoint>,
    modes: HashMap<String, Mode>,
    responders: HashSet<String>,
    blocked: Option<String>,
}

/// In-memory directory plus a delivery double that can stall one target.
struct World {
    inner: Mutex<Inner>,
    entered: Notify,
    release: Notify,
}

impl World {
    fn new(agents: &[&str], responders: &[&str], modes: &[(&str, Mode)], blocked: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                agents: agents.iter().map(|id| endpoint(id)).collect(),
                modes: modes.iter().map(|(id, mode)| (id.to_string(), *mode)).collect(),
                responders: responders.iter().map(|id| id.to_string()).collect(),
                blocked: blocked.map(str::to_string),
            }),
            entered: Notify::new(),
            release: Notify::new(),
        })
    }

    fn mode(&self, id: &str) -> Mode {
        self.inner.lock().unwrap().modes.get(id).copied().unwrap_or(Mode::Woken)
    }
}

impl Directory for World {
    fn resolve<'a>(&'a self, _sender: &'a str, target: &'a str) -> BoxFuture<'a, Result<Endpoint, String>> {
        Box::pin(async move {
            let inner = self.inner.lock().unwrap();
            inner
                .agents
                .iter()
                .find(|agent| agent.id.as_ref() == target)
                .map(clone_endpoint)
                .ok_or_else(|| format!("unknown \"{target}\""))
        })
    }

    fn broadcast_targets<'a>(&'a self, _sender: &'a str) -> BoxFuture<'a, Result<Vec<Endpoint>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn roster<'a>(&'a self, _sender: &'a str) -> BoxFuture<'a, Vec<Endpoint>> {
        Box::pin(async { Vec::new() })
    }
}

impl Deliver for World {
    fn deliver<'a>(
        &'a self,
        target: &'a Endpoint,
        _message: &'a IrcMessage,
        _from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult> {
        Box::pin(async move {
            let mode = self.mode(target.id.as_ref());
            let blocked = self.inner.lock().unwrap().blocked.as_deref() == Some(target.id.as_ref());
            if blocked {
                self.entered.notify_one();
                self.release.notified().await;
            }
            match mode {
                Mode::Woken => DeliverResult::Woken,
                Mode::Errored => DeliverResult::Errored("hand-off failed".into()),
            }
        })
    }

    fn has_pending_input<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn can_respond<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { self.inner.lock().unwrap().responders.contains(agent) })
    }
}

fn endpoint(id: &str) -> Endpoint {
    Endpoint {
        id: Arc::from(id),
        handle: None,
        parent: None,
        status: Arc::from("running"),
    }
}

fn clone_endpoint(endpoint: &Endpoint) -> Endpoint {
    Endpoint {
        id: Arc::clone(&endpoint.id),
        handle: endpoint.handle.as_ref().map(Arc::clone),
        parent: endpoint.parent.as_ref().map(Arc::clone),
        status: Arc::clone(&endpoint.status),
    }
}

fn fixture(
    agents: &[&str],
    responders: &[&str],
    modes: &[(&str, Mode)],
    blocked: Option<&str>,
) -> (Arc<IrcBus>, Arc<World>) {
    let world = World::new(agents, responders, modes, blocked);
    let bus = IrcBus::new(
        IrcLimits::default(),
        Arc::clone(&world) as Arc<dyn Directory>,
        Arc::clone(&world) as Arc<dyn Deliver>,
    );
    (bus, world)
}

fn request(from: &str, to: &str, body: &str) -> SendRequest {
    SendRequest {
        from: Arc::from(from),
        to: to.to_string(),
        body: body.to_string(),
        reply_to: None,
        progress: false,
        wake_relay: false,
    }
}

async fn send(bus: &IrcBus, from: &str, to: &str, body: &str) -> crate::SendReport {
    bus.send(request(from, to, body))
        .await
        .expect("the double accepts every valid send")
}

fn from_peer(peer: &str) -> WaitFilter {
    WaitFilter {
        from: Some(Arc::from(peer)),
        direct_only: true,
    }
}

fn label(outcome: &WaitOutcome) -> &'static str {
    match outcome {
        WaitOutcome::Message(_) => "message",
        WaitOutcome::Timeout => "timeout",
        WaitOutcome::NothingToWaitFor => "nothing",
        WaitOutcome::Unanswerable(_) => "unanswerable",
        WaitOutcome::PendingInput => "pending",
        WaitOutcome::Cancelled => "cancelled",
    }
}

/// Cancel future that signals once, on its first poll, then never resolves.
///
/// `wait` polls the cancel branch only after it has registered its waiter, so
/// the signal proves the waiter is parked and an abort can no longer miss it.
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

fn parked(notify: &Arc<Notify>) -> Parked {
    Parked(Some(Arc::clone(notify)))
}

#[tokio::test]
async fn dropped_wait_future_removes_its_waiter() {
    let (bus, _world) = fixture(&["a", "b"], &["b"], &[], None);
    send(&bus, "a", "b", "question").await;

    let signal = Arc::new(Notify::new());
    let handle = tokio::spawn({
        let bus = Arc::clone(&bus);
        let signal = Arc::clone(&signal);
        async move {
            bus.wait("a", from_peer("b"), Duration::from_secs(60), parked(&signal))
                .await
        }
    });
    signal.notified().await;
    assert_eq!(
        bus.stats().waiters_live,
        1,
        "the waiter must be parked before the abort"
    );

    handle.abort();
    assert!(
        handle.await.is_err_and(|error| error.is_cancelled()),
        "the wait task must be cancelled"
    );
    assert_eq!(
        bus.stats().waiters_live,
        0,
        "a dropped wait future must unregister its waiter"
    );

    // Dropping the future from an outer timeout takes the same path.
    let dropped = tokio::time::timeout(
        Duration::from_millis(20),
        bus.wait("a", from_peer("b"), Duration::from_secs(60), std::future::pending()),
    )
    .await;
    assert!(dropped.is_err(), "the outer timeout must fire and drop the wait future");
    assert_eq!(
        bus.stats().waiters_live,
        0,
        "a wait dropped by an outer timeout must unregister its waiter"
    );
}

#[tokio::test]
async fn reply_after_dropped_wait_reaches_the_next_waiter() {
    let (bus, _world) = fixture(&["a", "b"], &["b"], &[], None);
    send(&bus, "a", "b", "question").await;

    let signal = Arc::new(Notify::new());
    let dead = tokio::spawn({
        let bus = Arc::clone(&bus);
        let signal = Arc::clone(&signal);
        async move {
            bus.wait("a", from_peer("b"), Duration::from_secs(60), parked(&signal))
                .await
        }
    });
    signal.notified().await;
    dead.abort();
    assert!(
        dead.await.is_err_and(|error| error.is_cancelled()),
        "the first wait must be gone"
    );
    assert_eq!(bus.stats().waiters_live, 0, "the dead waiter must not stay queued");

    let signal = Arc::new(Notify::new());
    let live = tokio::spawn({
        let bus = Arc::clone(&bus);
        let signal = Arc::clone(&signal);
        async move {
            bus.wait("a", from_peer("b"), Duration::from_secs(60), parked(&signal))
                .await
        }
    });
    signal.notified().await;

    let report = send(&bus, "b", "a", "answer").await;
    assert_eq!(report.delivered(), 1, "the reply must reach the live waiter");

    let outcome = tokio::time::timeout(Duration::from_secs(2), live)
        .await
        .expect("the live waiter settles")
        .expect("wait task");
    match outcome {
        WaitOutcome::Message(message) => assert_eq!(message.body.as_ref(), "answer"),
        other => panic!("the live waiter must receive the reply: got {}", label(&other)),
    }
    assert_eq!(bus.stats().waiters_live, 0);
    assert_eq!(bus.unread_count("a"), 0, "a handed-off reply must not be buffered too");
}

#[tokio::test]
async fn dropped_wait_after_handoff_keeps_the_message() {
    let (bus, _world) = fixture(&["a", "b"], &["b"], &[], None);
    send(&bus, "a", "b", "question").await;

    let signal = Arc::new(Notify::new());
    let handle = tokio::spawn({
        let bus = Arc::clone(&bus);
        let signal = Arc::clone(&signal);
        async move {
            bus.wait("a", from_peer("b"), Duration::from_secs(60), parked(&signal))
                .await
        }
    });
    signal.notified().await;

    let report = send(&bus, "b", "a", "answer").await;
    assert_eq!(report.delivered(), 1, "the reply is handed to the parked waiter");

    // Abort before the woken task is polled: the message sits in the abandoned
    // oneshot and must be recovered rather than lost.
    handle.abort();
    assert!(
        handle.await.is_err_and(|error| error.is_cancelled()),
        "the wait task must be cancelled"
    );
    assert_eq!(bus.stats().waiters_live, 0);

    let recovered = bus.take("a", None).expect("the abandoned reply must not be lost");
    assert_eq!(recovered.body.as_ref(), "answer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errored_delivery_racing_wait_registration_is_not_missed() {
    let (bus, world) = fixture(&["a", "b"], &["b"], &[("a", Mode::Errored)], Some("a"));
    send(&bus, "a", "b", "question").await;

    let sender = tokio::spawn({
        let bus = Arc::clone(&bus);
        async move { bus.send(request("b", "a", "late")).await }
    });
    world.entered.notified().await;

    // The reply is already inside the delivery double: no waiter exists yet.
    let signal = Arc::new(Notify::new());
    let waiter = tokio::spawn({
        let bus = Arc::clone(&bus);
        let signal = Arc::clone(&signal);
        async move {
            bus.wait("a", from_peer("b"), Duration::from_secs(5), parked(&signal))
                .await
        }
    });
    signal.notified().await;

    world.release.notify_one();
    let report = tokio::time::timeout(Duration::from_secs(2), sender)
        .await
        .expect("the send settles")
        .expect("send task")
        .expect("valid send");
    assert!(
        report
            .receipts
            .iter()
            .any(|receipt| !matches!(receipt.outcome, Outcome::Failed(_))),
        "a registration that raced the errored delivery counts as delivered"
    );

    let outcome = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("the waiter settles")
        .expect("wait task");
    match outcome {
        WaitOutcome::Message(message) => assert_eq!(message.body.as_ref(), "late"),
        other => panic!(
            "the registered waiter must not miss the buffered delivery: got {}",
            label(&other)
        ),
    }
    assert_eq!(
        bus.unread_count("a"),
        0,
        "a handed-off message must not also be buffered"
    );
    assert_eq!(bus.stats().waiters_live, 0);
}

#[tokio::test]
async fn recovered_message_keeps_sender_order_in_mailbox() {
    let (bus, _world) = fixture(&["a", "b"], &["b"], &[("a", Mode::Errored)], None);
    send(&bus, "a", "b", "question").await;

    let signal = Arc::new(Notify::new());
    let handle = tokio::spawn({
        let bus = Arc::clone(&bus);
        let signal = Arc::clone(&signal);
        async move {
            bus.wait("a", from_peer("b"), Duration::from_secs(60), parked(&signal))
                .await
        }
    });
    signal.notified().await;

    let first = send(&bus, "b", "a", "one").await;
    assert_eq!(first.delivered(), 1, "the parked waiter takes the first reply");
    let second = send(&bus, "b", "a", "two").await;
    assert!(
        second
            .receipts
            .iter()
            .all(|receipt| matches!(receipt.outcome, Outcome::Failed(_))),
        "the second reply has no waiter left and is buffered"
    );

    // Abort hands the first reply back after the second one was buffered: the
    // recovered message must sit before it, keeping the sender's order.
    handle.abort();
    assert!(
        handle.await.is_err_and(|error| error.is_cancelled()),
        "the wait task must be cancelled"
    );

    let bodies: Vec<String> = bus
        .drain("a", None)
        .into_iter()
        .map(|message| message.body.to_string())
        .collect();
    assert_eq!(
        bodies,
        ["one", "two"],
        "the recovered reply keeps its seq position ahead of the buffered one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_during_handoff_never_loses_the_reply() {
    // The interleaving inside `Drop` cannot be paused from a test, so this is a
    // stress check of the lock-serialized invariant: a reply either reaches the
    // waiter or lands in the mailbox exactly once, never neither and never both.
    let (bus, _world) = fixture(&["a", "b"], &["b"], &[("a", Mode::Errored)], None);
    send(&bus, "a", "b", "question").await;

    for round in 0..200 {
        // Every round needs a fresh direct message from `a` so `a` still owes
        // `b` a reply; otherwise the wait returns before it parks.
        send(&bus, "a", "b", "question").await;
        let signal = Arc::new(Notify::new());
        let waiter = tokio::spawn({
            let bus = Arc::clone(&bus);
            let signal = Arc::clone(&signal);
            async move {
                bus.wait("a", from_peer("b"), Duration::from_secs(60), parked(&signal))
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), signal.notified())
            .await
            .expect("the waiter must park");

        let sender = tokio::spawn({
            let bus = Arc::clone(&bus);
            async move { bus.send(request("b", "a", "reply")).await }
        });
        waiter.abort();
        let received = match waiter.await {
            Ok(WaitOutcome::Message(_)) => 1,
            _ => 0,
        };
        let report = sender.await.expect("send task").expect("valid send");
        assert!(report.delivered() <= 1, "one target, one receipt");
        let buffered = bus.drain("a", None);
        assert!(
            buffered.iter().all(|message| message.body.as_ref() == "reply"),
            "only the reply may be buffered"
        );
        assert_eq!(
            received + buffered.len(),
            1,
            "round {round}: the reply must reach the waiter or the mailbox exactly once"
        );
    }
    assert_eq!(bus.stats().waiters_live, 0);
}

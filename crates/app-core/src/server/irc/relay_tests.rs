use super::relay;
use super::test_support::{Fixture, NeverDeliver, TestEnv, install_bus, irc_message, member};
use futures::future::BoxFuture;
use horon_agent_irc_core::{Deliver, DeliverResult, Endpoint, IrcLimits, IrcMessage, SendRequest};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

/// Counts delivered legs and accepts each one without buffering it.
struct CountingDeliver {
    calls: Arc<AtomicUsize>,
}

impl Deliver for CountingDeliver {
    fn deliver<'a>(
        &'a self,
        _target: &'a Endpoint,
        _message: &'a IrcMessage,
        _from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            DeliverResult::Injected
        })
    }

    fn has_pending_input<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn can_respond<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
}

async fn pending_map() -> Arc<StdMutex<HashMap<Arc<str>, usize>>> {
    Arc::new(StdMutex::new(HashMap::new()))
}

#[tokio::test]
async fn relay_is_skipped_when_target_already_replied() {
    let _env = TestEnv::new("");
    let fixture = Fixture::new();
    let (target_member, _target_rx) = member("target", Some("swarm-a"), "ready", Some("bear"), None);
    let (sender_member, _sender_rx) = member("sender", Some("swarm-a"), "running", Some("falcon"), None);
    fixture.add(target_member).await;
    fixture.add(sender_member).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let bus = install_bus(
        Arc::new(fixture.directory()),
        Arc::new(CountingDeliver {
            calls: Arc::clone(&calls),
        }),
        IrcLimits::default(),
    );

    let direct = bus
        .send(SendRequest {
            from: Arc::from("target"),
            to: "sender".to_string(),
            body: "already answered".to_string(),
            reply_to: None,
            progress: false,
            wake_relay: false,
        })
        .await
        .expect("the direct answer should be accepted");
    assert_eq!(direct.delivered(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let pending = pending_map().await;
    pending.lock().expect("pending lock").insert(Arc::from("target"), 1);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let message = irc_message(90, 0, "sender", "target");
    relay::spawn(rx, message, Arc::clone(&pending));
    tx.send(Some("relayed answer".to_string()))
        .expect("relay receiver should be live");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let released = match pending.lock() {
                Ok(relays) => relays.is_empty(),
                Err(poisoned) => poisoned.into_inner().is_empty(),
            };
            if released {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the relay mark should be released");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "an already-answered message must not relay"
    );
    assert_eq!(bus.unread_count("sender"), 0);
}

#[tokio::test]
async fn removed_session_is_forgotten() {
    let _env = TestEnv::new("");
    let fixture = Fixture::new();
    let (peer_member, _peer_rx) = member("peer", Some("swarm-a"), "running", Some("falcon"), None);
    let (victim_member, _victim_rx) = member("victim", Some("swarm-a"), "running", Some("bear"), None);
    fixture.add(peer_member).await;
    fixture.add(victim_member).await;
    let bus = install_bus(
        Arc::new(fixture.directory()),
        Arc::new(NeverDeliver),
        IrcLimits::default(),
    );

    bus.send(SendRequest {
        from: Arc::from("peer"),
        to: "victim".to_string(),
        body: "never read".to_string(),
        reply_to: None,
        progress: false,
        wake_relay: false,
    })
    .await
    .expect("the offline session keeps the message");
    assert_eq!(bus.unread_count("victim"), 1);

    crate::server::forget_removed_session("victim", None);

    assert_eq!(
        bus.unread_count("victim"),
        0,
        "removing a session must drop its mailbox"
    );
}

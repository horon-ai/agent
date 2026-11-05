use super::test_support::{
    Fixture, TestEnv, endpoint_of, install_bus, irc_message, member, quiet_agent, scripted_agent, sessions_of,
};
use crate::protocol::ServerEvent;
use crate::server::{SessionInterruptQueues, register_session_interrupt_queue};
use horon_agent_irc_core::{Deliver, DeliverResult, IrcLimits, Outcome, SendRequest};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

async fn wait_for_status(fixture: &Fixture, session_id: &str, status: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reached = fixture
                .members
                .read()
                .await
                .get(session_id)
                .is_some_and(|member| member.status == status);
            if reached {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("member should reach the expected status");
}

#[tokio::test]
async fn idle_target_is_woken_and_relay_reaches_sender() {
    let _env = TestEnv::new("");
    let sender = quiet_agent().await;
    let sender_id = sender.lock().await.session_id().to_string();
    let sender_queue = sender.lock().await.soft_interrupt_queue();
    let target = scripted_agent(vec!["relayed answer".to_string()]).await;
    let target_id = target.lock().await.session_id().to_string();

    let fixture = Fixture::new();
    let (sender_member, _sender_rx) = member(&sender_id, Some("swarm-a"), "running", Some("falcon"), None);
    let (target_member, _target_rx) = member(&target_id, Some("swarm-a"), "ready", Some("bear"), None);
    fixture.add(sender_member).await;
    fixture.add(target_member).await;

    let sessions = sessions_of(&[(&sender_id, Arc::clone(&sender)), (&target_id, Arc::clone(&target))]);
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    register_session_interrupt_queue(&queues, &sender_id, sender_queue.clone()).await;
    let deliver = Arc::new(fixture.deliver(Arc::clone(&sessions), Arc::clone(&queues)));
    let bus = install_bus(Arc::new(fixture.directory()), deliver.clone(), IrcLimits::default());

    let _sender_busy = sender.lock().await;
    let report = bus
        .send(SendRequest {
            from: Arc::from(sender_id.as_str()),
            to: target_id.clone(),
            body: "hello".to_string(),
            reply_to: None,
            progress: false,
            wake_relay: false,
        })
        .await
        .expect("send should reach the idle target");
    assert_eq!(report.delivered(), 1);
    assert!(matches!(report.receipts[0].outcome, Outcome::Woken));

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let relayed = match sender_queue.lock() {
                Ok(pending) => pending.iter().any(|message| message.content.contains("relayed answer")),
                Err(poisoned) => poisoned
                    .into_inner()
                    .iter()
                    .any(|message| message.content.contains("relayed answer")),
            };
            if relayed {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the wake relay should reach the sender");

    let content = match sender_queue.lock() {
        Ok(pending) => pending
            .last()
            .map(|message| message.content.clone())
            .unwrap_or_default(),
        Err(poisoned) => poisoned
            .into_inner()
            .last()
            .map(|message| message.content.clone())
            .unwrap_or_default(),
    };
    assert!(
        content.contains("wake_relay=\"true\""),
        "relay envelope missing wake_relay: {content}"
    );
    assert!(
        content.contains(&format!("from=\"{target_id}\"")),
        "relay envelope has the wrong sender: {content}"
    );
    assert!(content.contains("relayed answer"));

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let released = match deliver.pending_relays().lock() {
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
    .expect("the pending relay mark should be released");
}

#[tokio::test]
async fn envelope_handle_names_the_sender_not_the_target() {
    let _env = TestEnv::new("");
    let sender = quiet_agent().await;
    let sender_id = sender.lock().await.session_id().to_string();
    let target = scripted_agent(vec!["ack".to_string()]).await;
    let target_id = target.lock().await.session_id().to_string();

    let fixture = Fixture::new();
    let (sender_member, _sender_rx) = member(&sender_id, Some("swarm-a"), "running", Some("falcon"), None);
    let (target_member, _target_rx) = member(&target_id, Some("swarm-a"), "ready", Some("bear"), None);
    fixture.add(sender_member).await;
    fixture.add(target_member).await;

    let sessions = sessions_of(&[(&sender_id, Arc::clone(&sender)), (&target_id, Arc::clone(&target))]);
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let deliver = Arc::new(fixture.deliver(Arc::clone(&sessions), Arc::clone(&queues)));
    let bus = install_bus(Arc::new(fixture.directory()), deliver, IrcLimits::default());

    let _sender_busy = sender.lock().await;
    let report = bus
        .send(SendRequest {
            from: Arc::from(sender_id.as_str()),
            to: target_id.clone(),
            body: "hello".to_string(),
            reply_to: None,
            progress: false,
            wake_relay: false,
        })
        .await
        .expect("send should reach the idle target");
    assert_eq!(report.delivered(), 1);

    let prompt = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = match target.try_lock() {
                Ok(agent) => agent
                    .messages()
                    .iter()
                    .flat_map(|message| message.content.iter())
                    .filter_map(|block| match block {
                        crate::message::ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .find(|text| text.contains("<irc ")),
                Err(_) => None,
            };
            if let Some(prompt) = found {
                return prompt;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the target session should receive the IRC envelope");

    assert!(
        prompt.contains("handle=\"falcon\""),
        "the envelope must carry the sender's handle: {prompt}"
    );
    assert!(
        !prompt.contains("handle=\"bear\""),
        "the envelope must not carry the receiver's handle: {prompt}"
    );
}

#[tokio::test]
async fn busy_peer_gets_non_urgent_soft_interrupt() {
    let _env = TestEnv::new("");
    let sender = quiet_agent().await;
    let sender_id = sender.lock().await.session_id().to_string();
    let target = quiet_agent().await;
    let target_id = target.lock().await.session_id().to_string();
    let target_queue = target.lock().await.soft_interrupt_queue();

    let fixture = Fixture::new();
    fixture
        .add(member(&target_id, Some("swarm-a"), "running", Some("bear"), None).0)
        .await;
    let sessions = sessions_of(&[(&target_id, Arc::clone(&target))]);
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    register_session_interrupt_queue(&queues, &target_id, target_queue.clone()).await;
    let deliver = fixture.deliver(sessions, queues);

    let _target_busy = target.lock().await;
    let endpoint = endpoint_of(&fixture.members.read().await[&target_id]);
    let message = irc_message(7, 3, &sender_id, &target_id);
    let result = deliver.deliver(&endpoint, &message, false).await;
    assert!(matches!(result, DeliverResult::Injected));

    match target_queue.lock() {
        Ok(pending) => {
            assert_eq!(pending.len(), 1);
            assert!(!pending[0].urgent, "a peer message must not be urgent");
            assert!(pending[0].content.contains("<irc id=\"7\""));
        }
        Err(poisoned) => panic!("queue poisoned: {}", poisoned.into_inner().len()),
    }
}

#[tokio::test]
async fn busy_child_gets_urgent_soft_interrupt_from_parent() {
    let _env = TestEnv::new("");
    let parent = quiet_agent().await;
    let parent_id = parent.lock().await.session_id().to_string();
    let child = quiet_agent().await;
    let child_id = child.lock().await.session_id().to_string();
    let child_queue = child.lock().await.soft_interrupt_queue();

    let fixture = Fixture::new();
    fixture
        .add(member(&child_id, Some("swarm-a"), "running", Some("bear"), Some(&parent_id)).0)
        .await;
    let sessions = sessions_of(&[(&child_id, Arc::clone(&child))]);
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    register_session_interrupt_queue(&queues, &child_id, child_queue.clone()).await;
    let deliver = fixture.deliver(sessions, queues);

    let _child_busy = child.lock().await;
    let endpoint = endpoint_of(&fixture.members.read().await[&child_id]);
    let message = irc_message(11, 4, &parent_id, &child_id);
    let from_parent = endpoint.parent.as_deref() == Some(parent_id.as_str());
    let result = deliver.deliver(&endpoint, &message, from_parent).await;
    assert!(matches!(result, DeliverResult::Injected));

    match child_queue.lock() {
        Ok(pending) => {
            assert_eq!(pending.len(), 1);
            assert!(pending[0].urgent, "a direct message from the parent must be urgent");
        }
        Err(poisoned) => panic!("queue poisoned: {}", poisoned.into_inner().len()),
    }
}

#[tokio::test]
async fn external_wake_mode_emits_wake_requested() {
    let _env = TestEnv::new("[server]\nwake_mode = \"external\"\n");
    let target = quiet_agent().await;
    let target_id = target.lock().await.session_id().to_string();

    let fixture = Fixture::new();
    let (target_member, mut target_rx) = member(&target_id, Some("swarm-a"), "ready", Some("bear"), None);
    fixture.add(target_member).await;
    let sessions = sessions_of(&[(&target_id, Arc::clone(&target))]);
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let deliver = fixture.deliver(sessions, queues);

    let endpoint = endpoint_of(&fixture.members.read().await[&target_id]);
    let message = irc_message(3, 1, "sender", &target_id);
    let result = deliver.deliver(&endpoint, &message, false).await;
    assert!(matches!(result, DeliverResult::Woken));

    let event = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
        .await
        .expect("a wake request should be fanned out")
        .expect("the member channel should stay open");
    match event {
        ServerEvent::WakeRequested {
            session_id,
            reason,
            notification,
        } => {
            assert_eq!(session_id, target_id);
            assert_eq!(reason, "irc_delivery");
            assert!(notification.contains("<irc id=\"3\""));
        }
        _ => panic!("expected a wake request event"),
    }
    assert!(
        match deliver.pending_relays().lock() {
            Ok(relays) => relays.is_empty(),
            Err(poisoned) => poisoned.into_inner().is_empty(),
        },
        "external wake mode must not track a relay"
    );
}

#[tokio::test]
async fn broadcast_and_relay_wakes_do_not_relay() {
    let _env = TestEnv::new("");
    let target = scripted_agent(vec!["first answer".to_string(), "second answer".to_string()]).await;
    let target_id = target.lock().await.session_id().to_string();

    let fixture = Fixture::new();
    let (sender_member, _sender_rx) = member("sender", Some("swarm-a"), "running", Some("falcon"), None);
    let (target_member, _target_rx) = member(&target_id, Some("swarm-a"), "ready", Some("bear"), None);
    fixture.add(sender_member).await;
    fixture.add(target_member).await;
    let sessions = sessions_of(&[(&target_id, Arc::clone(&target))]);
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let deliver = fixture.deliver(sessions, queues);
    let endpoint = endpoint_of(&fixture.members.read().await[&target_id]);

    let mut broadcast = irc_message(21, 1, "sender", &target_id);
    broadcast.broadcast = true;
    let result = deliver.deliver(&endpoint, &broadcast, false).await;
    assert!(matches!(result, DeliverResult::Woken));
    assert_eq!(
        match deliver.pending_relays().lock() {
            Ok(relays) => relays.get(&endpoint.id).copied(),
            Err(poisoned) => poisoned.into_inner().get(&endpoint.id).copied(),
        },
        None,
        "a broadcast wake must not schedule a relay"
    );
    wait_for_status(&fixture, &target_id, "ready").await;

    let mut relayed = irc_message(22, 2, "sender", &target_id);
    relayed.wake_relay = true;
    let result = deliver.deliver(&endpoint, &relayed, false).await;
    assert!(!matches!(result, DeliverResult::Errored(_)));
    assert_eq!(
        match deliver.pending_relays().lock() {
            Ok(relays) => relays.get(&endpoint.id).copied(),
            Err(poisoned) => poisoned.into_inner().get(&endpoint.id).copied(),
        },
        None,
        "a relayed wake must not schedule another relay"
    );
    wait_for_status(&fixture, &target_id, "ready").await;
}

#[tokio::test]
async fn ready_target_without_pending_relay_cannot_respond() {
    let _env = TestEnv::new("");
    let target = quiet_agent().await;
    let target_id = target.lock().await.session_id().to_string();

    let fixture = Fixture::new();
    fixture
        .add(member(&target_id, Some("swarm-a"), "ready", Some("bear"), None).0)
        .await;
    fixture
        .add(member("busy", Some("swarm-a"), "running", Some("otter"), None).0)
        .await;
    let sessions = sessions_of(&[(&target_id, Arc::clone(&target))]);
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let deliver = fixture.deliver(sessions, queues);

    assert!(
        !deliver.can_respond(&target_id).await,
        "a ready session with no relay cannot respond"
    );
    assert!(deliver.can_respond("busy").await, "a running session can still respond");
    assert!(!deliver.can_respond("missing").await, "an unknown agent cannot respond");
}

use super::test_support::{Fixture, NeverDeliver, TestEnv, install_bus, member};
use horon_agent_irc_core::{Directory, IrcBus, IrcError, IrcLimits, SendRequest, format_envelope};
use std::sync::Arc;

async fn fixture_with_swarm() -> Fixture {
    let fixture = Fixture::new();
    let (root, _root_rx) = member("root", Some("swarm-a"), "ready", Some("falcon"), None);
    let (child, _child_rx) = member("child", Some("swarm-a"), "running", Some("bear"), Some("root"));
    let (grandchild, _grandchild_rx) = member("grandchild", Some("swarm-a"), "running", Some("otter"), Some("child"));
    let (stopped, _stopped_rx) = member("stopped", Some("swarm-a"), "stopped", Some("gone"), Some("root"));
    let (other, _other_rx) = member("other", Some("swarm-b"), "ready", Some("wolf"), None);
    fixture.add(root).await;
    fixture.add(child).await;
    fixture.add(grandchild).await;
    fixture.add(stopped).await;
    fixture.add(other).await;
    fixture
}

fn bus_over(fixture: &Fixture) -> Arc<IrcBus> {
    install_bus(
        Arc::new(fixture.directory()),
        Arc::new(NeverDeliver),
        IrcLimits::default(),
    )
}

#[tokio::test]
async fn handle_and_session_id_both_resolve() {
    let fixture = fixture_with_swarm().await;
    let directory = fixture.directory();

    let by_handle = directory.resolve("root", "bear").await.expect("handle should resolve");
    assert_eq!(by_handle.id.as_ref(), "child");
    assert_eq!(by_handle.handle.as_deref(), Some("bear"));
    assert_eq!(by_handle.parent.as_deref(), Some("root"));

    let by_session = directory
        .resolve("root", "child")
        .await
        .expect("session id should resolve");
    assert_eq!(by_session.id.as_ref(), "child");

    let roster = directory.roster("root").await;
    let ids: Vec<&str> = roster.iter().map(|endpoint| endpoint.id.as_ref()).collect();
    assert_eq!(ids, vec!["child", "grandchild", "stopped"]);
}

#[tokio::test]
async fn unknown_target_fails_without_mailbox() {
    let _env = TestEnv::new("");
    let fixture = fixture_with_swarm().await;
    let bus = bus_over(&fixture);

    let error = bus
        .send(SendRequest {
            from: Arc::from("root"),
            to: "nobody".to_string(),
            body: "hello".to_string(),
            reply_to: None,
            progress: false,
            wake_relay: false,
        })
        .await
        .expect_err("an unknown target must fail");
    match error {
        IrcError::Directory(message) => assert_eq!(message, "Unknown agent \"nobody\" - use irc action=list"),
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(bus.unread_count("root"), 0);
    assert_eq!(bus.unread_count("nobody"), 0);
}

#[tokio::test]
async fn child_broadcast_is_rejected() {
    let fixture = fixture_with_swarm().await;
    let directory = fixture.directory();

    let error = directory
        .broadcast_targets("child")
        .await
        .err()
        .expect("a child session must not broadcast");
    assert_eq!(error, "only a root session can broadcast");
}

#[tokio::test]
async fn other_swarm_target_is_not_visible() {
    let fixture = fixture_with_swarm().await;
    let directory = fixture.directory();

    let error = directory
        .resolve("root", "wolf")
        .await
        .err()
        .expect("a peer in another swarm is not visible");
    assert_eq!(error, "agent \"wolf\" is not visible to this session");

    let error = directory
        .resolve("root", "other")
        .await
        .err()
        .expect("a session in another swarm is not visible");
    assert_eq!(error, "agent \"other\" is not visible to this session");
}

#[tokio::test]
async fn root_broadcast_reaches_descendants_only() {
    let fixture = fixture_with_swarm().await;
    let directory = fixture.directory();

    let targets = directory.broadcast_targets("root").await.expect("root may broadcast");
    let ids: Vec<&str> = targets.iter().map(|endpoint| endpoint.id.as_ref()).collect();
    assert_eq!(ids, vec!["child", "grandchild"]);

    let envelope = format_envelope(
        &horon_agent_irc_core::IrcMessage {
            id: horon_agent_irc_core::MessageId(1),
            seq: 1,
            from: Arc::from("root"),
            to: Arc::from("child"),
            body: Arc::from("hi"),
            ts_ms: 0,
            reply_to: None,
            broadcast: true,
            progress: false,
            wake_relay: false,
        },
        targets[0].handle.as_deref(),
    );
    assert!(envelope.contains("broadcast=\"true\""));
}

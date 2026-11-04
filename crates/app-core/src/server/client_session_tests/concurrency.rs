use super::*;

#[tokio::test]
async fn comm_status_releases_member_guard_before_reading_connections() {
    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        ("caller".to_string(), test_swarm_member("caller", "ready")),
        ("target".to_string(), test_swarm_member("target", "ready")),
    ])));
    let client_connections = Arc::new(RwLock::new(HashMap::<String, ClientConnectionInfo>::new()));
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    // Disconnect cleanup holds client_connections.write, then needs swarm_members.write.
    let cleanup_connections = client_connections.write().await;
    let status = tokio::spawn({
        let (swarm_members, client_connections) = (Arc::clone(&swarm_members), Arc::clone(&client_connections));
        async move {
            crate::server::comm_sync::handle_comm_status(
                1,
                "caller".to_string(),
                "target".to_string(),
                &sessions,
                &swarm_members,
                &client_connections,
                &FileTouchService::new(),
                &event_tx,
            )
            .await;
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let _members = tokio::time::timeout(std::time::Duration::from_secs(2), swarm_members.write())
        .await
        .expect("comm status must not hold swarm_members while waiting on client_connections");
    drop(_members);
    drop(cleanup_connections);
    status.await.unwrap();
    assert!(matches!(
        event_rx.recv().await,
        Some(ServerEvent::CommStatusResponse { .. })
    ));
}

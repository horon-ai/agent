//! The bridge must end with the terminal that started it (#429).
//!
//! It used to accept forever: the first bridge a terminal started kept the
//! socket path through its client quitting and its terminal closing, and the
//! single-instance lock made it the survivor of every later launch, so after an
//! upgrade the operator's next client reconnected to the previous release's
//! executable. These tests drive the lifecycle with a test-sized idle window
//! and an injected parent, which is the same seam the write deadline uses.
use super::*;
use horon_agent_platform::ipc::Listener;

/// A socket path unique to this process and iteration, short enough for the
/// 104-byte Unix socket limit macOS enforces.
fn socket_path(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "horon-bridge-life-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000_000
    ));
    std::fs::create_dir_all(&root).unwrap();
    root.join("api.sock")
}

/// Connect once the bridge is listening. A spawn that has not reached `bind`
/// yet refuses the connection, so this retries instead of racing it.
async fn connect_when_listening(api_socket: &std::path::Path) -> Stream {
    for _ in 0..200 {
        if let Ok(stream) = Stream::connect(api_socket).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the bridge never listened on {}", api_socket.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_exits_when_idle() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let api_socket = socket_path("idle");
        let legacy_socket = api_socket.with_file_name("legacy.sock");
        let idle_exit = Duration::from_millis(400);
        let bridge = tokio::spawn(run_bridge_with_lifecycle(
            api_socket.clone(),
            legacy_socket,
            writer::WRITE_STALL_DEADLINE,
            idle_exit,
            // No parent: the idle window alone has to end this bridge, which is
            // the only signal Windows has.
            None,
        ));

        // The client's ordinary shape: connect, then go away.
        drop(connect_when_listening(&api_socket).await);

        let finished = tokio::time::timeout(idle_exit * 10, bridge)
            .await
            .expect("the bridge ended within the idle window");
        finished.expect("the bridge task did not panic").expect("the bridge returned Ok");

        if horon_agent_platform::ipc::socket_nodes_can_be_stale() {
            assert!(!api_socket.exists(), "the bridge left its socket node behind");
        }
        // The next launch must be able to take the path: the node is gone and
        // the single-instance lock was released.
        let rebound = Listener::bind(&api_socket).expect("a new bridge can bind after the idle exit");
        drop(rebound);
    })
    .await
    .expect("the lifecycle test timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_keeps_serving_a_connected_client() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let api_socket = socket_path("connected");
        let legacy_socket = api_socket.with_file_name("legacy.sock");
        let idle_exit = Duration::from_millis(300);
        let bridge = tokio::spawn(run_bridge_with_lifecycle(
            api_socket.clone(),
            legacy_socket,
            writer::WRITE_STALL_DEADLINE,
            idle_exit,
            None,
        ));

        let client = connect_when_listening(&api_socket).await;
        tokio::time::sleep(idle_exit * 4).await;
        assert!(!bridge.is_finished(), "a bridge with a client must keep serving past the idle window");

        drop(client);
        let finished = tokio::time::timeout(idle_exit * 10, bridge)
            .await
            .expect("the bridge ended after its last client left");
        finished.expect("the bridge task did not panic").expect("the bridge returned Ok");
    })
    .await
    .expect("the lifecycle test timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_bridge_exits_on_the_lock_while_the_first_serves() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let api_socket = socket_path("lock");
        let legacy_socket = api_socket.with_file_name("legacy.sock");
        let first = tokio::spawn(run_bridge_with_lifecycle(
            api_socket.clone(),
            legacy_socket.clone(),
            writer::WRITE_STALL_DEADLINE,
            Duration::from_secs(60),
            None,
        ));
        let client = connect_when_listening(&api_socket).await;

        // The spawn a second client makes while the first bridge is live: it
        // must lose to the single-owner lock, not unlink the socket the live
        // client is using.
        let second = tokio::time::timeout(
            Duration::from_secs(5),
            run_bridge_with_lifecycle(
                api_socket.clone(),
                legacy_socket,
                writer::WRITE_STALL_DEADLINE,
                Duration::from_secs(60),
                None,
            ),
        )
        .await
        .expect("the second bridge exited instead of serving");
        second.expect("the losing bridge reports success");
        assert!(!first.is_finished(), "the live bridge must keep serving");

        drop(client);
        first.abort();
    })
    .await
    .expect("the lifecycle test timed out");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn bridge_exits_when_its_parent_is_gone() {
    tokio::time::timeout(Duration::from_secs(30), async {
        // A process that has already exited and been reaped: its pid answers
        // "gone" to the platform's liveness check.
        let mut child = std::process::Command::new("sleep").arg("0").spawn().expect("spawn a short-lived child");
        let dead_parent = child.id();
        child.wait().expect("reap the short-lived child");

        let api_socket = socket_path("parent");
        let legacy_socket = api_socket.with_file_name("legacy.sock");
        let bridge = tokio::spawn(run_bridge_with_lifecycle(
            api_socket,
            legacy_socket,
            writer::WRITE_STALL_DEADLINE,
            // Long enough that only the parent check can end the bridge.
            Duration::from_secs(600),
            Some(dead_parent),
        ));

        let finished = tokio::time::timeout(Duration::from_secs(10), bridge)
            .await
            .expect("a bridge whose parent is gone ended without waiting for the idle window");
        finished.expect("the bridge task did not panic").expect("the bridge returned Ok");
    })
    .await
    .expect("the lifecycle test timed out");
}

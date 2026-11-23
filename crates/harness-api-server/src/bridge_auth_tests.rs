//! The opt-in TCP exposure authenticates before it does anything else.
//!
//! Every test here observes the boundary the security contract names: a
//! connection without a valid credential is closed before the daemon is
//! dialled, the first frame is never read, and nothing is written back. The
//! valid-token tests drive the ordinary API afterwards, which is what keeps the
//! handshake honest: authentication must not change the frames that follow it.
use super::*;
use crate::credentials::CredentialStore;
use crate::writer::WRITE_STALL_DEADLINE;
use horon_agent_transport::{Listener, Stream};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_nanos()
        % 1_000_000_000;
    let root = std::env::temp_dir().join(format!("horon-api-auth-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

fn store_in(root: &Path) -> CredentialStore {
    CredentialStore::at(root.join("harness-api").join("credentials.json"))
}

fn hello_line() -> String {
    "{\"id\":1,\"req\":\"hello\",\"min_version\":1,\"max_version\":1,\"client\":\"auth-test\"}\n".to_string()
}

/// A stand-in daemon that answers the bridge's capability probe and counts the
/// connections it accepted: "the daemon was never dialled" is that count.
async fn fake_daemon(root: &Path) -> (PathBuf, Arc<AtomicUsize>) {
    let socket = root.join("daemon.sock");
    let mut listener = Listener::bind(&socket).expect("bind the fake daemon");
    let accepts = Arc::new(AtomicUsize::new(0));
    let counter = accepts.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                loop {
                    line.clear();
                    let Ok(read) = reader.read_line(&mut line).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                        continue;
                    };
                    if value["type"] == "ping" {
                        let reply = serde_json::json!({
                            "type": "pong",
                            "id": value["id"].clone(),
                            "capabilities": [],
                        });
                        if crate::write_json_line(&mut write_half, &reply).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    (socket, accepts)
}

/// One bridge connection over an in-memory duplex, the way a TCP connection is
/// handed to the auth layer.
fn spawn_duplex(
    store: CredentialStore,
    legacy_socket: PathBuf,
    auth_deadline: Duration,
) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<anyhow::Result<()>>) {
    let (client, bridge) = tokio::io::duplex(8192);
    let handle = tokio::spawn(serve_client(
        bridge,
        legacy_socket,
        store,
        auth_deadline,
        WRITE_STALL_DEADLINE,
        None,
    ));
    (client, handle)
}

/// Read every byte a rejected connection produces; it must be none.
async fn expect_silent_close(mut client: tokio::io::DuplexStream) {
    let mut answer = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(5),
        AsyncReadExt::read_to_end(&mut client, &mut answer),
    )
    .await
    .expect("the refused connection must be closed, not left hanging");
    // Closing with the peer's line still unread makes the kernel report a reset
    // on some platforms; the connection is over either way and carried no frame.
    match read {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("reading the closed connection: {error}"),
    }
    assert!(
        answer.is_empty(),
        "an unauthenticated peer must receive no frame: {:?}",
        String::from_utf8_lossy(&answer)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_authorization_line_closes_before_the_daemon_is_dialled() {
    let root = scratch("missing");
    let (daemon, accepts) = fake_daemon(&root).await;
    let store = store_in(&root);
    store.issue(None).expect("issue a credential");
    let (mut client, task) = spawn_duplex(store, daemon, AUTH_DEADLINE);
    client
        .write_all(hello_line().as_bytes())
        .await
        .expect("write hello first");
    expect_silent_close(client).await;
    task.await
        .expect("auth task")
        .expect("a refused connection ends cleanly");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(accepts.load(Ordering::SeqCst), 0, "the daemon must never be dialled");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_token_closes_before_the_daemon_is_dialled() {
    let root = scratch("wrong");
    let (daemon, accepts) = fake_daemon(&root).await;
    let store = store_in(&root);
    store.issue(None).expect("issue a credential");
    let (mut client, task) = spawn_duplex(store, daemon, AUTH_DEADLINE);
    let wrong = format!("Authorization: Bearer {}\n{}", "0".repeat(64), hello_line());
    client.write_all(wrong.as_bytes()).await.expect("write a wrong token");
    expect_silent_close(client).await;
    task.await
        .expect("auth task")
        .expect("a refused connection ends cleanly");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(accepts.load(Ordering::SeqCst), 0, "the daemon must never be dialled");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_token_is_refused_on_the_next_connection_without_a_restart() {
    let root = scratch("revoke");
    let (daemon, accepts) = fake_daemon(&root).await;
    let store = store_in(&root);
    let issued = store.issue(Some("cli")).expect("issue a credential");
    let api_socket = root.join("api.sock");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the exposure");
    let address = listener.local_addr().expect("exposed address");
    let bridge = tokio::spawn(crate::run_bridge_exposed_on(
        api_socket,
        daemon,
        listener,
        store.clone(),
        AUTH_DEADLINE,
    ));

    // The issued token works against the running bridge.
    let first = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect while authorized");
    let mut first = BufReader::new(first);
    AsyncWriteExt::write_all(
        first.get_mut(),
        format!("Authorization: Bearer {}\n{}", issued.token, hello_line()).as_bytes(),
    )
    .await
    .expect("write the authorization");
    let mut line = String::new();
    crate::read_frame(&mut first, &mut line).await.expect("read hello_ok");
    let reply: Value = serde_json::from_str(&line).expect("hello_ok is JSON");
    assert_eq!(reply["reply_to"], 1);
    assert_eq!(reply["ev"], "hello_ok");
    drop(first);

    store.revoke("cli").expect("revoke while the bridge runs");

    // The same token on a new connection is refused, with no restart and no frame.
    let accepts_before = accepts.load(Ordering::SeqCst);
    let second = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect after revoking");
    let (mut read_half, mut write_half) = second.into_split();
    AsyncWriteExt::write_all(
        &mut write_half,
        format!("Authorization: Bearer {}\n{}", issued.token, hello_line()).as_bytes(),
    )
    .await
    .expect("write the authorization");
    let mut answer = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(5),
        AsyncReadExt::read_to_end(&mut read_half, &mut answer),
    )
    .await
    .expect("the refused connection must be closed");
    // The bridge closes with the peer's authorization line still unread, so the
    // kernel may report a reset instead of an orderly end of stream. Either way
    // the connection is over and it produced no frame.
    match read {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("reading the refused connection: {error}"),
    }
    assert!(answer.is_empty(), "a revoked token must produce no frame: {answer:?}");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        accepts_before,
        "a revoked token must not reach the daemon"
    );
    bridge.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rotated_old_token_is_refused_and_the_new_one_reaches_hello() {
    let root = scratch("rotate");
    let (daemon, _accepts) = fake_daemon(&root).await;
    let store = store_in(&root);
    let issued = store.issue(Some("cli")).expect("issue a credential");
    let rotated = store.rotate("cli").expect("rotate the credential");

    let (mut stale, stale_task) = spawn_duplex(store.clone(), daemon.clone(), AUTH_DEADLINE);
    AsyncWriteExt::write_all(
        &mut stale,
        format!("Authorization: Bearer {}\n{}", issued.token, hello_line()).as_bytes(),
    )
    .await
    .expect("write the stale token");
    expect_silent_close(stale).await;
    stale_task.await.expect("auth task").expect("refused cleanly");

    let (mut fresh, fresh_task) = spawn_duplex(store, daemon, AUTH_DEADLINE);
    AsyncWriteExt::write_all(
        &mut fresh,
        format!("Authorization: Bearer {}\n{}", rotated.token, hello_line()).as_bytes(),
    )
    .await
    .expect("write the rotated token");
    let mut reader = BufReader::new(fresh);
    let mut line = String::new();
    crate::read_frame(&mut reader, &mut line).await.expect("read hello_ok");
    let reply: Value = serde_json::from_str(&line).expect("hello_ok is JSON");
    assert_eq!(reply["ev"], "hello_ok", "{line}");
    drop(reader);
    fresh_task.await.expect("auth task").expect("the session ends cleanly");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_pre_auth_line_is_closed() {
    let root = scratch("oversized");
    let (daemon, accepts) = fake_daemon(&root).await;
    let store = store_in(&root);
    store.issue(None).expect("issue a credential");
    let (mut client, task) = spawn_duplex(store, daemon, AUTH_DEADLINE);
    client
        .write_all(&vec![b'A'; MAX_AUTH_FRAME_BYTES + 64])
        .await
        .expect("write an oversized line");
    expect_silent_close(client).await;
    task.await.expect("auth task").expect("refused cleanly");
    assert_eq!(accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_peer_is_closed_within_the_auth_deadline() {
    let root = scratch("silent");
    let (daemon, accepts) = fake_daemon(&root).await;
    let store = store_in(&root);
    store.issue(None).expect("issue a credential");
    let deadline = Duration::from_millis(200);
    let (client, task) = spawn_duplex(store, daemon, deadline);
    let started = std::time::Instant::now();
    expect_silent_close(client).await;
    task.await.expect("auth task").expect("refused cleanly");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a silent peer must not hold the connection past the deadline"
    );
    assert_eq!(accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_valid_token_reaches_hello_and_a_ping_over_tcp() {
    let root = scratch("valid");
    let (daemon, accepts) = fake_daemon(&root).await;
    let store = store_in(&root);
    let issued = store.issue(Some("desktop")).expect("issue a credential");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the exposure");
    let address = listener.local_addr().expect("exposed address");
    let bridge = tokio::spawn(crate::run_bridge_exposed_on(
        root.join("api.sock"),
        daemon,
        listener,
        store,
        AUTH_DEADLINE,
    ));

    let stream = tokio::net::TcpStream::connect(address).await.expect("connect");
    let mut reader = BufReader::new(stream);
    AsyncWriteExt::write_all(
        reader.get_mut(),
        format!("Authorization: Bearer {}\n{}", issued.token, hello_line()).as_bytes(),
    )
    .await
    .expect("write the authorization and hello");
    let mut line = String::new();
    crate::read_frame(&mut reader, &mut line).await.expect("read hello_ok");
    let hello: Value = serde_json::from_str(&line).expect("hello_ok is JSON");
    assert_eq!(hello["reply_to"], 1);
    assert_eq!(hello["ev"], "hello_ok");

    AsyncWriteExt::write_all(reader.get_mut(), b"{\"id\":2,\"req\":\"ping\"}\n")
        .await
        .expect("write a ping");
    crate::read_frame(&mut reader, &mut line).await.expect("read pong");
    let pong: Value = serde_json::from_str(&line).expect("pong is JSON");
    assert_eq!(pong["reply_to"], 2);
    assert_eq!(pong["ev"], "pong");
    assert!(
        accepts.load(Ordering::SeqCst) >= 1,
        "the daemon is dialled after authentication"
    );
    bridge.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_local_socket_keeps_its_handshake_when_a_tcp_listener_is_exposed() {
    let root = scratch("local");
    let (daemon, _accepts) = fake_daemon(&root).await;
    let store = store_in(&root);
    store.issue(Some("desktop")).expect("issue a credential");
    let api_socket = root.join("api.sock");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the exposure");
    let bridge = tokio::spawn(crate::run_bridge_exposed_on(
        api_socket.clone(),
        daemon,
        listener,
        store,
        AUTH_DEADLINE,
    ));

    let mut client = connect_when_listening(&api_socket).await;
    client
        .write_all(hello_line().as_bytes())
        .await
        .expect("write hello without any authorization");
    let (read_half, write_half) = client.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    crate::read_frame(&mut reader, &mut line).await.expect("read hello_ok");
    let reply: Value = serde_json::from_str(&line).expect("hello_ok is JSON");
    assert_eq!(reply["ev"], "hello_ok", "the local socket must stay unauthenticated");
    drop((reader, write_half));
    bridge.abort();
}

/// Connect once the bridge is listening: a spawn that has not bound yet refuses.
async fn connect_when_listening(socket: &Path) -> Stream {
    for _ in 0..200 {
        if let Ok(stream) = Stream::connect(socket).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the bridge never listened on {}", socket.display());
}

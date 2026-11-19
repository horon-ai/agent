//! What a client is told when the daemon behind the bridge ends the connection.
//!
//! Two different events lead to the same closed socket, and a client has to be
//! able to tell them apart: a daemon that announced a reload is replacing its
//! own socket and will answer again, while one that stopped is gone. The phase
//! travels as its own frame before the close notice, and these tests read the
//! client's side of a real bridge connection to pin the order and the content.
use super::*;
use horon_agent_platform::ipc::{Listener, Stream};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

/// A client connected to a bridge that dials a fake daemon, past the handshake.
///
/// The daemon side stays open for the caller to write events on or drop, so one
/// helper serves every end-of-daemon case.
async fn connected_to_a_fake_daemon(
    name: &str,
) -> (
    PathBuf,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
    BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    Stream,
    tokio::task::JoinHandle<Result<()>>,
) {
    // Short enough for the 104-byte socket path limit macOS enforces.
    let path = std::env::temp_dir().join(format!(
        "hc-{name}-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000
    ));
    let mut listener = Listener::bind(&path).unwrap();
    let (client, bridge) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(bridge);
    let task = tokio::spawn(run_bridge_stream(read, write, path.clone()));
    let (client_read, mut client_write) = tokio::io::split(client);
    let mut reader = BufReader::new(client_read);
    write_json_line(
        &mut client_write,
        &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1,"client":"close-test"}),
    )
    .await
    .unwrap();
    let (daemon, _) = listener.accept().await.unwrap();
    let hello = next_frame(&mut reader).await;
    assert_eq!(hello["ev"], "hello_ok", "the handshake did not complete: {hello}");
    (path, client_write, reader, daemon, task)
}

/// The next frame the client reads, as the wire delivered it.
async fn next_frame(reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>) -> Value {
    let mut line = String::new();
    let read = reader.read_line(&mut line).await.unwrap();
    assert!(read > 0, "the bridge closed the connection instead of framing");
    serde_json::from_str(&line).unwrap()
}

/// Join the bridge task and remove the socket file.
async fn complete(task: tokio::task::JoinHandle<Result<()>>, path: PathBuf) {
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("the bridge did not end")
        .expect("the bridge task panicked")
        .expect("the bridge returned an error");
    let _ = std::fs::remove_file(path);
}

/// A daemon that announced a reload is not reported as stopping.
///
/// The close that ends the connection is the restart it promised, so the client
/// is told to reconnect and is never handed a `stopped` phase it would wait on
/// for ever.
#[tokio::test(flavor = "multi_thread")]
async fn a_reloading_daemon_is_announced_as_restarting_and_not_as_stopping() {
    let (path, client_write, mut reader, mut daemon, task) = connected_to_a_fake_daemon("reload").await;
    write_json_line(&mut daemon, &json!({"type": "reloading"}))
        .await
        .unwrap();
    let lifecycle = next_frame(&mut reader).await;
    assert_eq!(lifecycle["ev"], "server_lifecycle");
    assert_eq!(lifecycle["phase"], "restarting");

    // The reload replaces the daemon's socket: this is the close it announced,
    // and the error frame below it is unchanged.
    drop(daemon);
    let error = next_frame(&mut reader).await;
    assert_eq!(error["ev"], "error");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("daemon connection closed"),
        "the close notice changed: {error}"
    );
    let mut line = String::new();
    assert_eq!(
        reader.read_line(&mut line).await.unwrap(),
        0,
        "the connection stayed open: {line}"
    );
    complete(task, path).await;
    drop(client_write);
}

/// A daemon that goes away without a word is reported as stopping.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_that_stops_is_announced_before_the_close_notice() {
    let (path, client_write, mut reader, daemon, task) = connected_to_a_fake_daemon("stop").await;
    drop(daemon);

    let lifecycle = next_frame(&mut reader).await;
    assert_eq!(lifecycle["ev"], "server_lifecycle");
    assert_eq!(lifecycle["phase"], "stopped");
    let error = next_frame(&mut reader).await;
    assert_eq!(error["ev"], "error");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("daemon connection closed"),
        "the close notice changed: {error}"
    );
    let mut line = String::new();
    assert_eq!(
        reader.read_line(&mut line).await.unwrap(),
        0,
        "the connection stayed open: {line}"
    );
    complete(task, path).await;
    drop(client_write);
}

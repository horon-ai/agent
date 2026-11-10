//! A harness API v1 client round trip over the real API socket.
//!
//! The client below is written against the published v1 wire only: newline
//! delimited JSON frames of `ClientFrame`/`ServerFrame` from
//! `horon-agent-harness-api`. It connects to the socket `run_bridge` binds,
//! negotiates the version, and then lists sessions. The session it must see was
//! recorded in the v0.1.x layout (`sessions/<id>.json` snapshot), so the round
//! trip also pins that an external v1 client still discovers sessions written
//! by that release.

use horon_agent_harness_api::{API_VERSION_MAJOR, ApiEvent, ApiRequest, ClientFrame, ServerFrame};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const SESSION_ID: &str = "session_v1_round_trip";

/// v0.1.x wrote one snapshot per session. Only the fields that release emitted
/// appear here.
fn v0_1_record(session_id: &str) -> String {
    format!(
        r#"{{
  "id": "{session_id}",
  "parent_id": null,
  "title": "Round trip session",
  "created_at": "2025-10-02T10:00:00.000Z",
  "updated_at": "2025-10-02T10:04:00.000Z",
  "messages": [
    {{"id": "m1", "role": "user", "content": [{{"type": "text", "text": "hello"}}]}}
  ],
  "provider_key": "openai",
  "plugin_id": "router",
  "status": "Closed",
  "last_pid": null,
  "is_debug": false,
  "saved": false,
  "migration_epoch": 1
}}"#
    )
}

/// Short root: macOS rejects UNIX socket paths at 104 bytes.
fn temp_root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "v1rt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000_000
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

async fn send_frame<W>(writer: &mut W, frame: &ClientFrame) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut line = serde_json::to_string(frame).expect("serialize client frame");
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

/// Read frames until the one that answers `request_id`, ignoring unsolicited
/// events the bridge may interleave.
async fn read_reply<R>(reader: &mut R, request_id: u64) -> ServerFrame
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.expect("read server frame");
        assert!(
            read > 0,
            "bridge closed the socket before answering request {request_id}"
        );
        let frame: ServerFrame = serde_json::from_str(&line).expect("parse server frame");
        if frame.reply_to == Some(request_id) {
            return frame;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn v1_client_negotiates_and_lists_a_session_recorded_by_v0_1_x() {
    let root = temp_root();
    let home = root.join("home");
    let sessions = home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join(format!("{SESSION_ID}.json")), v0_1_record(SESSION_ID)).unwrap();

    // The bridge resolves the store from HORON_HOME; keep everything sandboxed.
    unsafe {
        std::env::set_var("HORON_HOME", &home);
        std::env::set_var("HORON_AGENT_RUNTIME_DIR", root.join("runtime"));
    }

    // A daemon stands in for the runtime so the bridge can dial it after hello.
    let daemon_socket = root.join("daemon.sock");
    let daemon = tokio::net::UnixListener::bind(&daemon_socket).unwrap();
    let api_socket = root.join("api.sock");
    let bridge_socket = api_socket.clone();
    let bridge =
        tokio::spawn(async move { horon_agent_harness_api_server::run_bridge(bridge_socket, daemon_socket).await });

    // Wait for the bridge to bind before dialing it.
    let mut connected = None;
    for _ in 0..200 {
        if let Ok(stream) = UnixStream::connect(&api_socket).await {
            connected = Some(stream);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let stream = connected.expect("bridge never bound the api socket");
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);

    // 1. Version negotiation.
    send_frame(
        &mut write,
        &ClientFrame::new(
            1,
            ApiRequest::Hello {
                min_version: API_VERSION_MAJOR,
                max_version: API_VERSION_MAJOR,
                client: "v1-round-trip-test".into(),
                supports_user_response: false,
                supports_ask_requests: false,
            },
        ),
    )
    .await
    .unwrap();
    let hello = read_reply(&mut read, 1).await;
    match hello.event {
        ApiEvent::HelloOk { version, .. } => assert_eq!(version, API_VERSION_MAJOR),
        other => panic!("expected HelloOk, got {other:?}"),
    }

    // The bridge dials the daemon after hello; hold the connection open.
    let (mut daemon_conn, _) = daemon.accept().await.expect("bridge never dialed the daemon");

    // 2. A real list request, answered from the store.
    send_frame(
        &mut write,
        &ClientFrame::new(
            2,
            ApiRequest::ListSessions {
                include_archived: false,
                limit: None,
            },
        ),
    )
    .await
    .unwrap();
    let reply = read_reply(&mut read, 2).await;
    let sessions = match reply.event {
        ApiEvent::Sessions { sessions } => sessions,
        other => panic!("expected Sessions, got {other:?}"),
    };
    let listed = sessions
        .iter()
        .find(|info| info.session_id == SESSION_ID)
        .unwrap_or_else(|| panic!("v0.1.x session missing from {sessions:?}"));
    assert_eq!(listed.title.as_deref(), Some("Round trip session"));

    // 3. Closing the client tears the connection down without taking the bridge.
    drop(write);
    let mut probe = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read(&mut daemon_conn, &mut probe).await;
    bridge.abort();
    let _ = bridge.await;
    std::fs::remove_dir_all(&root).unwrap();
}

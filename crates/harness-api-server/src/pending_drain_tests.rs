//! A connection that ends because of a fatal request error must say so.
//!
//! The loop's terminal frame carries no `reply_to`, so it takes the ordinary
//! path into the local `pending` buffer rather than the control queue. Exiting
//! the loop without draining that buffer dropped the frame, and the client saw
//! a bare close with no reason for it.
use super::*;
use horon_agent_platform::ipc::Listener;
use serde_json::json;
use tokio::io::AsyncWriteExt;

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_request_reports_its_error_before_the_connection_closes() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let path = std::env::temp_dir().join(format!(
            "horon-drain-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000_000
        ));
        let mut listener = Listener::bind(&path).unwrap();
        let (client, bridge) = tokio::io::duplex(8 * 1024);
        let (bridge_read, bridge_write) = tokio::io::split(bridge);
        let task = tokio::spawn(run_bridge_stream(bridge_read, bridge_write, path.clone()));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut replies = BufReader::new(client_read);
        let mut line = String::new();

        write_json_line(
            &mut client_write,
            &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1,"client":"pending-drain-test"}),
        )
        .await
        .unwrap();
        // Accepting in the wrong order hands the test the capability probe
        // instead of the session stream, and the handshake never completes.
        let (_daemon_stream, _) = listener.accept().await.unwrap();
        let (probe, _) = listener.accept().await.unwrap();
        let mut probe = BufReader::new(probe);
        read_frame(&mut probe, &mut line).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["type"], "ping");
        write_json_line(probe.get_mut(), &json!({"type":"pong","id":0}))
            .await
            .unwrap();
        drop(probe);
        read_frame(&mut replies, &mut line).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["ev"], "hello_ok");

        let chunk = vec![b'x'; 64 * 1024];
        let mut remaining = MAX_FRAME_BYTES as usize;
        while remaining > 0 {
            let take = remaining.min(chunk.len());
            client_write.write_all(&chunk[..take]).await.unwrap();
            remaining -= take;
        }

        let read = read_frame(&mut replies, &mut line).await.unwrap();
        assert!(
            read > 0,
            "the client must be told why the connection ends, not only see it close"
        );
        let frame: ServerFrame = serde_json::from_str(&line).unwrap();
        assert!(
            matches!(
                frame.event,
                ApiEvent::Error {
                    code: ErrorCode::InvalidRequest,
                    ..
                }
            ),
            "expected the oversized-frame error, got {:?}",
            frame.event
        );
        assert_eq!(read_frame(&mut replies, &mut line).await.unwrap(), 0);

        task.await.unwrap().unwrap();
        drop(listener);
        std::fs::remove_file(path).unwrap();
    })
    .await
    .expect("an oversized request must still receive its error frame");
}

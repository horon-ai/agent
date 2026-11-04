//! A reply larger than the pending buffer must still reach the client.
//!
//! The connection used to write every reply inline, so a transcript of any size
//! was delivered. Once the writes moved behind the bounded queue, the ordinary
//! branch measured each frame against `PENDING_BYTES` *before* it looked at the
//! queue at all: one reply larger than that cap took the overflow path and the
//! connection closed, with the client never told why. The pending buffer is for
//! frames that arrive while the queue is full; the queue is where a reply
//! belongs, and waiting for it to drain is the backpressure that keeps a large
//! reply writable.
use super::*;
use crate::writer::PENDING_BYTES;
use serde_json::json;
use tokio::net::UnixListener;

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_larger_than_the_pending_buffer_reaches_the_client() {
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let path = std::env::temp_dir().join(format!(
            "horon-large-reply-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000_000
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let (client, bridge) = tokio::io::duplex(1024 * 1024);
        let (bridge_read, bridge_write) = tokio::io::split(bridge);
        let task = tokio::spawn(run_bridge_stream(bridge_read, bridge_write, path.clone()));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut replies = BufReader::new(client_read);
        let mut line = String::new();

        write_json_line(
            &mut client_write,
            &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1,"client":"large-reply-test"}),
        )
        .await
        .unwrap();
        // The session stream is dialled first and the disposable capability
        // probe second; accepting in the other order hands this test the probe
        // as its daemon and the handshake never completes.
        let (daemon, _) = listener.accept().await.unwrap();
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

        // One reply above the pending cap, which is the size that used to close
        // the connection instead of being written.
        let content = "x".repeat(PENDING_BYTES + 1024 * 1024);
        let expected = content.len();
        let (daemon_read, mut daemon_write) = daemon.into_split();
        let mut requests = BufReader::new(daemon_read).lines();
        let fake_daemon = tokio::spawn(async move {
            while let Ok(Some(line)) = requests.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                match request["type"].as_str() {
                    Some("state") => {
                        write_json_line(
                            &mut daemon_write,
                            &json!({"type":"state","id":request["id"],"session_id":"large-reply"}),
                        )
                        .await
                        .unwrap();
                    }
                    Some("get_history") => {
                        write_json_line(
                            &mut daemon_write,
                            &json!({
                                "type":"history",
                                "id":request["id"],
                                "session_id":"large-reply",
                                "messages":[{"role":"assistant","content":content}],
                            }),
                        )
                        .await
                        .unwrap();
                        return;
                    }
                    _ => {}
                }
            }
        });

        write_json_line(
            &mut client_write,
            &json!({"v":1,"id":2,"req":"attach_session","session_id":"large-reply"}),
        )
        .await
        .unwrap();
        loop {
            read_frame(&mut replies, &mut line).await.unwrap();
            if serde_json::from_str::<Value>(&line).unwrap()["ev"] == "attached" {
                break;
            }
        }
        write_json_line(
            &mut client_write,
            &json!({"v":1,"id":3,"req":"get_history","session_id":"large-reply"}),
        )
        .await
        .unwrap();

        // The payload has to arrive. A close here is the regression: the client
        // asked for a transcript the bridge could write, and got an EOF instead.
        let history = loop {
            let read = read_frame(&mut replies, &mut line).await.unwrap();
            assert!(
                read > 0,
                "the bridge closed the connection instead of writing the {expected} byte reply"
            );
            let frame: Value = serde_json::from_str(&line).unwrap();
            if frame["reply_to"] == 3 {
                break frame;
            }
        };
        assert_eq!(history["ev"], "history");
        assert_eq!(
            history["messages"][0]["content"].as_str().unwrap().len(),
            expected,
            "the whole transcript must reach the client"
        );

        drop(client_write);
        drop(replies);
        fake_daemon.abort();
        task.await.unwrap().unwrap();
        drop(listener);
        std::fs::remove_file(path).unwrap();
    })
    .await
    .expect("a reply larger than the pending buffer must be delivered");
}

use super::*;
use horon_agent_platform::ipc::Listener;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_history_replies_survive_api_requests_between_fragments() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let path = std::env::temp_dir().join(format!("horon-agent-fragments-{}-{}.sock", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut listener = Listener::bind(&path).unwrap();
        let (client, bridge) = tokio::io::duplex(4 * 1024 * 1024);
        let (bridge_read, bridge_write) = tokio::io::split(bridge);
        let task = tokio::spawn(run_bridge_stream(bridge_read, bridge_write, path.clone()));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut replies = BufReader::new(client_read).lines();
        write_json_line(&mut client_write, &json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1})).await.unwrap();
        let (daemon, _) = listener.accept().await.unwrap();
        let (daemon_read, mut daemon_write) = daemon.into_split();
        let mut requests = BufReader::new(daemon_read).lines();
        assert_eq!(serde_json::from_str::<Value>(&replies.next_line().await.unwrap().unwrap()).unwrap()["ev"], "hello_ok");
        write_json_line(&mut client_write, &json!({"v":1,"id":2,"req":"attach_session","session_id":"fragment-session"})).await.unwrap();
        for _ in 0..3 {
            let request: Value = serde_json::from_str(&requests.next_line().await.unwrap().unwrap()).unwrap();
            if request["type"] == "state" {
                write_json_line(&mut daemon_write, &json!({"type":"state","id":request["id"],"session_id":"fragment-session"})).await.unwrap();
            }
        }
        loop {
            let frame: Value = serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
            if frame["ev"] == "attached" { break; }
        }
        for id in 3..6 {
            write_json_line(&mut client_write, &json!({"v":1,"id":id,"req":"get_history","session_id":"fragment-session"})).await.unwrap();
        }
        let mut history_ids = Vec::new();
        for _ in 0..3 {
            let request: Value = serde_json::from_str(&requests.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(request["type"], "get_history");
            history_ids.push(request["id"].clone());
        }
        for (index, id) in history_ids.into_iter().enumerate() {
            // Larger than the socket send buffer, forcing the bridge to
            // consume an unterminated prefix before API activity cancels it.
            let text = "x".repeat(1024 * 1024);
            let prefix = format!("{{\"type\":\"history\",\"id\":{id},\"session_id\":\"fragment-session\",\"messages\":[{{\"role\":\"assistant\",\"content\":\"{text}");
            daemon_write.write_all(prefix.as_bytes()).await.unwrap();
            let ping_id = 20 + index;
            write_json_line(&mut client_write, &json!({"v":1,"id":ping_id,"req":"ping"})).await.unwrap();
            let ping: Value = serde_json::from_str(&requests.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(ping["type"], "ping");
            daemon_write.write_all(b"\"}]}\n").await.unwrap();
            write_json_line(&mut daemon_write, &json!({"type":"pong","id":ping["id"]})).await.unwrap();
            let mut history_seen = false;
            let mut pong_seen = false;
            while !history_seen || !pong_seen {
                let reply: Value = serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
                if reply["reply_to"] == index + 3 {
                    assert_eq!(reply["ev"], "history");
                    assert_eq!(reply["messages"][0]["content"], text);
                    history_seen = true;
                }
                if reply["reply_to"] == ping_id {
                    assert_eq!(reply["ev"], "pong");
                    pong_seen = true;
                }
            }
        }
        drop(client_write);
        drop(replies);
        task.await.unwrap().unwrap();
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }).await.expect("fragmented history requests must all receive correlated replies");
}

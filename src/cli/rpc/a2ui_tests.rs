use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

use crate::cli::provider_init::ProviderChoice;
use crate::transport::stream_pair;

use super::adapter::{self, Start};
use super::command::A2UI_BASIC_CATALOG_URI;
use super::link;

struct TestSession {
    host_tx: tokio::io::DuplexStream,
    host_lines: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    daemon_requests: mpsc::UnboundedReceiver<Value>,
    adapter_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    daemon_task: tokio::task::JoinHandle<()>,
}

impl TestSession {
    async fn start<F>(handler: F) -> Self
    where
        F: Fn(Value, &mpsc::UnboundedSender<Value>) + Send + Sync + 'static,
    {
        let (host_tx, adapter_in) = tokio::io::duplex(64 * 1024);
        let (adapter_out, host_rx) = tokio::io::duplex(64 * 1024);
        let (daemon_side, client_side) = stream_pair().expect("stream pair");
        let client_slot = Arc::new(Mutex::new(Some(client_side)));
        let connector: link::Connector = Arc::new(move || {
            let slot = Arc::clone(&client_slot);
            Box::pin(async move {
                let stream = slot
                    .lock()
                    .await
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("connector already consumed"))?;
                Ok(stream.into_split())
            })
        });

        let (daemon_req_tx, daemon_req_rx) = mpsc::unbounded_channel();
        let (daemon_out_tx, mut daemon_out_rx) = mpsc::unbounded_channel::<Value>();
        let handler = Arc::new(handler);
        let out_sender = daemon_out_tx.clone();
        let daemon_task = tokio::spawn(async move {
            let (read_half, mut write_half) = daemon_side.into_split();
            let mut lines = BufReader::new(read_half).lines();
            loop {
                tokio::select! {
                    maybe_out = daemon_out_rx.recv() => {
                        let Some(event) = maybe_out else { break; };
                        let mut line = event.to_string();
                        line.push('\n');
                        if write_half.write_all(line.as_bytes()).await.is_err() {
                            break;
                        }
                        if write_half.flush().await.is_err() {
                            break;
                        }
                    }
                    line = lines.next_line() => {
                        let Ok(Some(raw)) = line else { break; };
                        let req: Value = serde_json::from_str(&raw).expect("valid daemon request json");
                        let kind = req["type"].as_str().unwrap_or("");
                        let id = req["id"].as_u64().unwrap_or(0);
                        let _ = daemon_req_tx.send(req.clone());
                        match kind {
                            "subscribe" => {
                                let _ = out_sender.send(json!({"type": "ack", "id": id}));
                            }
                            "get_history" => {
                                let _ = out_sender.send(json!({
                                    "type": "history",
                                    "id": id,
                                    "session_id": "ses-a2ui-rpc",
                                    "messages": [],
                                }));
                            }
                            _ => handler(req, &out_sender),
                        }
                    }
                }
            }
        });

        let adapter_task = tokio::spawn(async move {
            adapter::run(Start {
                provider: &ProviderChoice::Auto,
                model: Some("test-model"),
                provider_profile: None,
                cwd: None,
                resume: None,
                connector: Some(connector),
                out: Box::new(adapter_out),
                input: Box::new(BufReader::new(adapter_in)),
            })
            .await
        });

        let mut session = Self {
            host_tx,
            host_lines: BufReader::new(host_rx).lines(),
            daemon_requests: daemon_req_rx,
            adapter_task,
            daemon_task,
        };

        // Drain the three startup announcement frames (`ready`, `extension_ui_request`,
        // `available_commands_update`).
        let ready = session.next_frame().await;
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["protocolVersion"], 1);
        let widget = session.next_frame().await;
        assert_eq!(widget["type"], "extension_ui_request");
        let commands = session.next_frame().await;
        assert_eq!(commands["type"], "available_commands_update");
        session
    }

    async fn send(&mut self, frame: Value) {
        let mut line = frame.to_string();
        line.push('\n');
        self.host_tx.write_all(line.as_bytes()).await.expect("write host frame");
        self.host_tx.flush().await.expect("flush host frame");
    }

    async fn next_frame(&mut self) -> Value {
        let line = tokio::time::timeout(std::time::Duration::from_secs(3), self.host_lines.next_line())
            .await
            .expect("timed out waiting for rpc frame")
            .expect("read rpc frame")
            .expect("unexpected eof waiting for rpc frame");
        serde_json::from_str(&line).expect("valid rpc frame json")
    }

    async fn read_until_settled(&mut self) -> Vec<Value> {
        let mut frames = Vec::new();
        loop {
            let frame = self.next_frame().await;
            let done = frame["type"] == "session_settled";
            frames.push(frame);
            if done {
                return frames;
            }
        }
    }

    async fn finish(mut self) -> Vec<Value> {
        drop(self.host_tx);
        self.adapter_task
            .await
            .expect("adapter task join")
            .expect("adapter exits cleanly");
        self.daemon_task.abort();
        let mut reqs = Vec::new();
        while let Ok(req) = self.daemon_requests.try_recv() {
            reqs.push(req);
        }
        reqs
    }
}

#[tokio::test]
async fn unnegotiated_connection_preserves_turn_sequence_and_ignores_a2ui() {
    let mut session = TestSession::start(|req, out| {
        if req["type"] == "message" {
            let id = req["id"].as_u64().unwrap_or(0);
            let _ = out.send(json!({"type": "ack", "id": id}));
            let _ = out.send(json!({"type": "text_delta", "text": "Plain answer"}));
            let _ = out.send(json!({"type": "text_done"}));
            let _ = out.send(json!({
                "type": "a2ui_envelope",
                "session_id": "ses-a2ui-rpc",
                "tool_call_id": "call-ignored",
                "sequence": 0,
                "envelope": {"version": "1.0", "createSurface": {"surfaceId": "s1", "catalogId": A2UI_BASIC_CATALOG_URI}},
                "fallback_text": "Should not appear"
            }));
            let _ = out.send(json!({"type": "message_end", "stop_reason": "stop"}));
            let _ = out.send(json!({"type": "done", "id": id}));
        }
    })
    .await;

    session
        .send(json!({"id": "neg-0", "type": "negotiate_protocol", "protocolVersion": 1}))
        .await;
    let neg_resp = session.next_frame().await;
    assert_eq!(neg_resp["type"], "response");
    assert_eq!(neg_resp["command"], "negotiate_protocol");
    assert_eq!(neg_resp["success"], true);
    assert_eq!(neg_resp["data"], json!({"protocolVersion": 1}));

    session
        .send(json!({"id": "p-1", "type": "prompt", "message": "Hello"}))
        .await;
    let prompt_resp = session.next_frame().await;
    assert_eq!(prompt_resp["type"], "response");
    assert_eq!(prompt_resp["command"], "prompt");
    assert_eq!(prompt_resp["success"], true);

    let turn_frames = session.read_until_settled().await;
    let types: Vec<&str> = turn_frames.iter().map(|f| f["type"].as_str().unwrap_or("")).collect();
    assert_eq!(
        types,
        vec![
            "agent_start",
            "message_start",
            "message_end",
            "turn_start",
            "message_start",
            "message_update",
            "message_update",
            "message_update",
            "message_end",
            "turn_end",
            "agent_end",
            "prompt_result",
            "session_settled",
        ]
    );
    assert!(!turn_frames.iter().any(|f| f["type"] == "a2ui_update"));
    let asst_end = turn_frames
        .iter()
        .find(|f| f["type"] == "message_end" && f["message"]["role"] == "assistant")
        .expect("assistant message_end");
    let content = asst_end["message"]["content"].as_array().expect("content array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0], json!({"type": "text", "text": "Plain answer"}));

    let daemon_reqs = session.finish().await;
    assert!(!daemon_reqs.iter().any(|r| r["type"] == "configure_a2ui"));
}

#[tokio::test]
async fn negotiate_a2ui_v1_configures_daemon_streams_updates_and_exposes_history() {
    let custom_catalog = "https://example.com/custom-catalog.json";
    let mut session = TestSession::start(|req, out| match req["type"].as_str().unwrap_or("") {
        "configure_a2ui" => {
            let id = req["id"].as_u64().unwrap_or(0);
            let _ = out.send(json!({"type": "ack", "id": id}));
            let _ = out.send(json!({"type": "done", "id": id}));
        }
        "message" => {
            let id = req["id"].as_u64().unwrap_or(0);
            let _ = out.send(json!({"type": "ack", "id": id}));
            let _ = out.send(json!({"type": "tool_start", "id": "call-ui", "name": "a2ui"}));
            let _ = out.send(json!({
                "type": "tool_input",
                "id": "call-ui",
                "delta": r#"{"envelopes":[],"fallback_text":"Status Card"}"#
            }));
            let _ = out.send(json!({"type": "tool_exec", "id": "call-ui", "name": "a2ui"}));
            // Streamed pre-MessageEnd envelope (no fallback_text yet)
            let _ = out.send(json!({
                "type": "a2ui_envelope",
                "session_id": "ses-a2ui-rpc",
                "tool_call_id": "call-ui",
                "sequence": 0,
                "envelope": {"version": "1.0", "createSurface": {"surfaceId": "card-1", "catalogId": A2UI_BASIC_CATALOG_URI}},
            }));
            let _ = out.send(json!({
                "type": "tool_done",
                "id": "call-ui",
                "name": "a2ui",
                "output": "Rendered card-1"
            }));
            let _ = out.send(json!({"type": "message_end", "stop_reason": "toolUse"}));
            // Post-MessageEnd supplemental envelope (sequence 1)
            let _ = out.send(json!({
                "type": "a2ui_envelope",
                "session_id": "ses-a2ui-rpc",
                "tool_call_id": "call-ui",
                "sequence": 1,
                "envelope": {"version": "1.0", "updateComponents": {"surfaceId": "card-1", "components": []}},
                "fallback_text": "Status Card"
            }));
            let _ = out.send(json!({"type": "done", "id": id}));
        }
        _ => {}
    })
    .await;

    session
        .send(json!({
            "id": "neg-a2ui",
            "type": "negotiate_protocol",
            "protocolVersion": 1,
            "capabilities": ["a2ui_v1"],
            "a2ui": {
                "version": "1.0",
                "supportedCatalogIds": [A2UI_BASIC_CATALOG_URI, custom_catalog],
                "acceptsInlineCatalogs": true
            }
        }))
        .await;
    let neg_resp = session.next_frame().await;
    assert_eq!(neg_resp["success"], true);
    assert_eq!(
        neg_resp["data"],
        json!({
            "protocolVersion": 1,
            "capabilities": ["a2ui_v1"]
        })
    );

    session
        .send(json!({"id": "p-a2ui", "type": "prompt", "message": "Show status"}))
        .await;
    let prompt_ack = session.next_frame().await;
    assert_eq!(prompt_ack["success"], true);

    let frames = session.read_until_settled().await;
    let updates: Vec<&Value> = frames.iter().filter(|f| f["type"] == "a2ui_update").collect();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0]["sessionId"], "ses-a2ui-rpc");
    assert_eq!(updates[0]["toolCallId"], "call-ui");
    assert_eq!(updates[0]["sequence"], 0);
    assert!(updates[0]["envelope"]["createSurface"].is_object());
    assert_eq!(updates[1]["sequence"], 1);
    assert_eq!(updates[1]["fallbackText"], "Status Card");

    let msg_end = frames
        .iter()
        .find(|f| f["type"] == "message_end" && f["message"]["role"] == "assistant")
        .expect("assistant message_end");
    let end_a2ui = msg_end["message"]["content"]
        .as_array()
        .expect("content array")
        .iter()
        .find(|b| b["type"] == "a2ui")
        .expect("a2ui content block in message_end");
    // Backfilled from tool_input args `fallback_text`
    assert_eq!(end_a2ui["fallbackText"], "Status Card");
    assert_eq!(end_a2ui["envelopes"].as_array().expect("envelopes").len(), 1);

    // agent_end also contains the post-MessageEnd envelope appended to the assistant message
    let agent_end = frames.iter().find(|f| f["type"] == "agent_end").expect("agent_end");
    let agent_end_asst = agent_end["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant message in agent_end");
    let agent_a2ui = agent_end_asst["content"]
        .as_array()
        .expect("content")
        .iter()
        .find(|b| b["type"] == "a2ui")
        .expect("a2ui block in agent_end");
    assert_eq!(agent_a2ui["envelopes"].as_array().expect("envelopes").len(), 2);

    session.send(json!({"id": "gm-1", "type": "get_messages"})).await;
    let gm_resp = session.next_frame().await;
    assert_eq!(gm_resp["command"], "get_messages");
    assert_eq!(gm_resp["success"], true);
    let history_msgs = gm_resp["data"]["messages"].as_array().expect("history messages");
    let history_asst = history_msgs
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant message in get_messages");
    let history_a2ui = history_asst["content"]
        .as_array()
        .expect("content")
        .iter()
        .find(|b| b["type"] == "a2ui")
        .expect("a2ui block in get_messages");
    assert_eq!(history_a2ui["envelopes"].as_array().expect("envelopes").len(), 2);

    // Also verify subscribe helper carries the negotiated capabilities.
    let caps = super::command::A2uiCapabilities::default();
    let sub_req = link::subscribe_request_with_a2ui(99, None, None, Some(&caps)).expect("subscribe req");
    let sub_json = serde_json::to_value(sub_req).expect("serialize subscribe");
    assert_eq!(sub_json["a2ui"]["version"], "1.0");
    assert_eq!(
        sub_json["a2ui"]["supported_catalog_ids"],
        json!([A2UI_BASIC_CATALOG_URI])
    );

    let daemon_reqs = session.finish().await;
    let configure = daemon_reqs
        .iter()
        .find(|r| r["type"] == "configure_a2ui")
        .expect("configure_a2ui sent to daemon");
    assert_eq!(configure["capabilities"]["version"], "1.0");
    assert_eq!(configure["capabilities"]["accepts_inline_catalogs"], true);
    assert_eq!(
        configure["capabilities"]["supported_catalog_ids"],
        json!([A2UI_BASIC_CATALOG_URI, custom_catalog])
    );
}

type MidTurnHandle = Arc<Mutex<Option<(u64, mpsc::UnboundedSender<Value>)>>>;

#[tokio::test]
async fn inbound_a2ui_event_routes_turns_sync_replies_and_mid_turn_events() {
    let mid_turn_tx: MidTurnHandle = Arc::new(Mutex::new(None));
    let mid_turn_tx_clone = Arc::clone(&mid_turn_tx);

    let mut session = TestSession::start(move |req, out| match req["type"].as_str().unwrap_or("") {
        "configure_a2ui" => {
            let id = req["id"].as_u64().unwrap_or(0);
            let _ = out.send(json!({"type": "ack", "id": id}));
            let _ = out.send(json!({"type": "done", "id": id}));
        }
        "a2ui_event" => {
            let id = req["id"].as_u64().unwrap_or(0);
            let _ = out.send(json!({"type": "ack", "id": id}));
            if req["envelope"].get("action").is_some() {
                let _ = out.send(json!({"type": "text_delta", "text": "Action handled"}));
                let _ = out.send(json!({"type": "text_done"}));
                let _ = out.send(json!({"type": "message_end", "stop_reason": "stop"}));
                let _ = out.send(json!({"type": "done", "id": id}));
            } else if req["envelope"].get("rendererFunctionResponse").is_some() {
                let _ = out.send(json!({"type": "context_message_added", "id": id}));
            }
        }
        "message" => {
            let id = req["id"].as_u64().unwrap_or(0);
            let _ = out.send(json!({"type": "ack", "id": id}));
            let _ = out.send(json!({"type": "text_delta", "text": "Waiting on renderer"}));
            let slot = Arc::clone(&mid_turn_tx_clone);
            let sender = out.clone();
            tokio::spawn(async move {
                *slot.lock().await = Some((id, sender));
            });
        }
        _ => {}
    })
    .await;

    // 1. Before negotiating a2ui_v1, a2ui_event is rejected cleanly.
    session
        .send(json!({
            "id": "ev-unneg",
            "type": "a2ui_event",
            "envelope": {"version": "1.0", "error": {"code": "E", "surfaceId": "s1", "message": "m"}}
        }))
        .await;
    let err_resp = session.next_frame().await;
    assert_eq!(err_resp["id"], "ev-unneg");
    assert_eq!(err_resp["success"], false);
    assert_eq!(err_resp["code"], "invalid_request");

    // Negotiate a2ui_v1.
    session
        .send(json!({
            "id": "neg-1",
            "type": "negotiate_protocol",
            "protocolVersion": 1,
            "capabilities": ["a2ui_v1"]
        }))
        .await;
    let neg_ok = session.next_frame().await;
    assert_eq!(neg_ok["success"], true);

    // 2. Non-turn event (`rendererFunctionResponse`) settles with ContextMessageAdded and NO turn frames.
    session
        .send(json!({
            "id": "ev-ctx",
            "type": "a2ui_event",
            "envelope": {"version": "1.0", "rendererFunctionResponse": {"surfaceId": "s1", "callId": "c1", "result": {"ok": true}}},
            "dataModel": {"count": 2}
        }))
        .await;
    let ctx_resp = session.next_frame().await;
    assert_eq!(ctx_resp["id"], "ev-ctx");
    assert_eq!(ctx_resp["command"], "a2ui_event");
    assert_eq!(ctx_resp["success"], true);

    // 3. Action event starts a turn, streams assistant response, and emits prompt_result + session_settled.
    session
        .send(json!({
            "id": "ev-act",
            "type": "a2ui_event",
            "envelope": {"version": "1.0", "action": {"name": "submit", "surfaceId": "s1", "sourceComponentId": "btn-1", "timestamp": "2026-01-01T00:00:00Z", "userMessage": "Save clicked"}},
            "dataModel": {"draft": "abc"}
        }))
        .await;
    let act_ack = session.next_frame().await;
    assert_eq!(act_ack["id"], "ev-act");
    assert_eq!(act_ack["command"], "a2ui_event");
    assert_eq!(act_ack["success"], true);
    let act_frames = session.read_until_settled().await;
    assert!(act_frames.iter().any(|f| f["type"] == "turn_start"));
    let act_result = act_frames.last().expect("settled");
    assert_eq!(act_result["type"], "session_settled");
    assert_eq!(
        act_frames[act_frames.len() - 2],
        json!({"type": "prompt_result", "id": "ev-act", "agentInvoked": true, "status": "completed", "sessionSettled": true})
    );

    // 4. Mid-turn a2ui_event while a prompt is actively running forwards Request::A2uiEvent cleanly.
    session
        .send(json!({"id": "p-mid", "type": "prompt", "message": "Start long turn"}))
        .await;
    let p_ack = session.next_frame().await;
    assert_eq!(p_ack["success"], true);
    // Read until text_delta arrives so the turn is actively in flight.
    loop {
        let frame = session.next_frame().await;
        if frame["type"] == "message_update" && frame["assistantMessageEvent"]["type"] == "text_delta" {
            break;
        }
    }
    session
        .send(json!({
            "id": "ev-mid",
            "type": "a2ui_event",
            "envelope": {"version": "1.0", "rendererFunctionResponse": {"surfaceId": "s1", "callId": "c2", "result": "done"}}
        }))
        .await;
    let mid_resp = session.next_frame().await;
    assert_eq!(mid_resp["id"], "ev-mid");
    assert_eq!(mid_resp["command"], "a2ui_event");
    assert_eq!(mid_resp["success"], true);

    // Finish the in-flight prompt turn.
    let (mid_id, mid_sender) = loop {
        if let Some(pair) = mid_turn_tx.lock().await.take() {
            break pair;
        }
        tokio::task::yield_now().await;
    };
    let _ = mid_sender.send(json!({"type": "text_done"}));
    let _ = mid_sender.send(json!({"type": "message_end", "stop_reason": "stop"}));
    let _ = mid_sender.send(json!({"type": "done", "id": mid_id}));
    let mid_end = session.read_until_settled().await;
    assert_eq!(mid_end.last().expect("settled")["type"], "session_settled");

    let daemon_reqs = session.finish().await;
    let a2ui_events: Vec<&Value> = daemon_reqs.iter().filter(|r| r["type"] == "a2ui_event").collect();
    assert_eq!(a2ui_events.len(), 3);
    assert_eq!(a2ui_events[0]["data_model"], json!({"count": 2}));
    assert_eq!(a2ui_events[1]["data_model"], json!({"draft": "abc"}));
    assert_eq!(a2ui_events[2]["envelope"]["rendererFunctionResponse"]["callId"], "c2");
}

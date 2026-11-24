use crate::run_bridge_stream;
use crate::translate::{BridgeState, Outbound};
use horon_agent_harness_api::{
    A2UI_BASIC_CATALOG_V1_0, A2UI_VERSION_V1_0, A2uiSessionCapabilities, ApiEvent, CAPABILITY_A2UI_V1, ErrorCode,
};
use horon_agent_transport::Listener;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn attached_a2ui_state(a2ui_supported: bool) -> BridgeState {
    BridgeState {
        a2ui_supported,
        session_id: Some("s1".into()),
        ..BridgeState::default()
    }
}

#[test]
fn subscribe_carries_a2ui_only_when_configured_and_supported() {
    let caps = A2uiSessionCapabilities::default();

    let mut plain = BridgeState {
        a2ui_supported: true,
        ..BridgeState::default()
    };
    let out = plain.api_request_to_legacy(&json!({"v": 1, "id": 1, "req": "create_session"}));
    let Outbound::Legacy(sub) = &out[0] else {
        panic!("expected subscribe frame");
    };
    assert_eq!(sub["type"], "subscribe");
    assert!(sub.get("a2ui").is_none(), "unset a2ui must leave subscribe unchanged");

    let mut from_hello = BridgeState {
        a2ui_supported: true,
        default_a2ui: Some(caps.clone()),
        ..BridgeState::default()
    };
    let out = from_hello.api_request_to_legacy(&json!({"v": 1, "id": 2, "req": "attach_session", "session_id": "s1"}));
    let Outbound::Legacy(sub) = &out[0] else {
        panic!("expected subscribe frame");
    };
    assert_eq!(sub["a2ui"], serde_json::to_value(&caps).unwrap());

    let custom = A2uiSessionCapabilities {
        version: A2UI_VERSION_V1_0.into(),
        supported_catalog_ids: vec![A2UI_BASIC_CATALOG_V1_0.into()],
        accepts_inline_catalogs: true,
    };
    let mut overridden = BridgeState {
        a2ui_supported: true,
        default_a2ui: Some(caps),
        ..BridgeState::default()
    };
    let out = overridden.api_request_to_legacy(&json!({
        "v": 1,
        "id": 3,
        "req": "create_session",
        "a2ui": custom,
    }));
    let Outbound::Legacy(sub) = &out[0] else {
        panic!("expected subscribe frame");
    };
    assert_eq!(sub["a2ui"], serde_json::to_value(&custom).unwrap());

    let mut unsupported = BridgeState {
        a2ui_supported: false,
        ..BridgeState::default()
    };
    let out = unsupported.api_request_to_legacy(&json!({
        "v": 1,
        "id": 4,
        "req": "create_session",
        "a2ui": custom,
    }));
    let [Outbound::Reply(reply)] = out.as_slice() else {
        panic!("expected unsupported reply, got {out:?}");
    };
    assert_eq!(reply.reply_to, Some(4));
    assert!(matches!(
        reply.event,
        ApiEvent::Error {
            code: ErrorCode::Unsupported,
            ..
        }
    ));
}

#[test]
fn configure_a2ui_forwards_and_correlates_done_without_ending_turn() {
    let mut unsupported = attached_a2ui_state(false);
    let refused = unsupported.api_request_to_legacy(&json!({
        "v": 1,
        "id": 10,
        "req": "configure_a2ui",
        "session_id": "s1",
        "capabilities": A2uiSessionCapabilities::default(),
    }));
    let [Outbound::Reply(reply)] = refused.as_slice() else {
        panic!("expected unsupported reply");
    };
    assert!(matches!(
        reply.event,
        ApiEvent::Error {
            code: ErrorCode::Unsupported,
            ..
        }
    ));

    let mut state = attached_a2ui_state(true);
    state.observed_turn_active = true;
    let caps = A2uiSessionCapabilities::default();
    let outbound = state.api_request_to_legacy(&json!({
        "v": 1,
        "id": 11,
        "req": "configure_a2ui",
        "session_id": "s1",
        "capabilities": caps,
    }));
    let [Outbound::Legacy(frame)] = outbound.as_slice() else {
        panic!("expected configure_a2ui frame");
    };
    assert_eq!(frame["type"], "configure_a2ui");
    let legacy_id = frame["id"].as_u64().unwrap();
    assert_eq!(frame["capabilities"], serde_json::to_value(&caps).unwrap());

    let replies = state.legacy_event_to_api(&json!({"type": "done", "id": legacy_id}));
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].reply_to, Some(11));
    assert!(matches!(replies[0].event, ApiEvent::Ok));
    assert!(
        state.observed_turn_active,
        "configure_a2ui done must not end an active turn"
    );

    let disable_out = state.api_request_to_legacy(&json!({
        "v": 1,
        "id": 12,
        "req": "configure_a2ui",
        "session_id": "s1",
    }));
    let [Outbound::Legacy(disable_frame)] = disable_out.as_slice() else {
        panic!("expected configure_a2ui frame");
    };
    assert_eq!(disable_frame["capabilities"], Value::Null);
}

#[test]
fn a2ui_event_forwards_and_maps_ack_and_validation_error() {
    let mut state = attached_a2ui_state(true);
    let outbound = state.api_request_to_legacy(&json!({
        "v": 1,
        "id": 21,
        "req": "a2ui_event",
        "session_id": "s1",
        "envelope": { "version": "v0.9", "action": { "name": "approve", "surfaceId": "main" } },
        "data_model": { "checked": true },
    }));
    let [Outbound::Legacy(frame)] = outbound.as_slice() else {
        panic!("expected a2ui_event frame");
    };
    assert_eq!(frame["type"], "a2ui_event");
    assert_eq!(frame["data_model"], json!({"checked": true}));
    let first_id = frame["id"].as_u64().unwrap();
    let ack_replies = state.legacy_event_to_api(&json!({"type": "ack", "id": first_id}));
    assert_eq!(ack_replies.len(), 1);
    assert_eq!(ack_replies[0].reply_to, Some(21));
    assert!(matches!(ack_replies[0].event, ApiEvent::Ok));

    let bad_out = state.api_request_to_legacy(&json!({
        "v": 1,
        "id": 22,
        "req": "a2ui_event",
        "session_id": "s1",
        "envelope": { "version": "v0.9", "action": { "name": "", "surfaceId": "main" } },
    }));
    let [Outbound::Legacy(bad_frame)] = bad_out.as_slice() else {
        panic!("expected a2ui_event frame");
    };
    assert!(bad_frame.get("data_model").is_none());
    let second_id = bad_frame["id"].as_u64().unwrap();
    let err_replies = state.legacy_event_to_api(&json!({
        "type": "error",
        "id": second_id,
        "message": "action.name must not be empty",
    }));
    assert_eq!(err_replies.len(), 1);
    assert_eq!(err_replies[0].reply_to, Some(22));
    assert!(matches!(
        &err_replies[0].event,
        ApiEvent::Error { message, .. } if message.contains("action.name")
    ));
}

#[test]
fn a2ui_envelope_respects_session_scope_and_completes_turn() {
    let mut state = attached_a2ui_state(true);

    let other_session = state.legacy_event_to_api(&json!({
        "type": "a2ui_envelope",
        "session_id": "other",
        "tool_call_id": "call-1",
        "sequence": 0,
        "envelope": { "version": "v0.9", "deleteSurface": { "surfaceId": "main" } },
    }));
    assert!(other_session.is_empty());

    let matched = state.legacy_event_to_api(&json!({
        "type": "a2ui_envelope",
        "session_id": "s1",
        "tool_call_id": "call-1",
        "sequence": 1,
        "envelope": { "version": "v0.9", "createSurface": { "surfaceId": "main", "catalogId": A2UI_BASIC_CATALOG_V1_0 } },
        "fallback_text": "Card fallback",
    }));
    assert_eq!(matched.len(), 1);
    assert!(matches!(
        &matched[0].event,
        ApiEvent::A2uiEnvelope {
            session_id,
            tool_call_id,
            sequence: 1,
            fallback_text,
            ..
        } if session_id == "s1" && tool_call_id == "call-1" && fallback_text.as_deref() == Some("Card fallback")
    ));

    let done = state.legacy_event_to_api(&json!({"type": "done", "id": 0}));
    assert!(
        done.iter()
            .any(|frame| matches!(frame.event, ApiEvent::TurnDone { .. })),
        "pure A2UI turn must close with TurnDone when daemon sends done"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_stream_gates_a2ui_negotiation_and_filters_unopted_envelopes() {
    async fn write_line<W: AsyncWriteExt + Unpin>(writer: &mut W, value: &Value) {
        let mut encoded = serde_json::to_string(value).unwrap();
        encoded.push('\n');
        writer.write_all(encoded.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
    }

    async fn read_line<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Value {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.unwrap();
        assert!(read > 0, "unexpected EOF waiting for frame");
        serde_json::from_str(line.trim()).unwrap()
    }

    for (request_cap, daemon_supports) in [(false, true), (true, false), (true, true)] {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let root = std::env::temp_dir().join(format!("horon-a2ui-bridge-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("daemon.sock");
        let mut listener = Listener::bind(&socket).unwrap();
        let (client, bridge) = tokio::io::duplex(8192);
        let (bridge_read, bridge_write) = tokio::io::split(bridge);
        let task = tokio::spawn(run_bridge_stream(bridge_read, bridge_write, socket));
        let (read, mut write) = tokio::io::split(client);
        let mut read = BufReader::new(read);

        let mut hello =
            json!({"v": 1, "id": 1, "req": "hello", "min_version": 1, "max_version": 1, "client": "a2ui-bridge-test"});
        if request_cap {
            hello["event_capabilities"] = json!([CAPABILITY_A2UI_V1]);
        }
        write_line(&mut write, &hello).await;

        let (daemon, _) = listener.accept().await.unwrap();
        let (probe, _) = listener.accept().await.unwrap();
        let mut probe = BufReader::new(probe);
        let _ping = read_line(&mut probe).await;
        let mut pong = json!({"type": "pong", "id": 0});
        if daemon_supports {
            pong["capabilities"] = json!([CAPABILITY_A2UI_V1]);
        }
        write_line(probe.get_mut(), &pong).await;
        drop(probe);

        let hello_ok = read_line(&mut read).await;
        let negotiated = request_cap && daemon_supports;
        assert_eq!(
            hello_ok["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!(CAPABILITY_A2UI_V1)),
            negotiated
        );

        let (daemon_read, mut daemon_write) = daemon.into_split();
        let mut daemon_read = BufReader::new(daemon_read);
        write_line(
            &mut write,
            &json!({"v": 1, "id": 2, "req": "attach_session", "session_id": "s1"}),
        )
        .await;
        for _ in 0..3 {
            let req = read_line(&mut daemon_read).await;
            if req["type"] == "state" {
                write_line(
                    &mut daemon_write,
                    &json!({"type": "state", "id": req["id"], "session_id": "s1"}),
                )
                .await;
            }
        }

        write_line(
            &mut daemon_write,
            &json!({
                "type": "a2ui_envelope",
                "session_id": "s1",
                "tool_call_id": "call-1",
                "sequence": 0,
                "envelope": { "version": "v0.9", "deleteSurface": { "surfaceId": "main" } },
            }),
        )
        .await;
        write_line(&mut daemon_write, &json!({"type": "text_delta", "text": "after"})).await;

        let envelopes = tokio::time::timeout(Duration::from_secs(5), async {
            let mut seen = Vec::new();
            loop {
                let frame = read_line(&mut read).await;
                match frame["ev"].as_str() {
                    Some("a2ui_envelope") => seen.push(frame),
                    Some("text_delta") => return seen,
                    _ => {}
                }
            }
        })
        .await
        .expect("stream continues even when a2ui_envelope is filtered");

        assert_eq!(envelopes.len(), usize::from(negotiated));
        write.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}

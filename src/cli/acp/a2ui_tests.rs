use super::tests::connection_core::*;
use super::*;
use crate::message::ToolCall;
use crate::protocol::a2ui::{A2UI_BASIC_CATALOG_V1_0, A2UI_MIME_TYPE, A2UI_VERSION_V1_0, render_a2ui_text_card};
use crate::protocol::{HistoryMessage, ServerEvent};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

const V1_SCHEMA_JSON: &str = include_str!("../../../tests/fixtures/acp/schema-v1.32.0.json");
const V2_SCHEMA_JSON: &str = include_str!("../../../tests/fixtures/acp/schema-v2.0.0-alpha.16.json");

fn cached_validator(
    cache_cell: &OnceLock<Mutex<HashMap<String, Arc<jsonschema::Validator>>>>,
    schema_json: &str,
    def_name: &str,
) -> Arc<jsonschema::Validator> {
    let cache = cache_cell.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().expect("schema cache lock");
    if let Some(cached) = cache.get(def_name) {
        return Arc::clone(cached);
    }
    let schema: Value = serde_json::from_str(schema_json).expect("vendored schema is valid JSON");
    let defs = schema.get("$defs").cloned().expect("vendored schema has $defs");
    let wrapper = json!({ "$ref": format!("#/$defs/{def_name}"), "$defs": defs });
    let compiled = Arc::new(jsonschema::validator_for(&wrapper).expect("$def compiles"));
    cache.insert(def_name.to_string(), Arc::clone(&compiled));
    compiled
}

fn v1_validator(def_name: &str) -> Arc<jsonschema::Validator> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<jsonschema::Validator>>>> = OnceLock::new();
    cached_validator(&CACHE, V1_SCHEMA_JSON, def_name)
}

fn v2_validator(def_name: &str) -> Arc<jsonschema::Validator> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<jsonschema::Validator>>>> = OnceLock::new();
    cached_validator(&CACHE, V2_SCHEMA_JSON, def_name)
}

#[track_caller]
fn assert_v1_matches(def_name: &str, value: &Value) {
    if let Err(error) = v1_validator(def_name).validate(value) {
        panic!("v1 {def_name} rejected {value}: {error}");
    }
}

#[track_caller]
fn assert_v1_update_matches(session_id: &str, update: &Value) {
    assert_v1_matches(
        "SessionNotification",
        &json!({ "sessionId": session_id, "update": update }),
    );
}

#[track_caller]
fn assert_v2_matches(def_name: &str, value: &Value) {
    if let Err(error) = v2_validator(def_name).validate(value) {
        panic!("v2 {def_name} rejected {value}: {error}");
    }
}

#[track_caller]
fn assert_v2_update_matches(update: &Value) {
    assert_v2_matches("SessionUpdate", update);
    assert_v2_matches(
        "UpdateSessionNotification",
        &json!({ "sessionId": "s1", "update": update }),
    );
}

async fn attach_v1_fake_session(client: &Client, session_id: &str) -> (Arc<DaemonSession>, FakeDaemon) {
    let (session, daemon) = daemon_pair(session_id);
    let session = Arc::new(session);
    client
        .runtime
        .sessions
        .lock()
        .await
        .insert(session_id.to_string(), Arc::clone(&session));
    (session, daemon)
}

fn create_surface_envelope(surface_id: &str) -> Value {
    json!({
        "version": "1.0",
        "createSurface": {
            "surfaceId": surface_id,
            "catalogId": A2UI_BASIC_CATALOG_V1_0
        }
    })
}

fn update_components_envelope(surface_id: &str, text: &str) -> Value {
    json!({
        "version": "1.0",
        "updateComponents": {
            "surfaceId": surface_id,
            "components": [
                { "id": "root", "component": "Column", "children": ["title"] },
                { "id": "title", "component": "Text", "text": text }
            ]
        }
    })
}

fn renderer_call_without_surface(call_id: &str) -> Value {
    json!({
        "version": "1.0",
        "callRendererFunction": {
            "functionCallId": call_id,
            "name": "openDialog",
            "args": { "title": "Confirm" }
        }
    })
}

fn valid_action_envelope(surface_id: &str) -> Value {
    json!({
        "version": "1.0",
        "action": {
            "name": "submit_form",
            "surfaceId": surface_id,
            "sourceComponentId": "submit_btn",
            "timestamp": "2026-11-24T00:00:00Z",
            "context": { "approved": true }
        }
    })
}

fn valid_renderer_response_envelope(surface_id: &str) -> Value {
    json!({
        "version": "1.0",
        "rendererFunctionResponse": {
            "surfaceId": surface_id,
            "functionCallId": "fn-call-1",
            "value": { "copied": true }
        }
    })
}

fn fake_history_event(id: u64, session_id: &str) -> Value {
    json!({
        "type": "history",
        "id": id,
        "session_id": session_id,
        "provider_model": "m0",
        "available_models": ["m0"],
        "messages": [],
        "acp_protocol_version": 2
    })
}

async fn finish_subscribe_handshake(daemon: &mut FakeDaemon, session_id: &str) -> Value {
    let subscribe = daemon.recv_type("subscribe").await;
    let subscribe_id = subscribe["id"].as_u64().expect("subscribe id");
    daemon.emit(json!({ "type": "done", "id": subscribe_id })).await;

    let history_req = daemon.recv_type("get_history").await;
    let history_id = history_req["id"].as_u64().expect("history id");
    daemon.emit(fake_history_event(history_id, session_id)).await;

    let mode_req = daemon.recv_type("set_session_approval_mode").await;
    let mode_id = mode_req["id"].as_u64().expect("set_session_approval_mode id");
    daemon.emit(json!({ "type": "ack", "id": mode_id })).await;

    subscribe
}

#[tokio::test]
async fn non_a2ui_initialize_and_subscribe_stay_unchanged_for_v1_and_v2() {
    let cwd = std::env::temp_dir().join("horon-acp-a2ui-non-a2ui");
    std::fs::create_dir_all(&cwd).expect("temp cwd");

    // V1 baseline initialize + session/new
    {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": 1 }
            }))
            .await;
        let response = client.recv().await;
        assert_eq!(response["result"]["protocolVersion"], 1);
        assert!(response["result"]["agentCapabilities"].get("_meta").is_none());
        assert_v1_matches("InitializeResponse", &response["result"]);

        let daemon_task = tokio::spawn(async move {
            let mut daemon = daemons.recv().await.expect("daemon connection");
            finish_subscribe_handshake(&mut daemon, "s-v1").await
        });

        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/new",
                "params": { "cwd": cwd, "mcpServers": [] }
            }))
            .await;
        let (_before, session_resp) = client.recv_response(json!(2)).await;
        assert!(session_resp.get("result").is_some(), "{session_resp}");
        let subscribe = daemon_task.await.expect("daemon task");
        assert!(subscribe.get("a2ui").is_none() || subscribe["a2ui"].is_null());
        client.close_stdin().await;
    }

    // V2 baseline initialize + session/new
    {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client.initialize_v2().await;

        let daemon_task = tokio::spawn(async move {
            let mut daemon = daemons.recv().await.expect("daemon connection");
            finish_subscribe_handshake(&mut daemon, "s-v2").await
        });

        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/new",
                "params": { "cwd": cwd, "mcpServers": [] }
            }))
            .await;
        let (_before, session_resp) = client.recv_response(json!(2)).await;
        assert!(session_resp.get("result").is_some(), "{session_resp}");
        let subscribe = daemon_task.await.expect("daemon task");
        assert!(subscribe.get("a2ui").is_none() || subscribe["a2ui"].is_null());
        client.close_stdin().await;
    }

    // Non-A2UI live mappers ignore A2uiEnvelope and preserve normal tool UI.
    let env = create_surface_envelope("main");
    let a2ui_event = ServerEvent::A2uiEnvelope {
        session_id: "s1".to_string(),
        tool_call_id: "call-1".to_string(),
        sequence: 0,
        envelope: env,
        fallback_text: Some("fallback".to_string()),
    };
    let mut v1_mapper = EventMapper::new(AcpProfile::Standard, MapperOptions::default(), "t1".to_string());
    assert!(v1_mapper.map_event(a2ui_event.clone()).is_empty());
    let tool_start = v1_mapper.map_event(ServerEvent::ToolStart {
        id: "call-1".to_string(),
        name: "a2ui".to_string(),
    });
    assert_eq!(tool_start[0]["sessionUpdate"], "tool_call");

    let mut v2_mapper = v2::mapper::V2Mapper::new("start-1", None);
    assert!(v2_mapper.map_event(a2ui_event).is_empty());
    let v2_tool = v2_mapper.map_event(ServerEvent::ToolStart {
        id: "call-1".to_string(),
        name: "a2ui".to_string(),
    });
    assert_eq!(v2_tool[0]["sessionUpdate"], "tool_call_update");
}

#[tokio::test]
async fn v1_and_v2_a2ui_initialize_negotiation_advertises_and_subscribes() {
    let cwd = std::env::temp_dir().join("horon-acp-a2ui-negotiate");
    std::fs::create_dir_all(&cwd).expect("temp cwd");

    // V1 using _meta.a2uiRendererCapabilities (nested v1.0 shape)
    {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "_meta": {
                            "a2uiRendererCapabilities": {
                                "v1.0": {
                                    "supportedCatalogIds": [A2UI_BASIC_CATALOG_V1_0],
                                    "acceptsInlineCatalogs": true
                                }
                            }
                        }
                    }
                }
            }))
            .await;
        let response = client.recv().await;
        assert_v1_matches("InitializeResponse", &response["result"]);
        let adv = &response["result"]["agentCapabilities"]["_meta"]["a2ui"];
        assert_eq!(adv["supported"], true);
        assert_eq!(adv["version"], A2UI_VERSION_V1_0);
        assert_eq!(adv["mimeType"], A2UI_MIME_TYPE);
        assert_eq!(adv["acceptsInlineCatalogs"], true);
        assert_eq!(adv["supportedCatalogIds"], json!([A2UI_BASIC_CATALOG_V1_0]));

        let daemon_task = tokio::spawn(async move {
            let mut daemon = daemons.recv().await.expect("daemon connection");
            finish_subscribe_handshake(&mut daemon, "s-v1-a2ui").await
        });

        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/new",
                "params": { "cwd": cwd, "mcpServers": [] }
            }))
            .await;
        let (_before, session_resp) = client.recv_response(json!(2)).await;
        assert!(session_resp.get("result").is_some(), "{session_resp}");
        let subscribe = daemon_task.await.expect("daemon task");
        assert_eq!(subscribe["a2ui"]["version"], A2UI_VERSION_V1_0);
        assert_eq!(subscribe["a2ui"]["accepts_inline_catalogs"], true);
        assert_eq!(
            subscribe["a2ui"]["supported_catalog_ids"],
            json!([A2UI_BASIC_CATALOG_V1_0])
        );
        client.close_stdin().await;
    }

    // V2 using capabilities._meta.a2ui
    {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": 2,
                    "info": { "name": "v2-a2ui-client", "version": "1.0.0" },
                    "capabilities": {
                        "_meta": {
                            "a2ui": {
                                "supportedCatalogIds": ["https://a2ui.org/specification/v1_0/standard_catalog.json"],
                                "acceptsInlineCatalogs": false
                            }
                        }
                    }
                }
            }))
            .await;
        let response = client.recv().await;
        assert_v2_matches("InitializeResponse", &response["result"]);
        let adv = &response["result"]["capabilities"]["_meta"]["a2ui"];
        assert_eq!(adv["supported"], true);
        assert_eq!(adv["version"], A2UI_VERSION_V1_0);
        assert_eq!(adv["mimeType"], A2UI_MIME_TYPE);
        assert_eq!(
            adv["supportedCatalogIds"],
            json!(["https://a2ui.org/specification/v1_0/standard_catalog.json"])
        );

        let daemon_task = tokio::spawn(async move {
            let mut daemon = daemons.recv().await.expect("daemon connection");
            finish_subscribe_handshake(&mut daemon, "s-v2-a2ui").await
        });

        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/new",
                "params": { "cwd": cwd, "mcpServers": [] }
            }))
            .await;
        let (_before, session_resp) = client.recv_response(json!(2)).await;
        assert!(session_resp.get("result").is_some(), "{session_resp}");
        let subscribe = daemon_task.await.expect("daemon task");
        assert_eq!(subscribe["a2ui"]["version"], A2UI_VERSION_V1_0);
        assert!(
            !subscribe["a2ui"]
                .get("accepts_inline_catalogs")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
        assert_eq!(
            subscribe["a2ui"]["supported_catalog_ids"],
            json!(["https://a2ui.org/specification/v1_0/standard_catalog.json"])
        );
        client.close_stdin().await;
    }

    // Explicit opt-out does not negotiate even when capability object is present.
    let mut opt_out_client = start_client_with(None);
    opt_out_client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 2,
                "info": { "name": "v2-opt-out", "version": "1.0.0" },
                "capabilities": {
                    "_meta": { "a2ui": { "enabled": false } }
                }
            }
        }))
        .await;
    let response = opt_out_client.recv().await;
    assert_v2_matches("InitializeResponse", &response["result"]);
    assert!(response["result"]["capabilities"]["_meta"].get("a2ui").is_none());
    assert!(!opt_out_client.runtime.is_a2ui_negotiated());
    opt_out_client.close_stdin().await;
}

#[test]
fn outbound_a2ui_envelope_conforms_to_v1_and_v2_schemas_and_suppresses_tool_ui() {
    let env_surface = create_surface_envelope("card:1/main");
    let env_call = renderer_call_without_surface("fn call/1");

    let events = vec![
        ServerEvent::ToolStart {
            id: "call-a2ui-1".to_string(),
            name: "a2ui".to_string(),
        },
        ServerEvent::ToolInput {
            id: None,
            delta: r#"{"envelopes":[]}"#.to_string(),
        },
        ServerEvent::ToolExec {
            id: "call-a2ui-1".to_string(),
            name: "a2ui".to_string(),
        },
        ServerEvent::A2uiEnvelope {
            session_id: "s1".to_string(),
            tool_call_id: "call-a2ui-1".to_string(),
            sequence: 0,
            envelope: env_surface.clone(),
            fallback_text: Some("Surface fallback".to_string()),
        },
        ServerEvent::A2uiEnvelope {
            session_id: "s1".to_string(),
            tool_call_id: "call-a2ui-1".to_string(),
            sequence: 1,
            envelope: env_call.clone(),
            fallback_text: None,
        },
        ServerEvent::ToolDone {
            id: "call-a2ui-1".to_string(),
            name: "a2ui".to_string(),
            output: "[A2UI response]".to_string(),
            error: None,
            verdict: None,
        },
        ServerEvent::TextDelta {
            text: "Summary text".to_string(),
        },
    ];

    // V1 mapping
    let options = MapperOptions::new(&json!({ "_meta": { "a2ui": true } }), None);
    assert!(options.a2ui);
    let mut v1_mapper = EventMapper::new(AcpProfile::Standard, options, "turn-1".to_string());
    let v1_updates: Vec<Value> = events
        .iter()
        .cloned()
        .flat_map(|event| v1_mapper.map_event(event))
        .collect();
    assert_eq!(
        v1_updates.len(),
        3,
        "only two A2UI chunks + text chunk expected: {v1_updates:?}"
    );
    for update in &v1_updates {
        assert_v1_update_matches("s1", update);
    }
    assert_eq!(v1_updates[0]["sessionUpdate"], "agent_message_chunk");
    assert_eq!(v1_updates[0]["content"]["type"], "resource");
    assert_eq!(
        v1_updates[0]["content"]["resource"]["uri"],
        "a2ui://surface/card%3A1%2Fmain"
    );
    assert_eq!(v1_updates[0]["content"]["resource"]["mimeType"], A2UI_MIME_TYPE);
    let decoded_surface: Value =
        serde_json::from_str(v1_updates[0]["content"]["resource"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(decoded_surface, env_surface);

    assert_eq!(
        v1_updates[1]["content"]["resource"]["uri"],
        "a2ui://surface/fn%20call%2F1"
    );
    let decoded_call: Value =
        serde_json::from_str(v1_updates[1]["content"]["resource"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(decoded_call, env_call);
    assert_eq!(v1_updates[2]["content"]["type"], "text");
    assert_eq!(v1_updates[2]["content"]["text"], "Summary text");

    // V2 mapping (streamed chunks + TextReplace whole message preserving embedded resources)
    let mut v2_mapper = v2::mapper::V2Mapper::new("msg-1", None).with_a2ui(true);
    let mut v2_updates: Vec<Value> = events
        .into_iter()
        .flat_map(|event| v2_mapper.map_event(event))
        .collect();
    v2_updates.extend(v2_mapper.map_event(ServerEvent::TextReplace {
        text: "Final summary".to_string(),
    }));
    assert_eq!(v2_updates.len(), 4, "{v2_updates:?}");
    for update in &v2_updates {
        assert_v2_update_matches(update);
    }
    assert_eq!(v2_updates[0]["sessionUpdate"], "agent_message_chunk");
    assert_eq!(v2_updates[0]["content"]["type"], "resource");
    assert_eq!(v2_updates[1]["sessionUpdate"], "agent_message_chunk");
    assert_eq!(v2_updates[2]["sessionUpdate"], "agent_message_chunk");
    // All belong to same messageId because a2ui is not a tool round.
    let msg_id = v2_updates[0]["messageId"].as_str().unwrap();
    assert_eq!(v2_updates[2]["messageId"], msg_id);
    assert_eq!(v2_updates[3]["sessionUpdate"], "agent_message");
    assert_eq!(v2_updates[3]["messageId"], msg_id);
    let finalized = v2_updates[3]["content"].as_array().unwrap();
    assert_eq!(finalized.len(), 3);
    assert_eq!(finalized[0], json!({ "type": "text", "text": "Final summary" }));
    assert_eq!(finalized[1]["type"], "resource");
    assert_eq!(finalized[2]["type"], "resource");
}

#[tokio::test]
async fn inbound_v1_and_v2_session_prompt_routes_a2ui_events_and_rejects_invalid() {
    let action_env = valid_action_envelope("form-1");
    let response_env = valid_renderer_response_envelope("form-1");
    let data_model = json!({ "name": "Ada", "count": 2 });

    // V1 inbound EmbeddedResource action + _meta.a2uiEvent no-reply + invalid -32602
    {
        let mut client = start_client_with(None);
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": 1,
                    "clientCapabilities": { "_meta": { "a2ui": true } }
                }
            }))
            .await;
        let _init = client.recv().await;
        let (_session, mut daemon) = attach_v1_fake_session(&client, "s1").await;

        // 1) Invalid A2UI payload rejected before touching daemon.
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "session/prompt",
                "params": {
                    "sessionId": "s1",
                    "prompt": [{
                        "type": "resource",
                        "resource": {
                            "uri": "a2ui://surface/form-1",
                            "mimeType": A2UI_MIME_TYPE,
                            "text": "{\"version\":\"1.0\",\"unknown\":true}"
                        }
                    }]
                }
            }))
            .await;
        let (_before, invalid_resp) = client.recv_response(json!(10)).await;
        assert_eq!(invalid_resp["error"]["code"], JSONRPC_INVALID_PARAMS);

        // 2) Valid action via EmbeddedResource block + _meta.a2uiRendererDataModel.
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "session/prompt",
                "params": {
                    "sessionId": "s1",
                    "prompt": [{
                        "type": "resource",
                        "resource": {
                            "uri": "a2ui://surface/form-1",
                            "mimeType": A2UI_MIME_TYPE,
                            "text": action_env.to_string()
                        }
                    }],
                    "_meta": { "a2uiRendererDataModel": data_model }
                }
            }))
            .await;
        let req = daemon.recv_type("a2ui_event").await;
        assert_eq!(req["envelope"], action_env);
        assert_eq!(req["data_model"], data_model);
        let req_id = req["id"].as_u64().unwrap();
        daemon.emit(json!({ "type": "done", "id": req_id })).await;
        let (_before, action_resp) = client.recv_response(json!(11)).await;
        assert_eq!(action_resp["result"]["stopReason"], "end_turn");

        // 3) Context-only rendererFunctionResponse via _meta.a2uiEvent -> ContextMessageAdded ends prompt cleanly.
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 12,
                "method": "session/prompt",
                "params": {
                    "sessionId": "s1",
                    "_meta": { "a2uiEvent": response_env }
                }
            }))
            .await;
        let req2 = daemon.recv_type("a2ui_event").await;
        assert_eq!(req2["envelope"], response_env);
        let req2_id = req2["id"].as_u64().unwrap();
        daemon
            .emit(json!({ "type": "context_message_added", "id": req2_id }))
            .await;
        let (_before, ctx_resp) = client.recv_response(json!(12)).await;
        assert_eq!(ctx_resp["result"]["stopReason"], "end_turn");

        client.close_stdin().await;
    }

    // V2 inbound action (UserMessageInserted) + no-reply rendererFunctionResponse (ContextMessageAdded)
    {
        let (connector, mut daemons) = scripted_daemon();
        let mut client = start_client_with(Some(connector));
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": 2,
                    "info": { "name": "v2-a2ui", "version": "1.0.0" },
                    "capabilities": { "_meta": { "a2ui": true } }
                }
            }))
            .await;
        let _init = client.recv().await;

        let daemon_task = tokio::spawn(async move {
            let mut daemon = daemons.recv().await.expect("daemon connection");
            let _sub = finish_subscribe_handshake(&mut daemon, "s2").await;
            daemon
        });
        let cwd = std::env::temp_dir().join("horon-acp-a2ui-v2-prompt");
        std::fs::create_dir_all(&cwd).expect("temp cwd");
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/new",
                "params": { "cwd": cwd, "mcpServers": [] }
            }))
            .await;
        let (_before, _new_resp) = client.recv_response(json!(2)).await;
        let mut daemon = daemon_task.await.expect("daemon ready");

        // 1) Action via EmbeddedResource -> UserMessageInserted -> running/idle.
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 20,
                "method": "session/prompt",
                "params": {
                    "sessionId": "s2",
                    "prompt": [{
                        "type": "resource",
                        "resource": {
                            "uri": "a2ui://surface/form-1",
                            "mimeType": A2UI_MIME_TYPE,
                            "text": action_env.to_string()
                        }
                    }],
                    "_meta": { "a2uiRendererDataModel": data_model }
                }
            }))
            .await;
        let ev1 = daemon.recv_type("a2ui_event").await;
        assert_eq!(ev1["envelope"], action_env);
        assert_eq!(ev1["data_model"], data_model);
        let ev1_id = ev1["id"].as_u64().unwrap();
        daemon
            .emit(json!({ "type": "user_message_inserted", "id": ev1_id, "message_id": "m-act-1" }))
            .await;
        let (_before, resp1) = client.recv_response(json!(20)).await;
        assert_eq!(resp1["result"]["messageId"], "m-act-1");

        daemon
            .emit(json!({
                "type": "foreground_state",
                "session_id": "s2",
                "running": true,
                "run_id": "r1",
                "start_message_id": "m-act-1"
            }))
            .await;
        daemon
            .emit(json!({
                "type": "foreground_state",
                "session_id": "s2",
                "running": false,
                "run_id": "r1",
                "start_message_id": "m-act-1",
                "stop_reason": "end_turn"
            }))
            .await;
        let _settled = client.barrier("after-action").await;

        // 2) No-reply rendererFunctionResponse via _meta.a2uiEvent -> ContextMessageAdded releases slot.
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 21,
                "method": "session/prompt",
                "params": {
                    "sessionId": "s2",
                    "prompt": [{ "type": "text", "text": " " }],
                    "_meta": { "a2uiEvent": response_env }
                }
            }))
            .await;
        let ev2 = daemon.recv_type("a2ui_event").await;
        assert_eq!(ev2["envelope"], response_env);
        let ev2_id = ev2["id"].as_u64().unwrap();
        daemon
            .emit(json!({ "type": "context_message_added", "id": ev2_id }))
            .await;
        let (_before, resp2) = client.recv_response(json!(21)).await;
        assert_eq!(resp2["result"]["messageId"], format!("ctx-{ev2_id}"));

        // 3) Subsequent normal prompt is not blocked by ContextMessageAdded.
        client
            .send(json!({
                "jsonrpc": "2.0",
                "id": 22,
                "method": "session/prompt",
                "params": {
                    "sessionId": "s2",
                    "prompt": [{ "type": "text", "text": "next question" }]
                }
            }))
            .await;
        let msg3 = daemon.recv_type("message").await;
        assert_eq!(msg3["content"], "next question");
        let msg3_id = msg3["id"].as_u64().unwrap();
        daemon
            .emit(json!({ "type": "user_message_inserted", "id": msg3_id, "message_id": "m-next" }))
            .await;
        let (_before, resp3) = client.recv_response(json!(22)).await;
        assert_eq!(resp3["result"]["messageId"], "m-next");

        client.close_stdin().await;
    }
}

#[test]
fn replay_renders_embedded_resource_for_a2ui_clients_and_text_fallback_otherwise() {
    let env_surface = create_surface_envelope("profile-card");
    let env_components = update_components_envelope("profile-card", "Ada Lovelace");
    let envelopes = vec![env_surface.clone(), env_components.clone()];
    let card_text = render_a2ui_text_card(&envelopes, Some("Ada Lovelace profile"));
    let messages = vec![
        HistoryMessage {
            role: "user".to_string(),
            content: "Show profile".to_string(),
            message_id: Some("u-1".to_string()),
            turn_start: true,
            ..HistoryMessage::default()
        },
        HistoryMessage {
            role: "tool".to_string(),
            content: "[A2UI response]".to_string(),
            tool_data: Some(ToolCall {
                id: "call-a2ui".to_string(),
                name: "a2ui".to_string(),
                input: json!({
                    "envelopes": envelopes,
                    "fallback_text": "Ada Lovelace profile"
                }),
                intent: None,
                thought_signature: None,
            }),
            ..HistoryMessage::default()
        },
        HistoryMessage {
            role: "assistant".to_string(),
            content: format!("Here is your card:\n\n{card_text}"),
            ..HistoryMessage::default()
        },
    ];

    // V1 replay with A2UI negotiated -> EmbeddedResource chunks + prose, no tool_call.
    let v1_a2ui_opts = MapperOptions::new(&json!({ "_meta": { "a2ui": true } }), None);
    let v1_a2ui = replay_updates(&messages, &[], &v1_a2ui_opts);
    for update in &v1_a2ui {
        assert_v1_update_matches("s1", update);
    }
    assert!(
        v1_a2ui
            .iter()
            .all(|u| u["sessionUpdate"] != "tool_call" && u["sessionUpdate"] != "tool_call_update"),
        "a2ui tool call must be suppressed: {v1_a2ui:?}"
    );
    let v1_resources: Vec<&Value> = v1_a2ui.iter().filter(|u| u["content"]["type"] == "resource").collect();
    assert_eq!(v1_resources.len(), 2);
    assert_eq!(
        v1_resources[0]["content"]["resource"]["uri"],
        "a2ui://surface/profile-card"
    );
    assert_eq!(v1_resources[0]["content"]["resource"]["mimeType"], A2UI_MIME_TYPE);
    assert!(v1_a2ui.iter().any(|u| u["content"]["text"] == "Here is your card:"));

    // V1 replay without A2UI -> readable text card, no EmbeddedResource, no noisy a2ui tool_call.
    let v1_plain = replay_updates(&messages, &[], &MapperOptions::default());
    assert!(v1_plain.iter().all(|u| u["sessionUpdate"] != "tool_call"));
    assert!(v1_plain.iter().all(|u| u["content"]["type"] != "resource"));
    assert!(
        v1_plain
            .iter()
            .any(|u| u["content"]["text"].as_str().is_some_and(|t| t.contains(&card_text)))
    );

    // V2 replay with A2UI negotiated -> whole agent_message containing prose + EmbeddedResources.
    let v2_a2ui = v2::replay::replay_updates_with_a2ui(&messages, &[], Path::new("/"), true);
    for update in &v2_a2ui {
        assert_v2_update_matches(update);
    }
    let agent_msg = v2_a2ui
        .iter()
        .find(|u| u["sessionUpdate"] == "agent_message")
        .expect("agent_message in v2 replay");
    let blocks = agent_msg["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0], json!({ "type": "text", "text": "Here is your card:" }));
    assert_eq!(blocks[1]["type"], "resource");
    assert_eq!(blocks[2]["type"], "resource");
    assert!(v2_a2ui.iter().all(|u| u["sessionUpdate"] != "tool_call_update"));

    // V2 replay without A2UI -> plain text card in agent_message, no EmbeddedResource.
    let v2_plain = v2::replay::replay_updates_with_a2ui(&messages, &[], Path::new("/"), false);
    for update in &v2_plain {
        assert_v2_update_matches(update);
    }
    let plain_agent_msg = v2_plain
        .iter()
        .find(|u| u["sessionUpdate"] == "agent_message")
        .expect("agent_message in non-a2ui v2 replay");
    assert!(
        plain_agent_msg["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&card_text)
    );
}

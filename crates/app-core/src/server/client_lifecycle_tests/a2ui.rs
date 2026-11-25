//! Daemon socket round-trip test for A2UI capability negotiation and inbound events.

use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use futures::stream;
use horon_agent_tool_types::a2ui::{A2UI_BASIC_CATALOG_V1_0, A2uiSessionCapabilities};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct IsolatedA2uiSocketEnv {
    prev_home: Option<std::ffi::OsString>,
    prev_runtime: Option<std::ffi::OsString>,
    home: tempfile::TempDir,
    _runtime: tempfile::TempDir,
}

impl IsolatedA2uiSocketEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("isolated agent home");
        crate::storage::create_store_dir(&home.path().join("sessions"), false).expect("create sessions dir");
        let runtime = tempfile::tempdir().expect("isolated runtime dir");
        let prev_home = std::env::var_os("HORON_HOME");
        let prev_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
        crate::env::set_var("HORON_HOME", home.path());
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        crate::config::Config::invalidate_cache();
        Self {
            prev_home,
            prev_runtime,
            home,
            _runtime: runtime,
        }
    }
}

impl Drop for IsolatedA2uiSocketEnv {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(v) => crate::env::set_var("HORON_HOME", v),
            None => crate::env::remove_var("HORON_HOME"),
        }
        match &self.prev_runtime {
            Some(v) => crate::env::set_var("HORON_AGENT_RUNTIME_DIR", v),
            None => crate::env::remove_var("HORON_AGENT_RUNTIME_DIR"),
        }
        crate::config::Config::invalidate_cache();
    }
}

type RecordedRequest = (Vec<Message>, Vec<ToolDefinition>);

#[derive(Clone, Default)]
struct SocketA2uiProvider {
    script: Arc<StdMutex<VecDeque<Vec<StreamEvent>>>>,
    requests: Arc<StdMutex<Vec<RecordedRequest>>>,
    calls: Arc<AtomicUsize>,
}

impl SocketA2uiProvider {
    fn push(&self, events: Vec<StreamEvent>) {
        self.script.lock().expect("script lock").push_back(events);
    }
}

#[async_trait]
impl Provider for SocketA2uiProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("requests lock")
            .push((messages.to_vec(), tools.to_vec()));
        let events = self
            .script
            .lock()
            .expect("script lock")
            .pop_front()
            .expect("unexpected provider round");
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }

    async fn complete_split(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        static_system: &str,
        _dynamic_system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.complete(messages, tools, static_system, resume_session_id).await
    }

    fn name(&self) -> &str {
        "a2ui-socket-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn send_req(writer: &mut crate::transport::WriteHalf, value: serde_json::Value) {
    writer.write_all(format!("{value}\n").as_bytes()).await.unwrap();
}

async fn recv_until(
    reader: &mut BufReader<crate::transport::ReadHalf>,
    predicate: impl Fn(&ServerEvent) -> bool,
) -> (Vec<ServerEvent>, ServerEvent) {
    let mut seen = Vec::new();
    let matched = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).await.unwrap() > 0);
            let event: ServerEvent = serde_json::from_str(&line).unwrap();
            let hit = predicate(&event);
            seen.push(event.clone());
            if hit {
                return event;
            }
        }
    })
    .await
    .expect("daemon event timeout");
    (seen, matched)
}

#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "serializes HORON_HOME mutation across this async socket test"
)]
async fn socket_subscribe_a2ui_round_trip_and_invalid_event_rejection() {
    let _lock = crate::storage::lock_test_env();
    let env = IsolatedA2uiSocketEnv::new();
    let provider = SocketA2uiProvider::default();

    let a2ui_args = json!({
        "envelopes": [
            {
                "version": "1.0",
                "createSurface": {
                    "surfaceId": "s1",
                    "catalogId": A2UI_BASIC_CATALOG_V1_0
                }
            },
            {
                "version": "1.0",
                "updateComponents": {
                    "surfaceId": "s1",
                    "components": [
                        {"id": "root", "component": "Button", "child": "Approve", "action": {"event": {"name": "approve"}}}
                    ]
                }
            }
        ],
        "fallback_text": "Approve deployment card"
    })
    .to_string();

    provider.push(vec![
        StreamEvent::ToolUseStart {
            id: "call_ui_1".to_string(),
            name: crate::tool::A2UI_TOOL.to_string(),
        },
        StreamEvent::ToolInputDeltaFor {
            id: "call_ui_1".to_string(),
            delta: a2ui_args,
        },
        StreamEvent::ToolUseEndFor {
            id: "call_ui_1".to_string(),
        },
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".to_string()),
        },
    ]);
    provider.push(vec![
        StreamEvent::TextDelta("Approved".to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
    ]);

    let provider_template: Arc<dyn Provider> = Arc::new(provider.clone());
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let plan_proposals = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (_global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
    let server_task = tokio::spawn(handle_client(
        server_stream,
        Arc::clone(&sessions),
        _global_event_tx,
        provider_template,
        global_is_processing,
        global_session_id,
        client_count,
        client_connections,
        swarm_members,
        swarms_by_id,
        plan_proposals,
        swarm_plans,
        swarm_coordinators,
        file_touch,
        event_history,
        event_counter,
        swarm_event_tx,
        "horon-agent-test".to_string(),
        "🧪".to_string(),
        mcp_pool,
        shutdown_signals,
        soft_interrupt_queues,
        AwaitMembersRuntime::default(),
        SwarmMutationRuntime::default(),
        std::time::Duration::ZERO,
    ));

    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);

    send_req(
        &mut client_writer,
        serde_json::to_value(Request::Subscribe {
            system_prompt: None,
            supports_user_response: false,
            supports_ask_requests: false,
            supports_permission_requests: false,
            id: 1,
            working_dir: env.home.path().to_str().map(str::to_string),
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
            acp: None,
            history_message_ids: false,
            a2ui: Some(A2uiSessionCapabilities::default()),
            fresh_session: false,
        })
        .unwrap(),
    )
    .await;
    recv_until(&mut client_reader, |ev| matches!(ev, ServerEvent::Done { id: 1 })).await;

    send_req(
        &mut client_writer,
        json!({"type": "message", "id": 2, "content": "open approval card"}),
    )
    .await;
    let (turn1_events, _) = recv_until(&mut client_reader, |ev| matches!(ev, ServerEvent::Done { id: 2 })).await;
    assert_eq!(
        turn1_events
            .iter()
            .filter(|ev| matches!(ev, ServerEvent::A2uiEnvelope { .. }))
            .count(),
        2
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    // Malformed A2UI event must be rejected without calling the provider.
    send_req(
        &mut client_writer,
        serde_json::to_value(Request::A2uiEvent {
            id: 3,
            envelope: json!({"version": "0.1", "action": {"name": "approve"}}),
            data_model: None,
        })
        .unwrap(),
    )
    .await;
    let (_, bad_ev) = recv_until(&mut client_reader, |ev| matches!(ev, ServerEvent::Error { id: 3, .. })).await;
    assert!(matches!(bad_ev, ServerEvent::Error { id: 3, .. }));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    // Valid interactive A2UI action opens turn 2 with turn 1 A2UI summary + dataModel.
    send_req(
        &mut client_writer,
        serde_json::to_value(Request::A2uiEvent {
            id: 4,
            envelope: json!({
                "version": "1.0",
                "action": {
                    "name": "approve",
                    "surfaceId": "s1",
                    "sourceComponentId": "root",
                    "timestamp": "2026-11-24T00:00:00Z",
                    "context": {"id": 42}
                }
            }),
            data_model: Some(json!({"count": 3})),
        })
        .unwrap(),
    )
    .await;
    let (turn2_events, _) = recv_until(&mut client_reader, |ev| matches!(ev, ServerEvent::Done { id: 4 })).await;
    assert!(
        turn2_events
            .iter()
            .any(|ev| matches!(ev, ServerEvent::TextDelta { text } if text == "Approved"))
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    let requests = provider.requests.lock().expect("requests lock").clone();
    let (turn1_msgs, turn1_tools) = &requests[0];
    assert!(
        serde_json::to_string(turn1_msgs)
            .unwrap()
            .contains("open approval card")
    );
    assert!(turn1_tools.iter().any(|t| t.name == crate::tool::A2UI_TOOL));
    let (turn2_msgs, _) = &requests[1];
    assert!(turn2_msgs.iter().flat_map(|m| &m.content).any(|b| matches!(
        b,
        crate::message::ContentBlock::A2ui { fallback_text, .. }
            if fallback_text.as_deref() == Some("Approve deployment card")
    )));
    assert!(turn2_msgs.iter().flat_map(|m| &m.content).any(|b| matches!(
        b,
        crate::message::ContentBlock::Text { text, .. }
            if text.contains("[A2UI action]") && text.contains("approve") && text.contains("\"count\":3")
    )));

    sessions.write().await.clear();
    drop(client_writer);
    drop(client_reader);
    tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

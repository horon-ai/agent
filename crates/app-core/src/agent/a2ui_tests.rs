//! Behavioral acceptance tests for capability-gated A2UI v1.0 turns.

use super::*;
use crate::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use horon_agent_tool_types::a2ui::A2uiSessionCapabilities;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc as tokio_mpsc;

struct IsolatedHome {
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("isolated agent home");
        crate::storage::create_store_dir(&home.path().join("sessions"), false).expect("create sessions dir");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.previous {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

#[derive(Clone, Default)]
struct ScriptedA2uiProvider {
    streams: Arc<StdMutex<VecDeque<Vec<Result<StreamEvent>>>>>,
    requests: Arc<StdMutex<Vec<Vec<Message>>>>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedA2uiProvider {
    fn push_stream(&self, events: Vec<StreamEvent>) {
        self.streams
            .lock()
            .expect("streams lock")
            .push_back(events.into_iter().map(Ok).collect());
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Vec<Message>> {
        self.requests.lock().expect("requests lock").clone()
    }
}

#[async_trait]
impl Provider for ScriptedA2uiProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().expect("requests lock").push(messages.to_vec());
        let events = self
            .streams
            .lock()
            .expect("streams lock")
            .pop_front()
            .expect("unexpected extra provider round");
        Ok(Box::pin(futures::stream::iter(events)))
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
        "a2ui-scripted-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn user_message(text: &str) -> (Role, Vec<ContentBlock>) {
    (
        Role::User,
        vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    )
}

#[tokio::test]
async fn non_a2ui_session_keeps_tool_surface_and_prompt_unchanged_and_rejects_a2ui() {
    let _guard = crate::storage::lock_test_env();
    let _home = IsolatedHome::new();
    let provider = Arc::new(ScriptedA2uiProvider::default());
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let baseline_tools = agent.tool_definitions().await;
    let baseline_tools_json = serde_json::to_string(&*baseline_tools).expect("serialize baseline tools");
    let baseline_prompt = agent.build_request_system_prompt(&baseline_tools);
    assert!(
        !baseline_tools.iter().any(|t| t.name == crate::tool::A2UI_TOOL),
        "default session must not expose a2ui tool"
    );
    assert!(
        agent.validate_tool_allowed(crate::tool::A2UI_TOOL).is_err(),
        "default session must reject executing hallucinated a2ui tool calls"
    );

    agent
        .set_a2ui_capabilities(Some(A2uiSessionCapabilities::default()))
        .await;
    let enabled_tools = agent.tool_definitions().await;
    let enabled_prompt = agent.build_request_system_prompt(&enabled_tools);
    assert!(enabled_tools.iter().any(|t| t.name == crate::tool::A2UI_TOOL));
    assert!(agent.validate_tool_allowed(crate::tool::A2UI_TOOL).is_ok());
    assert_eq!(
        enabled_prompt.static_part, baseline_prompt.static_part,
        "enabling A2UI must not mutate the static system prompt"
    );

    agent.set_a2ui_capabilities(None).await;
    let restored_tools = agent.tool_definitions().await;
    let restored_tools_json = serde_json::to_string(&*restored_tools).expect("serialize restored tools");
    let restored_prompt = agent.build_request_system_prompt(&restored_tools);
    assert_eq!(restored_tools_json, baseline_tools_json);
    assert_eq!(restored_prompt.static_part, baseline_prompt.static_part);
    assert!(agent.validate_tool_allowed(crate::tool::A2UI_TOOL).is_err());
}

#[tokio::test]
async fn a2ui_streaming_turn_emits_envelopes_before_tool_done_and_finishes_in_one_round() {
    let _guard = crate::storage::lock_test_env();
    let _home = IsolatedHome::new();
    let provider = Arc::new(ScriptedA2uiProvider::default());
    let create_env = json!({
        "version": "1.0",
        "createSurface": {
            "surfaceId": "card-1",
            "catalogId": horon_agent_tool_types::a2ui::A2UI_BASIC_CATALOG_V1_0
        }
    });
    let update_env = json!({
        "version": "1.0",
        "updateComponents": {
            "surfaceId": "card-1",
            "components": [
                {"id": "root", "component": "Text", "text": "Build healthy"}
            ]
        }
    });
    let part1 = format!(r#"{{"envelopes":[{create_env},"#);
    let part2 = format!(r#"{update_env}],"fallback_text":"Status card"}}"#);
    provider.push_stream(vec![
        StreamEvent::ToolUseStart {
            id: "call_a2ui_1".to_string(),
            name: crate::tool::A2UI_TOOL.to_string(),
        },
        StreamEvent::ToolInputDeltaFor {
            id: "call_a2ui_1".to_string(),
            delta: part1,
        },
        StreamEvent::ToolInputDeltaFor {
            id: "call_a2ui_1".to_string(),
            delta: part2,
        },
        StreamEvent::ToolUseEndFor {
            id: "call_a2ui_1".to_string(),
        },
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".to_string()),
        },
    ]);

    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider.clone(), registry);
    agent
        .set_a2ui_capabilities(Some(A2uiSessionCapabilities::default()))
        .await;
    let (role, blocks) = user_message("Show build status card");
    agent.add_message(role, blocks);

    let (tx, mut rx) = tokio_mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(tx).await.expect("a2ui turn succeeds");
    assert_eq!(provider.call_count(), 1, "final_response defaults to single round");

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    let env0_pos = events
        .iter()
        .position(|e| matches!(e, ServerEvent::A2uiEnvelope { sequence: 0, .. }))
        .expect("sequence 0 envelope emitted");
    let env1_pos = events
        .iter()
        .position(|e| matches!(e, ServerEvent::A2uiEnvelope { sequence: 1, .. }))
        .expect("sequence 1 envelope emitted");
    let done_pos = events
        .iter()
        .position(|e| matches!(e, ServerEvent::ToolDone { name, error: None, .. } if name == crate::tool::A2UI_TOOL))
        .expect("a2ui ToolDone emitted");
    assert!(env0_pos < env1_pos && env1_pos < done_pos);

    let session = Session::load(agent.session_id()).expect("persisted session loads");
    let has_a2ui_block = session.visible_conversation_messages().into_iter().any(|m| {
        m.role == Role::Assistant
            && m.content.iter().any(|b| {
                matches!(
                    b,
                    ContentBlock::A2ui { envelopes, fallback_text }
                        if envelopes.len() == 2 && fallback_text.as_deref() == Some("Status card")
                )
            })
    });
    assert!(has_a2ui_block, "assistant message persists ContentBlock::A2ui");
}

#[tokio::test]
async fn a2ui_validation_error_triggers_self_correction_without_emitting_bad_envelopes() {
    let _guard = crate::storage::lock_test_env();
    let _home = IsolatedHome::new();
    let provider = Arc::new(ScriptedA2uiProvider::default());

    let bad_args = json!({
        "envelopes": [
            {
                "version": "1.0",
                "createSurface": {
                    "surfaceId": "img-1",
                    "catalogId": horon_agent_tool_types::a2ui::A2UI_BASIC_CATALOG_V1_0
                }
            },
            {
                "version": "1.0",
                "updateComponents": {
                    "surfaceId": "img-1",
                    "components": [
                        {"id": "root", "component": "Image", "url": "javascript:alert(1)"}
                    ]
                }
            }
        ],
        "fallback_text": "bad"
    })
    .to_string();
    let good_args = json!({
        "envelopes": [
            {
                "version": "1.0",
                "createSurface": {
                    "surfaceId": "img-1",
                    "catalogId": horon_agent_tool_types::a2ui::A2UI_BASIC_CATALOG_V1_0
                }
            },
            {
                "version": "1.0",
                "updateComponents": {
                    "surfaceId": "img-1",
                    "components": [
                        {"id": "root", "component": "Image", "url": "https://example.com/ok.png"}
                    ]
                }
            }
        ],
        "fallback_text": "Safe image"
    })
    .to_string();

    provider.push_stream(vec![
        StreamEvent::ToolUseStart {
            id: "call_bad".to_string(),
            name: crate::tool::A2UI_TOOL.to_string(),
        },
        StreamEvent::ToolInputDeltaFor {
            id: "call_bad".to_string(),
            delta: bad_args,
        },
        StreamEvent::ToolUseEndFor {
            id: "call_bad".to_string(),
        },
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".to_string()),
        },
    ]);
    provider.push_stream(vec![
        StreamEvent::ToolUseStart {
            id: "call_good".to_string(),
            name: crate::tool::A2UI_TOOL.to_string(),
        },
        StreamEvent::ToolInputDeltaFor {
            id: "call_good".to_string(),
            delta: good_args,
        },
        StreamEvent::ToolUseEndFor {
            id: "call_good".to_string(),
        },
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".to_string()),
        },
    ]);

    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider.clone(), registry);
    agent
        .set_a2ui_capabilities(Some(A2uiSessionCapabilities::default()))
        .await;
    let (role, blocks) = user_message("Show image");
    agent.add_message(role, blocks);

    let (tx, mut rx) = tokio_mpsc::unbounded_channel();
    agent
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("self-correction succeeds");
    assert_eq!(provider.call_count(), 2);

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert!(
        !events.iter().any(|e| matches!(
            e,
            ServerEvent::A2uiEnvelope { tool_call_id, envelope, .. }
                if tool_call_id == "call_bad" && envelope.get("updateComponents").is_some()
        )),
        "invalid javascript URL envelope must not be emitted"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolDone { id, error: Some(_), .. } if id == "call_bad"
        )),
        "invalid call must report tool error"
    );
    let round2_requests = &provider.requests()[1];
    assert!(round2_requests.iter().flat_map(|m| &m.content).any(|b| matches!(
        b,
        ContentBlock::ToolResult { tool_use_id, is_error: Some(true), .. } if tool_use_id == "call_bad"
    )));

    let session = Session::load(agent.session_id()).expect("persisted session loads");
    let final_a2ui_count = session
        .visible_conversation_messages()
        .into_iter()
        .filter(|m| {
            m.role == Role::Assistant
                && m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::A2ui { fallback_text, .. } if fallback_text.as_deref() == Some("Safe image")))
        })
        .count();
    assert_eq!(final_a2ui_count, 1);
}

use super::*;
use serde_json::json;
use std::collections::VecDeque;

/// Answers with the tool calls the test queued, then plain text.
#[derive(Clone, Default)]
struct PlanDiffProvider {
    calls: Arc<std::sync::Mutex<VecDeque<(String, String, serde_json::Value)>>>,
}

impl PlanDiffProvider {
    fn push_tool_call(&self, id: &str, name: &str, input: serde_json::Value) {
        self.calls.lock().expect("the call queue is never poisoned").push_back((
            id.to_string(),
            name.to_string(),
            input,
        ));
    }
}

#[async_trait]
impl Provider for PlanDiffProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        let next = self.calls.lock().expect("the call queue is never poisoned").pop_front();
        let events = match next {
            Some((id, name, input)) => vec![
                StreamEvent::ToolUseStart {
                    id: id.clone(),
                    name: name.clone(),
                },
                StreamEvent::ToolInputDelta(input.to_string()),
                StreamEvent::ToolUseEnd,
                StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".into()),
                },
            ],
            None => vec![
                StreamEvent::TextDelta("finished".into()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                },
            ],
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "acp-plan-diff-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn send_line(writer: &mut crate::transport::WriteHalf, value: serde_json::Value) {
    let mut line = value.to_string();
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .expect("the daemon reads the frame");
}

/// Every event the connection sent until `terminal` matched, in arrival order.
async fn collect_until<R>(reader: &mut R, terminal: impl Fn(&ServerEvent) -> bool) -> Vec<ServerEvent>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut seen = Vec::new();
    loop {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(30), reader.read_line(&mut line))
            .await
            .expect("the daemon answers within the deadline")
            .expect("the connection stays readable");
        assert!(read > 0, "the connection closed before the terminal event");
        let event: ServerEvent = serde_json::from_str(line.trim()).expect("the daemon frames a server event");
        let done = terminal(&event);
        seen.push(event);
        if done {
            return seen;
        }
    }
}

/// In-process QA for todo 8: one editor connection drives a turn that writes a
/// plan and edits a file, and a plain connection attached to the same session
/// sees neither ACP-only event.
#[tokio::test]
async fn acp_turn_plan_and_diff_reach_only_the_editor() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let work = tempfile::TempDir::new().expect("work dir");
    let provider = Arc::new(PlanDiffProvider::default());
    let provider_template: Arc<dyn Provider> = provider.clone();

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
    let (global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let connect = || {
        let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
        let server_task = tokio::spawn(handle_client(
            server_stream,
            Arc::clone(&sessions),
            global_event_tx.clone(),
            provider_template.clone(),
            global_is_processing.clone(),
            global_session_id.clone(),
            client_count.clone(),
            Arc::clone(&client_connections),
            swarm_members.clone(),
            swarms_by_id.clone(),
            plan_proposals.clone(),
            swarm_plans.clone(),
            swarm_coordinators.clone(),
            file_touch.clone(),
            event_history.clone(),
            event_counter.clone(),
            swarm_event_tx.clone(),
            "horon-agent-test".to_string(),
            "🧪".to_string(),
            mcp_pool.clone(),
            shutdown_signals.clone(),
            soft_interrupt_queues.clone(),
            AwaitMembersRuntime::default(),
            SwarmMutationRuntime::default(),
            std::time::Duration::ZERO,
        ));
        (server_task, client_stream)
    };

    // The editor connection: the only one that renders a plan and file diffs.
    let (acp_task, acp_stream) = connect();
    let (acp_reader, mut acp) = acp_stream.into_split();
    let mut acp_reader = BufReader::new(acp_reader);
    send_line(
        &mut acp,
        json!({
            "type": "subscribe",
            "id": 1,
            "working_dir": work.path().display().to_string(),
            "acp": {
                "client_capabilities": {},
                "permission_requests": false,
                "additional_dirs": ["/extra/one"],
            },
        }),
    )
    .await;
    collect_until(&mut acp_reader, |event| matches!(event, ServerEvent::Done { id: 1 })).await;
    let session_id = sessions
        .read()
        .await
        .keys()
        .next()
        .expect("a subscribed session")
        .clone();

    let (plain_task, plain_stream) = connect();
    let (plain_reader, mut plain) = plain_stream.into_split();
    let mut plain_reader = BufReader::new(plain_reader);
    send_line(
        &mut plain,
        json!({
            "type": "subscribe",
            "id": 50,
            "working_dir": work.path().display().to_string(),
            "target_session_id": session_id,
        }),
    )
    .await;
    collect_until(&mut plain_reader, |event| matches!(event, ServerEvent::Done { id: 50 })).await;

    provider.push_tool_call(
        "call-plan",
        "todo",
        json!({
            "todos": [
                {"content": "edit the note", "status": "in_progress", "priority": "high", "id": "t1", "confidence": 70},
            ],
        }),
    );
    send_line(&mut acp, json!({"type": "message", "id": 10, "content": "plan first"})).await;
    let acp_events = collect_until(&mut acp_reader, |event| matches!(event, ServerEvent::Done { id: 10 })).await;
    let plain_events = collect_until(&mut plain_reader, |event| matches!(event, ServerEvent::Done { id: 10 })).await;

    let plan = acp_events
        .iter()
        .find_map(|event| match event {
            ServerEvent::PlanUpdated { entries } => Some(entries),
            _ => None,
        })
        .expect("the editor receives the plan");
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].content, "edit the note");
    assert_eq!(plan[0].status, "in_progress");
    assert_eq!(plan[0].priority, "high");
    assert!(
        !plain_events
            .iter()
            .any(|event| matches!(event, ServerEvent::PlanUpdated { .. })),
        "a plain attachment never sees the editor's plan: {plain_events:?}"
    );

    let note = work.path().join("note.txt");
    std::fs::write(&note, "first line\n").expect("fixture");
    provider.push_tool_call(
        "call-edit",
        "edit",
        json!({
            "file_path": note.display().to_string(),
            "old_string": "first line",
            "new_string": "second line",
        }),
    );
    send_line(&mut acp, json!({"type": "message", "id": 20, "content": "edit it"})).await;
    let acp_events = collect_until(&mut acp_reader, |event| matches!(event, ServerEvent::Done { id: 20 })).await;
    let plain_events = collect_until(&mut plain_reader, |event| matches!(event, ServerEvent::Done { id: 20 })).await;

    let diffs = acp_events
        .iter()
        .find_map(|event| match event {
            ServerEvent::ToolDiff { id, diffs } => Some((id, diffs)),
            _ => None,
        })
        .expect("the editor receives the file diff");
    assert_eq!(diffs.0, "call-edit");
    assert_eq!(diffs.1.len(), 1);
    assert_eq!(diffs.1[0].path, note.display().to_string());
    assert_eq!(diffs.1[0].old_text.as_deref(), Some("first line\n"));
    assert_eq!(diffs.1[0].new_text, "second line\n");
    assert_eq!(
        std::fs::read_to_string(&note).expect("the edit landed"),
        "second line\n"
    );
    assert!(
        !plain_events
            .iter()
            .any(|event| matches!(event, ServerEvent::ToolDiff { .. })),
        "a plain attachment never sees the editor's diffs: {plain_events:?}"
    );
    assert!(
        plain_events
            .iter()
            .any(|event| matches!(event, ServerEvent::ToolDone { id, .. } if id == "call-edit")),
        "the plain attachment still sees the tool result itself: {plain_events:?}"
    );

    // The subscribe's additionalDirectories was captured into the immutable
    // session context, which is what the model reads on every later turn.
    let persisted = crate::session::Session::load(&session_id).expect("the turn persisted the session");
    let context = persisted.active_context()[0].content_preview();
    assert!(
        context.contains("Additional directories: /extra/one"),
        "the session context carries the editor's extra directories: {context}"
    );

    sessions.write().await.clear();
    drop(acp);
    drop(acp_reader);
    tokio::time::timeout(Duration::from_secs(10), acp_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(plain);
    drop(plain_reader);
    tokio::time::timeout(Duration::from_secs(10), plain_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

//! `new_context`: a rollover the model asks for keeps the notes and the newest
//! user turn, drops the rest of the active context, and stores the transcript.

use super::*;
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use std::sync::Arc;

struct RolloverProvider;

#[async_trait]
impl Provider for RolloverProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("the rollover tests never send a request")
    }

    fn name(&self) -> &str {
        "rollover-test"
    }

    fn model(&self) -> String {
        "rollover-test-model".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

fn text_block(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }]
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn stored_text(message: &StoredMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One session with a readable tool round before the newest user turn, so the
/// rollover has something to drop that also proves tool results do not leak
/// into the rebuilt request.
async fn rollover_fixture() -> (crate::auth::test_sandbox::AuthTestSandbox, Agent) {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("test sandbox");
    std::fs::write(sandbox.root().join("config.toml"), "[tools]\ncontext_notes = true\n").expect("write config");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(RolloverProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    // A stored boundary needs the route identity that produced it.
    agent.session.provider_key = Some("rollover-test".to_string());
    agent.session.model = Some("rollover-test-model".to_string());
    agent.session.route_api_method = Some("rollover-test-route".to_string());

    crate::context_notes::save(&agent.session.id, "- continue: finish the parser").expect("save notes");
    agent.add_message(Role::User, text_block("plan the parser"));
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "call-read".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "src/lib.rs"}),
            thought_signature: None,
        }],
    );
    agent.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call-read".to_string(),
            content: "fn main() { /* parser stub */ }".to_string(),
            is_error: None,
        }],
    );
    agent.add_message(Role::Assistant, text_block("I read the file."));
    agent.add_message(Role::User, text_block("now finish the parser"));
    (sandbox, agent)
}

#[tokio::test]
async fn new_context_rollover_keeps_notes_and_the_last_user_message_only() {
    let (_sandbox, mut agent) = rollover_fixture().await;
    let before = agent.session.active_context().len();

    // Two calls in one turn: the pending request is keyed by session, so the
    // second call does not queue a second rollover.
    crate::tool::new_context::request_rollover(&agent.session.id);
    crate::tool::new_context::request_rollover(&agent.session.id);
    agent.apply_pending_context_rollover();

    assert_eq!(
        agent.session.active_context().len(),
        1,
        "the rebuilt active context is the newest user turn"
    );
    assert_eq!(stored_text(&agent.session.active_context()[0]), "now finish the parser");
    let state = agent.session.compaction.as_ref().expect("the boundary is stored");
    assert_eq!(state.summary_text, super::CONTEXT_ROLLOVER_MARKER);
    assert_eq!(state.compacted_count, before - 1, "one boundary moved");
    assert_eq!(agent.session.active_history_offset(), before - 1);

    let (messages, _) = agent.messages_for_provider();
    let texts = messages.iter().map(message_text).collect::<Vec<_>>();
    assert_eq!(texts.len(), 2, "marker plus the newest user message: {texts:?}");
    assert!(
        texts[0].contains("history://current"),
        "the marker points at the stored transcript: {texts:?}"
    );
    assert!(texts[1].contains("now finish the parser"), "{texts:?}");
    assert!(
        !texts
            .iter()
            .any(|text| text.contains("fn main() { /* parser stub */ }")),
        "no earlier tool result survives the rollover: {texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.contains("plan the parser")),
        "no earlier user message survives the rollover: {texts:?}"
    );

    let prompt = agent.build_system_prompt_split(None);
    assert!(
        prompt.dynamic_part.contains("- continue: finish the parser"),
        "the working notes still ride in the dynamic system prompt"
    );

    // One rollover per request: a second application cannot move the boundary
    // again, because the active context already starts at the newest user turn.
    agent.apply_pending_context_rollover();
    let state = agent.session.compaction.as_ref().expect("the boundary stays");
    assert_eq!(state.compacted_count, before - 1);
    assert_eq!(agent.session.active_context().len(), 1);
}

#[tokio::test]
async fn new_context_rollover_keeps_the_full_transcript_at_history_current() {
    let (_sandbox, mut agent) = rollover_fixture().await;
    crate::tool::new_context::request_rollover(&agent.session.id);
    agent.apply_pending_context_rollover();
    agent.save_session().expect("persist the rollover");

    let reloaded = Session::load(agent.session_id()).expect("reload the session");
    assert_eq!(reloaded.active_context().len(), 1, "the boundary survives a restart");
    let history = reloaded.history_messages().expect("read the stored transcript");
    let stored = history.iter().map(stored_text).collect::<Vec<_>>().join("\n");
    assert!(stored.contains("plan the parser"), "{stored}");
    assert!(
        stored.contains("fn main() { /* parser stub */ }"),
        "an earlier tool result is still stored: {stored}"
    );
    assert!(stored.contains("now finish the parser"), "{stored}");

    let ctx = ToolContext {
        session_id: agent.session_id().to_string(),
        message_id: "rollover-history-message".to_string(),
        tool_call_id: "rollover-history-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    };
    let output = agent
        .registry
        .execute("read", serde_json::json!({"file_path": "history://current"}), ctx)
        .await
        .expect("history://current reads");
    assert!(
        output.output.contains("fn main() { /* parser stub */ }"),
        "history://current serves the pre-rollover transcript: {}",
        output.output
    );
    assert!(output.output.contains("plan the parser"), "{}", output.output);
}

#[tokio::test]
async fn new_context_rollover_keeps_a_prior_text_summary_in_front_of_the_marker() {
    let (_sandbox, mut agent) = rollover_fixture().await;
    let prior = crate::session::StoredCompactionState {
        summary_text: "## Goal\nfinish the parser".to_string(),
        covers_up_to_turn: 0,
        original_turn_count: 5,
        compacted_count: 0,
        ..Default::default()
    };
    agent.session.compaction = Some(prior.clone());
    {
        let messages = agent.session.provider_messages();
        let compaction = agent.registry.compaction();
        let mut manager = compaction.try_write().expect("compaction lock");
        manager.restore_persisted_state_with(&prior, messages);
    }

    crate::tool::new_context::request_rollover(&agent.session.id);
    agent.apply_pending_context_rollover();

    let state = agent.session.compaction.as_ref().expect("the boundary is stored");
    assert!(
        state.summary_text.starts_with("## Goal\nfinish the parser"),
        "a summary the model already had still describes the covered messages: {}",
        state.summary_text
    );
    assert!(
        state.summary_text.contains("history://current"),
        "{}",
        state.summary_text
    );
    assert_eq!(agent.session.active_context().len(), 1);
}

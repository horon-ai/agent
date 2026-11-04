use super::source_projection::{
    CoveredRange, PriorBoundary, SourceMessage, SourceProjection, halve_covered_range, plan_covered_range,
    render_source_range, render_tool_result,
};
use super::summary::{RestorationManifest, SummaryArtifact, SummaryError, SummaryLimits, generate_structured_summary};
use super::*;
use crate::provider::{EventStream, Provider};
use horon_agent_provider_core::SimpleCompletionUsage;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const SCRIPTED_WINDOW: usize = 200_000;

enum ScriptedResponse {
    Text(String),
    Error(String),
}

struct ScriptedProvider {
    responses: Mutex<VecDeque<ScriptedResponse>>,
    prompts: Mutex<Vec<String>>,
    window: usize,
}

impl ScriptedProvider {
    fn new(responses: Vec<ScriptedResponse>, window: usize) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            prompts: Mutex::new(Vec::new()),
            window,
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("prompts lock").clone()
    }

    fn next(&self, prompt: &str) -> Result<String> {
        self.prompts.lock().expect("prompts lock").push(prompt.to_string());
        match self.responses.lock().expect("responses lock").pop_front() {
            Some(ScriptedResponse::Text(text)) => Ok(text),
            Some(ScriptedResponse::Error(detail)) => Err(anyhow::anyhow!(detail)),
            None => Err(anyhow::anyhow!("scripted provider exhausted")),
        }
    }
}

#[async_trait::async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "scripted-summary"
    }

    fn model(&self) -> String {
        "scripted-summary-model".to_string()
    }

    fn context_window(&self) -> usize {
        self.window
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(ScriptedProvider::new(Vec::new(), self.window))
    }

    async fn complete_simple(&self, prompt: &str, _system: &str) -> Result<String> {
        self.next(prompt)
    }

    async fn complete_simple_with_usage(&self, prompt: &str, _system: &str) -> Result<(String, SimpleCompletionUsage)> {
        let text = self.next(prompt)?;
        Ok((
            text,
            SimpleCompletionUsage {
                input_tokens: Some(1_111),
                output_tokens: Some(222),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            },
        ))
    }
}

fn text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn tool_call(id: &str, name: &str, input: serde_json::Value) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
            thought_signature: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn tool_result(id: &str, content: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: id.to_string(),
            content: content.to_string(),
            is_error: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn image_message() -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Image {
            media_type: "image/png".to_string(),
            data: "aGVsbG8=".to_string(),
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn sourced(event_id: &str, message: Message) -> SourceMessage {
    SourceMessage {
        event_id: Some(event_id.to_string()),
        content_hash: Some(format!("hash-{event_id}")),
        message,
    }
}

fn artifact_json(goal: &str, next_action: &str) -> String {
    json!({
        "goal": goal,
        "done": ["implemented the generator"],
        "incomplete": ["wiring the scheduler"],
        "cancelled": ["dropping the legacy prompt"],
        "files": [{"path": "crates/base/src/compaction/summary.rs", "note": "generator"}],
        "evidence": ["cargo test structured_summary"],
        "decisions": ["typed JSON artifact over free prose"],
        "next_action": next_action,
    })
    .to_string()
}

fn limits(_tail_budget: usize) -> SummaryLimits {
    SummaryLimits::for_window_with_reserve(SCRIPTED_WINDOW, 4_000)
}

fn digest_of(entries: &[(&str, &str)]) -> String {
    let parts: Vec<serde_json::Value> = entries
        .iter()
        .map(|(event_id, content_hash)| json!({"event_id": event_id, "content_hash": content_hash}))
        .collect();
    horon_agent_session_store::canonical_digest_of_values(&parts).expect("digest")
}

fn conversation() -> SourceProjection {
    SourceProjection::from_source_messages(vec![
        sourced("e1", text_message(Role::User, "build the event store")),
        sourced("e2", text_message(Role::Assistant, "on it")),
        sourced(
            "e3",
            tool_call(
                "call-1",
                "read",
                json!({"path": "crates/base/src/compaction/summary.rs"}),
            ),
        ),
        sourced("e4", tool_result("call-1", "fn generate_structured_summary() {}")),
        sourced("e5", text_message(Role::Assistant, "wrote the generator")),
        sourced("e6", text_message(Role::User, "now wire the scheduler")),
    ])
}

#[tokio::test]
async fn structured_summary_parses_typed_artifact_and_binds_source() {
    let projection = conversation();
    let provider = ScriptedProvider::new(
        vec![ScriptedResponse::Text(artifact_json(
            "ship task 12",
            "run the compaction suites",
        ))],
        SCRIPTED_WINDOW,
    );

    let bound = generate_structured_summary(&provider, &projection, projection.len(), None, &limits(20_000))
        .await
        .expect("summary");

    assert_eq!(bound.artifact.goal, "ship task 12");
    assert_eq!(bound.artifact.done, vec!["implemented the generator"]);
    assert_eq!(bound.artifact.next_action, "run the compaction suites");
    assert!(bound.summary_text.contains("## Goal"));
    assert!(bound.summary_text.contains("## Next action"));
    assert!(bound.summary_text.contains("typed JSON artifact over free prose"));

    assert_eq!(bound.covered.end(), 5, "the newest user message stays verbatim");
    assert_eq!(bound.covered_event_ids, vec!["e1", "e2", "e3", "e4", "e5"]);
    assert_eq!(bound.covered_through_event_id.as_deref(), Some("e5"));
    assert_eq!(bound.first_kept_event_id.as_deref(), Some("e6"));
    assert_eq!(
        bound.source_prefix_digest.as_deref(),
        Some(
            digest_of(&[
                ("e1", "hash-e1"),
                ("e2", "hash-e2"),
                ("e3", "hash-e3"),
                ("e4", "hash-e4"),
                ("e5", "hash-e5")
            ])
            .as_str()
        )
    );
    assert_eq!(bound.input_tokens, Some(1_111));
    assert_eq!(bound.output_tokens, Some(222));
    assert_eq!(bound.shrink_attempts, 0);
    assert_eq!(bound.restoration.entries.len(), 1);
    assert_eq!(
        bound.restoration.entries[0].path,
        "crates/base/src/compaction/summary.rs"
    );
    assert_eq!(bound.restoration.entries[0].kind, "file");
    assert!(bound.restoration.total_bytes <= 50_000);
}

#[tokio::test]
async fn structured_summary_prior_assembly_carries_prior_id_and_text() {
    let projection = conversation();
    let prior = super::summary::PriorSummary {
        text: "## Goal\nprevious goal".to_string(),
        compaction_id: Some("c9".to_string()),
        content_hash: Some("hash-c9".to_string()),
    };
    let provider = ScriptedProvider::new(
        vec![ScriptedResponse::Text(artifact_json("ship task 12", "next"))],
        SCRIPTED_WINDOW,
    );

    let bound = generate_structured_summary(&provider, &projection, projection.len(), Some(&prior), &limits(20_000))
        .await
        .expect("summary");

    assert_eq!(bound.prior_compaction_id.as_deref(), Some("c9"));
    let prompt = &provider.prompts()[0];
    assert!(prompt.contains("previous goal"), "prior summary is replayed");
    assert!(prompt.contains("[event e1]"), "source ids are labelled");
    let parts = vec![
        json!({"prior_compaction_id": "c9", "content_hash": "hash-c9"}),
        json!({"event_id": "e1", "content_hash": "hash-e1"}),
        json!({"event_id": "e2", "content_hash": "hash-e2"}),
        json!({"event_id": "e3", "content_hash": "hash-e3"}),
        json!({"event_id": "e4", "content_hash": "hash-e4"}),
        json!({"event_id": "e5", "content_hash": "hash-e5"}),
    ];
    assert_eq!(
        bound.source_prefix_digest.as_deref(),
        Some(
            horon_agent_session_store::canonical_digest_of_values(&parts)
                .expect("digest")
                .as_str()
        )
    );
}

#[tokio::test]
async fn structured_summary_rejects_empty_response() {
    let projection = conversation();
    let provider = ScriptedProvider::new(vec![ScriptedResponse::Text("   \n ".to_string())], SCRIPTED_WINDOW);
    let error = generate_structured_summary(&provider, &projection, projection.len(), None, &limits(20_000))
        .await
        .expect_err("empty response must fail");
    assert_eq!(error, SummaryError::EmptyResponse);
}

#[tokio::test]
async fn structured_summary_rejects_tool_call_only_response() {
    let projection = conversation();
    for response in [
        "<tool_call>{\"name\":\"read\"}</tool_call>".to_string(),
        json!({"tool_calls": [{"id": "1", "function": {"name": "read"}}]}).to_string(),
    ] {
        let provider = ScriptedProvider::new(vec![ScriptedResponse::Text(response)], SCRIPTED_WINDOW);
        let error = generate_structured_summary(&provider, &projection, projection.len(), None, &limits(20_000))
            .await
            .expect_err("tool-call-only response must fail");
        assert_eq!(error, SummaryError::ToolCallOnly);
    }
}

#[tokio::test]
async fn structured_summary_rejects_wrong_fields() {
    let projection = conversation();
    let cases = [
        json!({"goal": "g", "done": []}).to_string(),
        json!({"goal": 5, "next_action": "n"}).to_string(),
        json!({"goal": "g", "next_action": "n", "done": [1, 2]}).to_string(),
        json!({"goal": "g", "next_action": "n", "files": [{"note": "no path"}]}).to_string(),
        json!({"goal": "", "next_action": "n"}).to_string(),
    ];
    for response in cases {
        let provider = ScriptedProvider::new(vec![ScriptedResponse::Text(response.clone())], SCRIPTED_WINDOW);
        let error = generate_structured_summary(&provider, &projection, projection.len(), None, &limits(20_000))
            .await
            .expect_err("mistyped artifact must fail");
        assert!(
            matches!(error, SummaryError::InvalidArtifact { .. }),
            "unexpected error for {response}: {error}"
        );
    }
}

#[tokio::test]
async fn structured_summary_provider_error_is_not_success() {
    let projection = conversation();
    let provider = ScriptedProvider::new(
        vec![ScriptedResponse::Error("provider exploded".to_string())],
        SCRIPTED_WINDOW,
    );
    let error = generate_structured_summary(&provider, &projection, projection.len(), None, &limits(20_000))
        .await
        .expect_err("provider error must fail");
    assert!(matches!(error, SummaryError::Provider { .. }));
}

/// Records which model each summary call ran on; calls on `failing_model` fail.
struct ModelRoutingProvider {
    model: Mutex<String>,
    failing_model: Option<String>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl ModelRoutingProvider {
    fn new(model: &str, failing_model: Option<&str>, calls: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            model: Mutex::new(model.to_string()),
            failing_model: failing_model.map(str::to_string),
            calls,
        }
    }
}

#[async_trait::async_trait]
impl Provider for ModelRoutingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "model-routing"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        *self.model.lock().expect("model lock") = model.to_string();
        Ok(())
    }

    fn context_window(&self) -> usize {
        SCRIPTED_WINDOW
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(ModelRoutingProvider::new(
            &self.model(),
            self.failing_model.as_deref(),
            Arc::clone(&self.calls),
        ))
    }

    async fn complete_simple_with_usage(
        &self,
        _prompt: &str,
        _system: &str,
    ) -> Result<(String, SimpleCompletionUsage)> {
        let model = self.model();
        self.calls.lock().expect("calls lock").push(model.clone());
        if self.failing_model.as_deref() == Some(model.as_str()) {
            return Err(anyhow::anyhow!("model {model} unavailable"));
        }
        Ok((artifact_json("ship task 12", "next"), SimpleCompletionUsage::default()))
    }
}

#[tokio::test]
async fn summary_model_routes_the_summary_call_to_a_forked_provider() {
    let projection = conversation();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(ModelRoutingProvider::new("session", None, Arc::clone(&calls)));
    let (_, used) = super::summary::generate_structured_summary_with_model(
        &provider,
        Some("mini"),
        &projection,
        projection.len(),
        None,
        &limits(20_000),
    )
    .await
    .expect("summary");
    assert_eq!(*calls.lock().unwrap(), vec!["mini".to_string()]);
    assert_eq!(used.model(), "mini");
    assert_eq!(provider.model(), "session", "the session provider keeps its model");
}

#[tokio::test]
async fn summary_model_failure_retries_once_on_the_session_model() {
    let projection = conversation();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(ModelRoutingProvider::new("session", Some("mini"), Arc::clone(&calls)));
    let (_, used) = super::summary::generate_structured_summary_with_model(
        &provider,
        Some("mini"),
        &projection,
        projection.len(),
        None,
        &limits(20_000),
    )
    .await
    .expect("fallback summary");
    assert_eq!(*calls.lock().unwrap(), vec!["mini".to_string(), "session".to_string()]);
    assert_eq!(used.model(), "session");
}

#[tokio::test]
async fn empty_or_same_summary_model_uses_the_session_provider() {
    let projection = conversation();
    for configured in [None, Some(""), Some("  "), Some("session")] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(ModelRoutingProvider::new("session", None, Arc::clone(&calls)));
        super::summary::generate_structured_summary_with_model(
            &provider,
            configured,
            &projection,
            projection.len(),
            None,
            &limits(20_000),
        )
        .await
        .expect("summary");
        assert_eq!(*calls.lock().unwrap(), vec!["session".to_string()], "{configured:?}");
    }
}

#[test]
fn structured_summary_long_tool_result_keeps_head_error_and_tail() {
    let mut content = String::from("HEAD-MARK start of output\n");
    content.push_str(&"a".repeat(20_000));
    content.push_str("\nerror[E0425]: cannot find value `x` in this scope\n");
    content.push_str(&"b".repeat(20_000));
    content.push_str("\nTAIL-MARK end of output");

    let rendered = render_tool_result(&content, 1_200);
    assert!(rendered.contains("HEAD-MARK"), "head survives");
    assert!(rendered.contains("error[E0425]"), "error line survives");
    assert!(rendered.contains("TAIL-MARK"), "tail survives");
    assert!(rendered.contains("chars omitted"), "omission is explicit");
    assert!(
        rendered.len() <= 1_200,
        "rendered {} chars exceeds the 1200 allowance",
        rendered.len()
    );
    assert!(!rendered.contains(&"a".repeat(4_000)), "middle is dropped");
}

#[test]
fn structured_summary_renders_event_ids_and_never_claims_to_read_images() {
    let projection = SourceProjection::from_source_messages(vec![
        sourced("e1", text_message(Role::User, "look at this screenshot")),
        sourced("e2", image_message()),
    ]);
    let range = CoveredRange::new(0, 2);
    let rendered = render_source_range(&projection, range, 4_000);
    assert!(rendered.contains("[event e1]"));
    assert!(rendered.contains("[event e2]"));
    assert!(rendered.contains("content not read"));
    assert!(!rendered.contains("base64"));
    assert_eq!(projection.covered_ids(range).expect("ids"), vec!["e1", "e2"]);
}

#[tokio::test]
async fn structured_summary_overflow_halves_at_tool_pair_boundary_and_keeps_excluded_tail() {
    let projection = SourceProjection::from_source_messages(vec![
        sourced("e1", text_message(Role::User, "start")),
        sourced("e2", tool_call("call-1", "bash", json!({"command": "cargo test"}))),
        sourced("e3", tool_result("call-1", &"x".repeat(8_000))),
        sourced("e4", text_message(Role::Assistant, "tests pass")),
        sourced("e5", tool_call("call-2", "read", json!({"path": "a.rs"}))),
        sourced("e6", tool_result("call-2", &"y".repeat(8_000))),
        sourced("e7", text_message(Role::Assistant, "read the file")),
        sourced("e8", text_message(Role::User, "keep going")),
    ]);
    let provider = ScriptedProvider::new(
        vec![
            ScriptedResponse::Error("maximum context length is 128000 tokens".to_string()),
            ScriptedResponse::Text(artifact_json("ship task 12", "finish")),
        ],
        SCRIPTED_WINDOW,
    );

    let bound = generate_structured_summary(&provider, &projection, projection.len(), None, &limits(20_000))
        .await
        .expect("summary after shrink");

    assert_eq!(bound.shrink_attempts, 1);
    assert!(bound.covered.end() < 7, "coverage shrank below the candidate");
    assert_eq!(
        bound.first_kept_event_id.as_deref(),
        projection
            .message_at(bound.covered.end())
            .and_then(|m| m.event_id.as_deref())
    );
    assert_eq!(
        bound.covered_through_event_id.as_deref(),
        projection
            .message_at(bound.covered.end() - 1)
            .and_then(|m| m.event_id.as_deref())
    );
    assert_eq!(bound.covered.end(), 3, "halving lands on the tool-pair boundary");
    let kept = &projection.messages()[bound.covered.end()..];
    assert_eq!(
        kept.iter().filter_map(|m| m.event_id.clone()).collect::<Vec<_>>(),
        vec!["e4", "e5", "e6", "e7", "e8"],
        "the excluded range stays in original order"
    );
    assert!(
        provider.prompts()[1].len() < provider.prompts()[0].len(),
        "the retry prompt is smaller"
    );
    let mut parts = Vec::new();
    for index in 0..bound.covered.end() {
        let id = projection
            .message_at(index)
            .and_then(|m| m.event_id.clone())
            .expect("id");
        parts.push(json!({"event_id": id, "content_hash": format!("hash-{id}")}));
    }
    assert_eq!(
        bound.source_prefix_digest.as_deref(),
        Some(
            horon_agent_session_store::canonical_digest_of_values(&parts)
                .expect("digest")
                .as_str()
        )
    );
}

#[tokio::test]
async fn structured_summary_refuses_when_tail_exceeds_budget() {
    let projection = conversation();
    let provider = ScriptedProvider::new(vec![ScriptedResponse::Text(artifact_json("g", "n"))], SCRIPTED_WINDOW);
    let mut tight = SummaryLimits::for_window_with_reserve(SCRIPTED_WINDOW, 4_000);
    tight.tail_budget_tokens = 4;
    let error = generate_structured_summary(&provider, &projection, projection.len(), None, &tight)
        .await
        .expect_err("oversized tail must be refused");
    assert!(matches!(error, SummaryError::TailExceedsBudget { .. }));
    assert!(provider.prompts().is_empty(), "no provider call is made");
}

#[tokio::test]
async fn structured_summary_overflow_stops_after_three_attempts() {
    let projection = SourceProjection::from_source_messages(
        (0..16)
            .map(|index| {
                sourced(
                    &format!("e{index}"),
                    text_message(
                        if index % 2 == 0 { Role::User } else { Role::Assistant },
                        &"z".repeat(4_000),
                    ),
                )
            })
            .collect(),
    );
    let provider = ScriptedProvider::new(
        (0..8)
            .map(|_| ScriptedResponse::Error("context_length_exceeded".to_string()))
            .collect(),
        SCRIPTED_WINDOW,
    );
    let error = generate_structured_summary(&provider, &projection, projection.len(), None, &limits(20_000))
        .await
        .expect_err("overflow must stop");
    assert_eq!(error, SummaryError::OverflowNoProgress { attempts: 3 });
    assert_eq!(provider.prompts().len(), 4);
}

#[test]
fn structured_summary_halving_never_splits_a_tool_pair() {
    let projection = SourceProjection::from_source_messages(vec![
        sourced("e1", text_message(Role::User, "start")),
        sourced("e2", text_message(Role::Assistant, "working")),
        sourced("e3", tool_call("call-1", "bash", json!({"command": "ls"}))),
        sourced("e4", tool_result("call-1", "output")),
        sourced("e5", text_message(Role::Assistant, "done")),
        sourced("e6", text_message(Role::User, "next")),
    ]);
    let range = plan_covered_range(&projection, projection.len(), true).expect("plan");
    assert_eq!(range.end(), 5);
    let halved = halve_covered_range(&projection, range, true).expect("halve");
    assert!(halved.end() < range.end());
    let kept = &projection.messages()[halved.end()..];
    let kept_ids: std::collections::HashSet<&str> = kept
        .iter()
        .flat_map(|message| message.message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    let kept_calls: std::collections::HashSet<&str> = kept
        .iter()
        .flat_map(|message| message.message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert!(kept_ids.is_subset(&kept_calls), "no orphan tool result in the tail");
}

#[test]
fn structured_summary_artifact_render_covers_every_typed_field() {
    let artifact = SummaryArtifact::parse(&artifact_json("goal text", "next text")).expect("artifact");
    let rendered = artifact.render();
    for heading in [
        "## Goal",
        "## Done",
        "## Incomplete",
        "## Cancelled",
        "## Files",
        "## Evidence",
        "## Decisions",
        "## Next action",
    ] {
        assert!(rendered.contains(heading), "missing {heading}");
    }
    assert!(rendered.contains("crates/base/src/compaction/summary.rs"));
}

#[test]
fn structured_summary_projection_without_event_ids_has_no_digest() {
    let projection = SourceProjection::from_messages(&[text_message(Role::User, "hello")]);
    let range = CoveredRange::new(0, 1);
    assert_eq!(projection.source_prefix_digest(range, PriorBoundary::None), None);
    assert_eq!(projection.covered_ids(range), None);
}

fn manager_messages() -> Vec<Message> {
    let mut messages = Vec::new();
    for index in 0..40 {
        if index % 5 == 3 {
            messages.push(tool_call(
                &format!("call-{index}"),
                "read",
                json!({"path": format!("src/step-{index}.rs")}),
            ));
            messages.push(tool_result(&format!("call-{index}"), &"o".repeat(3_900)));
        } else if index % 2 == 0 {
            messages.push(text_message(Role::User, &"u".repeat(4_000)));
        } else {
            messages.push(text_message(Role::Assistant, &"a".repeat(4_000)));
        }
    }
    messages
}

#[tokio::test]
async fn structured_summary_manager_applies_reduced_coverage_and_keeps_excluded_tail() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(SCRIPTED_WINDOW);
    let messages = manager_messages();
    for _ in &messages {
        manager.notify_message_added();
    }
    let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::new(
        vec![
            ScriptedResponse::Error("maximum context length exceeded".to_string()),
            ScriptedResponse::Text(artifact_json("finish task 12", "run suites")),
        ],
        SCRIPTED_WINDOW,
    ));
    manager
        .force_compact_with(&messages, provider)
        .expect("manual compaction starts");
    let cutoff = manager.pending_cutoff;
    let handle = manager.pending_task.take().expect("pending task");
    let result = handle.await.expect("join").expect("compaction result");
    assert!(result.covered_messages < cutoff, "the summary shrank");

    manager.apply_compaction_result(result, "manual".to_string(), &messages);

    assert_eq!(manager.compacted_count(), manager.compacted_count().min(messages.len()));
    assert!(manager.compacted_count() > 0);
    assert!(manager.compacted_count() < cutoff);
    let api = manager.messages_for_api_with(&messages);
    assert_eq!(
        api.len(),
        1 + messages.len() - manager.compacted_count(),
        "summary plus the kept tail, nothing else"
    );
    let state = manager.persisted_state().expect("persisted state");
    assert_eq!(state.structured.as_ref().and_then(|m| m.input_tokens), Some(1_111));
    assert_eq!(state.structured.as_ref().and_then(|m| m.output_tokens), Some(222));
    assert!(state.structured.as_ref().and_then(|m| m.restoration.as_ref()).is_some());
}

#[tokio::test]
async fn structured_summary_manager_failure_leaves_no_success_event() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(SCRIPTED_WINDOW);
    let messages = manager_messages();
    for _ in &messages {
        manager.notify_message_added();
    }
    let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::new(
        vec![ScriptedResponse::Text(String::new())],
        SCRIPTED_WINDOW,
    ));
    manager
        .force_compact_with(&messages, provider)
        .expect("manual compaction starts");
    let handle = manager.pending_task.take().expect("pending task");
    let joined = handle.await.expect("join");
    assert!(joined.is_err(), "empty response must not produce a result");

    let handle = tokio::spawn(async move { joined });
    while !handle.is_finished() {
        tokio::task::yield_now().await;
    }
    manager.pending_task = Some(handle);
    manager.check_and_apply_compaction_with(&messages);

    assert!(manager.last_compaction.is_none(), "no success event");
    assert_eq!(manager.compacted_count(), 0);
    assert!(manager.active_summary.is_none());
    assert!(manager.persisted_state().is_none());
    assert_eq!(manager.messages_for_api_with(&messages).len(), messages.len());
}

#[tokio::test]
async fn structured_summary_persisted_usage_and_restoration_round_trip() {
    let _lock = crate::storage::lock_test_env();
    let mut manager = CompactionManager::new().with_budget(SCRIPTED_WINDOW);
    let messages = manager_messages();
    for _ in &messages {
        manager.notify_message_added();
    }
    let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::new(
        vec![ScriptedResponse::Text(artifact_json("round trip", "verify"))],
        SCRIPTED_WINDOW,
    ));
    manager
        .force_compact_with(&messages, provider)
        .expect("manual compaction starts");
    let handle = manager.pending_task.take().expect("pending task");
    let result = handle.await.expect("join").expect("compaction result");
    manager.apply_compaction_result(result, "manual".to_string(), &messages);

    let state = manager.persisted_state().expect("persisted state");
    let mut restored = CompactionManager::new().with_budget(SCRIPTED_WINDOW);
    restored.restore_persisted_state_with(&state, &messages);
    let round_tripped = restored.persisted_state().expect("restored state");
    assert_eq!(round_tripped.summary_text, state.summary_text);
    assert_eq!(round_tripped.compacted_count, state.compacted_count);
    assert_eq!(round_tripped.structured, state.structured);
    assert_eq!(
        round_tripped
            .structured
            .as_ref()
            .and_then(|meta| meta.restoration.as_ref())
            .map(|manifest| manifest.entries.len()),
        state
            .structured
            .as_ref()
            .and_then(|meta| meta.restoration.as_ref())
            .map(|manifest| manifest.entries.len()),
        "the restoration manifest survives a reload"
    );
    assert!(
        round_tripped
            .structured
            .as_ref()
            .and_then(|meta| meta.restoration.as_ref())
            .is_some_and(|manifest| !manifest.entries.is_empty()),
        "restoration entries were captured"
    );
}

#[test]
fn structured_summary_restoration_is_bounded() {
    let mut messages = Vec::new();
    for index in 0..12 {
        messages.push(tool_call(
            &format!("call-{index}"),
            "read",
            json!({"path": format!("src/file-{index}.rs")}),
        ));
    }
    let projection = SourceProjection::from_messages(&messages);
    let range = CoveredRange::new(0, messages.len());
    let manifest = super::summary::collect_restoration(&projection, range, &limits(20_000));
    assert_eq!(manifest.entries.len(), 10);
    assert_eq!(manifest.total_bytes, 30_000);
    let _ = RestorationManifest::default();
}

#[test]
fn split_turn_prompt_asks_for_the_original_request() {
    let projection = conversation();
    let range = CoveredRange::new(0, projection.len());
    let whole = super::summary::build_structured_prompt(&projection, range, None, 100_000, false);
    assert!(
        !whole.contains("fill \"split_turn_context\""),
        "an aligned cut needs no continuity note"
    );
    assert!(
        !whole.contains("middle of a turn"),
        "an aligned cut needs no continuity note"
    );

    let split = super::summary::build_structured_prompt(&projection, range, None, 100_000, true);
    assert!(
        split.contains(
            "The retained recent messages start in the middle of a turn. Fill \"split_turn_context\" with the user's original request for that turn and the key decisions made before the cut."
        ),
        "the split prompt names the key and what belongs in it"
    );
    assert!(
        split.contains("## Conversation to summarize"),
        "the conversation section still follows"
    );
}

#[test]
fn pending_request_and_split_context_render_as_sections() {
    let artifact = SummaryArtifact::parse(
        &json!({
            "goal": "finish the scheduler",
            "next_action": "wire the queue",
            "pending_request": "should the scheduler keep its retry budget?",
            "split_turn_context": "user asked to wire the scheduler; decided to reuse the retry helper",
        })
        .to_string(),
    )
    .expect("artifact");
    let rendered = artifact.render();
    assert!(rendered.contains("## Pending request:"));
    assert!(rendered.contains("should the scheduler keep its retry budget?"));
    assert!(rendered.contains("## Turn in progress at the cut:"));
    assert!(rendered.contains("decided to reuse the retry helper"));

    let without = SummaryArtifact::parse(&artifact_json("finish", "wire")).expect("artifact");
    let plain = without.render();
    assert!(!plain.contains("Pending request"));
    assert!(!plain.contains("Turn in progress at the cut"));
}

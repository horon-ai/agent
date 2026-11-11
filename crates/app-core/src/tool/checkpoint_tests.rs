//! `checkpoint` and `rewind`: the pair is opt-in, a call only queues the
//! intent, and each rejection names the state that made the call useless.

use super::{
    CheckpointTool, MARKER_TAG, Pending, RewindTool, clear_pending, is_marker, marker_message, newest_marker_index,
    pending_request,
};
use crate::agent::Agent;
use crate::message::Role;
use crate::provider::{EventStream, Provider};
use crate::session::{StoredDisplayRole, StoredMessage};
use crate::tool::{Registry, Tool, ToolContext, ToolExecutionMode};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("the stub provider is never called")
    }

    fn name(&self) -> &str {
        "checkpoint-stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

fn text_block(text: &str) -> Vec<crate::message::ContentBlock> {
    vec![crate::message::ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }]
}

fn context(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "checkpoint-message".to_string(),
        tool_call_id: "checkpoint-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// An isolated home plus an agent whose transcript is stored, so the tools can
/// read the history they decide against.
struct Fixture {
    _sandbox: crate::auth::test_sandbox::AuthTestSandbox,
    agent: Agent,
}

impl Fixture {
    async fn new() -> Self {
        let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("test sandbox");
        std::fs::write(sandbox.root().join("config.toml"), "[tools]\ncheckpoint = true\n").expect("write config");
        crate::config::invalidate_config_cache();
        let provider: Arc<dyn Provider> = Arc::new(StubProvider);
        let registry = Registry::new(provider.clone()).await;
        let mut agent = Agent::new(provider, registry);
        agent.add_message(Role::User, text_block("try the parser rewrite"));
        agent.add_message(Role::Assistant, text_block("rewriting the parser now"));
        agent.save_session().expect("store the transcript");
        clear_pending(agent.session_id());
        Self {
            _sandbox: sandbox,
            agent,
        }
    }

    fn ctx(&self) -> ToolContext {
        context(self.agent.session_id())
    }

    /// Put a committed checkpoint record on disk, as a settled turn would.
    fn store_marker(&mut self) {
        let (role, content, display_role) = marker_message();
        self.agent.add_message_with_display_role(role, content, display_role);
        self.agent.save_session().expect("store the checkpoint");
    }
}

#[test]
fn checkpoint_pair_declares_a_write_effect() {
    assert!(
        CheckpointTool::new().effects().writes(),
        "a checkpoint record is a transcript mutation"
    );
    assert!(
        RewindTool::new().effects().writes(),
        "a rewind truncates the transcript, so plan mode must treat it as a mutation"
    );
}

#[test]
fn rewind_schema_requires_a_report() {
    let schema = RewindTool::new().parameters_schema();
    assert_eq!(schema["required"], json!(["report"]), "{schema}");
    assert_eq!(schema["properties"]["report"]["minLength"], 1, "{schema}");
    assert!(
        schema["properties"]["intent"].is_object(),
        "every mutating tool takes the shared intent field: {schema}"
    );
    assert_eq!(
        CheckpointTool::new().parameters_schema(),
        json!({"type": "object", "properties": {}}),
        "a checkpoint takes no arguments"
    );
}

#[test]
fn marker_is_recognised_only_where_a_checkpoint_was_recorded() {
    let (_, marker_content, _) = marker_message();
    let marker = StoredMessage {
        content: marker_content,
        ..Default::default()
    };
    assert!(is_marker(&marker));
    assert!(
        marker.content.iter().any(
            |block| matches!(block, crate::message::ContentBlock::Text { text, .. } if text.starts_with(MARKER_TAG))
        ),
        "the record keeps the tag other reminders use"
    );

    // The context rollover marker is the same reminder shape and a different
    // record: a rewind must not treat it as its checkpoint.
    let rollover = StoredMessage {
        content: text_block("<system-reminder>\nThe context was rolled over"),
        ..Default::default()
    };
    assert!(!is_marker(&rollover));

    let plain = StoredMessage {
        content: text_block("try the parser rewrite"),
        ..Default::default()
    };
    assert!(!is_marker(&plain));

    let history = vec![marker.clone(), plain.clone(), marker.clone()];
    assert_eq!(
        newest_marker_index(&history),
        Some(2),
        "a session with two records rewinds to the newest one"
    );
    assert_eq!(newest_marker_index(&[plain]), None);
}

#[test]
fn one_request_per_session_is_taken_exactly_once() {
    let session = "checkpoint-pending-map";
    clear_pending(session);
    assert_eq!(pending_request(session), None);

    super::request_checkpoint(session);
    assert_eq!(pending_request(session), Some(Pending::Checkpoint));
    assert_eq!(super::take_pending(session), Some(Pending::Checkpoint));
    assert_eq!(super::take_pending(session), None, "the turn end applies one request");

    super::request_rewind(session, "the parser rewrite broke the lexer".to_string());
    assert_eq!(
        pending_request(session),
        Some(Pending::Rewind {
            report: "the parser rewrite broke the lexer".to_string()
        })
    );
    super::forget_session(session);
    assert_eq!(pending_request(session), None, "a closed session leaves nothing behind");
}

#[tokio::test]
async fn checkpoint_queues_one_record_per_turn_and_refuses_a_second() {
    let fixture = Fixture::new().await;
    let ctx = fixture.ctx();
    let tool = CheckpointTool::new();

    let first = tool
        .execute(json!({}), ctx.clone())
        .await
        .expect("the first checkpoint is accepted");
    assert!(
        first.output.contains("rewind"),
        "the result names the next step: {}",
        first.output
    );
    assert_eq!(pending_request(&ctx.session_id), Some(Pending::Checkpoint));

    let refusal = tool
        .execute(json!({}), ctx.clone())
        .await
        .expect_err("one turn applies one request");
    assert!(refusal.to_string().contains("already queued"), "{refusal}");

    clear_pending(&ctx.session_id);
    assert_eq!(pending_request(&ctx.session_id), None);
}

#[tokio::test]
async fn checkpoint_refuses_a_session_that_already_has_one() {
    let mut fixture = Fixture::new().await;
    fixture.store_marker();
    let ctx = fixture.ctx();

    let refusal = CheckpointTool::new()
        .execute(json!({}), ctx.clone())
        .await
        .expect_err("a second checkpoint would orphan the first");
    assert!(refusal.to_string().contains("already active"), "{refusal}");
    assert_eq!(pending_request(&ctx.session_id), None, "a refusal queues nothing");
}

#[tokio::test]
async fn checkpoint_refuses_a_session_with_no_stored_history() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("test sandbox");
    let ctx = context("checkpoint-no-such-session");

    let refusal = CheckpointTool::new()
        .execute(json!({}), ctx.clone())
        .await
        .expect_err("there is no transcript to mark yet");
    assert!(refusal.to_string().contains("No readable stored history"), "{refusal}");
    assert_eq!(pending_request(&ctx.session_id), None);
}

#[tokio::test]
async fn rewind_refuses_an_empty_report_and_a_missing_checkpoint() {
    let mut fixture = Fixture::new().await;
    let ctx = fixture.ctx();
    let tool = RewindTool::new();

    let empty = tool
        .execute(json!({"report": "   "}), ctx.clone())
        .await
        .expect_err("an empty report discards the only record of the branch");
    assert!(empty.to_string().contains("non-empty report"), "{empty}");

    let missing = tool
        .execute(json!({"report": "the rewrite broke the lexer"}), ctx.clone())
        .await
        .expect_err("there is no checkpoint to rewind to");
    assert!(missing.to_string().contains("No checkpoint is active"), "{missing}");
    assert_eq!(pending_request(&ctx.session_id), None);

    fixture.store_marker();
    let with_report = tool
        .execute(json!({"report": "  the rewrite broke the lexer  "}), ctx.clone())
        .await
        .expect("a committed checkpoint can be rewound to");
    assert!(
        with_report.output.contains("When this turn ends"),
        "{}",
        with_report.output
    );
    assert_eq!(
        pending_request(&ctx.session_id),
        Some(Pending::Rewind {
            report: "the rewrite broke the lexer".to_string()
        }),
        "the report is stored trimmed"
    );
}

#[tokio::test]
async fn rewind_refuses_a_checkpoint_queued_in_the_same_turn() {
    let fixture = Fixture::new().await;
    let ctx = fixture.ctx();
    CheckpointTool::new()
        .execute(json!({}), ctx.clone())
        .await
        .expect("the checkpoint queues");

    let refusal = RewindTool::new()
        .execute(json!({"report": "nothing to report"}), ctx.clone())
        .await
        .expect_err("the record does not exist until the turn ends");
    assert!(refusal.to_string().contains("not recorded yet"), "{refusal}");
    assert_eq!(
        pending_request(&ctx.session_id),
        Some(Pending::Checkpoint),
        "the refusal does not replace the queued checkpoint"
    );
    clear_pending(&ctx.session_id);
}

struct HomeGuard {
    home: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
    previous_profile: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl HomeGuard {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let previous_home = std::env::var_os("HORON_HOME");
        let previous_profile = std::env::var_os("HORON_AGENT_TOOL_PROFILE");
        crate::env::set_var("HORON_HOME", home.path());
        crate::env::set_var("HORON_AGENT_TOOL_PROFILE", "full");
        Self {
            home,
            previous_home,
            previous_profile,
            _lock: lock,
        }
    }

    fn set_checkpoint(&self, enabled: bool) {
        std::fs::write(
            self.home.path().join("config.toml"),
            format!("[tools]\ncheckpoint = {enabled}\n"),
        )
        .expect("write config");
        crate::config::invalidate_config_cache();
    }

    async fn registered_names(&self) -> Vec<String> {
        let provider: Arc<dyn Provider> = Arc::new(StubProvider);
        let registry = Registry::new(provider).await;
        registry
            .definitions(None)
            .await
            .into_iter()
            .map(|definition| definition.name)
            .collect()
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        match &self.previous_profile {
            Some(profile) => crate::env::set_var("HORON_AGENT_TOOL_PROFILE", profile),
            None => crate::env::remove_var("HORON_AGENT_TOOL_PROFILE"),
        }
        crate::config::invalidate_config_cache();
    }
}

#[tokio::test]
async fn checkpoint_pair_is_registered_only_when_the_switch_is_on() {
    let guard = HomeGuard::new();

    guard.set_checkpoint(false);
    let disabled = guard.registered_names().await;
    assert!(
        !disabled.iter().any(|name| name == "checkpoint" || name == "rewind"),
        "both tools stay out of every default profile: {disabled:?}"
    );

    guard.set_checkpoint(true);
    let enabled = guard.registered_names().await;
    for name in ["checkpoint", "rewind"] {
        assert!(
            enabled.iter().any(|registered| registered == name),
            "{name} is registered with the switch: {enabled:?}"
        );
    }

    let core = crate::config::ToolConfig {
        profile: "core".to_string(),
        checkpoint: true,
        ..Default::default()
    };
    let core_allowed = core
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("the core profile has a closed allow-list");
    assert!(
        core_allowed.contains("checkpoint") && core_allowed.contains("rewind"),
        "the closed core list must join both tools when the switch is on: {core_allowed:?}"
    );

    let core_off = crate::config::ToolConfig {
        profile: "core".to_string(),
        ..Default::default()
    };
    let off = core_off
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("the core profile has a closed allow-list");
    assert!(
        !off.contains("checkpoint") && !off.contains("rewind"),
        "the switch is the only way into the allow-list: {off:?}"
    );
}

#[tokio::test]
async fn checkpoint_record_is_a_hidden_reminder() {
    let (role, content, display_role) = marker_message();
    assert_eq!(role, Role::User);
    assert_eq!(display_role, Some(StoredDisplayRole::System));
    assert!(
        StoredMessage {
            content,
            ..Default::default()
        }
        .to_message()
        .is_internal_system_reminder(),
        "the record is a reminder, so neither the transcript view nor a rewind target shows it"
    );

    let mut fixture = Fixture::new().await;
    let visible_before = crate::session::Session::load(fixture.agent.session_id())
        .expect("load the stored session")
        .rewind_target_count();
    fixture.store_marker();
    let reloaded = crate::session::Session::load(fixture.agent.session_id()).expect("reload the session");
    assert_eq!(
        reloaded.rewind_target_count(),
        visible_before,
        "the checkpoint is stored but is not a rewind target"
    );
    let stored = reloaded.history_messages().expect("read the transcript");
    assert_eq!(
        stored.last().expect("the marker is stored").display_role,
        Some(StoredDisplayRole::System),
        "the record is hidden from the rendered transcript"
    );
    assert_eq!(super::newest_marker_index(&stored), Some(stored.len() - 1));
}

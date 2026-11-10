//! `new_context`: the rollover tool is a mutation, reachable only with the
//! notes it keeps, and one turn records one request.

use super::{NewContextTool, clear_rollover, rollover_pending};
use crate::provider::{EventStream, Provider};
use crate::tool::{Registry, Tool, ToolContext, ToolExecutionMode};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

fn context(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "new-context-message".to_string(),
        tool_call_id: "new-context-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

#[test]
fn new_context_declares_a_write_effect() {
    assert!(
        NewContextTool::new().effects().writes(),
        "plan mode and approval must treat a rollover as a mutation"
    );
}

#[tokio::test]
async fn new_context_is_refused_in_plan_mode() {
    let registry = Registry::empty();
    registry
        .register("new_context".to_string(), Arc::new(NewContextTool::new()))
        .await;

    let session = "new-context-plan-mode";
    clear_rollover(session);
    crate::plan_mode::clear_state(session);
    crate::plan_mode::set_state(session, crate::plan_mode::PlanState::Planning);
    let refusal = registry
        .execute(
            "new_context",
            json!({"intent": "roll the context over"}),
            context(session),
        )
        .await
        .expect_err("plan mode is read-only, so a rollover must be refused");
    crate::plan_mode::clear_state(session);

    assert_eq!(
        refusal.to_string(),
        crate::plan_mode::block_message("new_context"),
        "the refusal must be the plan-mode message"
    );
    assert!(!rollover_pending(session), "a refused call must not queue a rollover");
}

#[tokio::test]
async fn new_context_records_one_request_per_turn() {
    let session = "new-context-twice-in-one-turn";
    clear_rollover(session);
    let tool = NewContextTool::new();

    let first = tool
        .execute(json!({"intent": "roll the context over"}), context(session))
        .await
        .expect("the first call is accepted");
    assert!(
        first.output.contains("history://current"),
        "the result must name where the transcript stays: {}",
        first.output
    );
    tool.execute(json!({"intent": "roll the context over"}), context(session))
        .await
        .expect("the second call is accepted");
    assert!(rollover_pending(session), "the request is pending for the turn end");

    clear_rollover(session);
    assert!(!rollover_pending(session), "the turn end consumes it");
}

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
        "new-context-stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// Isolated home that keeps the shared test environment lock for its lifetime.
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

    fn set_context_notes(&self, enabled: bool) {
        std::fs::write(
            self.home.path().join("config.toml"),
            format!("[tools]\ncontext_notes = {enabled}\n"),
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
async fn new_context_is_registered_only_when_context_notes_is_on() {
    let guard = HomeGuard::new();

    guard.set_context_notes(true);
    let enabled = guard.registered_names().await;
    assert!(enabled.iter().any(|name| name == "new_context"), "{enabled:?}");
    assert!(
        enabled.iter().any(|name| name == "context_notes"),
        "the notes tool and the rollover travel together: {enabled:?}"
    );

    guard.set_context_notes(false);
    let disabled = guard.registered_names().await;
    assert!(!disabled.iter().any(|name| name == "new_context"), "{disabled:?}");
}

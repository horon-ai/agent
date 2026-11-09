#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::{Registry, Tool, ToolContext, ToolExecutionMode, aux_complete};
use crate::provider::Provider;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        Err(anyhow::anyhow!("mock provider does not stream"))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn new(path: &std::path::Path) -> Self {
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", path);
        crate::config::Config::invalidate_cache();
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            crate::env::set_var("HORON_HOME", previous);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
        crate::config::Config::invalidate_cache();
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn context() -> ToolContext {
    ToolContext {
        session_id: "aux-complete".to_string(),
        message_id: "message".to_string(),
        tool_call_id: "aux-1".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

#[test]
fn tool_is_absent_by_default() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    runtime().block_on(async {
        assert!(
            !crate::config::ToolConfig::default().aux_completion,
            "aux completion must default to off"
        );
        let registry = Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        let names = registry.tool_names().await;
        assert!(
            !names.iter().any(|name| name == aux_complete::AUX_COMPLETE_TOOL),
            "aux_complete must stay unregistered by default, got {names:?}"
        );
        assert!(
            !registry
                .definitions(None)
                .await
                .iter()
                .any(|definition| definition.name == aux_complete::AUX_COMPLETE_TOOL),
            "an unregistered tool must not reach a definition list"
        );
    });
}

#[test]
fn tool_is_registered_only_when_enabled() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    let path = crate::config::Config::path().expect("config path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(&path, "[tools]\naux_completion = true\n").expect("write config");
    crate::config::Config::invalidate_cache();

    runtime().block_on(async {
        assert!(
            crate::config::config().tools.aux_completion,
            "the switch must be read from the written config"
        );
        let registry = Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        let names = registry.tool_names().await;
        assert!(
            names.iter().any(|name| name == aux_complete::AUX_COMPLETE_TOOL),
            "[tools] aux_completion = true must register the tool, got {names:?}"
        );
        let definition = registry
            .definitions(None)
            .await
            .into_iter()
            .find(|definition| definition.name == aux_complete::AUX_COMPLETE_TOOL)
            .expect("the registered tool must appear in a definition list");
        assert!(
            definition
                .input_schema
                .get("properties")
                .is_some_and(|properties| { properties.get("prompt").is_some() && properties.get("schema").is_some() }),
            "the definition must carry the prompt and schema arguments, got {}",
            definition.input_schema
        );
    });
}

#[test]
fn tool_is_outside_core_tools() {
    assert!(
        !crate::config::CORE_TOOLS.contains(&aux_complete::AUX_COMPLETE_TOOL),
        "aux_complete must never join CORE_TOOLS, got {:?}",
        crate::config::CORE_TOOLS
    );
    assert_eq!(
        crate::config::CORE_TOOLS.len(),
        15,
        "the headless core surface stays at fifteen tools"
    );

    let core = crate::config::ToolConfig {
        profile: crate::config::CORE_TOOL_PROFILE.to_string(),
        aux_completion: true,
        ..crate::config::ToolConfig::default()
    };
    let allowed = core.allowed_tools().expect("core is an allow-list");
    assert!(
        !allowed.contains(aux_complete::AUX_COMPLETE_TOOL),
        "turning the switch on must not widen the core allow-list, got {allowed:?}"
    );
}

#[test]
fn tool_declares_a_network_effect() {
    let effects = aux_complete::AuxCompleteTool::new().effects();
    assert!(
        effects.networks(),
        "the tool calls a model, so it must declare a network effect"
    );
    assert!(
        effects.parallel_safe(),
        "a model call touches no local state and may overlap another call"
    );
}

#[test]
fn tool_rejects_a_request_outside_the_contract() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    runtime().block_on(async {
        let error = aux_complete::AuxCompleteTool::new()
            .execute(
                json!({"prompt": "hi", "max_tokens": horon_agent_base::aux_completion::MAX_MAX_TOKENS + 1}),
                context(),
            )
            .await
            .expect_err("an out-of-range token bound must be refused before any model call");
        assert!(
            error
                .to_string()
                .starts_with(horon_agent_base::aux_completion::AUX_COMPLETION_INPUT),
            "the tool must surface the contract's named error, got {error}"
        );
    });
}

#[test]
fn tool_has_no_argument_for_a_model() {
    let schema = aux_complete::AuxCompleteTool::new().parameters_schema();
    let properties = schema
        .get("properties")
        .and_then(|properties| properties.as_object())
        .expect("the tool declares properties");
    for forbidden in ["model", "endpoint", "credential", "provider", "tools"] {
        assert!(
            !properties.contains_key(forbidden),
            "the tool surface may not offer '{forbidden}', got {:?}",
            properties.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn tool_effect_bitmask_is_not_a_write() {
    let tool: Arc<dyn Tool> = Arc::new(aux_complete::AuxCompleteTool::new());
    assert!(!tool.effects().writes());
    assert!(!tool.effects().processes());
}

/// A scripted provider that answers a side completion with fixed text.
struct ScriptedProvider {
    reply: String,
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        Err(anyhow::anyhow!("the scripted provider answers through complete_simple"))
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model(&self) -> String {
        "scripted-model".to_string()
    }

    async fn complete_simple(&self, _user: &str, _system: &str) -> anyhow::Result<String> {
        Ok(self.reply.clone())
    }

    async fn complete_simple_with_usage(
        &self,
        _prompt: &str,
        _system: &str,
    ) -> anyhow::Result<(String, horon_agent_provider_core::SimpleCompletionUsage)> {
        Ok((
            self.reply.clone(),
            horon_agent_provider_core::SimpleCompletionUsage::default(),
        ))
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(ScriptedProvider {
            reply: self.reply.clone(),
        })
    }
}

/// The success path of `aux_complete` has to run, not just its rejections.
///
/// Every other test in this file stops at a refusal, so the code that actually
/// answers - the sidecar dispatch and the text hand-back - was never executed.
/// This drives `AuxCompleteTool::execute` against a scripted provider registered
/// as the active one, which is how the daemon wires the sidecar.
#[tokio::test]
async fn aux_completion_returns_the_provider_text() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());
    let path = crate::config::Config::path().expect("config path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(&path, "[tools]\naux_completion = true\n").expect("write config");
    crate::config::Config::invalidate_cache();

    crate::provider::set_active_provider(Arc::new(ScriptedProvider {
        reply: "the sidecar answered".to_string(),
    }));

    let output = aux_complete::AuxCompleteTool::new()
        .execute(json!({"prompt": "say something"}), context())
        .await
        .expect("a valid request against a live provider must succeed");

    assert_eq!(
        output.output, "the sidecar answered",
        "the tool must hand back the provider's text unchanged"
    );
}

/// A schema the answer does not satisfy is refused, and the refusal names the
/// schema rather than the transport.
#[tokio::test]
async fn aux_completion_rejects_a_response_that_breaks_the_schema() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());
    let path = crate::config::Config::path().expect("config path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(&path, "[tools]\naux_completion = true\n").expect("write config");
    crate::config::Config::invalidate_cache();

    crate::provider::set_active_provider(Arc::new(ScriptedProvider {
        reply: "not json at all".to_string(),
    }));

    let error = aux_complete::AuxCompleteTool::new()
        .execute(
            json!({
                "prompt": "give me an object",
                "schema": {"type": "object", "required": ["answer"]},
            }),
            context(),
        )
        .await
        .expect_err("a non-JSON answer cannot satisfy a JSON Schema");

    let message = error.to_string();
    assert!(
        message.contains("schema") || message.contains("JSON"),
        "the refusal must name the schema contract, got: {message}"
    );
}

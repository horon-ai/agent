use super::*;

use crate::provider::Provider;
use async_trait::async_trait;
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
        session_id: "ask-registry".to_string(),
        message_id: "message".to_string(),
        tool_call_id: "call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn payload() -> serde_json::Value {
    serde_json::json!({"questions": [
        {
            "id": "first",
            "question": "Which crate owns the seam?",
            "options": [{"label": "base"}, {"label": "app-core"}],
            "recommended": 1
        },
        {
            "id": "second",
            "question": "Should the change be gated?",
            "options": [{"label": "always on"}, {"label": "opt-in"}]
        }
    ]})
}

#[test]
fn ask_is_registered_and_in_the_core_profile() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    runtime().block_on(async {
        let registry = Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        let names = registry.tool_names().await;
        assert!(
            names.iter().any(|name| name == "ask"),
            "ask must be registered, got {names:?}"
        );
        assert!(
            crate::config::CORE_TOOLS.contains(&"ask"),
            "the core profile must reach ask, got {:?}",
            crate::config::CORE_TOOLS
        );

        let allowed = crate::config::ToolConfig {
            profile: crate::config::CORE_TOOL_PROFILE.to_string(),
            ..crate::config::ToolConfig::default()
        }
        .allowed_tools()
        .expect("core is an allow-list");
        assert!(allowed.contains("ask"), "core profile must keep ask");

        let definitions = registry.definitions(Some(&allowed)).await;
        assert!(
            definitions.iter().any(|definition| definition.name == "ask"),
            "the core profile's tool list must include ask"
        );
    });
}

#[test]
fn ask_resolves_through_registry_execute() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    runtime().block_on(async {
        let registry = Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        let output = registry
            .execute("ask", payload(), context())
            .await
            .expect("ask through registry");
        let metadata = output.metadata.expect("answers travel in metadata");
        let answers = metadata["answers"].as_array().expect("answers array");
        assert_eq!(answers[0]["selected"], serde_json::json!(["app-core"]));
        assert_eq!(answers[1]["selected"], serde_json::json!(["always on"]));
        println!("{}", output.output);
    });
}

#[test]
fn ask_is_ordered() {
    assert_eq!(
        super::ask::AskTool::new().concurrency_class(),
        ConcurrencyClass::Ordered
    );
}

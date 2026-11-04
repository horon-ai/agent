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
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", path);
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            crate::env::set_var("HORON_AGENT_HOME", previous);
        } else {
            crate::env::remove_var("HORON_AGENT_HOME");
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn undo_is_registered_and_in_the_core_profile() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    runtime().block_on(async {
        let registry = Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        let names = registry.tool_names().await;
        assert!(
            names.iter().any(|name| name == "undo"),
            "undo must be registered, got {names:?}"
        );
        assert!(
            crate::config::CORE_TOOLS.contains(&"undo"),
            "the core profile must reach undo, got {:?}",
            crate::config::CORE_TOOLS
        );
    });
}

#[test]
fn redo_stays_out_of_the_default_surface() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());

    runtime().block_on(async {
        let registry = Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        assert!(!crate::config::ToolConfig::default().redo, "redo must default to off");
        let names = registry.tool_names().await;
        assert!(
            !names.iter().any(|name| name == "redo"),
            "redo must stay unregistered, got {names:?}"
        );
        assert!(
            !crate::config::CORE_TOOLS.contains(&"redo"),
            "redo must never join CORE_TOOLS"
        );
    });
}

#[test]
fn undo_reaches_the_file_through_registry_execute() {
    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _guard = HomeGuard::new(home.path());
    let work = home.path().join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let file = work.join("qa.txt");
    std::fs::write(&file, b"before write\n").expect("seed");

    runtime().block_on(async {
        let registry = Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        let ctx = ToolContext {
            session_id: "undo-registry-execute".to_string(),
            message_id: "message".to_string(),
            tool_call_id: "write-1".to_string(),
            working_dir: Some(work.clone()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        };

        registry
            .execute(
                "write",
                serde_json::json!({"file_path": "qa.txt", "content": "after write\n"}),
                ctx.clone(),
            )
            .await
            .expect("write through registry");
        assert_eq!(std::fs::read(&file).expect("after write"), b"after write\n");

        registry
            .execute("undo", serde_json::json!({}), ctx)
            .await
            .expect("undo through registry");
        assert_eq!(std::fs::read(&file).expect("after undo"), b"before write\n");
    });
}

#[test]
fn undo_and_redo_are_ordered() {
    assert_eq!(
        super::undo::UndoTool::new().concurrency_class(),
        ConcurrencyClass::Ordered
    );
    assert_eq!(
        super::undo::RedoTool::new().concurrency_class(),
        ConcurrencyClass::Ordered
    );
}

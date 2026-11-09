#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use std::ffi::OsString;

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("stub provider must never be called")
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

struct HomeGuard {
    previous: Option<OsString>,
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

/// Built the way the daemon builds it, so no assertion below falls back to the
/// unknown-name branch and passes for the wrong reason.
async fn full_registry() -> Registry {
    let registry = Registry::new(Arc::new(StubProvider)).await;
    registry
        .register_mcp_tools_for_dir(None, None, None, Some(std::env::temp_dir()))
        .await;
    registry
}

const CONCURRENT: [&str; 5] = ["read", "ls", "agentgrep", "webfetch", "websearch"];

const ORDERED: [&str; 9] = [
    "bash",
    "write",
    "edit",
    "replace",
    "apply_patch",
    "todo",
    "skill_manage",
    "mcp",
    "batch",
];

#[tokio::test]
async fn concurrent_class_is_exactly_the_read_only_lookups() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("create isolated HORON_HOME");
    let _home = HomeGuard::new(home.path());
    let registry = full_registry().await;

    let registered = registry.tool_names().await;
    for name in CONCURRENT.iter().chain(ORDERED.iter()) {
        assert!(
            registered.iter().any(|known| known == name),
            "{name} is not registered, so its class assertion would be vacuous"
        );
    }

    for name in CONCURRENT {
        assert_eq!(
            registry.concurrency_class(name).await,
            ConcurrencyClass::Concurrent,
            "{name} only reads state and must be allowed to overlap"
        );
    }

    for name in ORDERED {
        assert_eq!(
            registry.concurrency_class(name).await,
            ConcurrencyClass::Ordered,
            "{name} mutates state or starts work and must never overlap"
        );
    }
}

#[tokio::test]
async fn aliases_resolve_before_classification() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("create isolated HORON_HOME");
    let _home = HomeGuard::new(home.path());
    let registry = full_registry().await;

    // An alias that missed would classify through the unknown-name branch and
    // still look plausible, so each expected value below is the tool's own.
    for (alias, expected) in [
        ("read", ConcurrencyClass::Concurrent),
        ("Read", ConcurrencyClass::Concurrent),
        ("read_file", ConcurrencyClass::Concurrent),
        ("functions.read_file", ConcurrencyClass::Concurrent),
        ("file_grep", ConcurrencyClass::Concurrent),
        ("Grep", ConcurrencyClass::Concurrent),
        ("shell_exec", ConcurrencyClass::Ordered),
        ("functions.shell_exec", ConcurrencyClass::Ordered),
        ("multiedit", ConcurrencyClass::Ordered),
        ("patch", ConcurrencyClass::Ordered),
        ("skill", ConcurrencyClass::Ordered),
        ("todos", ConcurrencyClass::Ordered),
    ] {
        assert_eq!(registry.concurrency_class(alias).await, expected, "alias {alias}");
    }

    // An unknown tool is never assumed safe to overlap.
    assert_eq!(
        registry.concurrency_class("definitely_not_a_tool").await,
        ConcurrencyClass::Ordered
    );
}

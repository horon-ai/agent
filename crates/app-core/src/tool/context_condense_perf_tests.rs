//! Structural gate, section `context`: what a large output, a repeated read,
//! and a pressure crossing cost the model's context. Printed for
//! `scripts/perf/check_structural.py`, bounded by
//! `scripts/perf/structural_budget.json`.

// The env lock must outlive the awaited tool and compaction calls; safe under
// the single-threaded test runtime.
#![allow(clippy::await_holding_lock)]

use super::{Registry, Tool, ToolContext, ToolExecutionMode, artifacts};
use crate::message::{ContentBlock, Role};
use crate::provider::{EventStream, Provider};
use std::sync::Arc;

struct PressureProvider;

#[async_trait::async_trait]
impl Provider for PressureProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model(&self) -> String {
        "scripted-model".to_string()
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn uses_horon_agent_compaction(&self) -> bool {
        true
    }

    fn context_window(&self) -> usize {
        200_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(PressureProvider)
    }
}

struct Home {
    dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Home {
    fn isolated() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", dir.path().join("home"));
        crate::config::Config::invalidate_cache();
        Self {
            dir,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => crate::env::set_var("HORON_AGENT_HOME", home),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

#[tokio::test]
async fn context_condensation_stays_bounded() {
    let home = Home::isolated();
    let session_id = "context-perf";
    crate::tool::read_state::forget_session(session_id);

    // 1. A 1 MiB line-oriented output with one error line in the middle.
    let mut log = String::new();
    let mut line = 0usize;
    while log.len() < 1024 * 1024 {
        line += 1;
        if line == 8_000 {
            log.push_str("error: boom\n");
        } else {
            log.push_str(&format!("{line:06} building crate {}\n", "z".repeat(100)));
        }
    }
    let condensed = artifacts::condense_for_context(session_id, "bash", log, artifacts::GENERIC_CONDENSE_BUDGET_BYTES);
    assert!(condensed.contains("L8000: error: boom"), "the error line survives");
    let spill_kept_bytes = condensed.len();

    // 2. The same file read twice.
    std::fs::write(home.dir.path().join("f.txt"), "fn main() {}\n".repeat(2_000)).unwrap();
    let ctx = |call: &str| ToolContext {
        session_id: session_id.to_string(),
        message_id: "m".to_string(),
        tool_call_id: call.to_string(),
        working_dir: Some(home.dir.path().to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    };
    let read = super::read::ReadTool::new();
    let first = read
        .execute(serde_json::json!({"file_path": "f.txt"}), ctx("read-1"))
        .await
        .expect("first read");
    assert!(first.output.len() > 10_000);
    let second = read
        .execute(serde_json::json!({"file_path": "f.txt"}), ctx("read-2"))
        .await
        .expect("second read");
    let read_stub_bytes = second.output.len();

    // 3. Ten 9 KB results past the recent window, under compaction pressure,
    //    aged on two consecutive requests.
    let provider: Arc<dyn Provider> = Arc::new(PressureProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = crate::agent::Agent::new(provider, registry);
    for index in 0..10 {
        agent.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: format!("call-{index}"),
                name: "bash".to_string(),
                input: serde_json::json!({}),
                thought_signature: None,
            }],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: format!("call-{index}"),
                content: format!("{index}\n{}", "row of output\n".repeat(700)),
                is_error: None,
            }],
        );
    }
    for turn in 0..24 {
        agent.add_message(
            if turn % 2 == 0 { Role::User } else { Role::Assistant },
            vec![ContentBlock::Text {
                text: format!("turn {turn}"),
                cache_control: None,
            }],
        );
    }
    let mut aging_batches = 0;
    for _ in 0..2 {
        {
            let compaction = agent.registry_sharing_compaction().compaction();
            let mut manager = compaction.write().await;
            let threshold = manager.compaction_threshold_tokens() as u64;
            assert!(threshold > 0, "the fixture needs a compaction threshold");
            manager.update_observed_input_tokens(threshold);
        }
        if agent.maybe_age_tool_results() > 0 {
            aging_batches += 1;
        }
    }

    println!(
        "STRUCTURAL_PERF section=context source=app-core spill_kept_bytes={spill_kept_bytes} read_stub_bytes={read_stub_bytes} aging_batches={aging_batches}"
    );
    crate::tool::read_state::forget_session(session_id);
}

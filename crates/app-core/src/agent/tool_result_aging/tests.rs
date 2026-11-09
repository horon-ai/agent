// The env lock must outlive awaited compaction-lock calls; safe under the
// single-threaded test runtime.
#![allow(clippy::await_holding_lock)]

use super::super::*;
use super::*;
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::time::Instant;

const RESULT_BYTES: usize = 9 * 1024;

struct AgingProvider;

#[async_trait]
impl Provider for AgingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
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
        Arc::new(AgingProvider)
    }
}

struct Home {
    _dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Home {
    fn isolated() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::config::Config::invalidate_cache();
        Self {
            _dir: dir,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn result_body(index: usize) -> String {
    let mut body = format!("result {index} head\n");
    while body.len() < RESULT_BYTES {
        body.push_str(&format!("row {index} {}\n", "y".repeat(60)));
    }
    body.push_str(&format!("result {index} tail\n"));
    body
}

fn add_tool_round(agent: &mut Agent, index: usize, body: String) {
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: format!("call-{index}"),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "cat log"}),
            thought_signature: None,
        }],
    );
    agent.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: format!("call-{index}"),
            content: body,
            is_error: None,
        }],
    );
}

/// Ten old 9 KB results, then 24 small turns, then one more 9 KB result that
/// stays inside the protected recent window.
async fn agent_with_history() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(AgingProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    for index in 0..10 {
        add_tool_round(&mut agent, index, result_body(index));
    }
    for turn in 0..24 {
        let role = if turn % 2 == 0 { Role::User } else { Role::Assistant };
        agent.add_message(
            role,
            vec![ContentBlock::Text {
                text: format!("turn {turn}"),
                cache_control: None,
            }],
        );
    }
    add_tool_round(&mut agent, 99, result_body(99));
    agent
}

async fn press(agent: &Agent) {
    let compaction = agent.registry.compaction();
    let mut manager = compaction.write().await;
    let threshold = manager.compaction_threshold_tokens();
    assert!(threshold > 0, "the fixture needs a compaction threshold");
    manager.update_observed_input_tokens(threshold as u64);
}

fn tool_results(agent: &mut Agent) -> Vec<(String, String)> {
    agent
        .session
        .provider_messages()
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id, content, ..
            } => Some((tool_use_id.clone(), content.clone())),
            _ => None,
        })
        .collect()
}

fn artifact_id(aged: &str) -> u64 {
    let start = aged.find("artifact://").expect("pointer") + "artifact://".len();
    let digits: String = aged[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().expect("numeric artifact id")
}

#[tokio::test]
async fn pressure_ages_every_old_result_in_one_batch_and_only_once() {
    let _home = Home::isolated();
    let mut agent = agent_with_history().await;
    agent.last_provider_response_at = Some(Instant::now());
    press(&agent).await;

    assert_eq!(agent.maybe_age_tool_results(), 10);
    let results = tool_results(&mut agent);
    for (id, content) in &results {
        if id == "call-99" {
            assert_eq!(content, &result_body(99), "the recent window is untouched");
            continue;
        }
        assert!(content.starts_with(AGED_MARKER_PREFIX), "{id}: {}", &content[..80]);
        assert!(content.contains("head\n"), "keeps the head");
        assert!(content.ends_with("tail\n"), "keeps the tail");
        let index: usize = id.trim_start_matches("call-").parse().unwrap();
        let original = artifacts::load(&agent.session.id, artifact_id(content)).expect("artifact");
        assert_eq!(original, result_body(index), "artifact:// restores the original");
    }

    press(&agent).await;
    assert_eq!(agent.maybe_age_tool_results(), 0, "aged results are never re-aged");
}

#[tokio::test]
async fn warm_cache_without_pressure_leaves_history_alone() {
    let _home = Home::isolated();
    let mut agent = agent_with_history().await;
    agent.last_provider_response_at = Some(Instant::now());
    assert_eq!(agent.maybe_age_tool_results(), 0);
    assert!(
        tool_results(&mut agent)
            .iter()
            .all(|(_, content)| !content.starts_with(AGED_MARKER_PREFIX))
    );
}

#[tokio::test]
async fn cold_cache_ages_without_pressure_and_survives_reload() {
    let _home = Home::isolated();
    let mut agent = agent_with_history().await;
    agent.last_provider_response_at = Some(Instant::now() - Duration::from_secs(3_600));

    assert_eq!(agent.maybe_age_tool_results(), 10);
    let session_id = agent.session.id.clone();
    let reloaded = crate::session::Session::load(&session_id).expect("reload");
    let aged = reloaded
        .active_context()
        .iter()
        .flat_map(|message| message.content.iter())
        .filter(|block| {
            matches!(block, ContentBlock::ToolResult { content, .. } if content.starts_with(AGED_MARKER_PREFIX))
        })
        .count();
    assert_eq!(aged, 10, "the condensed results are what was persisted");
}

#[tokio::test]
async fn pressure_alone_needs_a_batch_worth_the_rewrite() {
    let _home = Home::isolated();
    let provider: Arc<dyn Provider> = Arc::new(AgingProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    // Two old results: 18 KB, below the 64 KiB batch minimum.
    for index in 0..2 {
        add_tool_round(&mut agent, index, result_body(index));
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
    agent.last_provider_response_at = Some(Instant::now());
    press(&agent).await;
    assert_eq!(agent.maybe_age_tool_results(), 0);
}

#[test]
fn aged_text_starts_with_the_marker_and_points_at_the_artifact() {
    let body = result_body(3);
    let aged = aged_text(&body, 42);
    assert!(aged.starts_with(AGED_MARKER_PREFIX));
    assert!(aged.contains(&format!("of {} bytes omitted", body.len())));
    assert!(aged.contains("read file_path=\"artifact://42\""));
    assert!(aged.len() < AGING_KEEP_HEAD_BYTES + AGING_KEEP_TAIL_BYTES + 200);
}

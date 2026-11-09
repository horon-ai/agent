//! Wire-only tool-result pruning: markers reach the provider copy while the
//! stored transcript keeps every original result.

use super::*;
use crate::compaction::pruning::{PRUNED_SUPERSEDED_TEXT, PRUNED_USELESS_TEXT, PruneKind};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use std::ffi::OsString;
use std::sync::Arc;

struct RestoreHome(Option<OsString>);

impl Drop for RestoreHome {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn isolated_home() -> (tempfile::TempDir, RestoreHome) {
    let home = tempfile::tempdir().expect("tempdir");
    let restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    let sessions = home.path().join("sessions");
    std::fs::create_dir_all(&sessions).expect("session store root");
    horon_agent_platform::fs::set_dir_permissions_owner_only(&sessions).expect("owner-only session store root");
    crate::config::Config::invalidate_cache();
    (home, restore)
}

struct NoopProvider;

#[async_trait::async_trait]
impl Provider for NoopProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!("the pruning tests never reach the provider"))
    }

    fn name(&self) -> &str {
        "pruning-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(NoopProvider)
    }
}

async fn test_agent() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(NoopProvider);
    let registry = Registry::new(provider.clone()).await;
    Agent::new(provider, registry)
}

fn push_text(agent: &mut Agent, text: &str) {
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    );
}

fn push_read(agent: &mut Agent, id: &str, path: &str) {
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: "read".to_string(),
            input: serde_json::json!({ "file_path": path }),
            thought_signature: None,
        }],
    );
}

fn push_result(agent: &mut Agent, id: &str, content: &str) {
    agent.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: id.to_string(),
            content: content.to_string(),
            is_error: None,
        }],
    );
}

fn result_contents(messages: &[Message]) -> Vec<(String, String)> {
    messages
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

#[tokio::test]
async fn the_provider_copy_carries_markers_while_stored_history_keeps_the_output() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let mut agent = test_agent().await;

    push_text(&mut agent, "read a.rs twice");
    push_read(&mut agent, "r1", "a.rs");
    push_result(&mut agent, "r1", "1\tfn a() {}\n");
    push_read(&mut agent, "r2", "a.rs");
    push_result(&mut agent, "r2", "1\tfn a() {}\n");

    let stored = agent.session.provider_messages().to_vec();
    let rewritten = agent.prune_provider_messages_enabled(stored);

    let contents = result_contents(&rewritten);
    assert_eq!(
        contents,
        vec![
            ("r1".to_string(), PRUNED_SUPERSEDED_TEXT.to_string()),
            ("r2".to_string(), "1\tfn a() {}\n".to_string()),
        ]
    );
    // The stored transcript is untouched: only the request copy changed.
    let stored_after = result_contents(agent.session.provider_messages());
    assert_eq!(
        stored_after,
        vec![
            ("r1".to_string(), "1\tfn a() {}\n".to_string()),
            ("r2".to_string(), "1\tfn a() {}\n".to_string()),
        ]
    );
    assert_eq!(
        agent.session.pruned_tool_results.get("r1"),
        Some(&PruneKind::Superseded)
    );
    assert!(!agent.session.pruned_tool_results.contains_key("r2"));
}

#[tokio::test]
async fn a_second_request_reuses_the_ledger_without_replanning() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let mut agent = test_agent().await;

    push_text(&mut agent, "read a.rs twice");
    push_read(&mut agent, "r1", "a.rs");
    push_result(&mut agent, "r1", "1\tfn a() {}\n");
    push_read(&mut agent, "r2", "a.rs");
    push_result(&mut agent, "r2", "1\tfn a() {}\n");

    let copy = agent.session.provider_messages().to_vec();
    let first = agent.prune_provider_messages_enabled(copy);
    assert_eq!(result_contents(&first)[0].1, PRUNED_SUPERSEDED_TEXT.to_string());

    let copy = agent.session.provider_messages().to_vec();
    let second = agent.prune_provider_messages_enabled(copy);
    assert_eq!(
        result_contents(&second)[0].1,
        PRUNED_SUPERSEDED_TEXT.to_string(),
        "the persisted ledger keeps applying to later requests"
    );
}

#[tokio::test]
async fn the_switch_is_off_by_default_so_the_request_copy_is_unchanged() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let mut agent = test_agent().await;

    push_text(&mut agent, "read a.rs twice");
    push_read(&mut agent, "r1", "a.rs");
    push_result(&mut agent, "r1", "1\tfn a() {}\n");
    push_read(&mut agent, "r2", "a.rs");
    push_result(&mut agent, "r2", "1\tfn a() {}\n");

    assert!(!crate::config::CompactionConfig::default().prune_tool_results);
    assert!(!crate::config::config().compaction.prune_tool_results);

    let copy = agent.session.provider_messages().to_vec();
    let unchanged = agent.prune_provider_messages(copy);
    assert_eq!(
        result_contents(&unchanged),
        vec![
            ("r1".to_string(), "1\tfn a() {}\n".to_string()),
            ("r2".to_string(), "1\tfn a() {}\n".to_string()),
        ]
    );
    assert!(agent.session.pruned_tool_results.is_empty());
}

#[tokio::test]
async fn a_compacted_view_starts_with_an_empty_ledger() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let mut agent = test_agent().await;

    agent
        .session
        .pruned_tool_results
        .insert("r1".to_string(), PruneKind::Useless);
    agent.note_compaction_applied();
    assert!(agent.session.pruned_tool_results.is_empty());

    agent
        .session
        .pruned_tool_results
        .insert("r2".to_string(), PruneKind::Superseded);
    agent.clear_session_branch().expect("clear the branch");
    assert!(agent.session.pruned_tool_results.is_empty());
}

#[tokio::test]
async fn the_ledger_round_trips_through_the_session_store() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let mut agent = test_agent().await;
    let session_id = agent.session.id.clone();

    push_text(&mut agent, "read a.rs");
    agent
        .session
        .pruned_tool_results
        .insert("r1".to_string(), PruneKind::Superseded);
    agent.session.save().expect("save the session");

    let reloaded = crate::session::Session::load(&session_id).expect("reload the session");
    assert_eq!(reloaded.pruned_tool_results.get("r1"), Some(&PruneKind::Superseded));
}

#[test]
fn the_useless_marker_is_the_documented_string() {
    assert_eq!(PRUNED_USELESS_TEXT, "[Uneventful result elided]");
}

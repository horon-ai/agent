use super::*;
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const WINDOW: usize = 200_000;
const MESSAGE_CHARS: usize = 4_000;
const CONTEXT_LIMIT_ERROR: &str = "prompt is too long: maximum context length exceeded";

struct LimitProvider {
    calls: Arc<AtomicUsize>,
}

impl LimitProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for LimitProvider {
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
        "limit-scripted"
    }

    fn model(&self) -> String {
        "limit-scripted-model".to_string()
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn uses_horon_agent_compaction(&self) -> bool {
        true
    }

    fn context_window(&self) -> usize {
        WINDOW
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            calls: Arc::clone(&self.calls),
        })
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("the recovery path must not call the provider"))
    }

    async fn complete_simple_with_usage(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<(String, horon_agent_provider_core::SimpleCompletionUsage)> {
        Ok((
            self.complete_simple(prompt, system).await?,
            horon_agent_provider_core::SimpleCompletionUsage::default(),
        ))
    }
}

struct RestoreHome(Option<std::ffi::OsString>);

impl Drop for RestoreHome {
    fn drop(&mut self) {
        match &self.0 {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn isolated_home() -> (std::path::PathBuf, RestoreHome) {
    use std::sync::atomic::AtomicUsize;
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let home = std::env::temp_dir().join(format!("horon-context-limit-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&home).expect("home dir");
    let restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", &home);
    crate::config::Config::invalidate_cache();
    (home, restore)
}

fn text_block(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }]
}

fn fill_history(agent: &mut Agent, count: usize) {
    for index in 0..count {
        let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
        agent.add_message(role, text_block(&format!("turn {index} {}", "x".repeat(MESSAGE_CHARS))));
    }
}

fn prior_summary_state(compacted_count: usize) -> crate::session::StoredCompactionState {
    crate::session::StoredCompactionState {
        summary_text: "## Goal\nfinish the compaction recovery work".to_string(),
        openai_encrypted_content: None,
        native_route: None,
        native_window: Vec::new(),
        covers_up_to_turn: compacted_count,
        original_turn_count: compacted_count,
        compacted_count,
        structured: Some(crate::session::StoredSummaryMetadata {
            input_tokens: Some(1_000),
            output_tokens: Some(200),
            restoration: Some(crate::session::StoredRestorationManifest {
                entries: vec![crate::session::StoredRestorationEntry {
                    path: "crates/base/src/compaction/recovery.rs".to_string(),
                    kind: "file".to_string(),
                    note: Some("recovery rules".to_string()),
                }],
                total_bytes: 30_000,
            }),
            lossy_emergency: false,
            recovery_blocked: None,
            handoff: false,
        }),
    }
}

fn seed_prior_summary(agent: &mut Agent, state: &crate::session::StoredCompactionState) {
    let messages = agent.session.provider_messages();
    let compaction = agent.registry.compaction();
    let mut manager = compaction.try_write().expect("compaction lock");
    manager.set_budget(WINDOW);
    manager.restore_persisted_state_with(state, messages);
}

fn orphan_tool_result_agent(provider: Arc<dyn Provider>, registry: Registry) -> Agent {
    let mut agent = Agent::new(provider, registry);
    agent.add_message(Role::User, text_block(&format!("huge {}", "q".repeat(WINDOW))));
    agent.add_message(Role::Assistant, text_block("working"));
    agent.add_message(Role::User, text_block("continue"));
    agent.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call-without-a-call".to_string(),
            content: "result with no matching call".to_string(),
            is_error: None,
        }],
    );
    agent
}

#[tokio::test]
async fn context_limit_auto_recovery_surfaces_lossy_emergency_and_preserves_restoration() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let scripted = LimitProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    fill_history(&mut agent, 40);
    let originals = agent.session.provider_messages().len();
    seed_prior_summary(&mut agent, &prior_summary_state(0));

    assert!(
        agent.try_auto_compact_after_context_limit(CONTEXT_LIMIT_ERROR),
        "a context-limit error with a safe suffix recovers"
    );
    assert_eq!(
        agent.session.provider_messages().len(),
        originals,
        "recovery never removes originals from the transcript"
    );
    let state = agent.session.compaction.clone().expect("the recovery is persisted");
    let meta = state.structured.as_ref().expect("structured metadata");
    assert!(
        meta.lossy_emergency,
        "the recovery is surfaced as lossy, not as a summary"
    );
    assert!(
        meta.restoration
            .as_ref()
            .is_some_and(|manifest| !manifest.entries.is_empty()),
        "the restoration manifest survives the recovery"
    );
    assert_eq!(scripted.calls(), 0, "no provider call during recovery");

    let reloaded = Session::load(agent.session_id()).expect("reload the session");
    let restored = reloaded.compaction.expect("persisted compaction state");
    assert!(
        restored.structured.as_ref().is_some_and(|meta| meta.lossy_emergency),
        "the lossy flag survives a restart"
    );
    assert_eq!(
        restored
            .structured
            .as_ref()
            .and_then(|meta| meta.restoration.as_ref())
            .map(|manifest| manifest.entries.clone()),
        meta.restoration.as_ref().map(|manifest| manifest.entries.clone()),
        "the same restoration manifest is restored after a restart"
    );
}

#[tokio::test]
async fn context_limit_auto_recovery_blocks_without_a_usable_prior_summary_and_refuses_repeat() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let scripted = LimitProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = orphan_tool_result_agent(provider, registry);
    seed_prior_summary(&mut agent, &prior_summary_state(0));
    let before = agent.session.provider_messages().len();

    assert!(
        !agent.try_auto_compact_after_context_limit(CONTEXT_LIMIT_ERROR),
        "without a safe fallback the recovery is refused"
    );
    let compaction = agent.registry.compaction();
    let blocked = compaction
        .try_read()
        .expect("compaction lock")
        .recovery_blocked()
        .cloned()
        .expect("the refusal is recorded");
    assert_eq!(
        blocked.reason,
        crate::compaction::recovery::RecoveryBlockReason::OrphanToolPair
    );
    let digest_after_first = blocked.fingerprint.source_digest.clone();

    assert!(
        !agent.try_auto_compact_after_context_limit(CONTEXT_LIMIT_ERROR),
        "the same source is refused again"
    );
    let compaction = agent.registry.compaction();
    let manager = compaction.try_read().expect("compaction lock");
    assert_eq!(
        manager
            .recovery_blocked()
            .map(|blocked| blocked.fingerprint.source_digest.clone()),
        Some(digest_after_first),
        "the refusal fingerprint did not change"
    );
    drop(manager);
    assert_eq!(agent.session.provider_messages().len(), before);
    assert_eq!(scripted.calls(), 0, "a refused recovery calls no provider");
    let state = agent.session.compaction.as_ref().expect("the refusal is persisted");
    assert!(
        !state.structured.as_ref().is_some_and(|meta| meta.lossy_emergency),
        "a refused recovery never records a lossy summary"
    );
    assert_eq!(state.compacted_count, 0, "the boundary did not move");
    assert_eq!(
        state.summary_text,
        prior_summary_state(0).summary_text,
        "the previous valid summary is preserved unchanged"
    );
}

fn over_budget_agent(provider: Arc<dyn Provider>, registry: Registry) -> Agent {
    let mut agent = Agent::new(provider, registry);
    agent.add_message(Role::User, text_block(&format!("huge {}", "q".repeat(WINDOW))));
    agent
}

fn seeded_summary_agent(provider: Arc<dyn Provider>, registry: Registry) -> Agent {
    let mut agent = Agent::new(provider, registry);
    agent.add_message(Role::User, text_block("earlier work"));
    agent.add_message(Role::Assistant, text_block("done so far"));
    agent.add_message(Role::User, text_block("go on"));
    agent.add_message(Role::Assistant, text_block("working"));
    let boundary = agent.session.provider_messages().len() - 2;
    seed_prior_summary_into_session(&mut agent, prior_summary_state(boundary));
    agent
}

fn seed_prior_summary_into_session(agent: &mut Agent, state: crate::session::StoredCompactionState) {
    agent.session.compaction = Some(state.clone());
    let messages = agent.session.provider_messages();
    let compaction = agent.registry.compaction();
    let mut manager = compaction.try_write().expect("compaction lock");
    manager.set_budget(WINDOW);
    manager.restore_persisted_state_with(&state, messages);
}

#[tokio::test]
async fn context_limit_auto_recovery_refusal_with_existing_summary_is_recorded_and_repeats_are_refused() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let scripted = LimitProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = seeded_summary_agent(provider, registry);
    assert!(
        !agent.try_auto_compact_after_context_limit(CONTEXT_LIMIT_ERROR),
        "nothing safe is left to keep, so recovery is refused"
    );
    let compaction = agent.registry.compaction();
    let blocked = compaction
        .try_read()
        .expect("compaction lock")
        .recovery_blocked()
        .cloned()
        .expect("the refusal is recorded next to the existing summary");
    assert_eq!(
        blocked.reason,
        crate::compaction::recovery::RecoveryBlockReason::NothingToCover
    );
    let digest = blocked.fingerprint.source_digest.clone();
    drop(compaction);
    agent.session.save().expect("save the refused session");

    let reloaded = Session::load(agent.session_id()).expect("reload the session");
    let state = reloaded.compaction.as_ref().expect("the summary stays");
    assert!(
        state.has_summary(),
        "the previous summary is not replaced by the refusal"
    );
    assert_eq!(
        state
            .structured
            .as_ref()
            .and_then(|meta| meta.recovery_blocked.as_ref())
            .map(|blocked| (blocked.reason.as_str(), blocked.source_digest.as_str())),
        Some(("nothing_to_cover", digest.as_str())),
        "the refusal is persisted with the summary intact"
    );

    // The refusal also suppresses a repeat inside the same live session, so a
    // retry loop cannot burn provider calls on the identical input.
    assert!(
        !agent.try_auto_compact_after_context_limit(CONTEXT_LIMIT_ERROR),
        "the same session refuses the repeated attempt"
    );
    assert_eq!(scripted.calls(), 0, "a refused recovery calls no provider");
    let compaction = agent.registry.compaction();
    assert_eq!(
        compaction
            .try_read()
            .expect("compaction lock")
            .recovery_blocked()
            .map(|blocked| blocked.reason),
        Some(crate::compaction::recovery::RecoveryBlockReason::NothingToCover),
        "the refusal is unchanged after the repeat"
    );
}

#[tokio::test]
async fn context_limit_auto_recovery_refusal_survives_restart() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let scripted = LimitProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = over_budget_agent(provider, registry);
    seed_prior_summary_into_session(&mut agent, prior_summary_state(0));
    assert!(!agent.try_auto_compact_after_context_limit(CONTEXT_LIMIT_ERROR));
    agent.session.save().expect("save the refused session");

    let session_id = agent.session_id().to_string();
    let reloaded = Session::load(&session_id).expect("reload the session");
    let persisted = reloaded
        .compaction
        .as_ref()
        .and_then(|state| state.structured.as_ref())
        .and_then(|meta| meta.recovery_blocked.as_ref())
        .expect("the refusal is persisted with the session");
    assert_eq!(persisted.reason, "tail_exceeds_budget");

    let scripted_after_restart = LimitProvider::new();
    let provider: Arc<dyn Provider> = scripted_after_restart.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut restarted = Agent::new_with_session(provider, registry, reloaded, None);
    assert!(
        !restarted.try_auto_compact_after_context_limit(CONTEXT_LIMIT_ERROR),
        "a restart does not retry the same doomed recovery"
    );
    let compaction = restarted.registry.compaction();
    let manager = compaction.try_read().expect("compaction lock");
    assert!(
        manager.recovery_blocked().is_some(),
        "the restored manager keeps the refusal"
    );
    drop(manager);
    assert_eq!(
        scripted_after_restart.calls(),
        0,
        "the restarted agent makes no provider call"
    );
}

#[tokio::test]
async fn context_limit_auto_recovery_ignores_non_context_errors() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let scripted = LimitProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    fill_history(&mut agent, 10);

    assert!(!agent.try_auto_compact_after_context_limit("rate limit exceeded"));
    assert!(agent.session.compaction.is_none());
    assert_eq!(scripted.calls(), 0);
}

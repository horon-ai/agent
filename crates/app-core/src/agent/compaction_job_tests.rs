use super::*;
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

const WINDOW: usize = 200_000;
const MESSAGE_CHARS: usize = 4_000;

struct JobProvider {
    permits: Arc<Semaphore>,
    calls: Arc<AtomicUsize>,
}

impl JobProvider {
    fn new() -> (Arc<Self>, Arc<Semaphore>) {
        let permits = Arc::new(Semaphore::new(0));
        (
            Arc::new(Self {
                permits: Arc::clone(&permits),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            permits,
        )
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for JobProvider {
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
        WINDOW
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            permits: Arc::clone(&self.permits),
            calls: Arc::clone(&self.calls),
        })
    }

    async fn complete_simple(&self, prompt: &str, _system: &str) -> Result<String> {
        Ok(serde_json::json!({
            "goal": format!("summarized {} chars", prompt.len()),
            "done": ["covered the older turns"],
            "incomplete": [],
            "cancelled": [],
            "files": [],
            "evidence": ["compaction job fixture"],
            "decisions": [],
            "next_action": "continue"
        })
        .to_string())
    }

    async fn complete_simple_with_usage(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<(String, horon_agent_provider_core::SimpleCompletionUsage)> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("gate closed"))?;
        permit.forget();
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

fn isolated_home() -> (tempfile::TempDir, RestoreHome) {
    let home = tempfile::tempdir().expect("tempdir");
    let restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    crate::config::Config::invalidate_cache();
    (home, restore)
}

fn fill_history(agent: &mut Agent, count: usize) {
    for index in 0..count {
        let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
        agent.add_message(
            role,
            vec![ContentBlock::Text {
                text: format!("turn {index} {}", "x".repeat(MESSAGE_CHARS)),
                cache_control: None,
            }],
        );
    }
}

async fn set_observed_tokens(agent: &Agent, tokens: u64) {
    let compaction = agent.registry.compaction();
    let mut manager = compaction.write().await;
    manager.update_observed_input_tokens(tokens);
}

async fn is_compacting(agent: &Agent) -> bool {
    agent.registry.compaction().read().await.is_compacting()
}

async fn poll_until_settled(agent: &mut Agent) -> Option<CompactionEvent> {
    for _ in 0..20_000 {
        if let Some(event) = agent.poll_compaction_completion_event() {
            return Some(event);
        }
        if !is_compacting(agent).await {
            return None;
        }
        tokio::task::yield_now().await;
    }
    panic!("compaction job did not settle");
}

#[tokio::test]
async fn compaction_job_idle_starts_only_for_persistent_runtime_past_half_fill() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let (scripted, _permits) = JobProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    fill_history(&mut agent, 40);

    assert!(
        !agent.maybe_start_idle_compaction(),
        "a one-shot runtime never starts idle compaction"
    );

    agent.set_persistent_runtime(true);
    assert!(
        !agent.maybe_start_idle_compaction(),
        "below the lead point there is nothing to pre-compact"
    );

    set_observed_tokens(&agent, 150_000).await;
    assert!(
        agent.maybe_start_idle_compaction(),
        "the daemon starts an idle job past the lead point"
    );
    assert!(is_compacting(&agent).await);
    assert!(
        !agent.maybe_start_idle_compaction(),
        "a second idle start is refused while a job is in flight"
    );
}

#[tokio::test]
async fn compaction_job_manual_apply_survives_tail_append_exactly_once() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let (scripted, permits) = JobProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    fill_history(&mut agent, 40);

    let (_status, started) = agent.request_manual_compaction();
    assert!(started, "manual compaction starts");

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "appended while summarizing".to_string(),
            cache_control: None,
        }],
    );
    let expected_messages = agent.session.messages_for_provider().len();
    permits.add_permits(1);

    let event = poll_until_settled(&mut agent).await.expect("the finished job applies");
    let compacted = event.messages_compacted.expect("compacted count");
    assert!(compacted > 0);
    assert_eq!(
        agent.session.compaction.as_ref().map(|state| state.compacted_count),
        Some(compacted),
        "the applied boundary is persisted once"
    );
    assert_eq!(
        agent.session.messages_for_provider().len(),
        expected_messages,
        "originals plus the mid-flight append are preserved"
    );
    assert_eq!(scripted.calls(), 1, "one provider call per job");

    assert!(
        poll_until_settled(&mut agent).await.is_none(),
        "a settled job never applies a second time"
    );
    assert_eq!(
        agent.session.compaction.as_ref().map(|state| state.compacted_count),
        Some(compacted),
        "the boundary does not advance twice"
    );
}

#[tokio::test]
async fn compaction_job_clear_cancels_the_job_and_keeps_originals() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let (scripted, permits) = JobProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    fill_history(&mut agent, 40);

    let (_status, started) = agent.request_manual_compaction();
    assert!(started);
    assert!(is_compacting(&agent).await);

    agent.clear();
    assert!(
        !is_compacting(&agent).await,
        "clearing the branch cancels the in-flight job"
    );

    permits.add_permits(1);
    assert!(
        poll_until_settled(&mut agent).await.is_none(),
        "a cancelled job never applies"
    );
    assert!(
        agent.session.compaction.is_none(),
        "the cleared branch carries no compaction state"
    );
}

#[tokio::test]
async fn compaction_job_shutdown_cancel_releases_the_slot() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let (scripted, permits) = JobProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    fill_history(&mut agent, 40);

    let (_status, started) = agent.request_manual_compaction();
    assert!(started);
    agent.cancel_background_compaction(crate::compaction::jobs::CancelReason::Shutdown);
    assert!(!is_compacting(&agent).await);

    permits.add_permits(4);
    assert!(poll_until_settled(&mut agent).await.is_none());

    let (_status, restarted) = agent.request_manual_compaction();
    assert!(restarted, "a cancelled job frees the single-flight slot");
}

#[tokio::test]
async fn idle_compaction_decision_does_not_clone_the_provider_context() {
    let _lock = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let (scripted, _permits) = JobProvider::new();
    let provider: Arc<dyn Provider> = scripted.clone();
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    fill_history(&mut agent, 40);
    agent.set_persistent_runtime(true);

    // Warm the projection so a rebuild would be attributable to the decision.
    let _ = agent.session.provider_messages();
    let (materializations, clones) = agent.session.provider_context_stats();

    assert!(
        !agent.maybe_start_idle_compaction(),
        "below the lead point there is nothing to pre-compact"
    );
    assert_eq!(
        agent.session.provider_context_stats(),
        (materializations, clones),
        "a declined idle start neither rebuilds nor clones the provider context"
    );

    println!("QA_COUNTERS no_compaction materializations={materializations} clones={clones}");
    set_observed_tokens(&agent, 150_000).await;
    assert!(agent.maybe_start_idle_compaction(), "past the lead point a job starts");
    let (started_materializations, started_clones) = agent.session.provider_context_stats();
    assert_eq!(
        started_clones, clones,
        "starting a job reads the borrowed projection, so nothing is cloned"
    );
    assert!(
        started_materializations <= materializations + 1,
        "starting a job materializes the projection at most once"
    );
    println!("QA_COUNTERS compaction materializations={started_materializations} clones={started_clones}");
}

//! Pins the deliberate split: `Clone` gives a subagent an independent
//! `CompactionManager`, `clone_sharing_compaction` follows the parent's.

use super::*;
use crate::compaction::{CompactionManager, DEFAULT_TOKEN_BUDGET, SYSTEM_OVERHEAD_TOKENS};

fn registry_with_budget(budget: usize) -> Registry {
    let registry = Registry::empty();
    *registry.compaction().try_write().expect("fresh lock is uncontended") =
        CompactionManager::new().with_budget(budget);
    registry
}

async fn budget_of(registry: &Registry) -> usize {
    registry.compaction().read().await.token_budget()
}

async fn count_of(registry: &Registry) -> usize {
    registry.compaction().read().await.effective_token_count()
}

#[tokio::test]
async fn plain_clone_reports_default_budget_and_overhead_floor() {
    let parent = registry_with_budget(33_000);
    let clone = parent.clone();

    assert_eq!(DEFAULT_TOKEN_BUDGET, 200_000);
    assert_eq!(
        budget_of(&clone).await,
        DEFAULT_TOKEN_BUDGET,
        "Clone keeps its fresh manager, which starts at the default budget"
    );
    assert_eq!(
        count_of(&clone).await,
        SYSTEM_OVERHEAD_TOKENS,
        "a fresh manager counts only the system-prompt and tool-definition floor"
    );
    assert_ne!(
        budget_of(&parent).await,
        budget_of(&clone).await,
        "the parent's configured budget must not leak into a plain clone"
    );
}

#[tokio::test]
async fn sharing_clone_follows_the_parent_budget_and_count() {
    let parent = registry_with_budget(33_000);
    parent.compaction().write().await.update_observed_input_tokens(7_500);

    let clone = parent.clone_sharing_compaction();

    assert_eq!(budget_of(&clone).await, 33_000);
    assert_eq!(count_of(&clone).await, count_of(&parent).await);
    assert!(
        count_of(&clone).await >= 7_500,
        "the parent's observed count must be visible"
    );

    parent.compaction().write().await.set_budget(48_000);
    assert_eq!(
        budget_of(&clone).await,
        48_000,
        "sharing clone must observe a later set_budget on the parent"
    );

    let huge = ToolOutput::new("x".repeat(200_000));
    let refused = clone.guard_context_overflow("test", huge, false).await;
    assert!(
        refused.output.contains("OUTPUT WITHHELD") || refused.output.contains("CONTEXT LIMIT"),
        "guard must refuse against the shared 48k budget, got: {}",
        &refused.output[..refused.output.len().min(200)]
    );
}

#[tokio::test]
async fn plain_clone_stays_independent_after_parent_budget_change() {
    let parent = registry_with_budget(33_000);
    let clone = parent.clone();

    parent.compaction().write().await.set_budget(48_000);
    parent.compaction().write().await.update_observed_input_tokens(90_000);

    assert_eq!(
        budget_of(&clone).await,
        DEFAULT_TOKEN_BUDGET,
        "Clone semantics must be unchanged: the clone keeps its own manager"
    );
    assert_eq!(
        count_of(&clone).await,
        SYSTEM_OVERHEAD_TOKENS,
        "the clone must not observe the parent's token count"
    );
}

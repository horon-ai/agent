//! Agent-level coverage for per-turn effort keywords.
//!
//! Every test name contains `effort_keywords` so one filter selects the feature:
//! `cargo test -p horon-agent-app-core effort_keywords`.

use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::Mutex;

/// A provider whose effort ladder, stored effort, and the last context it saw
/// are all under test control. It records each `ProviderRequestContext` so a
/// test can assert the per-call override without touching shared state.
struct EffortLadderProvider {
    ladder: Vec<&'static str>,
    stored: Mutex<Option<String>>,
    seen: Mutex<Vec<Option<String>>>,
}

impl EffortLadderProvider {
    fn new(ladder: Vec<&'static str>) -> Arc<Self> {
        Arc::new(Self {
            ladder,
            stored: Mutex::new(None),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn overrides(&self) -> Vec<Option<String>> {
        self.seen.lock().unwrap().clone()
    }

    fn stored(&self) -> Option<String> {
        self.stored.lock().unwrap().clone()
    }

    fn events() -> Vec<Result<StreamEvent>> {
        vec![
            Ok(StreamEvent::TextDelta("done".into())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ]
    }
}

#[async_trait]
impl Provider for EffortLadderProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::iter(Self::events())))
    }

    async fn complete_with_context(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
        context: &horon_agent_provider_core::ProviderRequestContext,
    ) -> Result<EventStream> {
        self.seen.lock().unwrap().push(context.reasoning_effort.clone());
        Ok(Box::pin(futures::stream::iter(Self::events())))
    }

    async fn complete_split_with_context(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: &str,
        _: Option<&str>,
        context: &horon_agent_provider_core::ProviderRequestContext,
    ) -> Result<EventStream> {
        self.seen.lock().unwrap().push(context.reasoning_effort.clone());
        Ok(Box::pin(futures::stream::iter(Self::events())))
    }

    fn name(&self) -> &str {
        "effort-ladder-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.stored.lock().unwrap().clone()
    }

    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        *self.stored.lock().unwrap() = Some(effort.to_string());
        Ok(())
    }

    fn available_efforts(&self) -> Vec<&'static str> {
        self.ladder.clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        let forked = Arc::new(Self {
            ladder: self.ladder.clone(),
            stored: Mutex::new(self.stored.lock().unwrap().clone()),
            seen: Mutex::new(Vec::new()),
        });
        forked
    }
}

fn effort_test_agent(provider: Arc<EffortLadderProvider>) -> Agent {
    Agent::new(provider, Registry::empty())
}

#[tokio::test]
async fn effort_keywords_ultrathink_overrides_only_that_turn() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider = EffortLadderProvider::new(vec!["low", "medium", "high"]);
    let mut agent = effort_test_agent(provider.clone());

    agent.arm_effort_keywords("please ultrathink this");
    agent.run_once("please ultrathink this").await.unwrap();
    // Next turn is unarmed, so it must fall back to no override.
    agent.run_once("plain follow-up").await.unwrap();

    assert_eq!(
        provider.overrides(),
        vec![Some("high".to_string()), None],
        "only the armed turn carries the top rung, then it clears"
    );
    assert_eq!(provider.stored(), None, "the keyword must not mutate stored effort");
}

#[tokio::test]
async fn effort_keywords_chillax_uses_lowest_non_none_rung() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider = EffortLadderProvider::new(vec!["none", "low", "medium", "high"]);
    let mut agent = effort_test_agent(provider.clone());

    agent.arm_effort_keywords("chillax");
    agent.run_once("chillax").await.unwrap();

    assert_eq!(provider.overrides(), vec![Some("low".to_string())]);
    assert_eq!(provider.stored(), None);
}

#[tokio::test]
async fn effort_keywords_off_by_feature_flag_produces_no_override() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    std::fs::write(
        sandbox.root().join("config.toml"),
        "[features]\neffort_keywords = false\n",
    )
    .unwrap();
    crate::config::Config::invalidate_cache();

    let provider = EffortLadderProvider::new(vec!["low", "medium", "high"]);
    let mut agent = effort_test_agent(provider.clone());
    agent.arm_effort_keywords("ultrathink");
    agent.run_once("ultrathink").await.unwrap();

    assert_eq!(provider.overrides(), vec![None]);
}

#[tokio::test]
async fn effort_keywords_pinned_effort_wins() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider = EffortLadderProvider::new(vec!["low", "medium", "high"]);
    let mut agent = effort_test_agent(provider.clone());
    agent.set_reasoning_effort("low").unwrap();

    agent.arm_effort_keywords("ultrathink");
    agent.run_once("ultrathink").await.unwrap();

    assert_eq!(provider.overrides(), vec![None], "an explicit effort is never moved");
    assert_eq!(provider.stored(), Some("low".to_string()), "pinned effort stays put");
}

#[tokio::test]
async fn effort_keywords_swarm_current_effort_wins() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider = EffortLadderProvider::new(vec!["low", "medium", "high"]);
    let mut agent = effort_test_agent(provider.clone());
    agent.session.reasoning_effort = Some("swarm-deep".to_string());

    agent.arm_effort_keywords("chillax");
    agent.run_once("chillax").await.unwrap();

    assert_eq!(provider.overrides(), vec![None], "a swarm sentinel is an explicit mode");
}

#[tokio::test]
async fn effort_keywords_unarmed_turn_ignores_keyword_text() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider = EffortLadderProvider::new(vec!["low", "medium", "high"]);
    let mut agent = effort_test_agent(provider.clone());

    // The text contains the keyword, but nothing armed the slot: this is an
    // agent-initiated turn and must not scan the text.
    agent.run_once("ultrathink but nobody armed me").await.unwrap();

    assert_eq!(provider.overrides(), vec![None]);
}

#[tokio::test]
async fn effort_keywords_chillax_does_not_lower_a_lower_rung_below_it() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider = EffortLadderProvider::new(vec!["none", "low", "medium", "high"]);
    let mut agent = effort_test_agent(provider.clone());
    agent.set_reasoning_effort("low").unwrap();
    // Clear the pin so the ladder direction rule is what is under test.
    agent.session.reasoning_effort_pinned = false;
    agent.reasoning_effort_pinned = false;

    agent.arm_effort_keywords("chillax");
    agent.run_once("chillax").await.unwrap();

    assert_eq!(
        provider.overrides(),
        vec![Some("low".to_string())],
        "already at the lowest selectable rung"
    );
}

#[tokio::test]
async fn effort_keywords_empty_ladder_produces_no_override() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let provider = EffortLadderProvider::new(vec![]);
    let mut agent = effort_test_agent(provider.clone());

    agent.arm_effort_keywords("ultrathink");
    agent.run_once("ultrathink").await.unwrap();

    assert_eq!(provider.overrides(), vec![None], "a model without effort support");
}

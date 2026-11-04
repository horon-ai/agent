//! The streaming loop clones the registry once per tool call to execute that
//! call for the same session. That clone must share the session's
//! `CompactionManager`; with `Clone`'s fresh manager the overflow guard reads
//! the overhead floor against the default budget and never refuses anything.

use super::*;
use crate::provider::EventStream;
use crate::tool::ToolOutput;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The 200 KB result is condensed to the 48 KiB generic budget (~8k tokens)
/// before the guard sees it: above the ~7.2k single-output ceiling of a 24k
/// window, far below the 50k ceiling a plain clone's default budget allows.
const SESSION_WINDOW: usize = 24_000;

struct HugeTool;

#[async_trait]
impl crate::tool::Tool for HugeTool {
    fn name(&self) -> &str {
        "huge"
    }
    fn description(&self) -> &str {
        "Return a deliberately oversized result"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new("x".repeat(200_000)))
    }
}

#[derive(Clone)]
struct ToolThenTextProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for ToolThenTextProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(futures::stream::iter([
                Ok(StreamEvent::ToolUseStart {
                    id: "huge_1".into(),
                    name: "huge".into(),
                }),
                Ok(StreamEvent::ToolInputDeltaFor {
                    id: "huge_1".into(),
                    delta: "{}".into(),
                }),
                Ok(StreamEvent::ToolUseEndFor { id: "huge_1".into() }),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".into()),
                }),
            ])))
        } else {
            Ok(Box::pin(futures::stream::iter([
                Ok(StreamEvent::TextDelta("done".into())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                }),
            ])))
        }
    }
    fn name(&self) -> &str {
        "tool-then-text"
    }
    fn supports_compaction(&self) -> bool {
        false
    }
    fn context_window(&self) -> usize {
        SESSION_WINDOW
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn budget_and_count(registry: &Registry) -> (usize, usize) {
    let manager = registry.compaction();
    let guard = manager.read().await;
    (guard.token_budget(), guard.effective_token_count())
}

#[tokio::test]
async fn streaming_loop_clone_refuses_against_the_session_budget() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let registry = Registry::empty();
    registry
        .register("huge".into(), Arc::new(HugeTool) as Arc<dyn crate::tool::Tool>)
        .await;
    let provider = Arc::new(ToolThenTextProvider {
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "return the huge result".into(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    let turn = tokio::spawn(async move {
        agent.run_turn_streaming_mpsc(tx).await.unwrap();
        agent
    });
    while rx.recv().await.is_some() {}
    let agent = turn.await.unwrap();

    let (session_budget, session_count) = budget_and_count(&agent.registry).await;
    let (plain_budget, plain_count) = budget_and_count(&agent.registry.clone()).await;
    let (sharing_budget, sharing_count) = budget_and_count(&agent.registry.clone_sharing_compaction()).await;
    println!("QA session budget={session_budget} count={session_count}");
    println!("QA plain   budget={plain_budget} count={plain_count}");
    println!("QA sharing budget={sharing_budget} count={sharing_count}");

    assert_eq!(session_budget, SESSION_WINDOW);
    assert_eq!((plain_budget, plain_count), (200_000, 18_000));
    assert_eq!((sharing_budget, sharing_count), (session_budget, session_count));

    let results: Vec<String> = agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(results.len(), 1);
    println!(
        "QA tool_result_prefix={}",
        results[0].chars().take(90).collect::<String>()
    );
    assert!(
        results[0].contains("OUTPUT WITHHELD"),
        "the per-call clone must refuse against the session budget, got: {}",
        results[0]
    );
    assert!(
        results[0].contains("24k"),
        "the refusal must name the session budget, got: {}",
        results[0]
    );
}

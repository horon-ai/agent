//! Handoff compaction: the boundary is committed from a generated document,
//! and every failure leaves the transcript untouched.

use super::*;
use crate::provider::{EventStream, Provider};
use crate::session::event_store_api as store;
use async_trait::async_trait;
use std::sync::Arc;

struct HandoffProvider;

#[async_trait]
impl Provider for HandoffProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("the handoff tests never send a request")
    }

    fn name(&self) -> &str {
        "handoff-test"
    }

    fn model(&self) -> String {
        "handoff-test-model".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// Answers with a fixed document, or fails, without touching the network.
struct ScriptedCompleter {
    answer: std::result::Result<String, String>,
}

#[async_trait]
impl crate::compaction::handoff::HandoffCompleter for ScriptedCompleter {
    async fn complete(&self, _request: crate::aux_completion::CompletionRequest) -> anyhow::Result<String> {
        match &self.answer {
            Ok(text) => Ok(text.clone()),
            Err(detail) => Err(anyhow::anyhow!("{detail}")),
        }
    }
}

fn text_block(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }]
}

/// A transcript long enough that a small recent-token budget leaves a prefix.
async fn handoff_fixture() -> (crate::auth::test_sandbox::AuthTestSandbox, Agent) {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("test sandbox");

    let provider: Arc<dyn Provider> = Arc::new(HandoffProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.session.provider_key = Some("handoff-test".to_string());
    agent.session.model = Some("handoff-test-model".to_string());
    agent.session.route_api_method = Some("handoff-test-route".to_string());

    agent.add_message(Role::User, text_block("plan the handoff boundary work"));
    agent.add_message(Role::Assistant, text_block("I will write the commit hook."));
    agent.add_message(Role::User, text_block("keep going"));
    agent.add_message(Role::Assistant, text_block("committing the boundary now"));
    (sandbox, agent)
}

/// A document with every heading the prompt asks for.
const COMPLETE_DOCUMENT: &str = "\
## Goal
Commit a handoff document as the compaction boundary.

## Done
The generator exists.

## In progress
The commit hook.

## Next steps
Wire the trigger.

## Open risks
None.

## Key files
crates/app-core/src/agent/handoff_compaction.rs
";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_compaction_commits_a_document_as_the_boundary() {
    let (_sandbox, mut agent) = handoff_fixture().await;
    let cancel = AtomicBool::new(false);
    let completer = ScriptedCompleter {
        answer: Ok(COMPLETE_DOCUMENT.to_string()),
    };

    let outcome = agent.request_handoff_compaction_with(&cancel, 1, &completer);

    let HandoffOutcome::Committed { covered } = outcome else {
        panic!("expected a committed boundary, got {outcome:?}");
    };
    assert!(covered > 0, "a handoff covers at least one message");
    let state = agent.session.compaction.as_ref().expect("the boundary is stored");
    assert!(
        !state.summary_text.trim().is_empty(),
        "the document is the boundary text"
    );
    assert_eq!(
        state.summary_text, COMPLETE_DOCUMENT,
        "the document is committed verbatim"
    );
    assert_eq!(state.compacted_count, covered);
    assert!(
        state.structured.as_ref().is_some_and(|meta| meta.handoff),
        "the boundary is tagged as a handoff"
    );

    // The stored event carries the handoff outcome, not a plain summary.
    agent.save_session().expect("persist the boundary");
    let id = store::SessionId::parse(&agent.session.id).unwrap();
    let paths = crate::session::store_paths().expect("store paths");
    let graph = store::EventGraph::open(&paths, &id).expect("open the event graph");
    let (_, payload) = graph
        .replay(None)
        .expect("replay")
        .compaction()
        .cloned()
        .expect("a compacted event exists");
    assert_eq!(payload.outcome, store::CompactionOutcome::Handoff);
    assert_eq!(payload.summary_text, COMPLETE_DOCUMENT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_compaction_commits_a_document_that_only_states_the_goal() {
    let (_sandbox, mut agent) = handoff_fixture().await;
    let cancel = AtomicBool::new(false);
    let partial = "## Goal\nfinish it\n";
    let completer = ScriptedCompleter {
        answer: Ok(partial.to_string()),
    };

    let outcome = agent.request_handoff_compaction_with(&cancel, 1, &completer);

    assert!(matches!(outcome, HandoffOutcome::Committed { covered } if covered > 0));
    let state = agent.session.compaction.as_ref().expect("the boundary is stored");
    assert_eq!(
        state.summary_text, partial,
        "a document that states the goal is still a document"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_compaction_rejected_document_leaves_the_transcript_unchanged() {
    let (_sandbox, mut agent) = handoff_fixture().await;
    let before_messages = agent.session.active_context().len();
    let before_state = agent.session.compaction.clone();
    let cancel = AtomicBool::new(false);

    for (rejected, reason) in [("", "empty"), (" \n", "empty"), ("## Done\nnothing yet\n", "Goal")] {
        let completer = ScriptedCompleter {
            answer: Ok(rejected.to_string()),
        };

        let outcome = agent.request_handoff_compaction_with(&cancel, 1, &completer);

        assert!(
            matches!(&outcome, HandoffOutcome::ProviderError(detail) if detail.contains(reason)),
            "a rejected document is reported as a failure, got {outcome:?}"
        );
        assert_eq!(
            agent.session.active_context().len(),
            before_messages,
            "no message is removed"
        );
        assert_eq!(agent.session.compaction, before_state, "no boundary is written");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_compaction_provider_error_leaves_the_transcript_unchanged() {
    let (_sandbox, mut agent) = handoff_fixture().await;
    let before_messages = agent.session.active_context().len();
    let before_state = agent.session.compaction.clone();
    let cancel = AtomicBool::new(false);
    let completer = ScriptedCompleter {
        answer: Err("the provider refused the request".to_string()),
    };

    let outcome = agent.request_handoff_compaction_with(&cancel, 1, &completer);

    let HandoffOutcome::ProviderError(detail) = outcome else {
        panic!("expected a provider error, got {outcome:?}");
    };
    assert!(detail.contains("the provider refused the request"), "{detail}");
    assert_eq!(
        agent.session.active_context().len(),
        before_messages,
        "no message is removed"
    );
    assert_eq!(agent.session.compaction, before_state, "no boundary is written");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_compaction_cancel_leaves_the_transcript_unchanged() {
    let (_sandbox, mut agent) = handoff_fixture().await;
    let before_messages = agent.session.active_context().len();
    let before_state = agent.session.compaction.clone();
    let cancel = AtomicBool::new(true);
    let completer = ScriptedCompleter {
        answer: Ok(COMPLETE_DOCUMENT.to_string()),
    };

    let outcome = agent.request_handoff_compaction_with(&cancel, 1, &completer);

    assert_eq!(outcome, HandoffOutcome::Cancelled);
    assert_eq!(
        agent.session.active_context().len(),
        before_messages,
        "no message is removed"
    );
    assert_eq!(agent.session.compaction, before_state, "no boundary is written");
}

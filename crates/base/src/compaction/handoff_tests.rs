use super::*;
use crate::aux_completion::CompletionRequest;
use crate::message::Message;
use anyhow::Result;
use horon_agent_compaction_core::message_token_count;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A transcript with ten user turns, each followed by a long assistant answer.
///
/// The answers are long on purpose: the recent-token budget has to be able to
/// keep a different number of them for two different budgets to be
/// distinguishable.
fn fixture_transcript() -> Vec<Message> {
    let mut messages = Vec::new();
    for turn in 0..10 {
        messages.push(Message::user(&format!(
            "user message {turn}: {}",
            "request detail ".repeat(120)
        )));
        messages.push(Message::assistant_text(&format!(
            "assistant message {turn}: {}",
            "answer detail ".repeat(120)
        )));
    }
    messages
}

/// Token cost of one fixture message, measured by the same estimator the cut uses.
fn fixture_unit() -> usize {
    message_token_count(&fixture_transcript()[0])
}

/// A budget that keeps a few messages verbatim and leaves a covered prefix.
fn covering_budget() -> usize {
    fixture_unit() * 6
}

/// A budget small enough that only the minimum kept tail survives.
fn minimal_budget() -> usize {
    fixture_unit() + fixture_unit() / 2
}

/// A completer that answers from a script and records every prompt it saw.
struct ScriptedCompleter {
    answer: std::result::Result<String, String>,
    prompts: Mutex<Vec<String>>,
}

impl ScriptedCompleter {
    fn answering(answer: impl Into<String>) -> Self {
        Self {
            answer: Ok(answer.into()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn failing(detail: impl Into<String>) -> Self {
        Self {
            answer: Err(detail.into()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("prompts lock").clone()
    }
}

#[async_trait::async_trait]
impl HandoffCompleter for ScriptedCompleter {
    async fn complete(&self, request: CompletionRequest) -> Result<String> {
        self.prompts
            .lock()
            .expect("prompts lock")
            .push(request.prompt().to_string());
        match &self.answer {
            Ok(answer) => Ok(answer.clone()),
            Err(detail) => Err(anyhow::anyhow!(detail.clone())),
        }
    }
}

/// A completer that never answers, so only cancellation can settle the call.
struct PendingCompleter;

#[async_trait::async_trait]
impl HandoffCompleter for PendingCompleter {
    async fn complete(&self, _request: CompletionRequest) -> Result<String> {
        futures::future::pending().await
    }
}

fn not_cancelled() -> AtomicBool {
    AtomicBool::new(false)
}

#[tokio::test]
async fn handoff_generates_a_non_empty_document_from_a_fixture_transcript() {
    let messages = fixture_transcript();
    let budget = covering_budget();
    let completer = ScriptedCompleter::answering("## Goal\nship it\n\n## Done\nmost of it\n");
    let cancel = not_cancelled();

    let document = generate_handoff(&messages, budget, &completer, &cancel)
        .await
        .expect("handoff generation");

    assert!(!document.document.is_empty(), "the document must not be empty");
    assert!(document.cutoff > 0, "some prefix has to be covered");
    assert!(document.cutoff < messages.len(), "the tail has to stay verbatim");

    let prompt = completer.prompts().into_iter().next().expect("one prompt");
    for heading in [
        "## Goal",
        "## Done",
        "## In progress",
        "## Next steps",
        "## Open risks",
        "## Key files",
    ] {
        assert!(prompt.contains(heading), "the prompt must ask for {heading}");
    }
    assert!(prompt.contains("user message 0"), "the covered prefix must be rendered");
    assert!(
        !prompt.contains("user message 9"),
        "the newest turn must stay out of the prompt"
    );
}

#[tokio::test]
async fn handoff_returns_a_document_with_only_a_goal_as_is() {
    let messages = fixture_transcript();
    let cancel = not_cancelled();

    for partial in [
        "## Goal\nfinish the migration\n",
        "# goal\nfinish the migration\n\n## Next steps\nrun the gate\n",
        "Here is the handoff.\n\n  ### GOAL:\nfinish the migration\n",
    ] {
        let completer = ScriptedCompleter::answering(partial);

        let document = generate_handoff(&messages, covering_budget(), &completer, &cancel)
            .await
            .expect("a document with a Goal heading is accepted");

        assert_eq!(
            document.document, partial,
            "the response is returned verbatim, without parsing or repair"
        );
    }
}

#[tokio::test]
async fn handoff_rejects_an_empty_document() {
    let messages = fixture_transcript();
    let cancel = not_cancelled();

    for empty in ["", " \n\t\n"] {
        let completer = ScriptedCompleter::answering(empty);

        let error = generate_handoff(&messages, covering_budget(), &completer, &cancel)
            .await
            .expect_err("an empty document cannot replace the covered messages");

        assert!(
            matches!(&error, HandoffError::InvalidDocument { reason } if reason.contains("empty")),
            "got {error}"
        );
    }
}

#[tokio::test]
async fn handoff_rejects_a_document_without_a_goal_heading() {
    let messages = fixture_transcript();
    let cancel = not_cancelled();

    for goalless in [
        "## Done\nmost of it\n\n## Next steps\nrun the gate\n",
        "The goal is to finish the migration.\n",
        "## Goals and non-goals\nfinish the migration\n",
    ] {
        let completer = ScriptedCompleter::answering(goalless);

        let error = generate_handoff(&messages, covering_budget(), &completer, &cancel)
            .await
            .expect_err("a document that never states the goal is refused");

        assert!(
            matches!(&error, HandoffError::InvalidDocument { reason } if reason.contains("Goal")),
            "got {error}"
        );
    }
}

#[tokio::test]
async fn handoff_small_budget_keeps_fewer_messages_than_a_large_one() {
    let messages = fixture_transcript();
    let completer = ScriptedCompleter::answering("## Goal\nx\n");
    let cancel = not_cancelled();

    let small = generate_handoff(&messages, minimal_budget(), &completer, &cancel)
        .await
        .expect("small-budget handoff");
    let large = generate_handoff(&messages, covering_budget(), &completer, &cancel)
        .await
        .expect("large-budget handoff");

    let small_kept = messages.len() - small.cutoff;
    let large_kept = messages.len() - large.cutoff;
    assert!(
        small_kept < large_kept,
        "a small recent_tokens budget must keep fewer messages verbatim ({small_kept} vs {large_kept})"
    );
}

#[tokio::test]
async fn handoff_with_a_budget_that_keeps_everything_reports_nothing_to_cover() {
    let messages = fixture_transcript();
    let completer = ScriptedCompleter::answering("unused");
    let cancel = not_cancelled();

    let error = generate_handoff(&messages, usize::MAX / 2, &completer, &cancel)
        .await
        .expect_err("everything is kept verbatim");

    assert!(matches!(&error, HandoffError::NothingToCover), "got {error}");
    assert!(completer.prompts().is_empty(), "no completion may be dispatched");
}

#[tokio::test]
async fn handoff_provider_error_is_reported_with_its_detail() {
    let messages = fixture_transcript();
    let completer = ScriptedCompleter::failing("upstream returned 503");
    let cancel = not_cancelled();

    let error = generate_handoff(&messages, covering_budget(), &completer, &cancel)
        .await
        .expect_err("the provider failed");

    match error {
        HandoffError::Provider { detail } => assert!(
            detail.contains("upstream returned 503"),
            "the provider detail must survive: {detail}"
        ),
        other => panic!("expected a provider error, got {other}"),
    }
}

#[tokio::test(start_paused = true)]
async fn handoff_cancellation_reports_cancelled_not_success() {
    let messages = fixture_transcript();
    let budget = covering_budget();
    let cancel = Arc::new(AtomicBool::new(false));

    let trigger = {
        let cancel = Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.store(true, Ordering::Relaxed);
        })
    };

    let error = tokio::time::timeout(
        Duration::from_secs(30),
        generate_handoff(&messages, budget, &PendingCompleter, &cancel),
    )
    .await
    .expect("cancellation must settle the call")
    .expect_err("cancellation is not a document");

    trigger.await.expect("cancellation trigger");
    assert!(matches!(&error, HandoffError::Cancelled), "got {error}");
}

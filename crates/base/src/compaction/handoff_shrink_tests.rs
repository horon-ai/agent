//! Prompt-budget behaviour of handoff generation.
//!
//! The generator never clips a prompt: when the covered range does not fit
//! [`MAX_PROMPT_BYTES`] it shrinks the range and asks again, and when even the
//! smallest range does not fit it refuses. These tests pin both halves, plus the
//! `prior` argument that keeps a second handoff anchored to the first one's goal.

use super::handoff::{HandoffCompleter, HandoffError, generate_handoff_at_cutoff};
use crate::aux_completion::{CompletionRequest, MAX_PROMPT_BYTES};
use crate::message::Message;
use anyhow::Result;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

/// A context window wide enough that the restoration allowance is the
/// absolute ceiling, so these tests pin prompt sizing alone.
const TEST_WINDOW: usize = 400_000;

/// A transcript long enough that one prompt over all of it overflows the cap.
///
/// The messages are short on purpose: a short message renders whole, so the
/// rendered body grows linearly with the covered count and shrinking it really
/// does shrink the prompt.
fn long_transcript(count: usize) -> Vec<Message> {
    (0..count)
        .map(|index| {
            if index % 2 == 0 {
                Message::user(&format!("user {index} {}", "detail ".repeat(4)))
            } else {
                Message::assistant_text(&format!("assistant {index} {}", "detail ".repeat(4)))
            }
        })
        .collect()
}

/// A completer that answers once and records the prompt it was given.
struct RecordingCompleter {
    answer: String,
    prompts: Mutex<Vec<String>>,
}

impl RecordingCompleter {
    fn new() -> Self {
        Self {
            answer: "## Goal\nfinish the shrink test\n\n## Next steps\nverify\n".to_string(),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn prompt(&self) -> String {
        self.prompts
            .lock()
            .expect("prompts lock")
            .first()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl HandoffCompleter for RecordingCompleter {
    async fn complete(&self, request: CompletionRequest) -> Result<String> {
        assert!(
            request.prompt().len() <= MAX_PROMPT_BYTES,
            "a prompt over the cap must never reach the completer"
        );
        self.prompts
            .lock()
            .expect("prompts lock")
            .push(request.prompt().to_string());
        Ok(self.answer.clone())
    }
}

fn not_cancelled() -> AtomicBool {
    AtomicBool::new(false)
}

#[tokio::test]
async fn an_oversized_range_shrinks_instead_of_producing_a_clipped_prompt() {
    let messages = long_transcript(4000);
    let completer = RecordingCompleter::new();
    let cancel = not_cancelled();

    let document = generate_handoff_at_cutoff(&messages, messages.len(), TEST_WINDOW, None, &completer, &cancel)
        .await
        .expect("the range shrinks and the document is written");

    assert!(
        document.cutoff < messages.len(),
        "the covered range must shrink below the requested end"
    );
    assert!(document.cutoff > 0, "some prefix still has to be covered");
    let prompt = completer.prompt();
    assert!(prompt.len() <= MAX_PROMPT_BYTES, "the prompt fits the cap");
    assert!(
        prompt.contains("user 0 "),
        "the shrunk range still starts at the beginning"
    );
    assert!(
        !prompt.contains(&format!("user {} ", document.cutoff)),
        "the shrunk range ends where the document says it does"
    );
}

#[tokio::test]
async fn a_prior_document_too_large_for_the_cap_is_refused_not_truncated() {
    let messages = long_transcript(4);
    let completer = RecordingCompleter::new();
    let cancel = not_cancelled();
    let prior = "p".repeat(MAX_PROMPT_BYTES + 1);

    let error = generate_handoff_at_cutoff(
        &messages,
        messages.len(),
        TEST_WINDOW,
        Some(&prior),
        &completer,
        &cancel,
    )
    .await
    .expect_err("no range fits beside an oversized prior document");

    assert!(
        matches!(error, HandoffError::CoveredRangeTooLarge { budget } if budget == MAX_PROMPT_BYTES),
        "expected CoveredRangeTooLarge, got {error}"
    );
    assert!(
        completer.prompt().is_empty(),
        "a refused prompt must not reach the completer"
    );
}

#[tokio::test]
async fn the_prior_document_is_carried_into_the_prompt() {
    let messages = long_transcript(6);
    let completer = RecordingCompleter::new();
    let cancel = not_cancelled();
    let prior = "## Goal\nkeep the migration green\n\n## Constraints\nno schema changes\n";

    generate_handoff_at_cutoff(&messages, messages.len(), TEST_WINDOW, Some(prior), &completer, &cancel)
        .await
        .expect("handoff generation");

    let prompt = completer.prompt();
    assert!(prompt.contains("keep the migration green"), "the prior goal travels");
    assert!(prompt.contains("no schema changes"), "the prior constraints travel");
    assert!(
        prompt.contains("Your earlier handoff document"),
        "the prior text is introduced as the earlier document"
    );
}

#[tokio::test]
async fn a_zero_cutoff_covers_nothing() {
    let messages = long_transcript(4);
    let completer = RecordingCompleter::new();
    let cancel = not_cancelled();

    let error = generate_handoff_at_cutoff(&messages, 0, TEST_WINDOW, None, &completer, &cancel)
        .await
        .expect_err("a zero cutoff covers nothing");

    assert!(matches!(error, HandoffError::NothingToCover), "got {error}");
}

#[tokio::test]
async fn a_cutoff_past_the_transcript_is_clamped() {
    let messages = long_transcript(4);
    let completer = RecordingCompleter::new();
    let cancel = not_cancelled();

    let document = generate_handoff_at_cutoff(&messages, messages.len() + 99, TEST_WINDOW, None, &completer, &cancel)
        .await
        .expect("an oversized cutoff clamps to the transcript");

    assert!(document.cutoff <= messages.len());
    assert!(!document.document.is_empty());
}

#[tokio::test]
async fn cancellation_stops_the_generation_before_it_is_committed() {
    struct Pending;
    #[async_trait::async_trait]
    impl HandoffCompleter for Pending {
        async fn complete(&self, _request: CompletionRequest) -> Result<String> {
            futures::future::pending().await
        }
    }

    let messages = long_transcript(4);
    let cancel = AtomicBool::new(true);

    let error = generate_handoff_at_cutoff(&messages, messages.len(), TEST_WINDOW, None, &Pending, &cancel)
        .await
        .expect_err("a cancelled generation reports cancellation");

    assert!(matches!(error, HandoffError::Cancelled), "got {error}");
}

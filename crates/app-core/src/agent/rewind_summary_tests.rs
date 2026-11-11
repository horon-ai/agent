//! Rewind branch summary: the report is written for the discarded branch, the
//! rewind itself never depends on it, and undo takes it back with the branch.

use super::*;
use crate::compaction::rewind_summary::RewindSummaryCompleter;
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

struct SummaryProvider;

#[async_trait]
impl Provider for SummaryProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("the rewind summary tests never send a request")
    }

    fn name(&self) -> &str {
        "rewind-summary-test"
    }

    fn model(&self) -> String {
        "rewind-summary-test-model".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// Answers with a fixed report, fails, or records the request it was handed.
struct ScriptedSummaryCompleter {
    answer: std::result::Result<String, anyhow::Error>,
    seen: Mutex<Vec<crate::aux_completion::CompletionRequest>>,
}

impl ScriptedSummaryCompleter {
    fn answering(answer: &str) -> Self {
        Self {
            answer: Ok(answer.to_string()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn failing(detail: &str) -> Self {
        Self {
            answer: Err(anyhow::anyhow!("{detail}")),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|request| serde_json::to_value(request).expect("a request serialises"))
            .collect()
    }
}

#[async_trait]
impl RewindSummaryCompleter for ScriptedSummaryCompleter {
    async fn complete(&self, request: crate::aux_completion::CompletionRequest) -> anyhow::Result<String> {
        self.seen.lock().unwrap().push(request);
        match &self.answer {
            Ok(text) => Ok(text.clone()),
            Err(error) => Err(anyhow::anyhow!("{error}")),
        }
    }
}

fn text_block(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }]
}

/// An agent with four visible messages, saved so a rewind has a durable head.
///
/// The caller owns the sandbox: taking the process lock twice in one test would
/// deadlock, so a test that wants two agents reuses one sandbox.
async fn summary_agent() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(SummaryProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(Role::User, text_block("try the parser rewrite"));
    agent.add_message(Role::Assistant, text_block("rewriting the parser now"));
    agent.add_message(Role::User, text_block("that broke the lexer"));
    agent.add_message(Role::Assistant, text_block("the lexer now fails on quotes"));
    agent.session.save().unwrap();
    agent
}

/// One agent inside its own sandbox.
async fn summary_fixture() -> (crate::auth::test_sandbox::AuthTestSandbox, Agent) {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("test sandbox");
    let agent = summary_agent().await;
    (sandbox, agent)
}

const REPORT: &str = "\
## What was tried
Rewrote the parser to keep the lexer untouched.

## What failed and why
The lexer still tokenises quotes, so the rewrite could not reach a passing build.
";

/// The stored texts of the session, oldest first.
fn stored_texts(agent: &Agent) -> Vec<String> {
    agent
        .session
        .history_messages()
        .unwrap()
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn visible_count(agent: &Agent) -> usize {
    agent.session.history_snapshot().unwrap().rewind_target_count()
}

#[tokio::test]
async fn rewind_summary_is_appended_to_the_kept_branch() {
    let (_sandbox, mut agent) = summary_fixture().await;
    let completer = ScriptedSummaryCompleter::answering(REPORT);

    let attempt = agent
        .rewind_to_message_and_summarise(2, true, true, 1500, &completer)
        .expect("the rewind succeeds");

    assert_eq!(attempt.removed, 2, "the rewind still removes the discarded tail");
    assert!(
        matches!(attempt.outcome, RewindSummaryOutcome::Written { .. }),
        "a successful completion writes a report: {:?}",
        attempt.outcome
    );
    assert_eq!(visible_count(&agent), 2, "the summary is not a visible message");
    let texts = stored_texts(&agent);
    assert_eq!(texts.len(), 3, "kept branch plus one hidden summary message: {texts:?}");
    let summary = texts.last().unwrap();
    assert!(summary.starts_with("<system-reminder>"), "{summary}");
    assert!(summary.contains("## What failed and why"), "{summary}");
    assert!(
        summary.contains("could not reach a passing build"),
        "the report body survives verbatim: {summary}"
    );
    let stored = agent.session.history_messages().unwrap();
    assert_eq!(
        stored.last().unwrap().display_role,
        Some(StoredDisplayRole::System),
        "the summary is hidden from the rendered transcript"
    );
}

#[tokio::test]
async fn rewind_summary_absent_flag_matches_a_plain_rewind() {
    let (_sandbox, mut plain) = summary_fixture().await;
    let mut flagged = summary_agent().await;
    let completer = ScriptedSummaryCompleter::answering(REPORT);

    let plain_removed = plain.rewind_to_message(2).expect("the plain rewind succeeds");
    let attempt = flagged
        .rewind_to_message_and_summarise(2, false, true, 1500, &completer)
        .expect("the flagged rewind succeeds");

    assert_eq!(attempt.outcome, RewindSummaryOutcome::NotRequested);
    assert_eq!(attempt.removed, plain_removed);
    assert_eq!(
        stored_texts(&flagged),
        stored_texts(&plain),
        "a rewind without the flag stores exactly what it always did"
    );
    assert_eq!(visible_count(&flagged), visible_count(&plain));
    assert!(completer.requests().is_empty(), "no completion is attempted");
}

#[tokio::test]
async fn rewind_summary_disabled_setting_still_rewinds() {
    let (_sandbox, mut agent) = summary_fixture().await;
    let completer = ScriptedSummaryCompleter::answering(REPORT);

    let attempt = agent
        .rewind_to_message_and_summarise(2, true, false, 1500, &completer)
        .expect("the rewind succeeds");

    assert_eq!(attempt.outcome, RewindSummaryOutcome::Disabled);
    assert_eq!(attempt.removed, 2);
    assert_eq!(visible_count(&agent), 2);
    assert_eq!(stored_texts(&agent).len(), 2, "nothing is appended");
    assert!(completer.requests().is_empty(), "the setting gates the network call");
}

#[tokio::test]
async fn rewind_summary_undo_removes_the_report_and_restores_messages() {
    let (_sandbox, mut agent) = summary_fixture().await;
    let before = stored_texts(&agent);
    let completer = ScriptedSummaryCompleter::answering(REPORT);

    agent
        .rewind_to_message_and_summarise(2, true, true, 1500, &completer)
        .expect("the rewind succeeds");
    assert_eq!(visible_count(&agent), 2);

    let restored = agent.undo_rewind().expect("the undo succeeds");

    assert_eq!(restored, 2, "both discarded messages come back");
    assert_eq!(
        stored_texts(&agent),
        before,
        "the undo drops the summary along with the rewind"
    );
    assert!(agent.rewind_undo_snapshot.is_none());
}

#[tokio::test]
async fn rewind_summary_provider_error_still_rewinds_and_reports() {
    let (_sandbox, mut agent) = summary_fixture().await;
    let completer = ScriptedSummaryCompleter::failing("quota exhausted");

    let attempt = agent
        .rewind_to_message_and_summarise(2, true, true, 1500, &completer)
        .expect("a failed summary does not fail the rewind");

    assert_eq!(attempt.removed, 2, "the rewind still happened");
    match &attempt.outcome {
        RewindSummaryOutcome::ProviderError(detail) => {
            assert!(detail.contains("quota exhausted"), "{detail}");
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
    assert_eq!(visible_count(&agent), 2);
    assert_eq!(stored_texts(&agent).len(), 2, "a failure appends nothing");

    agent.record_rewind_summary_outcome(&attempt.outcome);
    let detail = agent.last_status_detail().expect("the failure is reported");
    assert!(detail.contains("quota exhausted"), "{detail}");
}

#[tokio::test]
async fn rewind_summary_uses_the_configured_token_allowance() {
    let (_sandbox, mut agent) = summary_fixture().await;
    let completer = ScriptedSummaryCompleter::answering(REPORT);

    agent
        .rewind_to_message_and_summarise(2, true, true, 700, &completer)
        .expect("the rewind succeeds");

    let requests = completer.requests();
    assert_eq!(requests.len(), 1, "one completion per rewind");
    assert_eq!(requests[0]["max_tokens"], 700);
    assert!(
        requests[0]["system"].as_str().unwrap().contains("abandoned branch"),
        "the summary asks for a report, not a handoff: {}",
        requests[0]["system"]
    );
}

#[tokio::test]
async fn rewind_summary_prompt_carries_the_discarded_branch_only() {
    let (_sandbox, mut agent) = summary_fixture().await;
    let completer = ScriptedSummaryCompleter::answering(REPORT);

    agent
        .rewind_to_message_and_summarise(2, true, true, 1500, &completer)
        .expect("the rewind succeeds");

    let prompt = completer.requests()[0]["prompt"].as_str().unwrap().to_string();
    assert!(
        prompt.contains("that broke the lexer") && prompt.contains("fails on quotes"),
        "the discarded tail is what gets summarised: {prompt}"
    );
    assert!(
        !prompt.contains("try the parser rewrite"),
        "the kept branch is not part of the report input: {prompt}"
    );
}

#[test]
fn rewind_summary_token_allowance_is_clamped() {
    use crate::compaction::rewind_summary::clamp_max_tokens;
    use crate::config::rewind_summary::{
        DEFAULT_REWIND_SUMMARY_TOKENS, MAX_REWIND_SUMMARY_TOKENS, MIN_REWIND_SUMMARY_TOKENS,
    };

    assert_eq!(clamp_max_tokens(0), DEFAULT_REWIND_SUMMARY_TOKENS as u32);
    assert_eq!(clamp_max_tokens(DEFAULT_REWIND_SUMMARY_TOKENS), 1500);
    assert_eq!(clamp_max_tokens(MIN_REWIND_SUMMARY_TOKENS), 1);
    assert_eq!(clamp_max_tokens(usize::MAX), MAX_REWIND_SUMMARY_TOKENS as u32);
}

#[test]
fn rewind_summary_config_defaults_are_documented() {
    let compaction = crate::config::CompactionConfig::default();
    assert!(
        !compaction.rewind_summary,
        "the feature is opt-in twice: config and request"
    );
    assert_eq!(
        compaction.rewind_summary_tokens,
        crate::config::rewind_summary::DEFAULT_REWIND_SUMMARY_TOKENS
    );
}

/// A checkpoint recorded through the turn-end path, as a settled turn leaves it.
fn record_checkpoint(agent: &mut Agent) {
    crate::tool::checkpoint::request_checkpoint(agent.session_id());
    agent.apply_pending_checkpoint_rewind();
}

#[tokio::test]
async fn checkpoint_marker_is_hidden_and_is_not_a_rewind_target() {
    let (_sandbox, mut agent) = summary_fixture().await;
    let visible_before = visible_count(&agent);

    record_checkpoint(&mut agent);

    assert_eq!(
        visible_count(&agent),
        visible_before,
        "the record is not a rewind target"
    );
    let stored = agent.session.history_messages().unwrap();
    let marker = stored.last().expect("the checkpoint is stored");
    assert_eq!(marker.display_role, Some(StoredDisplayRole::System));
    assert!(
        crate::tool::checkpoint::is_marker(marker),
        "the stored record is what a later rewind looks for"
    );
    assert_eq!(
        crate::tool::checkpoint::newest_marker_index(&stored),
        Some(stored.len() - 1)
    );
}

#[tokio::test]
async fn checkpoint_reaches_the_provider_as_background() {
    let (_sandbox, mut agent) = summary_fixture().await;
    record_checkpoint(&mut agent);

    let (messages, _) = agent.messages_for_provider();
    let marker = messages.last().expect("the marker is sent with the next request");
    assert!(
        marker.content.iter().any(|block| matches!(
            block,
            ContentBlock::Text { text, .. } if text.starts_with("<system-reminder>")
        )),
        "the model reads the marker as a reminder, not as a user request: {marker:?}"
    );
}

#[tokio::test]
async fn rewind_to_a_checkpoint_folds_the_branch_into_the_report() {
    let (_sandbox, mut agent) = summary_fixture().await;
    // Mark after the second message, then add a branch to be folded away.
    record_checkpoint(&mut agent);
    agent.add_message(Role::User, text_block("try the lexer generator instead"));
    agent.add_message(Role::Assistant, text_block("the generator needs a new grammar"));
    agent.save_session().unwrap();
    let before = visible_count(&agent);
    assert_eq!(before, 6, "four messages, the marker, and the two-message branch");

    crate::tool::checkpoint::request_rewind(
        &agent.session_id(),
        "the generator needs a grammar the parser lacks".to_string(),
    );
    agent.apply_pending_checkpoint_rewind();

    assert_eq!(visible_count(&agent), 4, "the branch after the checkpoint is gone");
    let texts = stored_texts(&agent);
    assert!(
        !texts.iter().any(|text| text.contains("lexer generator")),
        "the discarded branch left no message behind: {texts:?}"
    );
    let report = texts.last().expect("the report is the last record");
    assert!(
        report.starts_with("<system-reminder>") && report.contains("the parser lacks"),
        "the report replaces the branch as background: {report}"
    );
    let stored = agent.session.history_messages().unwrap();
    assert_eq!(stored.last().unwrap().display_role, Some(StoredDisplayRole::System));
    assert_eq!(
        crate::tool::checkpoint::newest_marker_index(&stored),
        None,
        "the rewind consumes the checkpoint it targeted"
    );

    // Acceptance 2: the next request carries the report and nothing the branch
    // between the two tool calls added.
    let (messages, _) = agent.messages_for_provider();
    let sent = messages.iter().map(message_text).collect::<Vec<_>>().join("\n");
    assert!(
        sent.contains("the parser lacks"),
        "the report reaches the provider: {sent}"
    );
    assert!(
        !sent.contains("lexer generator") && !sent.contains("needs a new grammar"),
        "the folded branch does not: {sent}"
    );
}

#[tokio::test]
async fn undoing_a_checkpoint_rewind_restores_the_branch_and_drops_the_report() {
    let (_sandbox, mut agent) = summary_fixture().await;
    record_checkpoint(&mut agent);
    agent.add_message(Role::User, text_block("try the lexer generator instead"));
    agent.add_message(Role::Assistant, text_block("the generator needs a new grammar"));
    agent.save_session().unwrap();
    let before = stored_texts(&agent);

    crate::tool::checkpoint::request_rewind(
        &agent.session_id(),
        "the generator needs a grammar the parser lacks".to_string(),
    );
    agent.apply_pending_checkpoint_rewind();
    assert_eq!(visible_count(&agent), 4);

    // Acceptance 5: undo restores the removed history, marker included, and the
    // report goes with the rewind that wrote it.
    let restored = agent.undo_rewind().expect("the undo succeeds");
    assert!(restored >= 2, "the discarded branch comes back: {restored}");
    assert_eq!(
        stored_texts(&agent),
        before,
        "the restored transcript is the one recorded before the rewind"
    );
    let stored = agent.session.history_messages().unwrap();
    assert!(
        crate::tool::checkpoint::newest_marker_index(&stored).is_some(),
        "the checkpoint marker is back in the transcript"
    );
    assert!(
        !stored_texts(&agent)
            .iter()
            .any(|text| text.contains("the parser lacks")),
        "the report is gone with the rewind"
    );
}

#[tokio::test]
async fn rewind_without_a_committed_checkpoint_is_dropped() {
    let (_sandbox, mut agent) = summary_fixture().await;
    let before = stored_texts(&agent);

    crate::tool::checkpoint::request_rewind(&agent.session_id(), "nothing to fold".to_string());
    agent.apply_pending_checkpoint_rewind();

    assert_eq!(stored_texts(&agent), before, "the transcript is untouched");
    assert_eq!(visible_count(&agent), 4);
    assert_eq!(
        crate::tool::checkpoint::take_pending(&agent.session_id()),
        None,
        "the request is applied or dropped exactly once"
    );
}

//! Structured questions for a mid-task decision.
//!
//! The daemon is headless: no client surface is guaranteed to be attached, and
//! a tool that waited for one would hang the turn. `ask` therefore resolves the
//! request itself unless a connection that advertised `supports_ask_requests`
//! is attached to this session:
//!
//! - no such client: validate the request, then answer under `[tools]
//!   ask_policy`. `recommended` (the default) answers every question with the
//!   model's own recommendation, or the first option when the model expressed
//!   none, and says in the tool text that the choice was automatic; `error`
//!   refuses the call so the model knows nobody can answer and continues on its
//!   own judgment. Nothing waits.
//! - a client is attached: emit `ask_requested` and wait for `ask_response`,
//!   bounded by [`CLIENT_ANSWER_TIMEOUT`]. A timeout or a disconnect (the
//!   channel drops) falls back to the policy above and says so, so a person who
//!   walked away costs the turn a bounded delay rather than a hang.

use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Result, bail};
use async_trait::async_trait;
use horon_agent_tool_types::{AskAnswer, AskOption, AskPolicy, AskQuestion, AskRequest, AskResponse};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

use super::ask_bridge::{self, AskPrompt};

/// How long a client that can answer gets before the tool decides on its own.
/// A person may need a moment; a turn must not park on one indefinitely.
pub(crate) const CLIENT_ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) const MAX_QUESTIONS: usize = 4;
pub(crate) const MIN_OPTIONS: usize = 2;
pub(crate) const MAX_OPTIONS: usize = 5;

pub(crate) struct AskTool {
    policy: Option<AskPolicy>,
    wait: Duration,
}

impl AskTool {
    pub(crate) fn new() -> Self {
        Self {
            policy: None,
            wait: CLIENT_ANSWER_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_policy(policy: AskPolicy) -> Self {
        Self {
            policy: Some(policy),
            wait: CLIENT_ANSWER_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    fn policy(&self) -> AskPolicy {
        self.policy.unwrap_or(crate::config::config().tools.ask_policy)
    }
}

#[derive(Deserialize)]
struct Input {
    questions: Vec<QuestionInput>,
}

#[derive(Deserialize)]
struct QuestionInput {
    #[serde(default)]
    id: Option<String>,
    question: String,
    #[serde(default)]
    header: Option<String>,
    options: Vec<OptionInput>,
    #[serde(default)]
    recommended: Option<usize>,
    #[serde(default)]
    multi: bool,
}

#[derive(Deserialize)]
struct OptionInput {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

impl Input {
    fn into_request(self) -> Result<AskRequest> {
        if self.questions.is_empty() {
            bail!("ask needs at least 1 question, got 0");
        }
        if self.questions.len() > MAX_QUESTIONS {
            bail!(
                "ask accepts at most {MAX_QUESTIONS} questions, got {}",
                self.questions.len()
            );
        }

        let mut questions = Vec::with_capacity(self.questions.len());
        let mut seen = std::collections::HashSet::new();
        for (index, question) in self.questions.into_iter().enumerate() {
            let id = question
                .id
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("q{}", index + 1));
            if !seen.insert(id.clone()) {
                bail!("ask question ids must be unique, but {id:?} repeats");
            }
            let option_count = question.options.len();
            if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&option_count) {
                bail!("question {id:?} needs between {MIN_OPTIONS} and {MAX_OPTIONS} options, got {option_count}");
            }
            if let Some(recommended) = question.recommended
                && recommended >= option_count
            {
                bail!("question {id:?} recommends option {recommended}, but it has {option_count} options");
            }
            if question.question.trim().is_empty() {
                bail!("question {id:?} has empty text");
            }
            for option in &question.options {
                if option.label.trim().is_empty() {
                    bail!("question {id:?} has an option with an empty label");
                }
            }
            questions.push(AskQuestion {
                id: Some(id),
                question: question.question,
                header: question.header,
                options: question
                    .options
                    .into_iter()
                    .map(|option| AskOption {
                        label: option.label,
                        description: option.description,
                    })
                    .collect(),
                recommended: question.recommended,
                multi: question.multi,
            });
        }

        Ok(AskRequest { questions })
    }
}

/// Every question answered with the model's recommendation, or its first
/// option when the model expressed none.
fn recommended_answers(request: &AskRequest) -> Vec<AskAnswer> {
    request
        .questions
        .iter()
        .map(|question| {
            let index = question.recommended.unwrap_or(0);
            AskAnswer {
                question_id: question.id.clone().unwrap_or_else(|| "q1".to_string()),
                question: question.question.clone(),
                selected: vec![question.options[index].label.clone()],
                auto_resolved: true,
            }
        })
        .collect()
}

fn render(headline: String, answers: &[AskAnswer], note: &str) -> String {
    let mut lines = Vec::with_capacity(answers.len() + 3);
    lines.push(headline);
    for answer in answers {
        lines.push(format!(
            "- {}: {} -> {}",
            answer.question_id,
            answer.question,
            answer.selected.join(", ")
        ));
    }
    lines.push(note.to_string());
    lines.join("\n")
}

fn output(text: String, answers: Vec<AskAnswer>, policy: AskPolicy) -> Result<ToolOutput> {
    let response = AskResponse { answers, policy };
    let metadata = serde_json::to_value(&response)?;
    Ok(ToolOutput::new(text).with_title("ask").with_metadata(metadata))
}

/// Why a client that said it could answer did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoAnswer {
    TimedOut,
    Disconnected,
    Unusable,
}

fn resolve(request: &AskRequest, policy: AskPolicy) -> Result<ToolOutput> {
    let answers = recommended_answers(request);
    let text = render(
        format!(
            "Resolved {} question(s) automatically: no interactive client is attached to answer them.",
            answers.len()
        ),
        &answers,
        "Note: each choice above is the recommended option, selected automatically rather than by a person.",
    );
    output(text, answers, policy)
}

fn resolve_after_wait(request: &AskRequest, policy: AskPolicy, reason: NoAnswer, wait: Duration) -> Result<ToolOutput> {
    let answers = recommended_answers(request);
    let cause = match reason {
        NoAnswer::TimedOut => format!("the attached client did not answer within {}s", wait.as_secs()),
        NoAnswer::Disconnected => "the answering client disconnected before replying".to_string(),
        NoAnswer::Unusable => "the attached client sent no usable choice".to_string(),
    };
    let text = render(
        format!("Resolved {} question(s) automatically: {cause}.", answers.len()),
        &answers,
        "Note: each choice above is the recommended option, selected automatically rather than by a person.",
    );
    output(text, answers, policy)
}

/// The answers the client actually chose, with any question it left unanswerable
/// falling back to its recommendation.
///
/// A client is not trusted to echo the option set it was shown: a label that is
/// not one of the question's options is dropped, and a question left with no
/// usable label is answered from the recommendation and marked automatic. When
/// no question has a usable choice the reply is not an answer at all.
fn merge_client_answers(request: &AskRequest, answers: Vec<AskAnswer>) -> Option<Vec<AskAnswer>> {
    let mut any_selected = false;
    let merged: Vec<AskAnswer> = request
        .questions
        .iter()
        .map(|question| {
            let question_id = question.id.clone().unwrap_or_else(|| "q1".to_string());
            let selected: Vec<String> = answers
                .iter()
                .filter(|answer| answer.question_id == question_id)
                .flat_map(|answer| answer.selected.iter())
                .filter(|label| question.options.iter().any(|option| &option.label == *label))
                .cloned()
                .collect();
            if selected.is_empty() {
                let index = question.recommended.unwrap_or(0);
                return AskAnswer {
                    question_id,
                    question: question.question.clone(),
                    selected: vec![question.options[index].label.clone()],
                    auto_resolved: true,
                };
            }
            any_selected = true;
            AskAnswer {
                question_id,
                question: question.question.clone(),
                selected,
                auto_resolved: false,
            }
        })
        .collect();
    any_selected.then_some(merged)
}

fn answered(request: &AskRequest, answers: Vec<AskAnswer>, policy: AskPolicy) -> Result<ToolOutput> {
    let automatic = answers.iter().filter(|answer| answer.auto_resolved).count();
    let note = if automatic == 0 {
        "Every choice above was selected by the person answering.".to_string()
    } else {
        format!(
            "{automatic} question(s) had no usable choice from the client and were answered with the recommended option."
        )
    };
    let text = render(
        format!(
            "Answered {} question(s) from the attached client.",
            request.questions.len()
        ),
        &answers,
        &note,
    );
    output(text, answers, policy)
}

/// What the attached client did with the prompt.
#[derive(Debug)]
enum ClientReply {
    Answered(Vec<AskAnswer>),
    None(NoAnswer),
}

#[async_trait]
impl Tool for AskTool {
    fn name(&self) -> &str {
        "ask"
    }

    fn description(&self) -> &str {
        // Always-on prompt cost. The timeout and fallback behavior is host
        // behavior the result reports, not something the model plans around.
        "Ask the user up to four questions with concrete options when only they can decide."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_QUESTIONS,
                    "description": "One to four questions to settle.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "id": {"type": "string", "description": "Optional stable id; derived from the position when omitted. Must be unique."},
                            "question": {"type": "string", "description": "The question to answer."},
                            "header": {"type": "string", "description": "Optional short heading."},
                            "options": {
                                "type": "array",
                                "minItems": MIN_OPTIONS,
                                "maxItems": MAX_OPTIONS,
                                "description": "Two to five concrete options.",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "label": {"type": "string"},
                                        "description": {"type": "string"}
                                    },
                                    "required": ["label"]
                                }
                            },
                            "recommended": {"type": "integer", "minimum": 0, "description": "Index into options that you recommend."},
                            "multi": {"type": "boolean", "description": "Whether more than one option may be selected."}
                        },
                        "required": ["question", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let input: Input = serde_json::from_value(input)?;
        let request = input.into_request()?;
        let policy = self.policy();
        if let Some(tx) = ask_bridge::sender_for(&ctx.session_id) {
            let _awaiting = ask_bridge::begin_awaiting_answer(&ctx.session_id);
            match self.wait_for_client(&request, tx).await {
                ClientReply::Answered(answers) => return answered(&request, answers, policy),
                ClientReply::None(reason) => {
                    if policy == AskPolicy::Error {
                        bail!(
                            "This session cannot answer questions: the client that could answer did not ({}s limit) and [tools] ask_policy = \"error\". Decide from the available context and continue without an answer.",
                            self.wait.as_secs()
                        );
                    }
                    return resolve_after_wait(&request, policy, reason, self.wait);
                }
            }
        }
        match policy {
            AskPolicy::Recommended => resolve(&request, policy),
            AskPolicy::Error => bail!(
                "This session cannot answer questions: no interactive client is attached and [tools] ask_policy = \"error\". Decide from the available context and continue without an answer."
            ),
        }
    }
}

impl AskTool {
    /// Emit the questions to the attached client and wait, bounded by `wait`.
    ///
    /// A dropped answer channel is a disconnect: the turn resumes on the
    /// recommended option instead of waiting for a person who is gone.
    async fn wait_for_client(
        &self,
        request: &AskRequest,
        tx: tokio::sync::mpsc::UnboundedSender<AskPrompt>,
    ) -> ClientReply {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let prompt = AskPrompt {
            ask_id: crate::id::new_id("ask"),
            questions: request.questions.clone(),
            response_tx,
        };
        if tx.send(prompt).is_err() {
            return ClientReply::None(NoAnswer::Disconnected);
        }
        match tokio::time::timeout(self.wait, response_rx).await {
            Ok(Ok(answers)) => match merge_client_answers(request, answers) {
                Some(merged) => ClientReply::Answered(merged),
                None => ClientReply::None(NoAnswer::Unusable),
            },
            Ok(Err(_)) => ClientReply::None(NoAnswer::Disconnected),
            Err(_) => ClientReply::None(NoAnswer::TimedOut),
        }
    }
}

#[cfg(test)]
#[path = "ask_tests.rs"]
mod ask_tests;

#[cfg(test)]
#[path = "ask_client_tests.rs"]
mod ask_client_tests;

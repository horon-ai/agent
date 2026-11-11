//! Typed judgments: the wire contract a judge call states, and nothing more.
//!
//! A judgment is a model call whose answer is typed rather than free text: a
//! probability for a yes/no question, one label for a choice question, or one
//! level for an ordered score. The point of the contract is that a caller never
//! parses prose: it builds a [`JudgeRequest`], and [`JudgeRequest::parse_answers`]
//! either returns every answer with its type checked or a [`JudgeError`].
//!
//! This module is pure. It builds the prompt, states the response shape and
//! validates the answer set, so the transport (which model, which endpoint,
//! which credential) stays in the layer that owns configuration and can be
//! tested with a mock.
//!
//! ## Limits
//!
//! - `1..=`[`MAX_JUDGE_QUESTIONS`] questions per request.
//! - `state` is text, an object or an array, at most [`MAX_STATE_BYTES`] bytes.
//! - `instructions` is at most [`MAX_INSTRUCTIONS_BYTES`] bytes.
//! - A choice question has `2..=`[`MAX_CHOICE_OPTIONS`] options; a score
//!   question has `2..=`[`MAX_SCORE_LEVELS`] ordered level descriptions.
//!
//! ## Answer strictness
//!
//! The reply is one JSON object whose `answers` array carries exactly one entry
//! per question, in any order, each naming its question `id` and exactly the one
//! value field its question type defines. A reply is rejected when it is not
//! JSON, when an answer is missing, repeated or unknown, when it carries a field
//! the question type does not define, when a probability is not a finite number
//! in `0..=1`, when a label is not one of the question's labels exactly, or when
//! a level is not an integer in `1..=levels`. Rejecting rather than repairing is
//! deliberate: a consumer that thresholds a probability must not have that
//! probability invented by a parser.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::fmt;

/// Error prefix for a request built outside the contract.
pub const JUDGE_INPUT: &str = "JUDGE_INPUT";

/// Error prefix for a reply that does not satisfy the contract.
pub const JUDGE_ANSWER: &str = "JUDGE_ANSWER";

/// Largest number of questions one request may ask.
pub const MAX_JUDGE_QUESTIONS: usize = 64;

/// Largest `state` payload, in bytes.
pub const MAX_STATE_BYTES: usize = 48 * 1024;

/// Largest `instructions` text, in bytes.
pub const MAX_INSTRUCTIONS_BYTES: usize = 4 * 1024;

/// Largest number of options in one choice question.
pub const MAX_CHOICE_OPTIONS: usize = 64;

/// Largest number of levels in one score question.
pub const MAX_SCORE_LEVELS: usize = 64;

/// One question a judgment call asks.
///
/// The variant is the answer type: a `Bool` question is answered with a
/// probability, never with `true`/`false`, because the useful signal is how
/// likely the answer is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum JudgeQuestion {
    /// A yes/no question answered with a probability in `0..=1`.
    Bool {
        instructions: String,
        /// The labels the probability is read against, as `(true_label,
        /// false_label)`. Kept so a consumer can name what `0.9` means.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        labels: Option<(String, String)>,
    },
    /// A question answered with exactly one of a fixed set of labels.
    Choice {
        instructions: String,
        /// `(label, rubric)` pairs. The rubric is what makes a label usable by
        /// a model and is not echoed back in the answer.
        options: Vec<JudgeChoiceOption>,
    },
    /// A question answered with one ordered level, `1` being the first entry.
    Score {
        instructions: String,
        /// Ordered level descriptions, lowest first.
        levels: Vec<String>,
    },
}

/// One label of a [`JudgeQuestion::Choice`], with the rubric that defines it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeChoiceOption {
    pub label: String,
    pub rubric: String,
}

impl JudgeChoiceOption {
    pub fn new(label: impl Into<String>, rubric: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            rubric: rubric.into(),
        }
    }
}

impl JudgeQuestion {
    pub fn boolean(instructions: impl Into<String>) -> Self {
        Self::Bool {
            instructions: instructions.into(),
            labels: None,
        }
    }

    pub fn choice(instructions: impl Into<String>, options: Vec<JudgeChoiceOption>) -> Self {
        Self::Choice {
            instructions: instructions.into(),
            options,
        }
    }

    pub fn score(instructions: impl Into<String>, levels: Vec<String>) -> Self {
        Self::Score {
            instructions: instructions.into(),
            levels,
        }
    }

    pub fn instructions(&self) -> &str {
        match self {
            Self::Bool { instructions, .. } | Self::Choice { instructions, .. } | Self::Score { instructions, .. } => {
                instructions
            }
        }
    }

    /// The `type` string the wire format uses for this question.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bool { .. } => "bool",
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
        }
    }
}

/// One question plus the caller's own identifier, as a request carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeItem {
    /// Caller-chosen identifier, echoed back in the answer. Never shown to the
    /// model as anything but a key.
    pub id: String,
    pub question: JudgeQuestion,
}

impl JudgeItem {
    pub fn new(id: impl Into<String>, question: JudgeQuestion) -> Self {
        Self {
            id: id.into(),
            question,
        }
    }

    fn validate(&self) -> Result<(), JudgeError> {
        if self.id.is_empty() || self.id.len() > 64 {
            return Err(input_error(format!("question id '{}' must be 1 to 64 bytes", self.id)));
        }
        let instructions = self.question.instructions();
        if instructions.trim().is_empty() {
            return Err(input_error(format!("question '{}' has empty instructions", self.id)));
        }
        if instructions.len() > MAX_INSTRUCTIONS_BYTES {
            return Err(input_error(format!(
                "question '{}' instructions are {} bytes; the contract caps them at {MAX_INSTRUCTIONS_BYTES}",
                self.id,
                instructions.len()
            )));
        }
        match &self.question {
            JudgeQuestion::Bool { labels, .. } => {
                if let Some((yes, no)) = labels
                    && (yes.trim().is_empty() || no.trim().is_empty())
                {
                    return Err(input_error(format!("question '{}' names an empty bool label", self.id)));
                }
            }
            JudgeQuestion::Choice { options, .. } => {
                if !(2..=MAX_CHOICE_OPTIONS).contains(&options.len()) {
                    return Err(input_error(format!(
                        "question '{}' has {} options; a choice needs 2 to {MAX_CHOICE_OPTIONS}",
                        self.id,
                        options.len()
                    )));
                }
                let mut seen: Vec<&str> = Vec::with_capacity(options.len());
                for option in options {
                    if option.label.trim().is_empty() {
                        return Err(input_error(format!("question '{}' has an empty label", self.id)));
                    }
                    if seen.contains(&option.label.as_str()) {
                        return Err(input_error(format!(
                            "question '{}' repeats the label '{}'",
                            self.id, option.label
                        )));
                    }
                    seen.push(&option.label);
                }
            }
            JudgeQuestion::Score { levels, .. } => {
                if !(2..=MAX_SCORE_LEVELS).contains(&levels.len()) {
                    return Err(input_error(format!(
                        "question '{}' has {} levels; a score needs 2 to {MAX_SCORE_LEVELS}",
                        self.id,
                        levels.len()
                    )));
                }
                if levels.iter().any(|level| level.trim().is_empty()) {
                    return Err(input_error(format!(
                        "question '{}' has an empty level description",
                        self.id
                    )));
                }
            }
        }
        Ok(())
    }
}

/// One judgment call: a state to read and the questions asked about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeRequest {
    /// Text, an object or an array. Serialized compactly into the prompt.
    pub state: Value,
    pub questions: Vec<JudgeItem>,
}

impl JudgeRequest {
    /// Build a request, refusing one that leaves the contract before any model
    /// call can spend money on it.
    pub fn new(state: Value, questions: Vec<JudgeItem>) -> Result<Self, JudgeError> {
        let request = Self { state, questions };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), JudgeError> {
        if !(self.state.is_string() || self.state.is_object() || self.state.is_array()) {
            return Err(input_error("state must be text, an object, or an array".to_string()));
        }
        if !(1..=MAX_JUDGE_QUESTIONS).contains(&self.questions.len()) {
            return Err(input_error(format!(
                "a judgment asks 1 to {MAX_JUDGE_QUESTIONS} questions, got {}",
                self.questions.len()
            )));
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.questions.len());
        for item in &self.questions {
            item.validate()?;
            if seen.contains(&item.id.as_str()) {
                return Err(input_error(format!("question id '{}' appears twice", item.id)));
            }
            seen.push(&item.id);
        }
        Ok(())
    }

    /// The state as the prompt renders it: a string verbatim, anything else as
    /// compact JSON.
    pub fn state_text(&self) -> Result<String, JudgeError> {
        let text = match &self.state {
            Value::String(text) => text.clone(),
            other => serde_json::to_string(other).map_err(|_| input_error("state could not be encoded".to_string()))?,
        };
        if text.len() > MAX_STATE_BYTES {
            return Err(input_error(format!(
                "state is {} bytes; the contract caps it at {MAX_STATE_BYTES}",
                text.len()
            )));
        }
        Ok(text)
    }

    /// The system text a judge call sends.
    pub const SYSTEM: &'static str = "You answer typed judgment questions. Reply with one JSON object and nothing else: \
{\"answers\":[{\"id\":\"<question id>\",\"probability\":<number>}]} for a bool question, \
{\"id\":\"<question id>\",\"label\":\"<one of the given labels>\"} for a choice question, \
{\"id\":\"<question id>\",\"level\":<integer>} for a score question. \
Every question is answered exactly once, with exactly its own value field and no others. \
A probability is your honest estimate that the answer is the yes side, from 0 to 1, and a 0.5 means you do not know. \
A level is the 1-based position of the level that fits best. Do not explain, hedge, or add fields.";

    /// The user text a judge call sends: the state, then every question with
    /// the exact labels or levels its answer may take.
    pub fn prompt(&self) -> Result<String, JudgeError> {
        let mut prompt = String::with_capacity(1024);
        prompt.push_str("## State\n");
        prompt.push_str(&self.state_text()?);
        prompt.push_str("\n\n## Questions\n");
        for item in &self.questions {
            prompt.push_str(&format!("\n### {}\n{}\n", item.id, item.question.instructions()));
            match &item.question {
                JudgeQuestion::Bool { labels, .. } => {
                    let (yes, no) = labels
                        .as_ref()
                        .map_or(("yes", "no"), |(yes, no)| (yes.as_str(), no.as_str()));
                    prompt.push_str(&format!(
                        "Answer with `probability`: your estimate that \"{yes}\" rather than \"{no}\".\n"
                    ));
                }
                JudgeQuestion::Choice { options, .. } => {
                    prompt.push_str("Answer with `label`, exactly one of:\n");
                    for option in options {
                        prompt.push_str(&format!("- `{}`: {}\n", option.label, option.rubric));
                    }
                }
                JudgeQuestion::Score { levels, .. } => {
                    prompt.push_str("Answer with `level`, the 1-based position of the best fit:\n");
                    for (index, level) in levels.iter().enumerate() {
                        prompt.push_str(&format!("{}. {}\n", index + 1, level));
                    }
                }
            }
        }
        Ok(prompt)
    }

    /// Parse and validate the reply against this exact question set.
    ///
    /// The returned answers follow the request order, so a caller indexes them
    /// positionally or by [`JudgeAnswers::get`].
    pub fn parse_answers(&self, reply: &str) -> Result<JudgeAnswers, JudgeError> {
        let value: Value =
            serde_json::from_str(reply.trim()).map_err(|_| malformed("the reply is not JSON".to_string()))?;
        let answers = value
            .get("answers")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("the reply has no `answers` array".to_string()))?;
        let mut by_id: std::collections::HashMap<String, &Value> =
            std::collections::HashMap::with_capacity(answers.len());
        for answer in answers {
            let id = answer
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("an answer has no string `id`".to_string()))?;
            if by_id.insert(id.to_string(), answer).is_some() {
                return Err(invalid(id, "answers the same question twice"));
            }
        }

        let mut parsed = Vec::with_capacity(self.questions.len());
        for item in &self.questions {
            let answer = by_id
                .remove(item.id.as_str())
                .ok_or_else(|| invalid(&item.id, "is not answered"))?;
            parsed.push(parse_one(&item.id, &item.question, answer)?);
        }
        if let Some(unknown) = by_id.keys().next() {
            return Err(invalid(unknown, "is not one of the requested questions"));
        }
        Ok(JudgeAnswers { answers: parsed })
    }
}

/// A typed answer to one question, carrying the id it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum JudgeAnswer {
    Bool { id: String, probability: f32 },
    Choice { id: String, label: String },
    Score { id: String, level: u32 },
}

impl JudgeAnswer {
    pub fn id(&self) -> &str {
        match self {
            Self::Bool { id, .. } | Self::Choice { id, .. } | Self::Score { id, .. } => id,
        }
    }
}

/// Every answer of one judgment, in request order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeAnswers {
    answers: Vec<JudgeAnswer>,
}

impl JudgeAnswers {
    pub fn iter(&self) -> impl Iterator<Item = &JudgeAnswer> {
        self.answers.iter()
    }

    pub fn len(&self) -> usize {
        self.answers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.answers.is_empty()
    }

    /// The answer whose id matches, if the request asked that question.
    pub fn get(&self, id: &str) -> Option<&JudgeAnswer> {
        self.answers.iter().find(|answer| answer.id() == id)
    }

    /// The probability of a bool question; `None` when the id is absent or is
    /// another answer type. A caller that thresholds this must handle `None`
    /// as "no answer", never as `0.0`.
    pub fn probability(&self, id: &str) -> Option<f32> {
        match self.get(id) {
            Some(JudgeAnswer::Bool { probability, .. }) => Some(*probability),
            _ => None,
        }
    }

    pub fn label(&self, id: &str) -> Option<&str> {
        match self.get(id) {
            Some(JudgeAnswer::Choice { label, .. }) => Some(label.as_str()),
            _ => None,
        }
    }

    pub fn level(&self, id: &str) -> Option<u32> {
        match self.get(id) {
            Some(JudgeAnswer::Score { level, .. }) => Some(*level),
            _ => None,
        }
    }
}

fn parse_one(id: &str, question: &JudgeQuestion, answer: &Value) -> Result<JudgeAnswer, JudgeError> {
    let object = answer.as_object().ok_or_else(|| invalid(id, "is not an object"))?;
    let expected = question.kind();
    if let Some(kind) = object.get("type").and_then(Value::as_str)
        && kind != expected
    {
        return Err(invalid(
            id,
            format!("declares type '{kind}' but the question asks for '{expected}'"),
        ));
    }
    let value_field = match question {
        JudgeQuestion::Bool { .. } => "probability",
        JudgeQuestion::Choice { .. } => "label",
        JudgeQuestion::Score { .. } => "level",
    };
    for key in object.keys() {
        if key != "id" && key != "type" && key.as_str() != value_field {
            return Err(invalid(id, format!("carries the unexpected field '{key}'")));
        }
    }
    match question {
        JudgeQuestion::Bool { .. } => {
            let probability = object
                .get(value_field)
                .and_then(Value::as_f64)
                .ok_or_else(|| invalid(id, "has no numeric `probability`"))?;
            if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
                return Err(invalid(id, "probability is outside 0..=1"));
            }
            Ok(JudgeAnswer::Bool {
                id: id.to_string(),
                probability: probability as f32,
            })
        }
        JudgeQuestion::Choice { options, .. } => {
            let label = object
                .get(value_field)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(id, "has no string `label`"))?;
            if !options.iter().any(|option| option.label == label) {
                return Err(invalid(
                    id,
                    format!("label '{label}' is not one of the question's labels"),
                ));
            }
            Ok(JudgeAnswer::Choice {
                id: id.to_string(),
                label: label.to_string(),
            })
        }
        JudgeQuestion::Score { levels, .. } => {
            let level = object
                .get(value_field)
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid(id, "has no integer `level`"))?;
            if !(1..=levels.len() as u64).contains(&level) {
                return Err(invalid(id, format!("level {level} is outside 1..={}", levels.len())));
            }
            Ok(JudgeAnswer::Score {
                id: id.to_string(),
                level: level as u32,
            })
        }
    }
}

/// Why a judgment could not be asked or could not be read.
#[derive(Debug, Clone, PartialEq)]
pub enum JudgeError {
    /// The request left the documented contract before any model call.
    Input(String),
    /// The reply was not the documented JSON shape.
    Malformed(String),
    /// The reply answered a question with a value the question does not allow.
    InvalidAnswer { id: String, reason: String },
    /// The attempt budget ran out. The consumer fails open.
    Timeout { after_ms: u64 },
    /// No judge model is configured for this process. The consumer fails open
    /// without a request, and this is a configuration fact, not a failure to
    /// report at the same volume as one.
    Unavailable(String),
    /// The backend refused or failed the call.
    Backend(String),
}

impl fmt::Display for JudgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(reason) => write!(formatter, "{JUDGE_INPUT}: {reason}"),
            Self::Malformed(reason) => write!(formatter, "{JUDGE_ANSWER}: {reason}"),
            Self::InvalidAnswer { id, reason } => {
                write!(formatter, "{JUDGE_ANSWER}: answer '{id}' {reason}")
            }
            Self::Timeout { after_ms } => write!(formatter, "judgment timed out after {after_ms} ms"),
            Self::Unavailable(reason) => write!(formatter, "no judge model is available: {reason}"),
            Self::Backend(reason) => write!(formatter, "judgment backend failed: {reason}"),
        }
    }
}

impl std::error::Error for JudgeError {}

/// Whether an error is worth retrying. A malformed reply and an invalid answer
/// are model behaviour, not transport failures, so a retry of the same prompt
/// is a fresh coin flip; this build does not spend a second request on one.
pub fn is_retryable(error: &JudgeError) -> bool {
    matches!(error, JudgeError::Backend(_) | JudgeError::Timeout { .. })
}

fn input_error(reason: String) -> JudgeError {
    JudgeError::Input(reason)
}

fn malformed(reason: String) -> JudgeError {
    JudgeError::Malformed(reason)
}

fn invalid(id: &str, reason: impl Into<String>) -> JudgeError {
    JudgeError::InvalidAnswer {
        id: id.to_string(),
        reason: reason.into(),
    }
}

/// The response shape as a JSON Schema object, for a transport that can send
/// one. Kept beside the parser so the two cannot drift: every value field named
/// here is the field `parse_answers` requires.
pub fn response_schema(request: &JudgeRequest) -> Value {
    let answer_items = request
        .questions
        .iter()
        .map(|item| {
            let (value_field, value_schema) = match &item.question {
                JudgeQuestion::Bool { .. } => ("probability", json!({"type": "number", "minimum": 0, "maximum": 1})),
                JudgeQuestion::Choice { options, .. } => {
                    let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
                    ("label", json!({"type": "string", "enum": labels}))
                }
                JudgeQuestion::Score { levels, .. } => (
                    "level",
                    json!({"type": "integer", "minimum": 1, "maximum": levels.len()}),
                ),
            };
            let mut properties = Map::new();
            properties.insert("id".to_string(), json!({"const": item.id}));
            properties.insert(value_field.to_string(), value_schema);
            json!({
                "type": "object",
                "required": ["id", value_field],
                "properties": properties,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "type": "object",
        "required": ["answers"],
        "properties": {
            "answers": {
                "type": "array",
                "minItems": request.questions.len(),
                "maxItems": request.questions.len(),
                "items": {"anyOf": answer_items},
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixed_request() -> JudgeRequest {
        JudgeRequest::new(
            json!({"turn": "wrote three files then stopped"}),
            vec![
                JudgeItem::new(
                    "stop",
                    JudgeQuestion::boolean("Did the response end before its work did?"),
                ),
                JudgeItem::new(
                    "reason",
                    JudgeQuestion::choice(
                        "Why did the response stop?",
                        vec![
                            JudgeChoiceOption::new("done", "The task was complete"),
                            JudgeChoiceOption::new("interrupted", "The response was cut off"),
                        ],
                    ),
                ),
                JudgeItem::new(
                    "severity",
                    JudgeQuestion::score(
                        "How incomplete is the output?",
                        vec![
                            "complete".to_string(),
                            "partly done".to_string(),
                            "barely started".to_string(),
                        ],
                    ),
                ),
            ],
        )
        .expect("a well-formed request")
    }

    #[test]
    fn prompt_names_every_label_and_level_with_its_position() {
        let prompt = mixed_request().prompt().expect("prompt");

        assert!(prompt.contains("### stop"));
        assert!(prompt.contains("`done`"), "labels must appear verbatim: {prompt}");
        assert!(prompt.contains("`interrupted`"));
        assert!(
            prompt.contains("3. barely started"),
            "levels must be numbered: {prompt}"
        );
        assert!(prompt.contains("wrote three files then stopped"), "state must appear");
    }

    #[test]
    fn a_well_formed_reply_parses_every_answer_type() {
        let request = mixed_request();
        let answers = request
            .parse_answers(
                r#"{"answers":[
                    {"id":"severity","level":2},
                    {"id":"stop","probability":0.82},
                    {"id":"reason","label":"interrupted"}
                ]}"#,
            )
            .expect("a complete reply");

        assert_eq!(answers.probability("stop"), Some(0.82));
        assert_eq!(answers.label("reason"), Some("interrupted"));
        assert_eq!(answers.level("severity"), Some(2));
        // Answers follow request order regardless of reply order.
        assert_eq!(
            answers.iter().map(JudgeAnswer::id).collect::<Vec<_>>(),
            vec!["stop", "reason", "severity"]
        );
    }

    #[test]
    fn a_missing_answer_is_rejected_rather_than_defaulted() {
        let error = mixed_request()
            .parse_answers(r#"{"answers":[{"id":"stop","probability":0.4},{"id":"reason","label":"done"}]}"#)
            .expect_err("an unanswered question must not default to a value");
        assert!(
            matches!(error, JudgeError::InvalidAnswer { ref id, .. } if id == "severity"),
            "the error must name the unanswered question: {error}"
        );
    }

    #[test]
    fn an_unknown_answer_is_rejected() {
        let error = mixed_request()
            .parse_answers(
                r#"{"answers":[
                    {"id":"stop","probability":0.4},
                    {"id":"reason","label":"done"},
                    {"id":"severity","level":1},
                    {"id":"ghost","probability":0.5}
                ]}"#,
            )
            .expect_err("an unrequested answer must be refused");
        assert!(
            matches!(error, JudgeError::InvalidAnswer { ref id, .. } if id == "ghost"),
            "{error}"
        );
    }

    #[test]
    fn a_probability_outside_the_unit_interval_is_rejected() {
        for bad in ["1.5", "-0.1"] {
            let error = JudgeRequest::new(
                json!("state"),
                vec![JudgeItem::new("q", JudgeQuestion::boolean("Is it so?"))],
            )
            .expect("request")
            .parse_answers(&format!(r#"{{"answers":[{{"id":"q","probability":{bad}}}]}}"#))
            .expect_err("a probability outside 0..=1 must be refused");
            assert!(matches!(error, JudgeError::InvalidAnswer { .. }), "{error}");
        }
    }

    #[test]
    fn an_invented_label_is_rejected() {
        let error = JudgeRequest::new(
            json!("state"),
            vec![JudgeItem::new(
                "q",
                JudgeQuestion::choice(
                    "Pick one",
                    vec![
                        JudgeChoiceOption::new("a", "first"),
                        JudgeChoiceOption::new("b", "second"),
                    ],
                ),
            )],
        )
        .expect("request")
        .parse_answers(r#"{"answers":[{"id":"q","label":"c"}]}"#)
        .expect_err("a label the question does not offer must be refused");
        assert!(matches!(error, JudgeError::InvalidAnswer { .. }), "{error}");
    }

    #[test]
    fn a_level_outside_the_scale_is_rejected() {
        let request = JudgeRequest::new(
            json!("state"),
            vec![JudgeItem::new(
                "q",
                JudgeQuestion::score("How much?", vec!["low".to_string(), "high".to_string()]),
            )],
        )
        .expect("request");
        for bad in ["0", "3"] {
            let error = request
                .parse_answers(&format!(r#"{{"answers":[{{"id":"q","level":{bad}}}]}}"#))
                .expect_err("a level outside 1..=2 must be refused");
            assert!(matches!(error, JudgeError::InvalidAnswer { .. }), "{error}");
        }
    }

    #[test]
    fn an_extra_field_is_rejected() {
        let error = JudgeRequest::new(
            json!("state"),
            vec![JudgeItem::new("q", JudgeQuestion::boolean("Is it so?"))],
        )
        .expect("request")
        .parse_answers(r#"{"answers":[{"id":"q","probability":0.5,"explanation":"because"}]}"#)
        .expect_err("an undeclared field must be refused");
        assert!(matches!(error, JudgeError::InvalidAnswer { .. }), "{error}");
    }

    #[test]
    fn a_duplicate_answer_is_rejected() {
        let error = JudgeRequest::new(
            json!("state"),
            vec![JudgeItem::new("q", JudgeQuestion::boolean("Is it so?"))],
        )
        .expect("request")
        .parse_answers(r#"{"answers":[{"id":"q","probability":0.5},{"id":"q","probability":0.9}]}"#)
        .expect_err("the same question answered twice is ambiguous");
        assert!(matches!(error, JudgeError::InvalidAnswer { .. }), "{error}");
    }

    #[test]
    fn a_reply_that_is_not_json_is_malformed() {
        let error = JudgeRequest::new(
            json!("state"),
            vec![JudgeItem::new("q", JudgeQuestion::boolean("Is it so?"))],
        )
        .expect("request")
        .parse_answers("I think the answer is probably yes.")
        .expect_err("prose is not a typed answer");
        assert!(matches!(error, JudgeError::Malformed(_)), "{error}");
        assert!(error.to_string().starts_with(JUDGE_ANSWER));
    }

    #[test]
    fn the_request_contract_is_checked_before_a_model_call() {
        let empty = JudgeRequest::new(json!("state"), Vec::new()).expect_err("no questions");
        assert!(empty.to_string().starts_with(JUDGE_INPUT));

        let one_option = JudgeRequest::new(
            json!("state"),
            vec![JudgeItem::new(
                "q",
                JudgeQuestion::choice("Pick", vec![JudgeChoiceOption::new("only", "one")]),
            )],
        )
        .expect_err("a choice needs two options");
        assert!(one_option.to_string().starts_with(JUDGE_INPUT));

        let scalar_state = JudgeRequest::new(json!(7), vec![JudgeItem::new("q", JudgeQuestion::boolean("?"))])
            .expect_err("a number is not a state");
        assert!(scalar_state.to_string().starts_with(JUDGE_INPUT));

        let duplicate = JudgeRequest::new(
            json!("state"),
            vec![
                JudgeItem::new("q", JudgeQuestion::boolean("one")),
                JudgeItem::new("q", JudgeQuestion::boolean("two")),
            ],
        )
        .expect_err("ids must be unique");
        assert!(duplicate.to_string().starts_with(JUDGE_INPUT));
    }

    #[test]
    fn the_schema_names_the_same_value_fields_the_parser_requires() {
        let schema = response_schema(&mixed_request());
        let items = schema["properties"]["answers"]["items"]["anyOf"]
            .as_array()
            .expect("one schema per question");
        assert_eq!(items.len(), 3);
        let required = items[0]["required"].as_array().expect("required list");
        assert!(required.iter().any(|value| value == "probability"));
    }

    #[test]
    fn only_transport_errors_are_retryable() {
        assert!(is_retryable(&JudgeError::Timeout { after_ms: 3000 }));
        assert!(is_retryable(&JudgeError::Backend("502".to_string())));
        assert!(!is_retryable(&JudgeError::Malformed("json".to_string())));
        assert!(!is_retryable(&JudgeError::Input("empty".to_string())));
        assert!(!is_retryable(&JudgeError::Unavailable("no role".to_string())));
    }
}

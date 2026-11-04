//! `ask` questions as ACP form elicitation (`elicitation/create`, mode `form`).
//!
//! The daemon sends `ask_requested` only to a Subscribe that set
//! `supports_ask_requests`, which the adapter sets exactly when the client
//! advertised `elicitation.form`. Each question becomes one form property keyed
//! by its id: a single-choice question a string enum, a multi-choice question an
//! array of that enum, with the recommended option as the default.
//!
//! `accept` with content maps to the chosen labels. `decline`, `cancel`, any
//! other action, a malformed result, a client error or a withdrawn call all send
//! an empty answer: the daemon then follows `[tools] ask_policy`
//! (crates/app-core/src/tool/ask.rs: `recommended` answers with the recommended
//! option, `error` fails the ask tool). Labels outside a question's options are
//! dropped by the daemon's merge, so the adapter forwards what the client sent.
//!
//! `AskQuestion` has no secret or password field, so no question is ever a
//! secret and the spec's ban on collecting secrets through a form cannot be hit
//! here; URL-mode elicitation is never used.
//!
//! A v2 connection gets the same session-scoped form (IS-14): support is read
//! from its v2 `capabilities.elicitation.form`, the request reports
//! `requires_action` while it waits, and an unknown `action` answers like
//! `cancel`. The daemon's `ask_requested` names no tool call, so the request
//! carries no `toolCallId`.

use super::*;
use crate::protocol::AskSelection;
use serde::Deserialize;

/// The fields of the daemon's `AskQuestion` (crates/tool-types) a form needs,
/// read from its wire form; the adapter does not depend on that crate.
#[derive(Debug, Deserialize)]
pub(super) struct AskQuestion {
    #[serde(default)]
    id: Option<String>,
    question: String,
    #[serde(default)]
    header: Option<String>,
    options: Vec<AskChoice>,
    #[serde(default)]
    recommended: Option<usize>,
    #[serde(default)]
    multi: bool,
}

#[derive(Debug, Deserialize)]
struct AskChoice {
    label: String,
}

pub(super) fn questions_from_wire(questions: &impl serde::Serialize) -> Vec<AskQuestion> {
    serde_json::to_value(questions)
        .and_then(serde_json::from_value)
        .unwrap_or_default()
}

fn question_id(question: &AskQuestion, index: usize) -> String {
    question.id.clone().unwrap_or_else(|| format!("q{}", index + 1))
}

pub(super) fn elicitation_params(session_id: &str, questions: &[AskQuestion]) -> Value {
    let message = questions
        .iter()
        .map(|question| match &question.header {
            Some(header) => format!("{header}: {}", question.question),
            None => question.question.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (index, question) in questions.iter().enumerate() {
        let id = question_id(question, index);
        let labels: Vec<&str> = question.options.iter().map(|option| option.label.as_str()).collect();
        let recommended = question
            .recommended
            .and_then(|index| question.options.get(index))
            .map(|option| option.label.clone());
        let mut property = if question.multi {
            json!({ "type": "array", "items": { "type": "string", "enum": labels } })
        } else {
            json!({ "type": "string", "enum": labels })
        };
        property["description"] = json!(question.question);
        if let Some(header) = &question.header {
            property["title"] = json!(header);
        }
        if let Some(label) = recommended {
            property["default"] = if question.multi { json!([label]) } else { json!(label) };
        }
        properties.insert(id.clone(), property);
        required.push(id);
    }
    json!({
        "sessionId": session_id,
        "mode": "form",
        "message": message,
        "requestedSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        },
    })
}

/// The selections an elicitation result carries; empty unless the client
/// accepted with content.
pub(super) fn answers_from_elicitation(
    questions: &[AskQuestion],
    result: &std::result::Result<Value, RpcError>,
) -> Vec<AskSelection> {
    let Ok(result) = result else {
        return Vec::new();
    };
    if result["action"] != "accept" {
        return Vec::new();
    }
    let Some(content) = result.get("content").and_then(Value::as_object) else {
        return Vec::new();
    };
    questions
        .iter()
        .enumerate()
        .filter_map(|(index, question)| {
            let question_id = question_id(question, index);
            let selected: Vec<String> = match content.get(&question_id)? {
                Value::String(label) => vec![label.clone()],
                Value::Array(labels) => labels.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                _ => return None,
            };
            (!selected.is_empty()).then_some(AskSelection { question_id, selected })
        })
        .collect()
}

impl AcpSideChannel {
    pub(super) async fn elicit(&self, request_id: String, questions: Vec<AskQuestion>) {
        let answers = if !questions.is_empty() && supports_elicitation_form(&self.client_capabilities()) {
            let params = elicitation_params(&self.session_id, &questions);
            let waiting = self.user_actions.as_ref().map(v2::state::UserActions::begin);
            let result = self.call_tracked(&request_id, "elicitation/create", params).await;
            drop(waiting);
            answers_from_elicitation(&questions, &result)
        } else {
            Vec::new()
        };
        let id = self.daemon.next_id();
        let response = Request::AskResponse {
            id,
            request_id: request_id.clone(),
            answers,
            answer_id: None,
        };
        self.send_to_daemon(id, &response, &format!("ask_response {request_id}"))
            .await;
    }
}

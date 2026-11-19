//! The two prompts the harness can park a turn on: a tool approval and the
//! model's structured question.
//!
//! Both are rendered as plain lines above the input and answered with single
//! keystrokes, so a user never has to type a command to unblock a run. The
//! module owns the prompt state and the key mapping; the app only forwards the
//! resulting [`Response`] to the bridge.

use crossterm::event::KeyEvent;
use horon_agent_sdk::api::AskSelection;
use horon_agent_tool_types::AskQuestion;

use crate::keys::{self, Bindings, Code, Key, Scope};

/// The harness is waiting for a permission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// Echoed back with the answer.
    pub request_id: String,
    /// Tool name or action the decision covers.
    pub action: String,
    /// Whether the user has asked for this decision to apply to later calls too.
    pub always: bool,
}

/// The harness is waiting for answers to one to four questions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// Echoed back with the answers.
    pub ask_id: String,
    /// Questions in the order they must be answered.
    pub questions: Vec<AskQuestion>,
    /// Index of the question the user is answering.
    pub current: usize,
    /// Labels chosen per question, parallel to `questions`.
    pub chosen: Vec<Vec<String>>,
}

impl Ask {
    fn new(ask_id: String, questions: Vec<AskQuestion>) -> Self {
        let chosen = vec![Vec::new(); questions.len()];
        Self {
            ask_id,
            questions,
            current: 0,
            chosen,
        }
    }

    fn question(&self) -> Option<&AskQuestion> {
        self.questions.get(self.current)
    }

    /// Toggle or set the option at `index` of the current question.
    ///
    /// Returns whether `index` named an option of the current question. A digit
    /// past the last option changes nothing, so the caller can leave the
    /// question where it is instead of moving past an empty answer.
    fn choose(&mut self, index: usize) -> bool {
        let Some(question) = self.questions.get(self.current) else {
            return false;
        };
        let Some(option) = question.options.get(index) else {
            return false;
        };
        let label = option.label.clone();
        let multi = question.multi;
        let Some(slot) = self.chosen.get_mut(self.current) else {
            return false;
        };
        if multi {
            if let Some(position) = slot.iter().position(|held| *held == label) {
                slot.remove(position);
            } else {
                slot.push(label);
            }
        } else {
            slot.clear();
            slot.push(label);
        }
        true
    }

    /// Answers for every question, in the order they were asked.
    ///
    /// `chosen` is built with one slot per question and only ever written
    /// through that pairing, so zipping is what the two vectors already mean.
    fn answers(&self) -> Vec<AskSelection> {
        self.questions
            .iter()
            .zip(self.chosen.iter())
            .map(|(question, held)| AskSelection {
                question_id: question_id(question),
                selected: held.clone(),
            })
            .collect()
    }

    /// Move to the next unanswered question, or finish when none is left.
    ///
    /// A question with no options cannot be answered by pressing a digit, and
    /// the daemon resolves it with an empty selection; waiting on one would park
    /// the prompt on a question the user has no way to settle.
    fn advance(&mut self) -> bool {
        while self.current < self.questions.len() {
            let answerable = self
                .questions
                .get(self.current)
                .is_some_and(|question| !question.options.is_empty());
            let answered = self.chosen.get(self.current).is_some_and(|held| !held.is_empty());
            if answerable && !answered {
                return false;
            }
            self.current += 1;
        }
        true
    }
}

/// The id the daemon matches an answer against.
///
/// A question without an id is named `q1` — the fixed fallback in
/// `crates/app-core/src/tool/ask.rs` (`question.id.clone().unwrap_or_else(||
/// "q1".to_string())`), not a per-position `q2`, `q3`. Numbering here would
/// address a question the daemon never looks up, so every id-less question in
/// a request answers to the same `q1`.
fn question_id(question: &AskQuestion) -> String {
    match &question.id {
        Some(id) => id.clone(),
        None => "q1".to_string(),
    }
}

/// One `[key] label` clause for an action, or `None` when nothing is bound.
///
/// An action a configuration turned off (`[]` in the file) has no key, and a
/// prompt that advertised one would tell the user to press a key that does
/// nothing.
fn clause(bindings: &Bindings, id: &str, label: &str) -> Option<String> {
    let keys = bindings.keys_for(id);
    if keys.is_empty() {
        return None;
    }
    let names: Vec<String> = keys.iter().map(|key| key.to_string()).collect();
    Some(format!("[{}] {label}", names.join("/")))
}

/// The approval hint, derived from the key map rather than hardcoded, so a
/// rebound answer key is the key the hint shows.
fn approval_hint(bindings: &Bindings) -> String {
    let clauses: Vec<String> = [
        (keys::ids::PROMPT_ALLOW, "allow"),
        (keys::ids::PROMPT_ALLOW_ALWAYS, "allow always"),
        (keys::ids::PROMPT_DENY, "deny"),
        (keys::ids::PROMPT_DISMISS, "dismiss"),
    ]
    .into_iter()
    .filter_map(|(id, label)| clause(bindings, id, label))
    .collect();
    if clauses.is_empty() {
        return String::new();
    }
    format!("  {}", clauses.join("  "))
}

/// The hint under a question.
///
/// Options are chosen by their digit, which is literal and cannot be rebound;
/// only the confirmation of a multi-select question is a binding worth naming.
/// A question with no options cannot be answered at all, so it says so instead
/// of advertising a key that changes nothing.
fn ask_hint(bindings: &Bindings, question: &AskQuestion) -> String {
    if question.options.is_empty() {
        return "  no options: the tool resolves this question on its own".to_string();
    }
    if !question.multi {
        return "  press the number of an option".to_string();
    }
    match clause(bindings, keys::ids::PROMPT_CONFIRM, "confirm") {
        Some(confirm) => format!("  digits toggle, then {confirm}"),
        None => "  digits toggle".to_string(),
    }
}

/// The prompt currently blocking the turn.
#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    /// A tool approval.
    Approval(Approval),
    /// A structured question.
    Ask(Ask),
}

/// What a keystroke settled, if anything.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// Answer a parked approval.
    Approval {
        request_id: String,
        approved: bool,
        always: bool,
    },
    /// Answer an ask request.
    Ask { ask_id: String, answers: Vec<AskSelection> },
}

impl Prompt {
    /// An approval prompt for a parked request.
    pub fn approval(request_id: String, action: String) -> Self {
        Self::Approval(Approval {
            request_id,
            action,
            always: false,
        })
    }

    /// An ask prompt for a question set.
    pub fn ask(ask_id: String, questions: Vec<AskQuestion>) -> Self {
        Self::Ask(Ask::new(ask_id, questions))
    }

    /// The lines to draw above the input.
    ///
    /// Uses the shipped key map; the app draws [`Prompt::lines_with`] with the
    /// map in force, so a rebound prompt key shows as the key that answers now.
    pub fn lines(&self) -> Vec<String> {
        self.lines_with(keys::default_bindings())
    }

    /// The lines to draw above the input, with hints from the given key map.
    ///
    /// The approval title names the action alone. The app adds the argument
    /// summary of the call the request covers, because only the transcript can
    /// pair a request id with its line; a prompt that guessed here is what
    /// showed one call's command on another call's decision.
    pub fn lines_with(&self, bindings: &Bindings) -> Vec<String> {
        match self {
            Self::Approval(approval) => {
                let hint = approval_hint(bindings);
                let mut lines = vec![format!("! approval needed: {}", approval.action)];
                if !hint.is_empty() {
                    lines.push(hint);
                }
                lines
            }
            Self::Ask(ask) => {
                let mut lines = Vec::new();
                if let Some(question) = ask.question() {
                    let counter = format!(" ({}/{})", ask.current + 1, ask.questions.len());
                    let header = match &question.header {
                        Some(header) => format!("{header}: {}", question.question),
                        None => question.question.clone(),
                    };
                    lines.push(format!("? {header}{counter}"));
                    for (index, option) in question.options.iter().enumerate() {
                        let mark = if question.recommended == Some(index) {
                            " (recommended)"
                        } else {
                            ""
                        };
                        let chosen = ask
                            .chosen
                            .get(ask.current)
                            .is_some_and(|held| held.contains(&option.label));
                        let box_mark = if chosen { "x" } else { " " };
                        if question.multi {
                            lines.push(format!("  [{box_mark}] {}) {}{mark}", index + 1, option.label));
                        } else {
                            lines.push(format!("  {}) {}{mark}", index + 1, option.label));
                        }
                    }
                    lines.push(ask_hint(bindings, question));
                }
                lines
            }
        }
    }

    /// Handle one keystroke with the shipped key map.
    ///
    /// This is the entry point for a caller that has no loaded key map, such
    /// as a test driving the prompt on its own; the app uses
    /// [`Prompt::on_key_with`] with the map in force.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Response> {
        self.on_key_with(keys::default_bindings(), key)
    }

    /// Handle one keystroke through the given key map.
    ///
    /// Returns a [`Response`] only when the prompt is answered completely; a
    /// partially answered ask keeps its state and keeps drawing. Digits are
    /// matched literally rather than through the map: a question is answered by
    /// pressing its number, which must keep working whatever the user bound.
    pub fn on_key_with(&mut self, bindings: &Bindings, key: KeyEvent) -> Option<Response> {
        let key = Key::from_event(key)?;
        match self {
            Self::Approval(approval) => {
                let (approved, always) = match bindings.resolve(Scope::Prompt, &key) {
                    Some(keys::ids::PROMPT_ALLOW) => (true, false),
                    Some(keys::ids::PROMPT_ALLOW_ALWAYS) => (true, true),
                    // Dismissing a parked approval is a decision, not a pause:
                    // the turn is waiting on it, and leaving it parked would
                    // hold the run until the request expires.
                    Some(keys::ids::PROMPT_DENY | keys::ids::PROMPT_DISMISS) => (false, false),
                    _ => return None,
                };
                approval.always = always;
                Some(Response::Approval {
                    request_id: approval.request_id.clone(),
                    approved,
                    always,
                })
            }
            Self::Ask(ask) => {
                let question = ask.question();
                let multi = question.is_some_and(|question| question.multi);
                let optionless = question.is_some_and(|question| question.options.is_empty());
                match bindings.resolve(Scope::Prompt, &key) {
                    // An ask has no deny: the tool resolves its own deadline.
                    // Closing the prompt here would take away the only way to
                    // answer a question that is still waiting.
                    Some(keys::ids::PROMPT_DISMISS) => return None,
                    // A multi-select question toggles digits and confirms with
                    // Enter; a single-choice one is settled by its digit. A
                    // question with no options has no digit to press, so the
                    // confirm key moves past it with an empty answer.
                    Some(keys::ids::PROMPT_CONFIRM) if multi || optionless => {}
                    Some(_) => return None,
                    None => {
                        let Code::Char(character) = key.code else {
                            return None;
                        };
                        if key.ctrl {
                            return None;
                        }
                        let index = character.to_digit(10)?.checked_sub(1)?;
                        // A digit past the last option selects nothing. Leaving
                        // the question untouched keeps the prompt on it, rather
                        // than stepping past an empty answer the daemon would
                        // fill in with a default the user never chose.
                        if !ask.choose(index as usize) {
                            return None;
                        }
                        if multi {
                            return None;
                        }
                        // `advance` steps past every answered question, so the
                        // just-answered one is behind it without a manual bump.
                    }
                }
                if !ask.advance() {
                    return None;
                }
                Some(Response::Ask {
                    ask_id: ask.ask_id.clone(),
                    answers: ask.answers(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use horon_agent_tool_types::AskOption;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn question(id: Option<&str>, multi: bool, options: &[&str]) -> AskQuestion {
        AskQuestion {
            id: id.map(str::to_string),
            question: "Which one?".to_string(),
            header: None,
            options: options
                .iter()
                .map(|label| AskOption {
                    label: (*label).to_string(),
                    description: None,
                })
                .collect(),
            recommended: None,
            multi,
        }
    }

    #[test]
    fn approval_answers_y_n_and_a() {
        let mut prompt = Prompt::approval("r1".into(), "bash: ls".into());
        assert_eq!(
            prompt.on_key(key(KeyCode::Char('y'))),
            Some(Response::Approval {
                request_id: "r1".into(),
                approved: true,
                always: false
            })
        );
        let mut prompt = Prompt::approval("r2".into(), "bash: ls".into());
        assert_eq!(
            prompt.on_key(key(KeyCode::Char('a'))),
            Some(Response::Approval {
                request_id: "r2".into(),
                approved: true,
                always: true
            })
        );
        let mut prompt = Prompt::approval("r3".into(), "bash: ls".into());
        assert_eq!(
            prompt.on_key(key(KeyCode::Char('n'))),
            Some(Response::Approval {
                request_id: "r3".into(),
                approved: false,
                always: false
            })
        );
    }

    #[test]
    fn approval_ignores_unrelated_keys() {
        let mut prompt = Prompt::approval("r1".into(), "bash: ls".into());
        assert_eq!(prompt.on_key(key(KeyCode::Char('q'))), None);
        assert!(prompt.lines()[0].contains("approval needed"));
    }

    #[test]
    fn single_choice_ask_walks_questions_and_answers_all() {
        let mut prompt = Prompt::ask(
            "a1".into(),
            vec![
                question(Some("scope"), false, &["one", "all"]),
                question(None, false, &["terse"]),
            ],
        );
        assert_eq!(prompt.on_key(key(KeyCode::Char('2'))), None);
        let answered = prompt.on_key(key(KeyCode::Char('1')));
        match answered {
            Some(Response::Ask { ask_id, answers }) => {
                assert_eq!(ask_id, "a1");
                assert_eq!(answers.len(), 2);
                assert_eq!(answers[0].question_id, "scope");
                assert_eq!(answers[0].selected, vec!["all".to_string()]);
                // A question without an id is addressed the way the daemon
                // addresses it.
                assert_eq!(answers[1].question_id, "q1");
                assert_eq!(answers[1].selected, vec!["terse".to_string()]);
            }
            other => panic!("unexpected response {other:?}"),
        }
    }

    #[test]
    fn multi_choice_toggles_and_needs_enter() {
        let mut prompt = Prompt::ask("a1".into(), vec![question(Some("q"), true, &["a", "b"])]);
        assert_eq!(prompt.on_key(key(KeyCode::Char('1'))), None);
        assert!(prompt.lines()[1].contains("[x]"));
        assert_eq!(prompt.on_key(key(KeyCode::Char('2'))), None);
        match prompt.on_key(key(KeyCode::Enter)) {
            Some(Response::Ask { answers, .. }) => {
                assert_eq!(answers[0].selected, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("unexpected response {other:?}"),
        }
    }

    /// The ask state a test inspects after a keystroke.
    fn ask_of(prompt: &Prompt) -> &Ask {
        match prompt {
            Prompt::Ask(ask) => ask,
            Prompt::Approval(approval) => panic!("expected an ask, got {approval:?}"),
        }
    }

    #[test]
    fn zero_leaves_a_single_choice_question_alone() {
        let mut prompt = Prompt::ask("a1".into(), vec![question(Some("q"), false, &["one", "two"])]);
        assert_eq!(prompt.on_key(key(KeyCode::Char('0'))), None);
        let ask = ask_of(&prompt);
        assert_eq!(ask.current, 0);
        assert!(ask.chosen[0].is_empty());
    }

    #[test]
    fn an_out_of_range_digit_on_the_last_question_does_not_answer() {
        let mut prompt = Prompt::ask("a1".into(), vec![question(Some("q"), false, &["only"])]);
        assert_eq!(
            prompt.on_key(key(KeyCode::Char('9'))),
            None,
            "an answer was sent for a question nobody chose an option for"
        );
        assert_eq!(ask_of(&prompt).current, 0);
    }

    #[test]
    fn a_valid_digit_after_an_invalid_one_still_answers() {
        let mut prompt = Prompt::ask("a1".into(), vec![question(Some("q"), false, &["one", "two"])]);
        assert_eq!(prompt.on_key(key(KeyCode::Char('7'))), None);
        assert_eq!(ask_of(&prompt).current, 0);
        match prompt.on_key(key(KeyCode::Char('2'))) {
            Some(Response::Ask { answers, .. }) => {
                assert_eq!(answers[0].selected, vec!["two".to_string()]);
            }
            other => panic!("unexpected response {other:?}"),
        }
    }

    #[test]
    fn a_multi_question_flow_skips_a_stray_digit_and_completes() {
        let mut prompt = Prompt::ask(
            "a1".into(),
            vec![
                question(Some("first"), false, &["one", "two"]),
                question(Some("second"), false, &["three", "four"]),
            ],
        );
        assert_eq!(prompt.on_key(key(KeyCode::Char('4'))), None);
        assert_eq!(ask_of(&prompt).current, 0);
        assert_eq!(prompt.on_key(key(KeyCode::Char('2'))), None);
        assert_eq!(ask_of(&prompt).current, 1);
        match prompt.on_key(key(KeyCode::Char('1'))) {
            Some(Response::Ask { ask_id, answers }) => {
                assert_eq!(ask_id, "a1");
                assert_eq!(answers.len(), 2);
                assert_eq!(answers[0].question_id, "first");
                assert_eq!(answers[0].selected, vec!["two".to_string()]);
                assert_eq!(answers[1].question_id, "second");
                assert_eq!(answers[1].selected, vec!["three".to_string()]);
            }
            other => panic!("unexpected response {other:?}"),
        }
    }
}

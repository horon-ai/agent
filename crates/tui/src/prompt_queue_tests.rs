//! Prompts: the queue behind the one on screen, the hints a prompt shows, what
//! each key answers, and the line the transcript keeps afterwards.
//!
//! Kept out of `app.rs` and `prompts.rs` for the size budget. Every test drives
//! the whole app, because the queue only exists there: `Prompt` alone answers
//! one decision and knows nothing about the next.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::{ApiEvent, TurnStopReason};
use horon_agent_tool_types::{AskOption, AskQuestion};
use toml::Table;

use crate::app::{Action, App};
use crate::keys::Bindings;
use crate::prompts::Prompt;
use crate::sdk_bridge::Incoming;
use crate::transcript::Item;

fn app() -> App {
    let (_sender, receiver) = mpsc::channel::<Incoming>();
    App::new("socket · session".to_string(), receiver)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn esc() -> KeyEvent {
    key(KeyCode::Esc)
}

fn done(session: &str) -> ApiEvent {
    ApiEvent::TurnDone {
        session_id: session.to_string(),
        pending_soft_interrupts: None,
    }
}

fn stopped(session: &str) -> ApiEvent {
    ApiEvent::TurnStopped {
        session_id: session.to_string(),
        reason: TurnStopReason::Interrupted,
        message: "turn cancelled by user".to_string(),
        provider_stop_reason: None,
    }
}

fn approval_pending(request: &str, action: &str) -> ApiEvent {
    ApiEvent::ApprovalPending {
        session_id: "s1".to_string(),
        request_id: request.to_string(),
        action: action.to_string(),
        expires_at: None,
    }
}

fn tool_start(call: &str, name: &str) -> ApiEvent {
    ApiEvent::ToolStart {
        session_id: "s1".to_string(),
        call_id: call.to_string(),
        name: name.to_string(),
    }
}

fn tool_input(call: &str, delta: &str) -> ApiEvent {
    ApiEvent::ToolInputDelta {
        session_id: "s1".to_string(),
        call_id: call.to_string(),
        delta: delta.to_string(),
    }
}

fn question(id: &str, multi: bool, options: &[&str]) -> AskQuestion {
    AskQuestion {
        id: Some(id.to_string()),
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

/// The last line the transcript holds, as text, whichever kind it is.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        Some(Item::User(text)) | Some(Item::Queued(text)) => text.clone(),
        other => format!("{other:?}"),
    }
}

/// Every line of the drawn prompt block, joined, for a text search.
fn drawn(app: &App) -> String {
    app.prompt_lines().join("\n")
}

#[test]
fn two_approvals_wait_their_turn_and_the_second_never_hides_the_first() {
    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&approval_pending("r1", "bash"));
    app.apply_event(&approval_pending("r2", "write"));

    assert!(
        drawn(&app).contains("bash"),
        "the first approval is not the one on screen:\n{}",
        drawn(&app)
    );
    assert!(
        drawn(&app).contains("1 more prompt waiting"),
        "the queued approval is not counted:\n{}",
        drawn(&app)
    );
    assert_eq!(app.waiting_prompts.len(), 1);

    let first = app.on_key(key(KeyCode::Char('y')));
    assert_eq!(
        first,
        Some(Action::Approval {
            request_id: "r1".to_string(),
            approved: true,
            always: false,
        }),
        "the first answer did not settle the first request"
    );
    assert!(
        drawn(&app).contains("write"),
        "the queued approval did not come forward:\n{}",
        drawn(&app)
    );
    assert!(
        !drawn(&app).contains("more prompt"),
        "the count still claims a waiting prompt:\n{}",
        drawn(&app)
    );

    let second = app.on_key(key(KeyCode::Char('n')));
    assert_eq!(
        second,
        Some(Action::Approval {
            request_id: "r2".to_string(),
            approved: false,
            always: false,
        }),
        "the second answer did not settle the second request"
    );
    assert!(app.prompt.is_none());
    assert!(app.waiting_prompts.is_empty());
}

#[test]
fn a_resolved_request_leaves_the_queue_it_was_waiting_in() {
    let mut app = app();
    app.apply_event(&approval_pending("r1", "bash"));
    app.apply_event(&approval_pending("r2", "write"));
    // Answered somewhere else: the desktop prompt, the CLI, a file.
    app.apply_event(&ApiEvent::ApprovalResolved {
        session_id: "s1".to_string(),
        request_id: "r2".to_string(),
        approved: true,
        via: "file".to_string(),
        resolved_by: "tool".to_string(),
    });
    assert!(
        app.waiting_prompts.is_empty(),
        "a decided request stayed queued: {:?}",
        app.waiting_prompts
    );
    assert!(app.prompt.is_some(), "the undecided prompt was dropped too");
}

/// The refusal the approval gate writes into the tool line, request id and all.
fn refusal(request: &str) -> String {
    format!(
        "Tool 'bash' needs approval before it runs: bash runs a process or reaches the network in write \
         mode. Request {request} is queued; answer it through the approval surface. Nothing was executed."
    )
}

/// A refused call: its line carries the argument summary and the request id.
fn refused_call(app: &mut App, call: &str, command: &str, request: &str) {
    app.apply_event(&tool_start(call, "bash"));
    app.apply_event(&tool_input(call, &format!(r#"{{"command":"{command}"}}"#)));
    app.apply_event(&ApiEvent::ToolDone {
        session_id: "s1".to_string(),
        call_id: call.to_string(),
        name: "bash".to_string(),
        output: String::new(),
        error: Some(refusal(request)),
        verdict: None,
    });
}

#[test]
fn parallel_same_name_approvals_each_name_their_own_call() {
    let mut app = app();
    refused_call(&mut app, "c1", "ls", "r1");
    refused_call(&mut app, "c2", "sleep 1; pwd", "r2");

    app.apply_event(&approval_pending("r1", "bash"));
    app.apply_event(&approval_pending("r2", "bash"));
    assert!(
        drawn(&app).contains("bash: ls"),
        "the first prompt did not name its own command:\n{}",
        drawn(&app)
    );

    assert!(
        matches!(app.on_key(key(KeyCode::Char('y'))), Some(Action::Approval { .. })),
        "the first approval did not answer"
    );
    let second = drawn(&app);
    assert!(
        second.contains("bash: sleep 1; pwd"),
        "the second prompt did not name its own command:\n{second}"
    );
    assert!(
        !second.contains("bash: ls\n"),
        "the second prompt repeated the first command:\n{second}"
    );
}

#[test]
fn an_unrelated_line_of_the_same_tool_is_never_paired_with_a_later_approval() {
    // The verifier's repro: a call that ran without a prompt (auto-approved)
    // and a refused call whose request was answered in an earlier turn. Both
    // lines are `bash`, and the later decision is about neither of them.
    let mut earlier = app();
    earlier.apply_event(&tool_start("c1", "bash"));
    earlier.apply_event(&tool_input("c1", r#"{"command":"sleep 2; ls -la"}"#));
    earlier.apply_event(&ApiEvent::ToolDone {
        session_id: "s1".to_string(),
        call_id: "c1".to_string(),
        name: "bash".to_string(),
        output: "ran without a prompt".to_string(),
        error: None,
        verdict: None,
    });
    refused_call(&mut earlier, "c2", "echo old", "r_old");
    earlier.apply_event(&approval_pending("r_old", "bash"));
    earlier.on_key(key(KeyCode::Char('y')));
    earlier.apply_event(&done("s1"));

    // A later turn asks about two different calls of the same tool.
    refused_call(&mut earlier, "c3", "ls", "r_new1");
    refused_call(&mut earlier, "c4", "sleep 1; pwd", "r_new2");
    earlier.apply_event(&approval_pending("r_new1", "bash"));
    let lines = drawn(&earlier);
    assert!(
        lines.contains("bash: ls"),
        "the prompt did not name its own call:\n{lines}"
    );
    assert!(
        !lines.contains("sleep 2; ls -la"),
        "the prompt showed an earlier turn's command:\n{lines}"
    );
    assert!(
        !lines.contains("echo old"),
        "the prompt showed a call that was already answered:\n{lines}"
    );

    // The request that has no line of its own shows the name alone, even though
    // an older `bash` line is right there.
    let mut lone = app();
    lone.apply_event(&tool_start("c1", "bash"));
    lone.apply_event(&tool_input("c1", r#"{"command":"sleep 2; ls -la"}"#));
    lone.apply_event(&approval_pending("r_missing", "bash"));
    assert_eq!(lone.prompt_lines()[0], "! approval needed: bash");
}

#[test]
fn an_approval_outliving_its_turn_keeps_its_summary_and_does_not_lend_it() {
    let mut app = app();
    app.pending_turn = true;
    refused_call(&mut app, "c1", "ls -la", "r1");
    app.apply_event(&approval_pending("r1", "bash"));
    app.apply_event(&done("s1"));
    assert!(
        app.prompt_lines()[0].contains("bash: ls -la"),
        "the answerable approval lost its own command: {:?}",
        app.prompt_lines()
    );

    // A second turn asks about another call; it queues behind the first and must
    // name its own command, not inherit the one still on screen.
    refused_call(&mut app, "c2", "pwd", "r2");
    app.apply_event(&approval_pending("r2", "bash"));
    assert!(app.prompt_lines()[0].contains("bash: ls -la"));
    app.on_key(key(KeyCode::Char('y')));
    let second = drawn(&app);
    assert!(
        second.contains("bash: pwd"),
        "the next turn's prompt inherited the earlier command:\n{second}"
    );
}

#[test]
fn a_prompt_without_a_tool_line_shows_the_name_alone() {
    let mut app = app();
    app.apply_event(&approval_pending("r1", "bash"));
    let lines = app.prompt_lines();
    assert_eq!(lines[0], "! approval needed: bash");
}

#[test]
fn the_hint_names_the_keys_the_map_holds_now() {
    let mut table = Table::new();
    table.insert("prompt.allow".to_string(), toml::Value::Array(vec!["k".into()]));
    table.insert("prompt.deny".to_string(), toml::Value::Array(vec!["d".into()]));
    let (bindings, warnings) = Bindings::from_table(&table);
    assert!(warnings.is_empty(), "unexpected key-map warnings: {warnings:?}");

    let mut app = app();
    app.bindings = bindings;
    app.apply_event(&approval_pending("r1", "bash"));
    let hint = app.prompt_lines()[1].clone();
    assert!(hint.contains("[k] allow"), "the rebound allow key is missing: {hint}");
    assert!(hint.contains("[d] deny"), "the rebound deny key is missing: {hint}");
    assert!(
        !hint.contains("[y] allow"),
        "the old allow key is still advertised: {hint}"
    );
    assert!(
        !hint.contains("[n] deny"),
        "the old deny key is still advertised: {hint}"
    );

    // The key that was taken away does nothing now, and the new one answers.
    assert_eq!(app.on_key(key(KeyCode::Char('y'))), None);
    assert_eq!(
        app.on_key(key(KeyCode::Char('k'))),
        Some(Action::Approval {
            request_id: "r1".to_string(),
            approved: true,
            always: false,
        })
    );
}

#[test]
fn escape_answers_a_parked_approval_with_a_denial() {
    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&approval_pending("r1", "bash"));

    let answer = app.on_key(esc());
    assert_eq!(
        answer,
        Some(Action::Approval {
            request_id: "r1".to_string(),
            approved: false,
            always: false,
        }),
        "escape left the approval parked"
    );
    assert!(app.prompt.is_none());
    assert!(
        last_line(&app).contains("you denied bash"),
        "no denial line was written: {}",
        last_line(&app)
    );
}

#[test]
fn a_stray_key_never_reaches_the_input_line() {
    let mut app = app();
    app.apply_event(&approval_pending("r1", "bash"));
    assert_eq!(app.on_key(key(KeyCode::Char('x'))), None);
    assert_eq!(app.input.text(), "", "a prompt keystroke leaked into the input");
    assert!(app.prompt.is_some(), "a stray key closed the prompt");
}

#[test]
fn every_approval_answer_leaves_one_line_that_states_what_happens_next() {
    let mut app = app();
    app.apply_event(&approval_pending("r1", "bash"));
    app.on_key(key(KeyCode::Char('y')));
    let allowed = last_line(&app);
    assert!(
        allowed.contains("you allowed bash") && allowed.contains("call it again"),
        "the confirmation line does not say the call must be retried: {allowed}"
    );

    app.apply_event(&approval_pending("r2", "write"));
    app.on_key(key(KeyCode::Char('a')));
    let always = last_line(&app);
    assert!(
        always.contains("you allowed write") && always.contains("no always-rule"),
        "the always answer claims a rule the daemon does not keep: {always}"
    );

    app.apply_event(&approval_pending("r3", "edit"));
    app.on_key(key(KeyCode::Char('n')));
    let denied = last_line(&app);
    assert!(
        denied.contains("you denied edit") && denied.contains("will not run"),
        "the denial line is missing or vague: {denied}"
    );
}

#[test]
fn an_ask_walks_to_the_second_question_and_leaves_a_line_when_it_finishes() {
    let mut app = app();
    app.apply_event(&ApiEvent::AskRequested {
        session_id: "s1".to_string(),
        ask_id: "a1".to_string(),
        questions: vec![
            question("first", false, &["one", "two"]),
            question("second", true, &["three", "four"]),
        ],
    });

    assert!(
        drawn(&app).contains("(1/2)"),
        "the first question is not the one on screen:\n{}",
        drawn(&app)
    );
    assert_eq!(app.on_key(key(KeyCode::Char('2'))), None);
    assert!(
        drawn(&app).contains("(2/2)"),
        "the second question did not come up:\n{}",
        drawn(&app)
    );
    // Enter without a choice keeps the question: nothing was selected.
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);

    assert_eq!(app.on_key(key(KeyCode::Char('1'))), None);
    let answer = app.on_key(key(KeyCode::Enter));
    match answer {
        Some(Action::Ask { ask_id, answers }) => {
            assert_eq!(ask_id, "a1");
            assert_eq!(answers.len(), 2);
            assert_eq!(answers[0].selected, vec!["two".to_string()]);
            assert_eq!(answers[1].selected, vec!["three".to_string()]);
        }
        other => panic!("the ask did not finish: {other:?}"),
    }
    assert!(app.prompt.is_none());
    assert!(
        last_line(&app).contains("you answered the ask tool (2 answers)"),
        "the ask confirmation line is missing: {}",
        last_line(&app)
    );
}

#[test]
fn escape_leaves_a_question_open_because_there_is_nothing_to_deny() {
    let mut app = app();
    app.apply_event(&ApiEvent::AskRequested {
        session_id: "s1".to_string(),
        ask_id: "a1".to_string(),
        questions: vec![question("only", false, &["one"])],
    });
    assert_eq!(app.on_key(esc()), None);
    assert!(app.prompt.is_some(), "escape closed a question nobody answered");
    assert!(
        drawn(&app).contains("press the number of an option"),
        "the question lost its hint:\n{}",
        drawn(&app)
    );
}

#[test]
fn a_question_with_no_options_is_confirmed_past_instead_of_blocking() {
    let mut app = app();
    app.apply_event(&ApiEvent::AskRequested {
        session_id: "s1".to_string(),
        ask_id: "a1".to_string(),
        questions: vec![question("empty", false, &[]), question("real", false, &["one"])],
    });
    assert!(
        drawn(&app).contains("no options"),
        "a question with nothing to choose does not say so:\n{}",
        drawn(&app)
    );
    // The first question has no digit to press, so the confirm key settles it
    // with an empty selection and the second question takes its place.
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert!(
        drawn(&app).contains("(2/2)"),
        "the empty question blocked the prompt:\n{}",
        drawn(&app)
    );
    match app.on_key(key(KeyCode::Char('1'))) {
        Some(Action::Ask { answers, .. }) => {
            assert!(answers[0].selected.is_empty());
            assert_eq!(answers[1].selected, vec!["one".to_string()]);
        }
        other => panic!("the ask did not finish: {other:?}"),
    }
}

#[test]
fn a_cancelled_turn_takes_the_prompt_and_the_queue_with_it() {
    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&approval_pending("r1", "bash"));
    app.apply_event(&approval_pending("r2", "write"));

    app.apply_event(&stopped("s1"));
    assert!(app.prompt.is_none(), "a prompt outlived the turn it belonged to");
    assert!(app.waiting_prompts.is_empty(), "a queued prompt outlived its turn");
    assert_eq!(app.on_key(esc()), None, "a withdrawn prompt still answered a key");
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Notice(text) if text.contains("2 prompts withdrawn"))),
        "the withdrawal was silent: {:?}",
        app.transcript.items()
    );
}

#[test]
fn an_approval_outlives_its_turn_but_a_question_does_not() {
    let mut approval = app();
    approval.pending_turn = true;
    approval.apply_event(&approval_pending("r1", "bash"));
    approval.apply_event(&done("s1"));
    assert!(
        approval.prompt.is_some(),
        "the approval was dropped with the turn, leaving its request unanswerable"
    );
    assert_eq!(
        approval.on_key(key(KeyCode::Char('y'))),
        Some(Action::Approval {
            request_id: "r1".to_string(),
            approved: true,
            always: false,
        }),
        "the approval could not be answered after the turn ended"
    );

    let mut asked = app();
    asked.pending_turn = true;
    asked.apply_event(&ApiEvent::AskRequested {
        session_id: "s1".to_string(),
        ask_id: "a1".to_string(),
        questions: vec![question("only", false, &["one"])],
    });
    asked.apply_event(&done("s1"));
    assert!(asked.prompt.is_none(), "a question outlived the tool that asked it");
    assert!(
        last_line(&asked).contains("1 question withdrawn"),
        "the withdrawal was silent: {}",
        last_line(&asked)
    );
}

#[test]
fn a_lost_connection_takes_the_prompt_with_it() {
    let mut app = app();
    app.apply_event(&approval_pending("r1", "bash"));
    app.disconnect("the daemon closed the socket");
    assert!(app.prompt.is_none(), "an answer would be sent over a dead socket");
    assert_eq!(app.on_key(key(KeyCode::Char('y'))), None);
    // The key falls through to the input line, which is all that is left.
    assert_eq!(app.input.text(), "y");
}

#[test]
fn an_answer_line_never_claims_the_call_ran() {
    // The engine records the approval and refuses the call, so the wording is
    // the only place a user learns a retry is needed.
    let mut app = app();
    app.apply_event(&approval_pending("r1", "bash"));
    app.on_key(key(KeyCode::Char('y')));
    let line = last_line(&app);
    assert!(!line.contains("approved"), "the line reads as a granted call: {line}");
    assert!(!line.contains("ran"), "the line claims the tool ran: {line}");
    assert!(line.contains("call it again"), "the retry is not named: {line}");
}

#[test]
fn a_prompt_does_not_swallow_the_key_that_leaves() {
    let mut app = app();
    app.apply_event(&approval_pending("r1", "bash"));
    // The prompt takes every other key, but a client whose only exit is
    // swallowed is a client the user cannot leave.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        Some(Action::Quit)
    );
    assert!(app.prompt.is_some(), "leaving the client answered the prompt");

    app.pending_turn = true;
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(Action::Cancel),
        "ctrl-c did not stop the turn it was meant to stop"
    );
    assert!(app.prompt.is_none(), "a cancelled turn left its prompt on screen");
}

#[test]
fn a_prompt_on_its_own_never_invents_arguments() {
    // `Prompt` cannot pair itself with a line: the transcript owns the request
    // ids. A title that grew arguments here would be a guess.
    let prompt = Prompt::approval("r1".to_string(), "bash".to_string());
    assert_eq!(prompt.lines()[0], "! approval needed: bash");
}

/// The state of every tool line in the transcript.
fn tool_states(app: &App) -> Vec<crate::transcript::ToolState> {
    app.transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            crate::transcript::Item::Tool(call) => Some(call.state.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_parked_call_is_drawn_as_waiting_until_the_decision() {
    let mut app = app();
    refused_call(&mut app, "c1", "ls", "r1");
    app.apply_event(&approval_pending("r1", "bash"));

    let states = tool_states(&app);
    assert_eq!(states.len(), 1, "the call was not recorded: {states:?}");
    assert!(
        matches!(states[0], crate::transcript::ToolState::Waiting(_)),
        "a call waiting for its decision is drawn as a failure: {states:?}"
    );
    // The line still names its request, which is how the prompt found its
    // arguments while it waited.
    assert!(
        app.transcript.argument_summary_for_request("r1").is_some(),
        "the parked line stopped naming its request"
    );

    // The decision ends the wait, whichever way it went: the row goes back to
    // the failure the harness reported, request id and all.
    app.apply_event(&ApiEvent::ApprovalResolved {
        session_id: "s1".to_string(),
        request_id: "r1".to_string(),
        approved: false,
        via: "tui".to_string(),
        resolved_by: "user".to_string(),
    });
    let states = tool_states(&app);
    assert!(
        matches!(
            states[0],
            crate::transcript::ToolState::Settled(crate::transcript::Decision::Denied, _)
        ),
        "the decided call does not state the answer: {states:?}"
    );
}

#[test]
fn a_refusal_that_arrives_before_its_prompt_is_still_parked() {
    let mut app = app();
    app.apply_event(&approval_pending("r1", "bash"));
    refused_call(&mut app, "c1", "ls", "r1");

    let states = tool_states(&app);
    assert!(
        matches!(states[0], crate::transcript::ToolState::Waiting(_)),
        "a refusal that reached the client before its prompt was drawn as a failure: {states:?}"
    );
}

/// The whole screen, as the pane draws it.
fn screen(app: &mut App) -> String {
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test backend");
    crate::draw(&mut terminal, app).expect("the frame did not draw");
    format!("{}", terminal.backend())
}

/// A refused call whose prompt is on screen.
fn parked_call(app: &mut App, call: &str, command: &str, request: &str) {
    app.pending_turn = true;
    refused_call(app, call, command, request);
    app.apply_event(&approval_pending(request, "bash"));
}

#[test]
fn each_answer_states_its_own_outcome_on_the_row() {
    let cases = [
        (key(KeyCode::Char('y')), "allowed", "allow once"),
        (key(KeyCode::Char('a')), "allowed always", "allow always"),
        (key(KeyCode::Char('n')), "denied", "deny"),
        (esc(), "denied", "esc"),
    ];
    for (pressed, word, what) in cases {
        let mut app = app();
        parked_call(&mut app, "c1", "ls", "r1");
        assert!(screen(&mut app).contains("needs approval"), "{what}: not parked first");
        app.on_key(pressed);
        let text = screen(&mut app);
        assert!(text.contains(word), "{what} does not say `{word}`:\n{text}");
        assert!(!text.contains("failed"), "{what} says it failed:\n{text}");
        assert!(!text.contains("needs approval"), "{what} still waits:\n{text}");
        assert!(!text.contains('◐'), "{what} still wears the waiting glyph:\n{text}");
    }
}

#[test]
fn a_withdrawn_prompt_says_not_answered() {
    let mut app = app();
    parked_call(&mut app, "c1", "ls", "r1");
    app.disconnect("the connection was lost");

    let text = screen(&mut app);
    assert!(
        text.contains("not answered"),
        "a lost connection left the row waiting:\n{text}"
    );
    assert!(!text.contains("needs approval"), "{text}");
    assert!(!text.contains('◐'), "{text}");
}

#[test]
fn a_cancelled_turn_marks_its_parked_row_not_answered() {
    let mut app = app();
    parked_call(&mut app, "c1", "ls", "r1");
    app.pending_turn = true;
    app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    let text = screen(&mut app);
    assert!(text.contains("not answered"), "ctrl+c left the row waiting:\n{text}");
    assert!(!text.contains("needs approval"), "{text}");
}

#[test]
fn a_turn_that_stops_marks_its_parked_row_not_answered() {
    let mut app = app();
    parked_call(&mut app, "c1", "ls", "r1");
    app.apply_event(&ApiEvent::TurnStopped {
        session_id: "s1".to_string(),
        reason: horon_agent_sdk::api::TurnStopReason::Interrupted,
        message: "the turn was interrupted".to_string(),
        provider_stop_reason: None,
    });

    let text = screen(&mut app);
    assert!(
        text.contains("not answered"),
        "a stopped turn left the row waiting:\n{text}"
    );
    assert!(!text.contains("needs approval"), "{text}");
}

#[test]
fn parallel_parked_calls_settle_independently() {
    let mut app = app();
    app.pending_turn = true;
    refused_call(&mut app, "c1", "ls", "r1");
    refused_call(&mut app, "c2", "pwd", "r2");
    app.apply_event(&approval_pending("r1", "bash"));
    app.apply_event(&approval_pending("r2", "bash"));

    app.on_key(key(KeyCode::Char('y')));
    let text = screen(&mut app);
    assert!(text.contains("allowed"), "the first answer is missing:\n{text}");
    assert!(
        text.matches("needs approval").count() >= 1,
        "the second call settled with the first:\n{text}"
    );

    app.on_key(key(KeyCode::Char('n')));
    let text = screen(&mut app);
    assert!(text.contains("denied"), "the second answer is missing:\n{text}");
    assert!(
        !text.contains("needs approval"),
        "a parked row was left behind:\n{text}"
    );
}

#[test]
fn a_real_failure_is_still_failed() {
    let mut app = app();
    app.apply_event(&tool_start("c1", "read"));
    app.apply_event(&ApiEvent::ToolDone {
        session_id: "s1".to_string(),
        call_id: "c1".to_string(),
        name: "read".to_string(),
        output: String::new(),
        error: Some("Error: read a.txt: no such file or directory".to_string()),
        verdict: None,
    });
    let text = screen(&mut app);
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("no such file"), "{text}");
    assert!(!text.contains("allowed") && !text.contains("denied"), "{text}");
}

#[test]
fn clear_does_not_resurrect_a_parked_label() {
    let mut app = app();
    parked_call(&mut app, "c1", "ls", "r1");
    app.run_local(crate::commands::Local::Clear, "");
    let text = screen(&mut app);
    assert!(
        !text.contains("needs approval"),
        "/clear left the parked label:\n{text}"
    );
    assert!(!text.contains("◐") || text.contains("prompt"), "{text}");
}

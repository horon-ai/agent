//! Copying: what the keys and `/copy` carry, and what the loop writes for them.
//!
//! Every test here drives a real [`App`], and the ones that reach the terminal
//! hand [`crate::loop_io::flush_copy`] a buffer and an environment instead of a
//! standard output, so a run never touches the user's clipboard. What is pinned
//! is the text a copy carries, the single sequence that leaves, and the sentence
//! a terminal that cannot take the sequence gets instead.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Action, App};
use crate::clipboard::{self, MAX_BYTES};
use crate::transcript::Item;

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    App::new("socket · session_a".to_string(), receiver)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn alt(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::ALT)
}

/// Type a whole line and press Enter, as a user would.
fn submit(app: &mut App, line: &str) -> Option<Action> {
    app.input.set_text(line);
    app.on_key(key(KeyCode::Enter))
}

/// The last line the transcript kept, notice or warning.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

/// An environment made of the pairs given, for the gate's lookup.
fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.clone())
    }
}

/// An answer holding one fenced block, then text, then a second block.
fn answer_with_blocks() -> &'static str {
    "here is the first:\n\n```rust\nfn one() {}\n```\n\nand the second:\n\n```text\ntwo\nlines\n```\nthe end"
}

#[test]
fn the_draft_the_line_and_the_newest_answer_are_one_key_away() {
    let mut app = app();
    app.input.set_text("first line\nsecond line");
    app.transcript.push_assistant("the answer");

    // The keys return no action: a copy never leaves the client.
    assert_eq!(app.on_key(alt('d')), None);
    let draft = app.take_copy().expect("alt+d queued a copy");
    assert_eq!(draft.text, "first line\nsecond line");
    assert_eq!(draft.label, "the draft");
    // The draft is still there to edit.
    assert_eq!(app.input.text(), "first line\nsecond line");

    // The cursor sits at the end of the draft, so its line is the second one.
    assert_eq!(app.on_key(alt('k')), None);
    let line = app.take_copy().expect("alt+k queued a copy");
    assert_eq!(line.text, "second line");
    assert_eq!(line.label, "the line under the cursor");

    assert_eq!(app.on_key(alt('l')), None);
    let answer = app.take_copy().expect("alt+l queued a copy");
    assert_eq!(answer.text, "the answer");
    assert_eq!(answer.label, "the last answer");
    assert!(app.take_copy().is_none(), "one keypress queued two copies");
}

#[test]
fn the_newest_code_block_is_the_last_one_and_carries_no_fence() {
    let mut app = app();
    app.transcript.push_assistant(answer_with_blocks());
    assert_eq!(app.on_key(alt('c')), None);
    let copied = app.take_copy().expect("alt+c queued a copy");
    assert_eq!(copied.text, "two\nlines");
    assert_eq!(copied.label, "the newest code block");
}

#[test]
fn a_streamed_answer_is_copyable_before_it_ends() {
    let mut app = app();
    app.apply_event(&horon_agent_sdk::api::ApiEvent::TextDelta {
        session_id: "session_a".into(),
        text: "still arriving".into(),
        message_id: Some("m1".into()),
    });
    app.copy_last_answer();
    assert_eq!(app.take_copy().expect("the stream was copyable").text, "still arriving");
}

#[test]
fn a_prompt_leaves_the_copy_keys_answerable() {
    let mut app = app();
    app.transcript.push_assistant("the answer to read while deciding");
    app.prompt = Some(crate::prompts::Prompt::Approval(crate::prompts::Approval {
        request_id: "req_1".into(),
        action: "bash".into(),
        always: true,
    }));

    assert_eq!(app.on_key(alt('l')), None);
    assert_eq!(
        app.take_copy().expect("a prompt swallowed the copy key").text,
        "the answer to read while deciding"
    );
    assert!(app.prompt.is_some(), "the copy answered the prompt");
}

#[test]
fn the_list_names_the_recent_answers_and_their_code_blocks() {
    let mut app = app();
    // An empty editor, which is the state `/copy` always runs in: Enter took
    // the command line.
    app.transcript.push_assistant(answer_with_blocks());

    assert_eq!(submit(&mut app, "/copy"), None);
    let picker = app.picker.as_ref().expect("/copy opened no list");
    let labels: Vec<&str> = picker.visible().iter().map(|row| row.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "code block 1 of the last answer",
            "code block 2 of the last answer",
            "the last answer",
        ]
    );

    // One row down is the second block; `enter` copies what that row named.
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert!(app.picker.is_none(), "the list stayed open after a choice");
    let copied = app.take_copy().expect("the chosen row queued no copy");
    assert_eq!(copied.text, "two\nlines");
}

#[test]
fn an_empty_copy_says_so_and_queues_nothing() {
    let mut app = app();
    app.copy_draft();
    assert!(app.take_copy().is_none());
    assert!(last_line(&app).contains("holds nothing to copy"), "{}", last_line(&app));

    submit(&mut app, "/copy last");
    assert!(app.take_copy().is_none());
    assert!(last_line(&app).contains("no answer to copy yet"), "{}", last_line(&app));

    // The list with nothing in it is a sentence, not an empty panel that would
    // swallow every key.
    submit(&mut app, "/copy");
    assert!(app.picker.is_none(), "an empty list was opened");
    assert!(last_line(&app).contains("nothing to copy yet"), "{}", last_line(&app));
}

#[test]
fn the_copy_command_answers_the_names_it_lists() {
    let mut app = app();
    app.transcript.push_assistant(answer_with_blocks());

    assert_eq!(submit(&mut app, "/copy last"), None);
    assert_eq!(
        app.take_copy().expect("/copy last queued nothing").text,
        answer_with_blocks()
    );
    assert_eq!(submit(&mut app, "/copy code"), None);
    assert_eq!(app.take_copy().expect("/copy code queued nothing").text, "two\nlines");

    // The draft cannot be copied by a command: Enter took it before the command
    // ran. The sentence names the key in force instead of claiming a copy.
    assert_eq!(submit(&mut app, "/copy draft"), None);
    assert!(app.take_copy().is_none(), "/copy draft claimed a copy");
    assert!(
        last_line(&app).contains("alt+d copies the draft"),
        "{}",
        last_line(&app)
    );
    assert_eq!(submit(&mut app, "/copy line"), None);
    assert!(
        last_line(&app).contains("alt+k copies the line under the cursor"),
        "{}",
        last_line(&app)
    );

    assert_eq!(submit(&mut app, "/copy nope"), None);
    assert!(last_line(&app).contains("unknown choice"), "{}", last_line(&app));
}

#[test]
fn a_supported_terminal_gets_one_sequence_and_a_notice() {
    let mut app = app();
    app.input.set_text("copy me");
    app.copy_draft();

    let mut written = Vec::new();
    let changed = crate::loop_io::flush_copy(&mut app, &mut written, env(&[("TERM_PROGRAM", "iTerm.app")]));
    assert!(changed, "the copy changed nothing");
    let expected = clipboard::sequence(&clipboard::prepare("copy me")).expect("the text carries a sequence");
    assert_eq!(String::from_utf8(written).expect("the sequence is UTF-8"), expected);
    assert_eq!(last_line(&app), "asked the terminal to copy the draft (7 characters)");
    // Written once: a second flush has nothing left to send.
    let mut again = Vec::new();
    assert!(!crate::loop_io::flush_copy(
        &mut app,
        &mut again,
        env(&[("TERM_PROGRAM", "iTerm.app")])
    ));
    assert!(again.is_empty());
}

#[test]
fn a_terminal_that_cannot_take_the_sequence_keeps_the_text_on_screen() {
    let mut app = app();
    app.input.set_text("stay selectable");
    app.copy_draft();

    let mut written = Vec::new();
    let changed = crate::loop_io::flush_copy(&mut app, &mut written, env(&[("TERM_PROGRAM", "Apple_Terminal")]));
    assert!(changed);
    assert!(written.is_empty(), "an unsupported terminal was written to");
    assert!(
        last_line(&app).contains("has not declared OSC 52"),
        "{}",
        last_line(&app)
    );
    assert!(
        last_line(&app).contains("the draft stays on screen"),
        "{}",
        last_line(&app)
    );
    // The draft is untouched, which is what makes it selectable.
    assert_eq!(app.input.text(), "stay selectable");

    // The same text with the emission turned off.
    let mut app = self::app();
    app.input.set_text("stay selectable");
    app.copy_draft();
    let mut written = Vec::new();
    assert!(crate::loop_io::flush_copy(
        &mut app,
        &mut written,
        env(&[("TERM_PROGRAM", "iTerm.app"), (clipboard::ENV, "0")])
    ));
    assert!(written.is_empty(), "HORON_TUI_OSC52=0 still wrote a sequence");
    assert!(last_line(&app).contains("HORON_TUI_OSC52"), "{}", last_line(&app));
}

#[test]
fn no_control_character_of_the_copied_text_reaches_the_sequence() {
    let mut app = app();
    // Model output is not trusted drawing: an escape in it must not become the
    // sequence's own introducer.
    app.transcript.push_assistant("before\u{1b}]0;title\u{7}after");
    app.copy_last_answer();

    let mut written = Vec::new();
    crate::loop_io::flush_copy(&mut app, &mut written, env(&[("TERM_PROGRAM", "WezTerm")]));
    let sequence = String::from_utf8(written).expect("the sequence is UTF-8");
    assert_eq!(sequence.matches('\u{1b}').count(), 1, "one sequence only: {sequence:?}");
    assert_eq!(sequence.matches('\u{7}').count(), 1, "one BEL only: {sequence:?}");
    assert!(
        last_line(&app).contains("2 control characters left out"),
        "{}",
        last_line(&app)
    );
}

#[test]
fn a_copy_over_the_cap_reports_the_cut_and_still_decodes() {
    let mut app = app();
    let text = "x".repeat(MAX_BYTES + 100);
    app.input.set_text(&text);
    app.copy_draft();

    let mut written = Vec::new();
    crate::loop_io::flush_copy(&mut app, &mut written, env(&[("TERM_PROGRAM", "iTerm.app")]));
    let notice = last_line(&app);
    assert!(
        notice.contains(&format!("cut from {} bytes", MAX_BYTES + 100)),
        "{notice}"
    );
    let sequence = String::from_utf8(written).expect("the sequence is UTF-8");
    let payload = sequence
        .strip_prefix("\x1b]52;c;")
        .and_then(|rest| rest.strip_suffix('\u{7}'))
        .expect("a sequence holds the header and one BEL");
    // Base64 of MAX_BYTES bytes is four thirds of it, all in the alphabet.
    assert_eq!(payload.len(), MAX_BYTES.div_ceil(3) * 4);
    assert!(
        payload
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    );
}

#[test]
fn crlf_trailing_blank_and_unclosed_fence_match_block_rules() {
    let mut app = app();
    let answer = "first:\r\n```rust\r\nfn ok() {}\r\n\r\n```\r\n\r\nunfinished:\r\n```rust\r\nfn wip() {}";
    app.transcript.push_assistant(answer);

    app.copy_newest_code();
    let copied = app.take_copy().expect("closed block before unclosed fence copies");
    assert_eq!(copied.text, "fn ok() {}\n");

    assert_eq!(submit(&mut app, "/copy"), None);
    let picker = app.picker.as_ref().expect("/copy opened picker");
    assert_eq!(picker.visible().len(), 2);
    assert_eq!(picker.visible()[0].detail, "rust · 2 lines");
}

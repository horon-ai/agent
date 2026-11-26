//! Reading a key name, the default table, and what `keybindings.toml` changes.
//!
//! A test that names a file writes one in a temporary directory and loads it
//! with [`Loader::at`], so the parse, the warnings and the reload all run the
//! same code the interface runs.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::bindings::{Bindings, Scope, ids};
use super::key::Key;
use super::load::Loader;
use crate::app::{Action, App};
use crate::prompts::{Prompt, Response};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn modified(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn control(character: char) -> KeyEvent {
    modified(KeyCode::Char(character), KeyModifiers::CONTROL)
}

fn parse(text: &str) -> Key {
    Key::parse(text).expect("a valid key name")
}

fn app() -> App {
    let (_sender, receiver) = mpsc::channel();
    App::new("socket · session".to_string(), receiver)
}

/// Write a bindings file and load it.
fn loader_with(contents: &str) -> (tempfile::TempDir, Loader) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("keybindings.toml");
    std::fs::write(&path, contents).expect("write the key bindings");
    let loader = Loader::at(path);
    (directory, loader)
}

#[test]
fn keys_normalize_names_case_order_and_aliases() {
    assert_eq!(parse("Shift+Ctrl+Return"), parse("ctrl+shift+enter"));
    assert_eq!(parse("Return"), parse("enter"));
    assert_eq!(parse("  CONTROL + J "), parse("ctrl+j"));
    assert_eq!(parse("Escape"), parse("esc"));
    assert_eq!(parse("SPACE"), parse("space"));
    // A character key carries its shift in the character itself.
    assert_eq!(parse("shift+j"), parse("j"));
    assert_eq!(format!("{}", parse("shift+alt+ctrl+f5")), "ctrl+alt+shift+f5");

    assert!(Key::parse("ctrl+hyper").is_err());
    assert!(Key::parse("ctrl+ctrl+j").is_err());
    assert!(Key::parse("ctrl+").is_err());
    assert!(Key::parse("").is_err());

    assert_eq!(
        Key::from_event(modified(KeyCode::Char('J'), KeyModifiers::SHIFT)),
        Some(parse("j"))
    );
    assert_eq!(
        Key::from_event(modified(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )),
        Some(parse("ctrl+j"))
    );
    assert_eq!(
        Key::from_event(modified(KeyCode::BackTab, KeyModifiers::SHIFT)),
        Some(parse("shift+tab"))
    );
    assert_eq!(Key::from_event(key(KeyCode::Char(' '))), Some(parse("space")));
    assert_eq!(Key::from_event(key(KeyCode::Insert)), None);
}

#[test]
fn keys_defaults_reproduce_the_shipped_keys() {
    let (bindings, warnings) = Bindings::from_table(&toml::Table::new());
    assert!(warnings.is_empty(), "{warnings:?}");

    let editing = |name: &str| bindings.resolve_editing(&parse(name));
    assert_eq!(editing("enter"), Some(ids::APP_SUBMIT));
    assert_eq!(editing("ctrl+c"), Some(ids::APP_CANCEL));
    assert_eq!(editing("ctrl+d"), Some(ids::APP_QUIT));
    // `esc` interrupts a turn in the app and dismisses a prompt: the same key
    // in two scopes is not a conflict, which is why the defaults warn about
    // nothing.
    assert_eq!(editing("esc"), Some(ids::APP_INTERRUPT));
    assert_eq!(editing("backspace"), Some(ids::EDITOR_BACKSPACE));
    assert_eq!(editing("delete"), Some(ids::EDITOR_DELETE));
    assert_eq!(editing("left"), Some(ids::EDITOR_LEFT));
    assert_eq!(editing("right"), Some(ids::EDITOR_RIGHT));
    assert_eq!(editing("home"), Some(ids::EDITOR_HOME));
    assert_eq!(editing("end"), Some(ids::EDITOR_END));
    assert_eq!(editing("tab"), Some(ids::EDITOR_COMPLETE));
    // The prompt's keys do not leak into the input line.
    assert_eq!(editing("y"), None);

    assert_eq!(bindings.resolve(Scope::Prompt, &parse("y")), Some(ids::PROMPT_ALLOW));
    assert_eq!(
        bindings.resolve(Scope::Prompt, &parse("a")),
        Some(ids::PROMPT_ALLOW_ALWAYS)
    );
    assert_eq!(bindings.resolve(Scope::Prompt, &parse("n")), Some(ids::PROMPT_DENY));
    assert_eq!(
        bindings.resolve(Scope::Prompt, &parse("esc")),
        Some(ids::PROMPT_DISMISS)
    );
    assert_eq!(
        bindings.resolve(Scope::Prompt, &parse("enter")),
        Some(ids::PROMPT_CONFIRM)
    );
    assert_eq!(bindings.resolve(Scope::Prompt, &parse("ctrl+d")), None);

    // An extra modifier answers the binding that names the key alone, which is
    // why `ctrl+left` still moves one character; `alt+left` is spelled out now
    // and moves by a word instead.
    assert_eq!(bindings.resolve_editing(&parse("ctrl+left")), Some(ids::EDITOR_LEFT));
    assert_eq!(
        bindings.resolve_editing(&parse("alt+left")),
        Some(ids::EDITOR_WORD_LEFT)
    );

    // The keys the editor grew, and the ones the shipped actions gained.
    for (id, name) in [
        (ids::EDITOR_NEWLINE, "shift+enter"),
        (ids::EDITOR_NEWLINE, "alt+enter"),
        (ids::EDITOR_NEWLINE, "ctrl+j"),
        (ids::EDITOR_KILL_TO_START, "ctrl+u"),
        (ids::EDITOR_KILL_TO_END, "ctrl+k"),
        (ids::EDITOR_DELETE_WORD, "ctrl+w"),
        (ids::EDITOR_DELETE_WORD, "alt+backspace"),
        (ids::EDITOR_WORD_LEFT, "alt+left"),
        (ids::EDITOR_WORD_RIGHT, "alt+right"),
        (ids::EDITOR_HISTORY_PREV, "up"),
        (ids::EDITOR_HISTORY_NEXT, "down"),
        (ids::EDITOR_HOME, "ctrl+a"),
        (ids::EDITOR_END, "ctrl+e"),
        (ids::EDITOR_LEFT, "ctrl+b"),
        (ids::EDITOR_RIGHT, "ctrl+f"),
        (ids::EDITOR_ACCEPT_SUGGESTION, "ctrl+right"),
        (ids::EDITOR_EXTERNAL, "ctrl+g"),
        (ids::APP_SCROLL_PAGE_UP, "pageup"),
        (ids::APP_SCROLL_PAGE_DOWN, "pagedown"),
        (ids::APP_SCROLL_UP, "shift+up"),
        (ids::APP_SCROLL_DOWN, "shift+down"),
        (ids::APP_SCROLL_BOTTOM, "ctrl+end"),
        (ids::APP_REPAINT, "ctrl+l"),
    ] {
        assert_eq!(editing(name), Some(id), "`{name}` is not bound to `{id}`");
        assert!(
            bindings.keys_for(id).contains(&parse(name)),
            "`{name}` is missing from the keys `{id}` answers to"
        );
    }
}

#[test]
fn keys_defaults_drive_the_app_and_the_prompt() {
    let mut app = app();
    for character in "hi".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Send("hi".into())));
    app.pending_turn = false;
    assert_eq!(app.on_key(control('c')), Some(Action::Quit));
    assert_eq!(app.on_key(control('d')), Some(Action::Quit));

    let mut approval = Prompt::approval("r1".into(), "bash".into());
    assert_eq!(
        approval.on_key(modified(KeyCode::Char('Y'), KeyModifiers::SHIFT)),
        Some(Response::Approval {
            request_id: "r1".into(),
            approved: true,
            always: false
        })
    );
    let mut approval = Prompt::approval("r2".into(), "bash".into());
    assert_eq!(
        approval.on_key(key(KeyCode::Char('a'))),
        Some(Response::Approval {
            request_id: "r2".into(),
            approved: true,
            always: true
        })
    );
    let mut approval = Prompt::approval("r3".into(), "bash".into());
    assert_eq!(
        approval.on_key(key(KeyCode::Char('n'))),
        Some(Response::Approval {
            request_id: "r3".into(),
            approved: false,
            always: false
        })
    );
}

#[test]
fn keys_file_rebinds_submit_to_ctrl_j() {
    let (_directory, loader) = loader_with("\"app.submit\" = [\"ctrl+j\"]\n");
    assert!(loader.warnings().is_empty(), "{:?}", loader.warnings());
    let mut app = app();
    app.bindings = loader.bindings().clone();

    for character in "hi".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    // Enter kept the typed line instead of sending it.
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert_eq!(app.input.text(), "hi");
    assert_eq!(app.on_key(control('j')), Some(Action::Send("hi".into())));
    assert!(app.input.text().is_empty());
}

#[test]
fn keys_empty_array_disables_an_action() {
    let (_directory, loader) = loader_with("\"app.quit\" = []\n");
    assert!(loader.warnings().is_empty(), "{:?}", loader.warnings());
    let mut app = app();
    app.bindings = loader.bindings().clone();
    assert_eq!(app.on_key(control('d')), None);
    assert!(app.running);
    // The other actions are untouched.
    assert_eq!(app.on_key(control('c')), Some(Action::Quit));
}

#[test]
fn ctrl_d_edits_the_draft_before_it_leaves() {
    let mut app = app();
    for character in "ab".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    app.on_key(key(KeyCode::Left));
    // The cursor sits on `b`, so the press takes that character; the draft
    // stays and the client keeps running.
    assert_eq!(app.on_key(control('d')), None);
    assert_eq!(app.input.text(), "a");
    assert_eq!(app.input.cursor(), 1);
    assert!(app.running);
    // At the end there is nothing to delete, and the draft is still kept
    // rather than thrown away.
    assert_eq!(app.on_key(control('d')), None);
    assert_eq!(app.input.text(), "a");
    // An empty draft with nothing running is the key that leaves.
    app.on_key(key(KeyCode::Backspace));
    assert!(app.input.is_empty());
    assert_eq!(app.on_key(control('d')), Some(Action::Quit));
}

#[test]
fn word_delete_keys_take_the_word_before_the_cursor() {
    let mut app = app();
    for character in "안녕하세요 세계".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    // Both spellings of the action walk the same word boundaries, which is
    // what keeps a Korean word from being cut in the middle of a syllable.
    assert_eq!(app.on_key(control('w')), None);
    assert_eq!(app.input.text(), "안녕하세요 ");
    assert_eq!(app.on_key(modified(KeyCode::Backspace, KeyModifiers::ALT)), None);
    assert_eq!(app.input.text(), "");
}

#[test]
fn a_remapped_quit_key_keeps_plain_leaving() {
    let (_directory, loader) = loader_with("\"app.quit\" = [\"ctrl+q\"]\n");
    assert!(loader.warnings().is_empty(), "{:?}", loader.warnings());
    let mut app = app();
    app.bindings = loader.bindings().clone();
    for character in "ab".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    // `ctrl+d` no longer answers `app.quit`: it edits nothing, leaves nothing,
    // and the draft is untouched.
    assert_eq!(app.on_key(control('d')), None);
    assert_eq!(app.input.text(), "ab");
    // The key the file gave the action leaves even with a draft in front of it.
    assert_eq!(app.on_key(control('q')), Some(Action::Quit));
}

#[test]
fn a_file_that_takes_ctrl_d_for_another_action_keeps_that_action() {
    let (_directory, loader) = loader_with("\"app.quit\" = []\n\"editor.delete\" = [\"ctrl+d\"]\n");
    assert!(loader.warnings().is_empty(), "{:?}", loader.warnings());
    let mut app = app();
    app.bindings = loader.bindings().clone();
    // With nothing under the cursor the delete action does nothing at all; the
    // key must not fall back to leaving.
    assert_eq!(app.on_key(control('d')), None);
    assert!(app.running);
    for character in "ab".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    app.on_key(key(KeyCode::Home));
    assert_eq!(app.on_key(control('d')), None);
    assert_eq!(app.input.text(), "b");
}

#[test]
fn scroll_keys_are_not_taken_by_the_draft_or_its_history() {
    let (bindings, warnings) = Bindings::from_table(&toml::Table::new());
    assert!(warnings.is_empty(), "{warnings:?}");
    // Shift is never inferred: the shifted arrow scrolls while the plain one
    // stays with the draft's history, so one modifier cannot mean two things.
    assert_eq!(bindings.resolve_editing(&parse("shift+up")), Some(ids::APP_SCROLL_UP));
    assert_eq!(
        bindings.resolve_editing(&parse("shift+down")),
        Some(ids::APP_SCROLL_DOWN)
    );
    assert_eq!(bindings.resolve_editing(&parse("up")), Some(ids::EDITOR_HISTORY_PREV));
    assert_eq!(bindings.resolve_editing(&parse("down")), Some(ids::EDITOR_HISTORY_NEXT));
    assert_eq!(
        bindings.resolve_editing(&parse("pageup")),
        Some(ids::APP_SCROLL_PAGE_UP)
    );
    assert_eq!(
        bindings.resolve_editing(&parse("ctrl+end")),
        Some(ids::APP_SCROLL_BOTTOM)
    );

    // A file that turns the scroll key off must not hand it to the history: an
    // unshifted binding answers only an unshifted key.
    let (_directory, loader) = loader_with("\"app.scroll_up\" = []\n");
    assert!(loader.warnings().is_empty(), "{:?}", loader.warnings());
    assert_eq!(loader.bindings().resolve_editing(&parse("shift+up")), None);
    assert_eq!(
        loader.bindings().resolve_editing(&parse("up")),
        Some(ids::EDITOR_HISTORY_PREV)
    );
}

#[test]
fn keys_conflict_reports_both_actions_and_keeps_running() {
    let (_directory, loader) = loader_with("\"prompt.deny\" = [\"y\"]\n");
    let warnings = loader.warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let warning = &warnings[0];
    assert!(warning.contains("prompt.allow"), "{warning}");
    assert!(warning.contains("prompt.deny"), "{warning}");
    assert!(warning.contains('y'), "{warning}");

    assert_eq!(
        loader.bindings().resolve(Scope::Prompt, &parse("y")),
        Some(ids::PROMPT_ALLOW)
    );
    let mut approval = Prompt::approval("r1".into(), "bash".into());
    assert_eq!(
        approval.on_key_with(loader.bindings(), key(KeyCode::Char('y'))),
        Some(Response::Approval {
            request_id: "r1".into(),
            approved: true,
            always: false
        })
    );
}

#[test]
fn keys_unknown_key_name_warns_and_is_ignored() {
    let (_directory, loader) = loader_with("\"app.submit\" = [\"ctrl+hyper\"]\n");
    let warnings = loader.warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("ctrl+hyper"), "{warnings:?}");
    assert!(warnings[0].contains("app.submit"), "{warnings:?}");
    assert!(loader.bindings().keys_for(ids::APP_SUBMIT).is_empty());

    let mut app = app();
    app.bindings = loader.bindings().clone();
    for character in "hi".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert_eq!(app.input.text(), "hi");
}

#[test]
fn keys_reload_picks_up_a_file_change() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("keybindings.toml");
    std::fs::write(&path, "\"app.submit\" = [\"ctrl+j\"]\n").expect("write the key bindings");
    let mut loader = Loader::at(path.clone());
    assert_eq!(
        loader.bindings().resolve_editing(&parse("ctrl+j")),
        Some(ids::APP_SUBMIT)
    );
    // An untouched file is not re-read.
    assert!(loader.check_now().is_none());

    std::fs::write(&path, "\"app.submit\" = [\"ctrl+i\"]\n# changed\n").expect("rewrite the key bindings");
    let reload = loader.check_now().expect("the changed file reloads");
    assert!(reload.warnings.is_empty(), "{:?}", reload.warnings);
    assert_eq!(reload.bindings.resolve_editing(&parse("ctrl+i")), Some(ids::APP_SUBMIT));
    // `ctrl+j` no longer sends: it falls back to the default that starts a new
    // line, which is what an unbound submit leaves in its place.
    assert_eq!(
        reload.bindings.resolve_editing(&parse("ctrl+j")),
        Some(ids::EDITOR_NEWLINE)
    );
    assert_eq!(
        loader.bindings().resolve_editing(&parse("ctrl+i")),
        Some(ids::APP_SUBMIT)
    );
}

#[test]
fn keys_broken_file_keeps_the_previous_map() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("keybindings.toml");
    std::fs::write(&path, "\"app.submit\" = [\"ctrl+j\"]\n").expect("write the key bindings");
    let mut loader = Loader::at(path.clone());

    std::fs::write(&path, "\"app.submit\" = [\n").expect("break the key bindings");
    let reload = loader.check_now().expect("the broken file reloads");
    assert_eq!(reload.warnings.len(), 1, "{:?}", reload.warnings);
    assert!(reload.warnings[0].contains("cannot parse"), "{:?}", reload.warnings);
    assert_eq!(reload.bindings.resolve_editing(&parse("ctrl+j")), Some(ids::APP_SUBMIT));
    assert_eq!(
        loader.bindings().resolve_editing(&parse("ctrl+j")),
        Some(ids::APP_SUBMIT)
    );
}

#[test]
fn keys_prompt_keys_can_be_remapped() {
    let (_directory, loader) = loader_with("\"prompt.allow\" = [\"f5\"]\n");
    assert!(loader.warnings().is_empty(), "{:?}", loader.warnings());
    let mut approval = Prompt::approval("r1".into(), "bash".into());
    assert_eq!(
        approval.on_key_with(loader.bindings(), key(KeyCode::F(5))),
        Some(Response::Approval {
            request_id: "r1".into(),
            approved: true,
            always: false
        })
    );
    let mut approval = Prompt::approval("r2".into(), "bash".into());
    assert_eq!(approval.on_key_with(loader.bindings(), key(KeyCode::Char('y'))), None);
}

#[test]
fn keys_editor_keys_can_be_remapped() {
    let (_directory, loader) = loader_with("\"editor.home\" = [\"ctrl+a\"]\n");
    assert!(loader.warnings().is_empty(), "{:?}", loader.warnings());
    let mut app = app();
    app.bindings = loader.bindings().clone();
    for character in "ab".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    assert_eq!(app.on_key(control('a')), None);
    app.on_key(key(KeyCode::Char('X')));
    assert_eq!(app.input.text(), "Xab");
    assert_eq!(app.input.cursor(), 1);
}

#[test]
fn keys_the_repaint_key_can_be_moved_off_ctrl_l() {
    let (_directory, loader) = loader_with("\"app.repaint\" = [\"f5\"]\n");
    assert!(loader.warnings().is_empty(), "{:?}", loader.warnings());
    let mut app = app();
    app.bindings = loader.bindings().clone();
    assert!(!app.take_repaint_request());
    assert_eq!(app.on_key(key(KeyCode::F(5))), None);
    assert!(app.take_repaint_request(), "the remapped key did not ask for a draw");

    // The key it moved off belongs to nobody now: it must not repaint, type
    // into the draft, or reach the harness.
    assert_eq!(app.on_key(control('l')), None);
    assert!(!app.take_repaint_request(), "`ctrl+l` still repainted after the move");
    assert!(app.input.text().is_empty(), "`ctrl+l` typed into the draft");

    // Turning the action off leaves the key doing nothing at all.
    let (_directory, loader) = loader_with("\"app.repaint\" = []\n");
    app.bindings = loader.bindings().clone();
    assert_eq!(app.on_key(control('l')), None);
    assert!(!app.take_repaint_request(), "an unbound `ctrl+l` asked for a draw");
}

fn typed(app: &mut App, text: &str) {
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
}

#[test]
fn shift_enter_starts_a_new_line_instead_of_sending() {
    // The key a kitty-protocol terminal reports once the client asks for
    // disambiguated keys: a new line in the draft, never a send.
    let mut app = app();
    typed(&mut app, "one");
    assert_eq!(app.on_key(modified(KeyCode::Enter, KeyModifiers::SHIFT)), None);
    typed(&mut app, "two");
    assert_eq!(app.input.text(), "one\ntwo");
}

#[test]
fn editor_keys_build_and_cut_a_multi_line_draft() {
    let mut app = app();
    typed(&mut app, "one");
    // `alt+enter` is the newline every terminal can deliver.
    assert_eq!(app.on_key(modified(KeyCode::Enter, KeyModifiers::ALT)), None);
    typed(&mut app, "two");
    assert_eq!(app.input.text(), "one\ntwo");

    // `up` walks the draft's own lines before the history is asked.
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.input.line_of_cursor(), 0);
    assert_eq!(app.input.cursor(), 3, "the caret keeps its column");

    // `ctrl+u` cuts the line under the caret and leaves the other one.
    app.on_key(control('u'));
    assert_eq!(app.input.text(), "\ntwo");
    // At the end of what is left, `ctrl+k` joins the two lines.
    app.on_key(key(KeyCode::End));
    app.on_key(control('k'));
    assert_eq!(app.input.text(), "two");
}

#[test]
fn ctrl_a_and_ctrl_e_move_within_the_line() {
    let mut app = app();
    typed(&mut app, "abc");
    app.on_key(control('a'));
    assert_eq!(app.input.cursor(), 0);
    app.on_key(control('e'));
    assert_eq!(app.input.cursor(), 3);
    app.on_key(control('b'));
    app.on_key(control('b'));
    assert_eq!(app.input.cursor(), 1);
    app.on_key(control('f'));
    assert_eq!(app.input.cursor(), 2);
}

#[test]
fn ctrl_w_deletes_a_word_and_alt_arrows_move_by_word() {
    let mut app = app();
    typed(&mut app, "two words here");
    app.on_key(control('w'));
    assert_eq!(app.input.text(), "two words ");
    app.on_key(modified(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(app.input.cursor(), 4);
    app.on_key(modified(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(app.input.cursor(), 10);
    // Option-← and Option-→ on a macOS terminal arrive as `alt+b` and `alt+f`.
    app.on_key(modified(KeyCode::Char('b'), KeyModifiers::ALT));
    assert_eq!(app.input.cursor(), 4);
    app.on_key(modified(KeyCode::Char('f'), KeyModifiers::ALT));
    assert_eq!(app.input.cursor(), 10);
    // An unbound Alt chord is a shortcut, never text.
    app.on_key(modified(KeyCode::Char('x'), KeyModifiers::ALT));
    assert_eq!(app.input.text(), "two words ");
}

#[test]
fn history_returns_the_line_the_user_was_typing() {
    let mut app = app();
    typed(&mut app, "first");
    assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Send("first".into())));
    app.pending_turn = false;
    typed(&mut app, "half-typed");
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.input.text(), "first", "up recalls the sent draft");
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.input.text(), "half-typed", "down gives the typing back");
}

#[test]
fn up_and_down_do_nothing_with_an_empty_history() {
    let mut app = app();
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Down));
    assert!(app.input.text().is_empty());
    assert_eq!(app.input.cursor(), 0);
}

#[test]
fn a_sent_draft_leaves_no_walk_behind() {
    let mut app = app();
    typed(&mut app, "one");
    app.on_key(key(KeyCode::Enter));
    app.pending_turn = false;
    // An empty submission changes nothing, and the next walk starts at the
    // newest draft rather than at the position the last one stopped on.
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    typed(&mut app, "two");
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.input.text(), "one");
}

#[test]
fn a_paste_is_one_draft_and_never_a_submission() {
    let mut app = app();
    app.on_paste("a\nb\nc");
    assert_eq!(app.input.text(), "a\nb\nc");
    assert_eq!(app.input.line_count(), 3);
    assert!(!app.pending_turn, "a paste must not start a turn");
    assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Send("a\nb\nc".into())));
}

#[test]
fn a_paste_does_not_answer_a_prompt() {
    let mut app = app();
    app.prompt = Some(Prompt::approval("r1".into(), "bash".into()));
    app.on_paste("y");
    assert!(app.prompt.is_some(), "the pasted letter must not approve anything");
    assert!(app.input.text().is_empty(), "and it must not reach the draft");
    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(Action::Approval {
            request_id: "r1".into(),
            approved: true,
            always: false
        }),
        "the prompt still answers its own key"
    );
}

#[test]
fn a_multi_line_draft_is_queued_whole_during_a_turn() {
    let mut app = app();
    app.pending_turn = true;
    app.on_paste("first\nsecond");
    app.on_key(key(KeyCode::Enter));
    assert!(app.input.text().is_empty(), "the draft left the editor");
    assert_eq!(app.queue.len(), 1);
    assert_eq!(app.queue[0].text, "first\nsecond", "and it is queued whole");
}

#[test]
fn a_refused_send_restores_the_whole_multi_line_draft() {
    let mut app = app();
    app.on_paste("line one\nline two");
    app.on_key(key(KeyCode::Enter));
    assert!(app.in_flight.is_some());
    app.send_failed("line one\nline two", "the harness refused it", false);
    assert_eq!(app.input.text(), "line one\nline two", "every line comes back");
    assert_eq!(app.input.line_count(), 2);
}

/// Turn on `editor.suggestions` through a settings file, which is the path the
/// client itself resolves.
fn enable_suggestions(app: &mut App) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let global = directory.path().join("tui.toml");
    std::fs::write(&global, "[editor]\nsuggestions = true\n").expect("write the settings");
    let sources = crate::settings::Sources {
        global,
        project: None,
        configs: Vec::new(),
    };
    let mut warnings = Vec::new();
    let mut layers = crate::settings::layers::LayerSet::new(&sources);
    layers.refresh(&mut warnings);
    assert!(warnings.is_empty(), "{warnings:?}");
    let (settings, resolve_warnings) = layers.resolve(|_: &str| None);
    assert!(resolve_warnings.is_empty(), "{resolve_warnings:?}");
    app.refresh_settings(&settings);
}

/// Type `text` one character at a time, as the terminal delivers it.
fn type_line(app: &mut App, text: &str) {
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
}

#[test]
fn a_suggestion_is_off_by_default_and_never_sends_the_draft() {
    let mut app = app();
    app.history.record("hello world");
    type_line(&mut app, "hel");
    assert_eq!(app.suggestion_text(), None, "the setting is off by default");
    assert_eq!(app.input.text(), "hel");
    // The accept key does nothing while there is no offer: it is not a send,
    // and it is not an edit.
    assert_eq!(app.on_key(modified(KeyCode::Right, KeyModifiers::CONTROL)), None);
    assert_eq!(app.input.text(), "hel");
    assert!(app.in_flight.is_none());
    assert!(app.queue.is_empty());
}

#[test]
fn a_suggestion_is_accepted_without_touching_the_draft_before_the_key() {
    let mut app = app();
    enable_suggestions(&mut app);
    app.history.record("hello world");
    type_line(&mut app, "hel");
    assert_eq!(app.suggestion_text().as_deref(), Some("lo world"));
    assert_eq!(app.input.text(), "hel", "the draft is still what was typed");
    assert_eq!(app.on_key(modified(KeyCode::Right, KeyModifiers::CONTROL)), None);
    assert_eq!(app.input.text(), "hello world", "the key filled the rest");
    assert_eq!(app.suggestion_text(), None, "the accepted offer is spent");
    assert!(app.in_flight.is_none() && app.queue.is_empty());
}

#[test]
fn escape_dismisses_a_suggestion_and_leaves_the_draft_alone() {
    let mut app = app();
    enable_suggestions(&mut app);
    app.history.record("hello");
    app.pending_turn = true;
    type_line(&mut app, "he");
    assert_eq!(app.suggestion_text().as_deref(), Some("llo"));
    assert_eq!(app.on_key(key(KeyCode::Esc)), None);
    assert_eq!(app.input.text(), "he");
    assert_eq!(app.suggestion_text(), None);
    assert!(app.running, "`esc` dismissed the offer instead of leaving");
    assert!(app.in_flight.is_none() && app.queue.is_empty());
}

#[test]
fn editing_withdraws_a_suggestion_and_the_accept_key_cannot_bring_it_back() {
    let mut app = app();
    enable_suggestions(&mut app);
    app.history.record("hello there");
    type_line(&mut app, "hel");
    assert_eq!(app.suggestion_text().as_deref(), Some("lo there"));
    // The old answer named "hel"; the draft now says "help", which no history
    // entry completes, so nothing stale may be drawn or accepted.
    app.on_key(key(KeyCode::Char('p')));
    assert_eq!(app.input.text(), "help");
    assert_eq!(app.suggestion_text(), None);
    app.on_key(modified(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(app.input.text(), "help");
}

#[test]
fn command_arrows_jump_to_either_end_of_the_conversation() {
    assert_eq!(parse("cmd+up").to_string(), "cmd+up");
    assert_eq!(parse("Super+Up"), parse("cmd+up"));
    let mut app = app();
    // A Command chord the terminal reports with the super modifier.
    app.on_key(modified(KeyCode::Up, KeyModifiers::SUPER));
    assert_eq!(app.scroll.offset(), usize::MAX, "cmd+up did not jump to the top");
    app.on_key(modified(KeyCode::Down, KeyModifiers::SUPER));
    assert_eq!(app.scroll.offset(), 0, "cmd+down did not follow the end again");
}

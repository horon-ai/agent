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
    // how alt+left kept moving the cursor before the map existed.
    assert_eq!(bindings.resolve_editing(&parse("alt+left")), Some(ids::EDITOR_LEFT));
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
    assert_eq!(reload.bindings.resolve_editing(&parse("ctrl+j")), None);
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

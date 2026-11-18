//! Skills in the client: the rows they add to the command list, what
//! `/skill:<name>` and a bare name expand to, the draft a chosen row leaves,
//! and the list a session change takes away.
//!
//! Every test drives the code the interface runs: the registry resolves the
//! lines, a real [`App`] answers `/skills`, and the picker rows are the ones the
//! bridge's answer produces.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::{Action, App};
use crate::overlays::picker::Kind;
use crate::sdk_bridge::{AttachedSession, SkillRow};
use crate::transcript::Item;

/// A registry over a temporary user directory, with the skills given.
fn registry_with(skills: &[(&str, &str)]) -> Registry {
    let user = tempfile::tempdir().expect("a user command directory");
    let mut state = Registry::new(user.path().to_path_buf());
    state.set_skills(
        skills
            .iter()
            .map(|(name, description)| Skill {
                name: (*name).to_string(),
                description: (*description).to_string(),
            })
            .collect(),
    );
    state
}

/// A registry pointed at a project directory the caller fills first.
fn project_registry(project: &std::path::Path) -> Registry {
    let mut state = Registry::new(project.join("user-commands"));
    state.sync(Some(project));
    state
}

/// Write one command file where a project keeps them.
fn write_project_command(project: &std::path::Path, name: &str, text: &str) {
    let directory = project.join(".horon").join("commands");
    std::fs::create_dir_all(&directory).expect("create the project command directory");
    std::fs::write(directory.join(format!("{name}.md")), text).expect("write the command file");
}

/// An app whose registry is handed to it, so no real home directory is read.
fn app_with(state: Registry) -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("test".to_string(), receiver);
    app.commands = state;
    app
}

/// One key press with no modifiers.
fn press(app: &mut App, code: KeyCode) -> Option<Action> {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Type a whole line and press Enter, as a user would.
fn type_line(app: &mut App, text: &str) -> Option<Action> {
    for character in text.chars() {
        assert!(
            press(app, KeyCode::Char(character)).is_none(),
            "typing a character produced an action"
        );
    }
    press(app, KeyCode::Enter)
}

/// The last line a typed command left in the transcript.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

/// One skill as the bridge reports it.
fn reported(name: &str, description: &str) -> SkillRow {
    SkillRow {
        name: name.to_string(),
        description: description.to_string(),
        path: Some(format!("/skills/{name}/SKILL.md")),
        allowed_tools: Vec::new(),
    }
}

/// A session the bridge answered with, holding nothing.
fn attached(session_id: &str) -> AttachedSession {
    AttachedSession {
        description: format!("socket · {session_id}"),
        session_id: session_id.to_string(),
        status: "idle".to_string(),
        model: None,
        effort: None,
        history: Vec::new(),
        title: None,
    }
}

#[test]
fn listed_skills_join_the_command_list_after_the_files() {
    let project = tempfile::tempdir().expect("a project directory");
    write_project_command(project.path(), "deploy", "ship it");
    let mut state = project_registry(project.path());
    state.set_skills(vec![
        Skill {
            name: "demo-notes".to_string(),
            description: "write notes".to_string(),
        },
        Skill {
            name: "demo-review".to_string(),
            description: "review a diff".to_string(),
        },
    ]);
    let entries = state.entries();
    let deploy = entries
        .iter()
        .position(|entry| entry.name == "deploy")
        .expect("the command file");
    let notes = entries
        .iter()
        .position(|entry| entry.name == "skill:demo-notes")
        .expect("the first skill");
    let review = entries
        .iter()
        .position(|entry| entry.name == "skill:demo-review")
        .expect("the second skill");
    assert!(
        deploy < notes && notes < review,
        "the sections are out of order: {entries:?}"
    );
    assert_eq!(entries[notes].group, Group::Skill);
    assert_eq!(entries[notes].description, "write notes");
    let help = state.help();
    assert!(help.contains("skills:"), "no skills section:\n{help}");
    assert!(help.contains("/skill:demo-notes"), "a listed skill is missing:\n{help}");
}

#[test]
fn an_invocation_without_a_request_reads_the_skill() {
    let state = registry_with(&[("demo-review", "review a diff")]);
    assert_eq!(
        state.resolve("/skill:demo-review"),
        Invocation::Expanded {
            display: "/skill:demo-review".to_string(),
            text: "Read skill://demo-review and follow its instructions.".to_string(),
        }
    );
}

#[test]
fn an_invocation_carries_the_request_typed_after_it() {
    let state = registry_with(&[("demo-review", "review a diff")]);
    assert_eq!(
        state.resolve("/skill:demo-review check this"),
        Invocation::Expanded {
            display: "/skill:demo-review check this".to_string(),
            text: "Read skill://demo-review and follow its instructions for this request: check this".to_string(),
        }
    );
}

#[test]
fn an_unknown_skill_is_an_error_rather_than_a_message() {
    let state = registry_with(&[("demo-review", "review a diff")]);
    assert_eq!(
        state.resolve("/skill:nope"),
        Invocation::Error("unknown skill `nope`; /skills lists them".to_string())
    );
}

#[test]
fn a_bare_skill_name_expands_where_nothing_else_owns_it() {
    let state = registry_with(&[("demo-review", "review a diff")]);
    assert_eq!(
        state.resolve("/demo-review fix the build"),
        Invocation::Expanded {
            display: "/demo-review fix the build".to_string(),
            text: "Read skill://demo-review and follow its instructions for this request: fix the build".to_string(),
        }
    );
}

#[test]
fn a_builtin_or_harness_name_is_not_taken_by_a_skill() {
    let state = registry_with(&[
        ("help", "a skill under a built-in name"),
        ("review", "a skill under a harness name"),
    ]);
    assert_eq!(
        state.resolve("/help"),
        Invocation::Local {
            command: Local::Help,
            arguments: String::new()
        }
    );
    // `review` is the harness's own command and still reaches it.
    match state.resolve("/review") {
        Invocation::Server { command, .. } => assert_eq!(command, RuntimeCommand::Review),
        other => panic!("a skill took the harness's name: {other:?}"),
    }
    // Both skills stay reachable under their own address.
    match state.resolve("/skill:help") {
        Invocation::Expanded { text, .. } => assert!(text.starts_with("Read skill://help"), "unexpected text: {text}"),
        other => panic!("`/skill:help` did not read the skill: {other:?}"),
    }
}

#[test]
fn a_command_file_keeps_its_name_against_a_skill() {
    let project = tempfile::tempdir().expect("a project directory");
    write_project_command(project.path(), "deploy", "ship $ARGUMENTS");
    let mut state = project_registry(project.path());
    state.set_skills(vec![Skill {
        name: "deploy".to_string(),
        description: "a skill under the file's name".to_string(),
    }]);
    match state.resolve("/deploy now") {
        Invocation::Expanded { text, .. } => assert_eq!(text, "ship now", "the skill took the file's name"),
        other => panic!("expected the file's expansion: {other:?}"),
    }
}

#[test]
fn a_name_the_shortcut_cannot_spell_is_still_listed() {
    let state = registry_with(&[("Demo Review", "with capitals and a space")]);
    assert!(
        state.entries().iter().any(|entry| entry.name == "skill:Demo Review"),
        "the skill is not listed"
    );
    match state.resolve("/Demo Review") {
        Invocation::Error(message) => assert!(message.contains("Demo"), "the error did not name the word: {message}"),
        other => panic!("a name with a space is not a bare command: {other:?}"),
    }
    match state.resolve("/skill:Demo Review") {
        Invocation::Expanded { text, .. } => {
            assert!(text.starts_with("Read skill://Demo Review"), "unexpected text: {text}");
        }
        other => panic!("the skill address did not match: {other:?}"),
    }
}

#[test]
fn the_longest_listed_name_wins_a_skill_address() {
    let state = registry_with(&[("demo", "the short name"), ("demo review", "the longer name")]);
    assert_eq!(
        state.resolve("/skill:demo review fix it"),
        Invocation::Expanded {
            display: "/skill:demo review fix it".to_string(),
            text: "Read skill://demo review and follow its instructions for this request: fix it".to_string(),
        }
    );
    // A word that continues the short name is not a boundary, so the short
    // name answers and the word is the request.
    assert_eq!(
        state.resolve("/skill:demo reviewer"),
        Invocation::Expanded {
            display: "/skill:demo reviewer".to_string(),
            text: "Read skill://demo and follow its instructions for this request: reviewer".to_string(),
        }
    );
    assert_eq!(
        state.resolve("/skill:demo"),
        Invocation::Expanded {
            display: "/skill:demo".to_string(),
            text: "Read skill://demo and follow its instructions.".to_string(),
        }
    );
}

#[test]
fn the_slash_popup_offers_the_skills() {
    let mut app = app_with(registry_with(&[
        ("demo-notes", "write notes"),
        ("demo-review", "review a diff"),
    ]));
    for character in "/demo".chars() {
        assert!(press(&mut app, KeyCode::Char(character)).is_none());
    }
    let names: Vec<&str> = app.slash.matches().iter().map(|entry| entry.name.as_str()).collect();
    assert!(
        names.contains(&"skill:demo-review"),
        "the popup does not offer the skill: {names:?}"
    );
    // The built-in still submits on enter: the skill rows are named `skill:…`,
    // and nothing else begins with `skills`.
    app.input.clear();
    assert_eq!(type_line(&mut app, "/skills"), Some(Action::Skills));
    assert!(press(&mut app, KeyCode::Esc).is_none(), "escape did not close the list");
    // A full skill address submits too: no other row begins with it, so the
    // popup's settled rule leaves the line alone.
    assert_eq!(
        type_line(&mut app, "/skill:demo-review"),
        Some(Action::Send(
            "Read skill://demo-review and follow its instructions.".to_string()
        ))
    );
}

#[test]
fn a_chosen_skill_becomes_a_draft_and_is_not_sent() {
    let mut app = app_with(registry_with(&[]));
    app.show_skills(vec![
        reported("demo-notes", "write notes"),
        reported("demo-review", "review a diff"),
    ]);
    assert_eq!(type_line(&mut app, "/skills"), Some(Action::Skills));
    {
        let picker = app.picker.as_ref().expect("/skills did not open the list");
        assert_eq!(picker.kind(), Kind::Skills);
        assert_eq!(
            picker.selected_label(),
            Some("demo-notes"),
            "the cursor is not on the first row"
        );
    }
    // Enter fills the draft and sends nothing.
    assert!(press(&mut app, KeyCode::Enter).is_none());
    assert_eq!(app.input.text(), "/skill:demo-notes ");
    assert!(!app.pending_turn, "choosing a skill started a turn");
    // What is typed after the invocation is the request it carries.
    for character in "check this".chars() {
        assert!(press(&mut app, KeyCode::Char(character)).is_none());
    }
    assert_eq!(app.input.text(), "/skill:demo-notes check this");
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Some(Action::Send(
            "Read skill://demo-notes and follow its instructions for this request: check this".to_string()
        ))
    );
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| item == &Item::User("/skill:demo-notes check this".to_string())),
        "the typed line is not in the conversation"
    );
}

#[test]
fn a_skill_message_queues_during_a_turn_like_any_message() {
    let mut app = app_with(registry_with(&[("demo-notes", "write notes")]));
    app.pending_turn = true;
    app.session_status = Some("generating".to_string());
    assert_eq!(type_line(&mut app, "/skill:demo-notes check this"), None);
    assert_eq!(app.queue.len(), 1, "the skill message did not wait for the turn");
    assert!(
        !app.transcript.items().iter().any(|item| matches!(item, Item::User(_))),
        "a queued skill was drawn as sent"
    );
    // The turn ends and the queue releases it, exactly as it releases a typed
    // line: nothing about the skill path reorders that device.
    app.pending_turn = false;
    assert_eq!(
        app.take_queued(),
        Some(Action::Send(
            "Read skill://demo-notes and follow its instructions for this request: check this".to_string()
        ))
    );
}

#[test]
fn the_preview_says_what_enter_does_with_the_row() {
    let mut app = app_with(registry_with(&[]));
    app.show_skills(vec![reported("demo-notes", "write notes")]);
    assert_eq!(type_line(&mut app, "/skills"), Some(Action::Skills));
    let picker = app.picker.as_ref().expect("/skills did not open the list");
    let row = picker.selected_row().expect("a selected row");
    assert_eq!(
        row.preview,
        vec![
            "write notes".to_string(),
            "path: /skills/demo-notes/SKILL.md".to_string(),
            "enter puts /skill:demo-notes in the draft".to_string(),
        ]
    );
}

#[test]
fn a_session_change_drops_the_skills_of_the_one_left() {
    let mut app = app_with(registry_with(&[]));
    app.show_skills(vec![reported("demo-review", "review a diff")]);
    assert!(matches!(
        app.commands.resolve("/skill:demo-review"),
        Invocation::Expanded { .. }
    ));
    app.attach_session(attached("session_b"), false);
    assert!(app.skills.is_empty(), "the old session's skills survived the move");
    assert_eq!(
        app.commands.resolve("/skill:demo-review"),
        Invocation::Error("unknown skill `demo-review`; /skills lists them".to_string())
    );
}

#[test]
fn help_names_the_skills_command() {
    let mut app = app_with(registry_with(&[]));
    assert!(type_line(&mut app, "/help skills").is_none());
    let line = last_line(&app);
    assert!(line.contains("/skills"), "/help skills did not name /skills: {line}");
    assert!(line.contains("built-in commands"), "the group is missing: {line}");
}

#[test]
fn an_unknown_skill_is_reported_and_nothing_is_sent() {
    let mut app = app_with(registry_with(&[("demo-review", "review a diff")]));
    assert_eq!(type_line(&mut app, "/skill:nope"), None);
    assert_eq!(last_line(&app), "unknown skill `nope`; /skills lists them");
    assert!(
        !app.transcript.items().iter().any(|item| matches!(item, Item::User(_))),
        "a refused skill reached the harness"
    );
    assert!(!app.pending_turn);
    // The next ordinary line is a message again.
    assert_eq!(type_line(&mut app, "hello"), Some(Action::Send("hello".to_string())));
}

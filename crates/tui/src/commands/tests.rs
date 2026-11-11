//! The registry's precedence, file discovery, substitution and completion.
//!
//! Every test drives the same code the interface runs: a temporary directory
//! stands in for the user's command folder and another for the project, so the
//! discovery, the shadowing and the reload are exercised as written. The last
//! group types into a real [`App`] and checks the action a line produces.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::RuntimeCommand;

use super::*;
use crate::app::{Action, App};
use crate::transcript::Item;

/// A registry over an empty temporary user directory and project directory.
fn registry() -> (tempfile::TempDir, tempfile::TempDir, Registry) {
    let user = tempfile::tempdir().expect("a user command directory");
    let project = tempfile::tempdir().expect("a project directory");
    let mut state = Registry::new(user.path().to_path_buf());
    state.sync(Some(project.path()));
    (user, project, state)
}

/// Write one command file into a command directory.
fn write_command(directory: &Path, name: &str, text: &str) -> PathBuf {
    let path = directory.join(format!("{name}.md"));
    std::fs::write(&path, text).expect("write the command file");
    path
}

/// The project's command directory, `<project>/.horon/commands`, created.
fn project_commands(project: &Path) -> PathBuf {
    let directory = project.join(".horon").join("commands");
    std::fs::create_dir_all(&directory).expect("create the project command directory");
    directory
}

/// Write one command file where a project keeps them.
fn write_project_command(project: &Path, name: &str, text: &str) -> PathBuf {
    write_command(&project_commands(project), name, text)
}

/// An app whose registry is handed to it, so no real home directory is read.
fn app_with(state: Registry) -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("test".to_string(), receiver);
    app.commands = state;
    app
}

/// A registry pointed at a fresh project, with the project directory returned
/// so the caller can write into it before loading.
fn app_fixture() -> (tempfile::TempDir, tempfile::TempDir, App) {
    let (user, project, state) = registry();
    (user, project, app_with(state))
}

/// Type a whole line and press Enter, as a user would.
fn type_line(app: &mut App, text: &str) -> Option<Action> {
    for character in text.chars() {
        let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
        assert!(app.on_key(key).is_none(), "typing a character produced an action");
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

fn press(app: &mut App, code: KeyCode) -> Option<Action> {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn commands_builtin_names_win_over_files() {
    let (user, _project, mut state) = registry();
    write_command(user.path(), "quit", "this must never run");
    state.load();
    assert_eq!(
        state.resolve("/quit"),
        Invocation::Local {
            command: Local::Quit,
            arguments: String::new()
        }
    );
    let shadowed = state.files().iter().find(|file| file.name == "quit");
    match shadowed {
        Some(file) => assert!(file.shadowed, "/quit must shadow the file"),
        None => panic!("the quit file was not discovered"),
    }
}

#[test]
fn commands_project_file_beats_the_user_file() {
    let (user, project, mut state) = registry();
    write_command(user.path(), "review", "user body");
    write_project_command(project.path(), "review", "project body");
    state.load();
    match state.resolve("/review") {
        Invocation::Expanded { text, .. } => assert_eq!(text, "project body"),
        other => panic!("the project file did not win: {other:?}"),
    }
    assert_eq!(state.files().len(), 1);
    assert_eq!(state.files()[0].source, Source::Project);
}

#[test]
fn commands_frontmatter_supplies_description_and_argument_hint() {
    let (user, project, mut state) = registry();
    write_project_command(
        project.path(),
        "deploy",
        "---\ndescription: Ship the current branch\nargument-hint: <environment>\n---\nship it",
    );
    write_command(user.path(), "plain", "first line is the description\nsecond line");
    state.load();
    let deploy = state
        .files()
        .iter()
        .find(|file| file.name == "deploy")
        .expect("deploy was not discovered");
    assert_eq!(deploy.description, "Ship the current branch");
    assert_eq!(deploy.argument_hint.as_deref(), Some("<environment>"));
    assert_eq!(deploy.body, "ship it");
    let plain = state
        .files()
        .iter()
        .find(|file| file.name == "plain")
        .expect("plain was not discovered");
    assert_eq!(plain.description, "first line is the description");
    assert!(state.help().contains("/deploy <environment>  Ship the current branch"));
}

#[test]
fn commands_arguments_and_positional_substitution() {
    let (_user, project, mut state) = registry();
    write_project_command(project.path(), "review", "review $1 then $2; all: $ARGUMENTS");
    state.load();
    match state.resolve("/review src tests") {
        Invocation::Expanded { display, text } => {
            assert_eq!(display, "/review src tests");
            assert_eq!(text, "review src then tests; all: src tests");
        }
        other => panic!("expected an expansion: {other:?}"),
    }
    // A missing positional becomes empty instead of staying literal.
    match state.resolve("/review src") {
        Invocation::Expanded { text, .. } => assert_eq!(text, "review src then ; all: src"),
        other => panic!("expected an expansion: {other:?}"),
    }
    assert_eq!(substitute("$1 $10 $x", "a"), "a $10 $x");
    // The argument string is trimmed, and substitution never rescans it.
    assert_eq!(substitute("[ $ARGUMENTS ]", "  spaced  "), "[ spaced ]");
    assert_eq!(substitute("$1", "$2"), "$2");
}

#[test]
fn commands_review_with_a_project_file_expands_to_a_message() {
    // The QA case: `/review src` with a project command file sends the
    // substituted body, not `RuntimeCommand::Review`.
    let (user, project, mut state) = registry();
    write_project_command(project.path(), "review", "Review these paths: $ARGUMENTS");
    state.load();
    match state.resolve("/review src") {
        Invocation::Expanded { display, text } => {
            assert_eq!(display, "/review src");
            assert_eq!(text, "Review these paths: src");
        }
        other => panic!("`/review src` did not run the project command: {other:?}"),
    }
    assert!(user.path().exists());
}

#[test]
fn commands_server_names_follow_the_protocol() {
    let (_user, _project, state) = registry();
    assert_eq!(
        state.resolve("/skills"),
        Invocation::Server {
            command: RuntimeCommand::Skills,
            arguments: Vec::new()
        }
    );
    match state.resolve("/context notes") {
        Invocation::Server { command, arguments } => {
            assert_eq!(command, RuntimeCommand::Context);
            assert_eq!(arguments, vec!["notes".to_string()]);
        }
        other => panic!("expected a server command: {other:?}"),
    }
    // The names come from serde, so every variant is reachable and unique.
    let names = server_commands();
    assert_eq!(names.len(), SERVER_VARIANTS.len());
    let mut unique: Vec<&str> = names.iter().map(|(name, _)| name.as_str()).collect();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len());
    for (name, _) in names {
        assert!(!name.is_empty() && !name.contains(char::is_whitespace));
    }
}

#[test]
fn commands_shadowed_file_is_listed_as_shadowed() {
    let (user, _project, mut state) = registry();
    write_command(user.path(), "help", "a file the built-in hides");
    state.load();
    let help = state.help();
    assert!(
        help.contains("/help  a file the built-in hides  (shadowed by the built-in)"),
        "shadowing is not reported:\n{help}"
    );
    // The built-in still answers, and the file never runs.
    assert_eq!(
        state.resolve("/help"),
        Invocation::Local {
            command: Local::Help,
            arguments: String::new()
        }
    );
    // A shadowed file is not offered as a completion either.
    let completion = state.complete("/he").expect("a completion for /he");
    assert_eq!(completion.matches, vec!["help".to_string()]);
}

#[test]
fn commands_unknown_slash_reports_and_sends_nothing() {
    let (_user, _project, state) = registry();
    match state.resolve("/nosuchthing") {
        Invocation::Error(message) => {
            assert!(message.contains("/nosuchthing"), "unhelpful error: {message}");
        }
        other => panic!("an unknown command must not become a message: {other:?}"),
    }
}

#[test]
fn commands_double_slash_escapes_a_literal_slash() {
    let (_user, _project, state) = registry();
    assert_eq!(
        state.resolve("//usr/bin/env is a path"),
        Invocation::Message("/usr/bin/env is a path".to_string())
    );
    // Text without a leading slash is a message, unchanged.
    assert_eq!(
        state.resolve("hello /world"),
        Invocation::Message("hello /world".to_string())
    );
    // A lone slash is prose, not a command.
    assert_eq!(state.resolve("/"), Invocation::Message("/".to_string()));
}

#[test]
fn commands_completion_offers_prefixes_and_the_common_prefix() {
    // File names only, so no harness command can join the match set.
    let (user, _project, mut state) = registry();
    write_command(user.path(), "alpha", "body");
    write_command(user.path(), "alpine", "body");
    write_command(user.path(), "alps", "body");
    state.load();

    let completion = state.complete("/al").expect("a completion for /al");
    assert_eq!(
        completion.matches,
        vec!["alpha".to_string(), "alpine".to_string(), "alps".to_string()]
    );
    assert_eq!(completion.insert, "/alp");

    // The longest common prefix already typed changes nothing.
    let completion = state.complete("/alp").expect("a completion for /alp");
    assert_eq!(completion.insert, "/alp");

    // A unique match completes whole.
    let completion = state.complete("/alps").expect("a completion for /alps");
    assert_eq!(completion.insert, "/alps");

    let completion = state.complete("/qui").expect("a completion for /qui");
    assert_eq!(completion.matches, vec!["quit".to_string()]);
    assert_eq!(completion.insert, "/quit");

    // An argument position and prose are not completed.
    assert!(state.complete("/alpha src").is_none());
    assert!(state.complete("hello").is_none());
    assert!(state.complete("/zzz").is_none());
}

#[test]
fn commands_reload_only_when_the_directory_changes() {
    let (user, _project, mut state) = registry();
    assert!(!state.check_now(), "an unchanged directory must not re-read");
    assert!(!state.tick(), "the interval has not elapsed, so nothing is stat-ed");
    write_command(user.path(), "fresh", "body");
    assert!(state.check_now(), "a new file must be picked up");
    assert!(state.files().iter().any(|file| file.name == "fresh"));
    // A second check with nothing changed does no work.
    assert!(!state.check_now());
}

#[test]
fn commands_separate_frontmatter_and_body() {
    let parsed = parse::parse("---\ndescription: \"quoted\"\nargument-hint: <x>\nunknown: y\n---\nbody $1");
    assert_eq!(parsed.description.as_deref(), Some("quoted"));
    assert_eq!(parsed.argument_hint.as_deref(), Some("<x>"));
    assert_eq!(parsed.body, "body $1");
    // An unclosed block is body text, not frontmatter that eats the file.
    let parsed = parse::parse("---\ndescription: x\nnever closed");
    assert!(parsed.description.is_none());
    assert!(parsed.body.contains("never closed"));
    assert_eq!(fallback_description("\n\n  first line  \nmore"), "first line");
}

#[test]
fn commands_typed_lines_become_actions() {
    let (_user, project, mut app) = app_fixture();
    write_project_command(project.path(), "review", "Review these paths: $ARGUMENTS");
    app.commands.load();

    // A harness command keeps its arguments.
    assert_eq!(
        type_line(&mut app, "/skills"),
        Some(Action::Runtime {
            command: RuntimeCommand::Skills,
            arguments: Vec::new()
        })
    );
    match type_line(&mut app, "/context notes") {
        Some(Action::Runtime { command, arguments }) => {
            assert_eq!(command, RuntimeCommand::Context);
            assert_eq!(arguments, vec!["notes".to_string()]);
        }
        other => panic!("`/context notes` did not reach the harness: {other:?}"),
    }

    // A project command file becomes the message, and the typed line is the
    // user's transcript line.
    assert_eq!(
        type_line(&mut app, "/review src"),
        Some(Action::Send("Review these paths: src".to_string()))
    );
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| item == &Item::User("/review src".to_string()))
    );

    // `/help` answers locally and names the commands.
    assert_eq!(type_line(&mut app, "/help"), None);
    let help = app
        .transcript
        .items()
        .iter()
        .find_map(|item| match item {
            Item::Notice(text) if text.contains("harness commands:") => Some(text.clone()),
            _ => None,
        })
        .expect("/help did not print the command list");
    assert!(help.contains("/quit"), "/help does not name /quit:\n{help}");

    // `/clear` empties the transcript pane.
    assert!(!app.transcript.items().is_empty());
    assert_eq!(type_line(&mut app, "/clear"), None);
    assert!(
        app.transcript.items().is_empty(),
        "/clear left the transcript populated"
    );
}

#[test]
fn commands_model_maps_to_the_model_path() {
    let (_user, _project, mut app) = app_fixture();
    // `/model` with no argument asks for the list; a name switches.
    assert_eq!(type_line(&mut app, "/model"), Some(Action::Model(None)));
    assert_eq!(
        type_line(&mut app, "/model gpt-5"),
        Some(Action::Model(Some("gpt-5".to_string())))
    );
}

#[test]
fn commands_unknown_slash_sends_no_action() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/nosuchthing"), None);
    let warned = app
        .transcript
        .items()
        .iter()
        .any(|item| matches!(item, Item::Warning(text) if text.contains("/nosuchthing")));
    assert!(
        warned,
        "an unknown command was not reported: {:?}",
        app.transcript.items()
    );
    // Nothing was recorded as a sent message.
    assert!(!app.transcript.items().iter().any(|item| matches!(item, Item::User(_))));
    assert!(!app.pending_turn);
}

#[test]
fn commands_tab_completes_the_typed_prefix() {
    let (user, _project, mut app) = app_fixture();
    write_command(user.path(), "alpha", "body");
    write_command(user.path(), "alpine", "body");
    write_command(user.path(), "alps", "body");
    app.commands.load();

    for character in "/al".chars() {
        assert!(press(&mut app, KeyCode::Char(character)).is_none());
    }
    assert_eq!(app.input.text(), "/al");
    assert_eq!(app.completion_hint().as_deref(), Some("alpha  alpine  alps"));

    // Tab inserts the longest common prefix and keeps the line editable.
    assert_eq!(press(&mut app, KeyCode::Tab), None);
    assert_eq!(app.input.text(), "/alp");
    assert_eq!(app.completion_hint().as_deref(), Some("alpha  alpine  alps"));

    // A unique match completes whole, and prose gets no hint.
    app.input.clear();
    for character in "/qu".chars() {
        assert!(press(&mut app, KeyCode::Char(character)).is_none());
    }
    assert_eq!(press(&mut app, KeyCode::Tab), None);
    assert_eq!(app.input.text(), "/quit");
    app.input.clear();
    for character in "hi".chars() {
        assert!(press(&mut app, KeyCode::Char(character)).is_none());
    }
    assert_eq!(app.completion_hint(), None);
}

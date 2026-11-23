//! Package commands in the client: the names the harness listed, the line each
//! answers to, and what its answer does to the conversation.
//!
//! Every test drives the code the interface runs: the registry resolves the
//! lines a user types, `/help` lists the same entries, and a real [`App`] takes
//! the answer the bridge would deliver. Nothing here talks to a daemon.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::RuntimeCommand;
use horon_agent_sdk::api::ApiEvent;

use super::*;
use crate::app::{Action, App};
use crate::sdk_bridge::PackageOutcome;
use crate::transcript::Item;

/// One package command as the harness's list reports it.
fn command(package_id: &str, command: &str, description: &str) -> PackageCommand {
    PackageCommand {
        name: format!("{package_id}:{command}"),
        package_id: package_id.to_string(),
        command: command.to_string(),
        argument_hint: None,
        description: description.to_string(),
    }
}

/// A registry over a temporary user directory, with the package commands given.
fn registry_with(packages: Vec<PackageCommand>) -> Registry {
    let user = tempfile::tempdir().expect("a user command directory");
    let mut state = Registry::new(user.path().to_path_buf());
    state.set_package_commands(packages);
    state
}

/// A registry pointed at a project directory, so its command files are read.
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

/// Type a whole line and press Enter, as a user would.
fn type_line(app: &mut App, text: &str) -> Option<Action> {
    for character in text.chars() {
        let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
        assert!(app.on_key(key).is_none(), "typing a character produced an action");
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

/// The last line a command left in the transcript.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

/// The turn-end event that releases the queue.
fn end_turn(app: &mut App) {
    app.apply_event(&ApiEvent::TurnDone {
        session_id: "s1".to_string(),
        pending_soft_interrupts: None,
    });
}

#[test]
fn a_package_command_resolves_with_the_words_after_it() {
    let state = registry_with(vec![command("demo", "hello", "greet one person")]);
    assert_eq!(
        state.resolve("/demo:hello Ada Lovelace"),
        Invocation::Package {
            package_id: "demo".to_string(),
            command: "hello".to_string(),
            arguments: "Ada Lovelace".to_string(),
            display: "/demo:hello Ada Lovelace".to_string(),
        }
    );
    // The display is the trimmed line the user typed, so the transcript can
    // show the invocation rather than the sentence it expands to.
    assert_eq!(
        state.resolve("  /demo:hello  "),
        Invocation::Package {
            package_id: "demo".to_string(),
            command: "hello".to_string(),
            arguments: String::new(),
            display: "/demo:hello".to_string(),
        }
    );
}

#[test]
fn a_name_a_builtin_or_the_harness_owns_never_reaches_a_package_command() {
    // A package command is the last source to answer: a list that claimed a
    // built-in name or a harness command's name must not take the address over.
    // (A command file's claim is the next test's subject, because it needs a
    // directory on disk.)
    let mut builtin = command("demo", "skills", "claim a built-in name");
    builtin.name = "skills".to_string();
    let mut harness = command("demo", "plan", "claim a harness name");
    harness.name = "plan".to_string();
    let state = registry_with(vec![builtin, harness]);

    assert_eq!(
        state.resolve("/skills"),
        Invocation::Local {
            command: Local::Skills,
            arguments: String::new(),
        }
    );
    assert_eq!(
        state.resolve("/plan now"),
        Invocation::Server {
            command: RuntimeCommand::Plan,
            arguments: vec!["now".to_string()],
        }
    );
}

#[test]
fn a_project_command_file_outranks_a_package_command_of_the_same_name() {
    let project = tempfile::tempdir().expect("a project directory");
    write_project_command(project.path(), "review", "Review these paths: $ARGUMENTS");
    let mut state = project_registry(project.path());
    let mut file = command("demo", "review", "a package command of the same name");
    file.name = "review".to_string();
    state.set_package_commands(vec![file]);

    assert_eq!(
        state.resolve("/review src"),
        Invocation::Expanded {
            display: "/review src".to_string(),
            text: "Review these paths: src".to_string(),
        }
    );
    // A name no other source spells still reaches the package.
    state.set_package_commands(vec![command("demo", "hello", "greet one person")]);
    assert_eq!(
        state.resolve("/demo:hello"),
        Invocation::Package {
            package_id: "demo".to_string(),
            command: "hello".to_string(),
            arguments: String::new(),
            display: "/demo:hello".to_string(),
        }
    );
}

#[test]
fn completion_offers_the_names_the_harness_listed() {
    let state = registry_with(vec![
        command("demo", "hello", "greet one person"),
        command("demo", "lint", "run the linter"),
    ]);
    assert_eq!(
        state.complete("/demo:h"),
        Some(Completion {
            matches: vec!["demo:hello".to_string()],
            insert: "/demo:hello".to_string(),
        })
    );
    assert_eq!(
        state.complete("/demo:").map(|completion| completion.matches),
        Some(vec!["demo:hello".to_string(), "demo:lint".to_string()])
    );
}

#[test]
fn help_lists_a_package_command_with_its_hint_and_description() {
    let mut hello = command("demo", "hello", "greet one person");
    hello.argument_hint = Some("<name>".to_string());
    let state = registry_with(vec![hello]);

    let entry = state
        .entries()
        .into_iter()
        .find(|entry| entry.name == "demo:hello")
        .expect("the package command is listed");
    assert_eq!(
        entry,
        Entry {
            name: "demo:hello".to_string(),
            argument_hint: Some("<name>".to_string()),
            description: "greet one person".to_string(),
            group: Group::Harness,
            shadowed: false,
        }
    );
    assert!(
        state.help().contains("/demo:hello <name>"),
        "the listing did not carry the package command:\n{}",
        state.help()
    );
}

#[test]
fn a_typed_package_command_asks_the_harness_to_run_it() {
    let state = registry_with(vec![command("demo", "hello", "greet one person")]);
    let mut app = app_with(state);
    assert_eq!(
        type_line(&mut app, "/demo:hello Ada"),
        Some(Action::PackageCommand {
            package_id: "demo".to_string(),
            command: "hello".to_string(),
            arguments: "Ada".to_string(),
            display: "/demo:hello Ada".to_string(),
        })
    );
}

#[test]
fn a_template_answer_is_sent_as_a_message_and_drawn_as_the_typed_line() {
    let state = registry_with(vec![command("demo", "hello", "greet one person")]);
    let mut app = app_with(state);
    app.package_command(PackageOutcome {
        display: "/demo:hello Ada".to_string(),
        text: Some("Hello Ada".to_string()),
        output: None,
    });

    // The expansion is what the harness receives, and the typed line is what
    // the conversation shows, exactly as a command file's answer behaves.
    let sent = app.in_flight.as_ref().expect("the expansion is the message in flight");
    assert_eq!(sent.text, "Hello Ada");
    assert_eq!(sent.display, "/demo:hello Ada");
    assert_eq!(app.take_queued(), Some(Action::Send("Hello Ada".to_string())));
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| item == &Item::User("/demo:hello Ada".to_string()))
    );
}

#[test]
fn an_expansion_typed_during_a_turn_waits_for_that_turn() {
    let state = registry_with(vec![command("demo", "hello", "greet one person")]);
    let mut app = app_with(state);
    assert_eq!(app.submit("plain text"), Some(Action::Send("plain text".to_string())));
    app.package_command(PackageOutcome {
        display: "/demo:hello Ada".to_string(),
        text: Some("Hello Ada".to_string()),
        output: None,
    });
    assert_eq!(app.take_queued(), None, "the running turn is still in flight");
    assert_eq!(app.queue.len(), 1, "the expansion was not queued");
    assert_eq!(app.queue[0].text, "Hello Ada");
    assert_eq!(app.queue[0].display, "/demo:hello Ada");

    end_turn(&mut app);
    assert_eq!(app.take_queued(), Some(Action::Send("Hello Ada".to_string())));
}

#[test]
fn an_executable_answer_is_drawn_and_nothing_is_sent() {
    let state = registry_with(vec![command("demo", "lint", "run the linter")]);
    let mut app = app_with(state);
    app.package_command(PackageOutcome {
        display: "/demo:lint".to_string(),
        text: None,
        output: Some("3 warnings".to_string()),
    });
    assert_eq!(app.take_queued(), None, "an executable command sends no message");
    assert!(!app.pending_turn);
    assert_eq!(last_line(&app), "3 warnings");

    // An answer with nothing in it still names the command, rather than leaving
    // an empty notice a reader cannot place.
    app.package_command(PackageOutcome {
        display: "/demo:lint".to_string(),
        text: None,
        output: None,
    });
    assert_eq!(last_line(&app), "`/demo:lint` returned nothing");
}

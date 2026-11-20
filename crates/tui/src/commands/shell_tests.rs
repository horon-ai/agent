//! The local shell: how `!` and `!!` resolve, what they run, and what the
//! transcript keeps.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::shell;
use super::{Invocation, Local, Registry};
use crate::app::App;
use crate::transcript::Item;

/// The local shell has one command at a time, so the tests that run one take
/// turns; the resolver test reads no global state and does not.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An app whose registry never reads a real command directory.
fn app_in(directory: &Path) -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("test".to_string(), receiver);
    app.commands = Registry::new(directory.join("commands"));
    app.cwd = Some(directory.display().to_string());
    app
}

/// Every notice and warning in the transcript, one per line.
fn shown(app: &App) -> String {
    app.transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Notice(text) | Item::Warning(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Start the pending command and wait for the event loop to collect it.
fn settle(app: &mut App) {
    shell::start_pending();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if shell::drain(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the local command did not finish");
}

#[test]
fn a_bang_line_is_local_shell_and_never_prose() {
    let state = Registry::new(PathBuf::from("/a/command/directory/that/does/not/exist"));
    assert_eq!(
        state.resolve("!echo hi"),
        Invocation::Local {
            command: Local::Shell,
            arguments: "echo hi".to_string()
        }
    );
    assert_eq!(
        state.resolve("  !!echo hi  "),
        Invocation::Local {
            command: Local::ShellExcluded,
            arguments: "echo hi".to_string()
        }
    );
    // A line that only names the form runs nothing and is never sent as prose.
    for line in ["!", "!!", "   !   "] {
        match state.resolve(line) {
            Invocation::Error(message) => assert!(message.contains("!command")),
            other => panic!("`{line}` resolved to {other:?}"),
        }
    }
    // An exclamation mark later in the line is ordinary text.
    assert_eq!(
        state.resolve("what does !echo do?"),
        Invocation::Message("what does !echo do?".to_string())
    );
}

#[test]
fn a_local_command_runs_once_and_keeps_its_output() {
    let _serial = serial();
    let directory = tempfile::tempdir().expect("a working directory");
    let mut app = app_in(directory.path());
    let action = app.submit("!echo once>once.txt&& echo printed");
    assert!(action.is_none(), "a local command must not ask the bridge to send");
    assert!(app.in_flight.is_none());
    assert!(app.queue.is_empty());

    // The header is recorded before the command starts, so the location is on
    // screen even while the worker is still running.
    let before = shown(&app);
    assert!(
        before.contains("local shell · "),
        "the location line is missing: {before}"
    );
    assert!(before.contains("runs in this terminal, not the agent session"));
    assert!(before.contains("$ echo once"));

    settle(&mut app);
    let written = std::fs::read_to_string(directory.path().join("once.txt")).expect("the fixture ran");
    assert_eq!(written.lines().collect::<Vec<_>>(), vec!["once"]);

    let after = shown(&app);
    assert!(
        after.contains("local shell output:\nprinted"),
        "the output block is missing: {after}"
    );
    assert!(after.contains("local shell: exit 0"));
    assert!(app.in_flight.is_none());
    assert!(app.queue.is_empty());
}

#[test]
fn the_excluded_form_keeps_neither_the_command_nor_its_output() {
    let _serial = serial();
    let directory = tempfile::tempdir().expect("a working directory");
    let mut app = app_in(directory.path());
    assert!(app.submit("!!echo secret-token > excluded.txt").is_none());
    settle(&mut app);

    let written = std::fs::read_to_string(directory.path().join("excluded.txt")).expect("the fixture ran");
    assert!(written.contains("secret-token"), "the command did not run");

    let after = shown(&app);
    assert!(!after.contains("secret-token"), "the output was kept: {after}");
    assert!(!after.contains("excluded.txt"), "the command was kept: {after}");
    assert!(after.contains("local shell (excluded)"));
    assert!(after.contains("the command and its output were not kept"));
    assert!(after.contains("exit 0"));
}

#[test]
fn a_second_command_is_refused_while_one_is_pending_or_running() {
    let _serial = serial();
    let directory = tempfile::tempdir().expect("a working directory");
    let mut app = app_in(directory.path());
    assert!(app.submit("!echo first").is_none());
    assert!(app.submit("!echo second").is_none());
    let before = shown(&app);
    assert!(before.contains("a command is already running"), "{before}");
    assert!(!before.contains("second"), "the second command was accepted: {before}");

    settle(&mut app);
    let after = shown(&app);
    assert!(after.contains("first"), "{after}");
    assert!(!after.contains("second"), "the second command ran: {after}");
}

#[test]
fn a_failed_command_states_its_exit_status_as_a_warning() {
    let _serial = serial();
    let directory = tempfile::tempdir().expect("a working directory");
    let mut app = app_in(directory.path());
    assert!(app.submit("!exit 3").is_none());
    settle(&mut app);
    match app.transcript.items().last() {
        Some(Item::Warning(text)) => assert!(text.contains("local shell: exit 3"), "{text}"),
        other => panic!("expected a warning, got {other:?}"),
    }
}

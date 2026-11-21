//! `/exit`, `/keys` and `/help <command>`, as the interface answers them.
//!
//! Every test types a line into a real [`App`], so the resolution, the handler
//! and the line it leaves behind are exercised together; the registry it reads
//! points at temporary directories, never the user's own command folder.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::{Action, App};
use crate::keys::{ACTIONS, ids};
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
fn write_command(directory: &Path, name: &str, text: &str) {
    std::fs::write(directory.join(format!("{name}.md")), text).expect("write the command file");
}

/// An app whose registry is handed to it, so no real home directory is read.
fn app_with(state: Registry) -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("test".to_string(), receiver);
    app.commands = state;
    app
}

/// A registry pointed at a fresh project, with both directories returned so the
/// caller can write into them before loading.
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

/// The last line a typed command left in the transcript.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

#[test]
fn builtin_exit_and_keys_resolve_to_the_local_commands() {
    let (_user, _project, state) = registry();
    assert_eq!(
        state.resolve("/exit"),
        Invocation::Local {
            command: Local::Exit,
            arguments: String::new()
        }
    );
    assert_eq!(
        state.resolve("/keys"),
        Invocation::Local {
            command: Local::Keys,
            arguments: String::new()
        }
    );
    // The argument survives resolution so the handler can decide; `/exit` only
    // ignores it, it does not turn the line into prose.
    assert_eq!(
        state.resolve("/exit now"),
        Invocation::Local {
            command: Local::Exit,
            arguments: "now".to_string()
        }
    );
    // Surrounding whitespace is not part of the name.
    assert_eq!(
        state.resolve("  /keys  "),
        Invocation::Local {
            command: Local::Keys,
            arguments: String::new()
        }
    );
}

#[test]
fn builtin_exit_leaves_through_the_quit_action() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/exit"), Some(Action::Quit));
    // The word after the name changes nothing: leaving is leaving.
    assert_eq!(type_line(&mut app, "/exit now"), Some(Action::Quit));
    // Both names leave the same way, so the exit code is the same.
    assert_eq!(type_line(&mut app, "/quit"), Some(Action::Quit));
}

#[test]
fn builtin_keys_lists_the_keys_in_force_with_their_words() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/keys"), None);
    let listing = last_line(&app);
    assert!(
        listing.starts_with("keys:"),
        "`/keys` did not print the key section:\n{listing}"
    );
    assert!(
        listing.contains("send the typed message"),
        "the submit row is missing:\n{listing}"
    );
    // A row per action, so a new action cannot quietly go unlisted.
    assert_eq!(
        listing.lines().count(),
        ACTIONS.len() + 1,
        "`/keys` printed a different number of rows:\n{listing}"
    );
    assert!(
        !listing.contains(ids::APP_SUBMIT),
        "an action id reached the screen:\n{listing}"
    );
    // The section `/help` prints is the same text, built once.
    assert!(
        app.commands.help_with(&app.bindings).ends_with(&listing),
        "`/keys` and the `/help` key section disagree:\n{listing}"
    );
}

/// Draw one frame and return the screen as text.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = crate::draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

#[test]
fn builtin_keys_starts_at_its_first_row_on_a_short_pane() {
    // The wrap at each width adds rows the logical line count does not show, so
    // the case that matters is not one size: a hold that lands right at a
    // hundred columns can land short at sixty.
    for (width, height) in [(100, 30), (60, 20)] {
        let (_user, _project, mut app) = app_fixture();
        // One frame first: the listing is held against the pane the last frame
        // drew, which is the pane the user is reading.
        screen(&mut app, width, height);
        let view = app.viewport_height.expect("a drawn frame knows the pane height");
        let rows = crate::help::key_text(&app.bindings).lines().count();
        assert!(
            rows > usize::from(view),
            "the key list fits the pane, so this case proves nothing: {rows} rows in {view} at {width}x{height}"
        );
        assert_eq!(type_line(&mut app, "/keys"), None);
        assert!(
            !app.scroll.is_following(),
            "the listing opened on its tail instead of its head at {width}x{height}"
        );
        let drawn = screen(&mut app, width, height);
        assert!(
            drawn.contains("keys:"),
            "the header was off screen at {width}x{height}:\n{drawn}"
        );
        assert!(
            drawn.contains("send the typed message"),
            "the head of the key list was off screen at {width}x{height}:\n{drawn}"
        );
    }
}

#[test]
fn builtin_keys_leaves_a_list_that_fits_the_pane_in_follow_mode() {
    let (_user, _project, mut app) = app_fixture();
    // A pane taller than the listing has nothing to hide, so the view stays at
    // the end where the whole list is on screen; holding it would be a jump for
    // no gain.
    screen(&mut app, 120, 60);
    assert_eq!(type_line(&mut app, "/keys"), None);
    let drawn = screen(&mut app, 120, 60);
    assert!(
        app.scroll.is_following(),
        "a list that fits the pane moved the view away from the end"
    );
    assert!(
        drawn.contains("keys:") && drawn.contains("send the typed message"),
        "the whole list fits, so its head must be on screen:\n{drawn}"
    );
}

#[test]
fn builtin_keys_before_the_first_frame_still_finds_its_head() {
    let (_user, _project, mut app) = app_fixture();
    // Nothing has measured the pane yet, so there is no height to count
    // against: the request waits for the first frame instead of guessing, and
    // that frame finds the listing's head.
    assert!(app.viewport_height.is_none());
    assert_eq!(type_line(&mut app, "/keys"), None);
    let drawn = screen(&mut app, 100, 30);
    assert!(
        drawn.contains("keys:") && drawn.contains("send the typed message"),
        "the first frame did not land on the listing's head:\n{drawn}"
    );
}

#[test]
fn builtin_keys_gives_the_view_back_to_the_end_on_ctrl_end() {
    let (_user, _project, mut app) = app_fixture();
    screen(&mut app, 100, 30);
    assert_eq!(type_line(&mut app, "/keys"), None);
    screen(&mut app, 100, 30);
    assert!(
        !app.scroll.is_following(),
        "the listing did not hold its head to give back"
    );
    assert_eq!(app.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL)), None);
    assert!(app.scroll.is_following(), "ctrl+end did not return the view to the end");
    let drawn = screen(&mut app, 100, 30);
    assert!(
        drawn.contains("confirm a multi-choice question"),
        "the end of the listing is not on screen again:\n{drawn}"
    );
}

#[test]
fn builtin_help_names_one_command_and_accepts_a_leading_slash() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/help model"), None);
    let bare = last_line(&app);
    assert!(
        bare.starts_with("/model"),
        "the detail does not open with the command:\n{bare}"
    );
    assert!(bare.contains("pick a model"), "the description is missing:\n{bare}");
    assert!(
        bare.contains("group: built-in commands"),
        "the detail does not say where the command comes from:\n{bare}"
    );

    assert_eq!(type_line(&mut app, "/help /model"), None);
    assert_eq!(last_line(&app), bare, "a leading slash must read the same");
}

#[test]
fn builtin_help_trims_spaces_and_reads_no_second_slash() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/help    model   "), None);
    assert!(last_line(&app).starts_with("/model"), "extra spaces reached the name");

    // `//text` is the escape for a message; for `/help` the second slash is
    // part of the name, and saying so beats an empty help block.
    assert_eq!(type_line(&mut app, "/help //model"), None);
    assert_eq!(last_line(&app), "unknown command `//model`; /help lists the commands");

    // A lone slash names nothing, so it cannot read as success either.
    assert_eq!(type_line(&mut app, "/help /"), None);
    assert_eq!(last_line(&app), "unknown command `/`; /help lists the commands");
}

#[test]
fn builtin_help_unknown_name_warns_and_keeps_the_client_running() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/help nope"), None);
    match app.transcript.items().last() {
        Some(Item::Warning(text)) => assert_eq!(
            text, "unknown command `/nope`; /help lists the commands",
            "the warning must name the command that was asked for"
        ),
        other => panic!("an unknown name must be a warning, got {other:?}"),
    }
    // Nothing left the client: the next command still answers.
    assert_eq!(type_line(&mut app, "/help keys"), None);
    assert!(
        last_line(&app).contains("list the keys in force"),
        "the client stopped answering after a bad name"
    );
}

#[test]
fn builtin_help_names_are_case_sensitive_like_the_resolver() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/help MODEL"), None);
    assert_eq!(last_line(&app), "unknown command `/MODEL`; /help lists the commands");
    // The resolver agrees, so `/help` describes what typing the line would do.
    assert!(matches!(app.commands.resolve("/MODEL"), Invocation::Error(_)));
}

#[test]
fn builtin_exit_shadows_a_command_file_and_help_says_so() {
    let (user, _project, mut app) = app_fixture();
    write_command(user.path(), "exit", "this must never run");
    write_command(user.path(), "keys", "this must never run either");
    app.commands.load();

    // The built-in answers the name; the file never runs.
    assert_eq!(type_line(&mut app, "/exit"), Some(Action::Quit));
    for name in ["exit", "keys"] {
        let file = app
            .commands
            .files()
            .iter()
            .find(|file| file.name == name)
            .unwrap_or_else(|| panic!("the {name} file was not discovered"));
        assert!(file.shadowed, "the built-in must shadow the {name} file");
    }

    assert_eq!(type_line(&mut app, "/help exit"), None);
    let detail = last_line(&app);
    assert!(detail.contains("/exit"), "the detail lost the command:\n{detail}");
    assert!(
        detail.contains("shadowed"),
        "the detail hid the file the name takes away:\n{detail}"
    );
}

#[test]
fn builtin_help_shows_a_command_files_hint_and_group() {
    let (user, _project, mut app) = app_fixture();
    write_command(
        user.path(),
        "deploy",
        "---\ndescription: Ship the current branch\nargument-hint: <environment>\n---\nship it",
    );
    app.commands.load();

    assert_eq!(type_line(&mut app, "/help deploy"), None);
    let detail = last_line(&app);
    assert!(
        detail.starts_with("/deploy <environment>"),
        "the argument hint is missing:\n{detail}"
    );
    assert!(
        detail.contains("Ship the current branch"),
        "the description is missing:\n{detail}"
    );
    assert!(
        detail.contains("group: command files"),
        "the detail does not say where the command comes from:\n{detail}"
    );
}

#[test]
fn builtin_help_describes_the_command_that_would_run() {
    let (user, _project, mut app) = app_fixture();
    // A command file outranks a harness command of the same name, so the detail
    // has to describe the file rather than the harness row it outranks.
    write_command(
        user.path(),
        "review",
        "---\ndescription: Review this project's own way\n---\nreview the diff",
    );
    app.commands.load();
    assert_eq!(type_line(&mut app, "/help review"), None);
    let detail = last_line(&app);
    assert!(
        detail.contains("Review this project's own way"),
        "the harness row won the name:\n{detail}"
    );
    assert!(
        detail.contains("group: command files"),
        "the detail named the wrong source:\n{detail}"
    );
}

#[test]
fn builtin_every_built_in_command_has_a_description() {
    let (_user, _project, state) = registry();
    let missing: Vec<String> = state
        .entries()
        .into_iter()
        .filter(|entry| entry.group == Group::Builtin && entry.description.is_empty())
        .map(|entry| entry.name)
        .collect();
    assert!(
        missing.is_empty(),
        "a built-in landed with no description in the listing: {missing:?}"
    );
}

#[test]
fn builtin_help_lists_every_built_in_command() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/help"), None);
    let listing = last_line(&app);
    for name in [
        "/artifact",
        "/clear",
        "/exit",
        "/help",
        "/keys",
        "/links",
        "/model",
        "/new",
        "/quit",
        "/resume",
        "/settings",
        "/switch",
    ] {
        assert!(
            listing.contains(name),
            "`{name}` is missing from the listing:\n{listing}"
        );
    }
}

#[test]
fn builtin_new_and_switch_resolve_to_the_local_commands() {
    let (_user, _project, state) = registry();
    assert_eq!(
        state.resolve("/new"),
        Invocation::Local {
            command: Local::New,
            arguments: String::new()
        }
    );
    assert_eq!(
        state.resolve("/switch session_b"),
        Invocation::Local {
            command: Local::Switch,
            arguments: "session_b".to_string()
        }
    );
}

#[test]
fn builtin_help_names_what_new_and_switch_do() {
    let (_user, _project, mut app) = app_fixture();
    assert_eq!(type_line(&mut app, "/help new"), None);
    let detail = last_line(&app);
    assert!(
        detail.contains("start a new session"),
        "the detail did not say what /new does:\n{detail}"
    );
    assert!(
        detail.contains("group: built-in commands"),
        "the detail named the wrong source:\n{detail}"
    );

    assert_eq!(type_line(&mut app, "/help switch"), None);
    let detail = last_line(&app);
    assert!(
        detail.contains("stored session"),
        "the detail did not say what /switch does:\n{detail}"
    );
}

/// An app attached to `session` on this client's own harness, with
/// `/artifact` pointed at a temporary store.
fn artifact_app(session: &str) -> (tempfile::TempDir, App) {
    let (_user, _project, mut app) = app_fixture();
    let root = tempfile::tempdir().expect("an artifact root");
    app.artifacts_dir = root.path().to_path_buf();
    app.session_id = Some(session.to_string());
    app.workspace_is_local = true;
    (root, app)
}

/// Write one artifact into the store under `root`, as the harness does.
fn write_artifact(root: &Path, session: &str, name: &str, text: &str) -> std::path::PathBuf {
    let directory = root.join(session);
    std::fs::create_dir_all(&directory).expect("a session store");
    let path = directory.join(name);
    std::fs::write(&path, text).expect("an artifact");
    path
}

#[test]
fn builtin_artifact_opens_a_copy_of_the_attached_sessions_artifact() {
    let (root, mut app) = artifact_app("session_a");
    let path = write_artifact(root.path(), "session_a", "3.bash.log", "output");
    assert_eq!(
        type_line(&mut app, "/artifact 3"),
        Some(Action::OpenArtifact(path.clone())),
        "a retained artifact did not produce the open action"
    );
    // The pointer the transcript prints names the same artifact.
    assert_eq!(
        type_line(&mut app, "/artifact artifact://3"),
        Some(Action::OpenArtifact(path))
    );
}

#[test]
fn builtin_artifact_refuses_another_sessions_remote_paths_and_stale_ids() {
    let (root, mut app) = artifact_app("session_a");
    write_artifact(root.path(), "session_b", "3.bash.log", "not this session");

    // The id exists, but only in the other session's store.
    assert_eq!(type_line(&mut app, "/artifact artifact://3"), None);
    assert!(
        last_line(&app).contains("not in this session's store"),
        "a foreign id resolved: {}",
        last_line(&app)
    );

    // A session prefix is not part of an address this client reads.
    assert_eq!(type_line(&mut app, "/artifact session_b:3"), None);
    assert!(
        last_line(&app).contains("not an artifact id"),
        "a session-qualified id was not refused: {}",
        last_line(&app)
    );

    assert_eq!(type_line(&mut app, "/artifact https://example.com/output.log"), None);
    assert!(
        last_line(&app).contains("not an artifact address"),
        "a remote address was not refused: {}",
        last_line(&app)
    );

    assert_eq!(type_line(&mut app, "/artifact /etc/passwd"), None);
    assert!(
        last_line(&app).contains("not an artifact address"),
        "a path was not refused: {}",
        last_line(&app)
    );
}

#[test]
fn builtin_artifact_needs_an_attached_session() {
    let (_user, _project, mut app) = app_fixture();
    app.workspace_is_local = true;
    assert_eq!(type_line(&mut app, "/artifact 0"), None);
    assert!(
        last_line(&app).contains("no session is attached"),
        "the missing session was not reported: {}",
        last_line(&app)
    );
}

#[test]
fn builtin_artifact_is_refused_when_the_connection_is_not_this_harness() {
    let (root, mut app) = artifact_app("session_a");
    // The store holds a file under the very id the remote session reported;
    // without the connection gate this would open a stranger's artifact.
    write_artifact(root.path(), "session_a", "3.bash.log", "a local session's output");
    app.workspace_is_local = false;

    assert_eq!(type_line(&mut app, "/artifact 3"), None);
    assert!(
        last_line(&app).contains("explicit socket or a remote bridge"),
        "an explicit or remote connection was not refused: {}",
        last_line(&app)
    );
    // The listing reads the same local disk, so it is refused too.
    assert_eq!(type_line(&mut app, "/artifact"), None);
    assert!(
        last_line(&app).contains("explicit socket or a remote bridge"),
        "the listing was not refused for a remote session: {}",
        last_line(&app)
    );
}

#[test]
fn builtin_artifact_lists_the_store_when_no_id_is_given() {
    let (root, mut app) = artifact_app("session_a");
    write_artifact(root.path(), "session_a", "0.read.log", "first");
    write_artifact(root.path(), "session_a", "2.bash.log", "later output");
    assert_eq!(type_line(&mut app, "/artifact"), None);
    let listing = last_line(&app);
    assert!(
        listing.starts_with("artifacts:"),
        "`/artifact` did not print the store section:\n{listing}"
    );
    assert!(
        listing.contains("   0") && listing.contains("read"),
        "the first artifact is missing from the listing:\n{listing}"
    );
    assert!(
        listing.contains("   2") && listing.contains("bash"),
        "the second artifact is missing from the listing:\n{listing}"
    );
}

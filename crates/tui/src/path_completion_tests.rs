//! Tab completion of a workspace path: what a press fills, how repeated presses
//! choose among several candidates, and where the key refuses to read at all.

use std::sync::mpsc;

use tempfile::TempDir;

use crate::app::App;
use crate::loop_io::drain_events;
use crate::path_completion::complete;
use crate::sdk_bridge::{AttachedSession, Incoming};

/// A workspace with files, a directory with one file, and a name with a space.
fn workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path();
    std::fs::create_dir(path.join("docs")).expect("a directory");
    std::fs::create_dir(path.join("src")).expect("a directory");
    for file in [
        "src/main.rs",
        "src/alpha.rs",
        "src/alpine.rs",
        "readme.md",
        "docs/guide.md",
        "foo",
        "foo.rs",
    ] {
        std::fs::write(path.join(file), "").expect("a file");
    }
    std::fs::write(path.join("name with space.txt"), "").expect("a file");
    dir
}

fn root(workspace: &TempDir) -> &str {
    workspace.path().to_str().expect("a utf-8 path")
}

/// One press, read on its own rather than through the client.
fn fill(draft: &str, workspace: &TempDir) -> Option<String> {
    complete(draft, Some(root(workspace)), None).map(|(next, _)| next)
}

/// A client attached to its own daemon, working in the fixture.
fn app_at(workspace: &TempDir) -> App {
    let (_sender, receiver) = mpsc::channel();
    let mut app = App::new("socket · session".to_string(), receiver);
    app.cwd = Some(root(workspace).to_string());
    app.workspace_is_local = true;
    app.workspace_known = true;
    app
}

/// A session the bridge moved to, as the answer carries it.
fn attached() -> AttachedSession {
    AttachedSession {
        description: "socket · next".to_string(),
        session_id: "next".to_string(),
        status: "idle".to_string(),
        model: None,
        effort: None,
        history: Vec::new(),
        title: None,
    }
}

#[test]
fn tab_fills_a_file_a_directory_and_the_prefix_candidates_share() {
    let workspace = workspace();
    assert_eq!(fill("read", &workspace).as_deref(), Some("readme.md"));
    // A directory carries the separator, and the next press descends into it.
    let mut app = app_at(&workspace);
    app.input.set_text("do");
    app.complete();
    assert_eq!(app.input.text(), "docs/");
    app.complete();
    assert_eq!(app.input.text(), "docs/guide.md");
    // Names that share a prefix are filled to that prefix first.
    app.input.set_text("src/al");
    app.complete();
    assert_eq!(app.input.text(), "src/alp");
    // A quote inside a word earlier on the line is text, not an opening quote.
    assert_eq!(
        fill("5\" 화면에서 rea", &workspace).as_deref(),
        Some("5\" 화면에서 readme.md"),
    );
}

#[test]
fn tab_quotes_a_name_with_a_space_and_keeps_the_mention_marker() {
    let workspace = workspace();
    assert_eq!(fill("name", &workspace).as_deref(), Some("\"name with space.txt\""));
    assert_eq!(fill("@name", &workspace).as_deref(), Some("@\"name with space.txt\""));
    // The filled form is one complete token: completing it again changes
    // nothing, so the quotes and the marker survive a second press.
    assert_eq!(fill("@\"name with space.txt\"", &workspace), None);
}

#[test]
fn repeated_tabs_select_among_the_candidates() {
    let workspace = workspace();
    let mut app = app_at(&workspace);
    app.input.set_text("src/al");
    app.complete();
    assert_eq!(app.input.text(), "src/alp");
    // Once the prefix is on screen, each press walks one candidate and the
    // last one wraps back to the first.
    app.complete();
    assert_eq!(app.input.text(), "src/alpha.rs");
    app.complete();
    assert_eq!(app.input.text(), "src/alpine.rs");
    app.complete();
    assert_eq!(app.input.text(), "src/alpha.rs");
    // A name typed out in full that also starts another name is the first
    // candidate, so the press moves to the next one instead of stopping.
    app.input.set_text("foo");
    app.complete();
    assert_eq!(app.input.text(), "foo.rs");
    app.complete();
    assert_eq!(app.input.text(), "foo");
    app.complete();
    assert_eq!(app.input.text(), "foo.rs");
}

#[test]
fn slash_completion_keeps_its_priority_over_a_workspace_path() {
    let workspace = workspace();
    // A workspace file with the name the command prefix would otherwise match:
    // the registry's answer is the one that reaches the draft.
    std::fs::write(workspace.path().join("cl"), "").expect("a file");
    let mut app = app_at(&workspace);
    app.input.set_text("/cl");
    app.complete();
    assert_eq!(app.input.text(), "/clear");
    // An argument position is the user's: a workspace file that starts the
    // same way is not filled into a model id.
    std::fs::write(workspace.path().join("gpt-notes.md"), "").expect("a file");
    app.input.set_text("/model gpt");
    app.complete();
    assert_eq!(app.input.text(), "/model gpt");
}

#[test]
fn tab_reads_only_the_sessions_confirmed_local_directory() {
    let workspace = workspace();
    // A switch answer names the session's own directory: the received handler
    // moves the root, so the names filled are the ones that session has rather
    // than the ones of the session being left.
    let elsewhere = self::workspace();
    std::fs::write(elsewhere.path().join("elsewhere.md"), "").expect("a file");
    let (sender, receiver) = mpsc::channel();
    let mut moved = App::new("socket · session".to_string(), receiver);
    moved.cwd = Some(root(&workspace).to_string());
    moved.workspace_is_local = true;
    moved.workspace_known = true;
    sender
        .send(Incoming::Attached {
            session: Box::new(attached()),
            working_dir: Some(root(&elsewhere).to_string()),
        })
        .expect("the app is listening");
    assert!(drain_events(&mut moved), "the switch answer was dropped");
    assert_eq!(moved.cwd.as_deref(), Some(root(&elsewhere)));
    moved.input.set_text("else");
    moved.complete();
    assert_eq!(moved.input.text(), "elsewhere.md");
    // An answer that names no directory is not the previous one either:
    // nothing is read until a session names one.
    sender
        .send(Incoming::Attached {
            session: Box::new(attached()),
            working_dir: None,
        })
        .expect("the app is listening");
    assert!(drain_events(&mut moved), "the second answer was dropped");
    moved.input.set_text("else");
    moved.complete();
    assert_eq!(moved.input.text(), "else");

    // A session behind an explicit socket or a remote bridge: the flag is off,
    // so the key does not read this machine's disk at all.
    let mut remote = app_at(&workspace);
    remote.workspace_is_local = false;
    remote.input.set_text("read");
    remote.complete();
    assert_eq!(remote.input.text(), "read");

    // Paths that are not one workspace-relative token are refused, not
    // resolved.
    assert_eq!(fill("/etc/pass", &workspace), None);
    assert_eq!(fill("~/proj", &workspace), None);
    assert_eq!(fill("../notes", &workspace), None);
    // A workspace this machine does not have is not read under a same-named
    // directory.
    let missing = workspace.path().join("missing-workspace");
    assert!(complete("read", Some(missing.to_str().expect("UTF-8 fixture path")), None).is_none());

    // The editor can only replace the whole text, so a cursor in the middle is
    // left where the user put it.
    let mut app = app_at(&workspace);
    app.input.set_text("readme.md");
    for _ in 0..3 {
        app.input.left();
    }
    app.complete();
    assert_eq!(app.input.text(), "readme.md");
    assert_eq!(app.input.cursor(), "readme.md".len() - 3);
}

//! `/diff` and `/commit`, driven through a real `App` and a real repository.
//!
//! Every test makes its own work tree in a temporary directory, with its own
//! identity, no hooks and no signing, so what it proves is this client's
//! behaviour and not the machine's git configuration. The lines are typed into
//! the app, so the resolver, the command and the text it leaves are exercised
//! together, and the assertions read the work tree back with git instead of
//! trusting the line the client printed.

use std::path::Path;
use std::process::Command;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::commands::{Invocation, Local};
use crate::transcript::Item;

/// A work tree this test owns, and an app attached to it.
struct Fixture {
    dir: tempfile::TempDir,
    hooks: tempfile::TempDir,
    app: App,
}

impl Fixture {
    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The hooks directory this repository is configured with.
    fn hooks_path(&self) -> &Path {
        self.hooks.path()
    }

    /// A line that is exactly what git says now.
    fn head(&self) -> String {
        git(self.path(), &["rev-parse", "HEAD"])
    }

    /// The staged paths, one per line.
    fn staged(&self) -> String {
        git(self.path(), &["diff", "--cached", "--name-only"])
    }

    /// The modified paths git has not been asked to stage.
    fn unstaged(&self) -> String {
        git(self.path(), &["diff", "--name-only"])
    }
}

/// Run one git command and return its standard output.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        // The test's own environment must not redirect git at another
        // repository, exactly as the client's environment must not.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed for {}:\n{}\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Write one file into the work tree, with no line-ending translation.
fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the file's directory");
    }
    std::fs::write(path, text).expect("write a file");
}

/// An app attached to `dir`, as a local session would be.
fn app_for(dir: &Path) -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("test".to_string(), receiver);
    app.cwd = Some(dir.display().to_string());
    app.workspace_known = true;
    app.workspace_is_local = true;
    app
}

/// A repository with one commit (`a.txt` and `c.txt`) and no hooks.
fn fixture() -> Fixture {
    let (dir, hooks) = blank();
    write(dir.path(), "a.txt", "one\n");
    write(dir.path(), "c.txt", "one\n");
    git(dir.path(), &["add", "--", "a.txt", "c.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    let app = app_for(dir.path());
    Fixture { dir, hooks, app }
}

/// A repository with no commit at all, and no hooks.
fn empty_fixture() -> Fixture {
    let (dir, hooks) = blank();
    let app = app_for(dir.path());
    Fixture { dir, hooks, app }
}

/// An initialised work tree whose configuration cannot reach this machine's
/// hooks, signing keys or identity.
fn blank() -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a workspace");
    let hooks = tempfile::tempdir().expect("an empty hooks directory");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    let hooks_path = hooks.path().to_str().expect("a UTF-8 hooks path");
    for (key, value) in [
        ("user.name", "git review test"),
        ("user.email", "git-review@example.invalid"),
        ("commit.gpgsign", "false"),
        ("tag.gpgsign", "false"),
        ("core.autocrlf", "false"),
        ("core.fsmonitor", "false"),
        ("core.hooksPath", hooks_path),
        ("gc.auto", "0"),
    ] {
        git(dir.path(), &["config", key, value]);
    }
    (dir, hooks)
}

/// Type a whole line and press Enter, as a user would.
fn type_line(app: &mut App, text: &str) -> Option<crate::app::Action> {
    for character in text.chars() {
        let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
        assert!(app.on_key(key).is_none(), "typing a character produced an action");
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

/// The last line the transcript holds.
fn last_line(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

#[test]
fn diff_and_commit_resolve_to_the_local_commands() {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let app = App::new("test".to_string(), receiver);
    assert_eq!(
        app.commands.resolve("/diff"),
        Invocation::Local {
            command: Local::Diff,
            arguments: String::new()
        }
    );
    assert_eq!(
        app.commands.resolve("/commit fix the parser"),
        Invocation::Local {
            command: Local::Commit,
            arguments: "fix the parser".to_string()
        }
    );
    // `/diff staged` keeps its word for the handler, which is what reads it.
    assert_eq!(
        app.commands.resolve("/diff staged"),
        Invocation::Local {
            command: Local::Diff,
            arguments: "staged".to_string()
        }
    );
}

#[test]
fn diff_shows_the_staged_and_the_unstaged_changes_apart() {
    let mut fixture = fixture();
    write(fixture.path(), "a.txt", "one\ntwo\n");
    git(fixture.path(), &["add", "--", "a.txt"]);
    write(fixture.path(), "c.txt", "one\nthree\n");
    write(fixture.path(), "untracked.txt", "new\n");

    assert_eq!(type_line(&mut fixture.app, "/diff"), None);
    let text = last_line(&fixture.app);
    let staged = text
        .find("staged (what /commit would include):")
        .expect("the staged section is named");
    let unstaged = text
        .find("unstaged (left alone by /commit):")
        .expect("the unstaged section is named");
    assert!(staged < unstaged, "the sections are in one pane, staged first:\n{text}");
    let (index_part, worktree_part) = text.split_at(unstaged);
    assert!(
        index_part.contains("M  a.txt"),
        "the staged change is in the staged section:\n{text}"
    );
    assert!(
        !index_part.contains("M  c.txt"),
        "an unstaged change is not in the staged section:\n{text}"
    );
    assert!(
        worktree_part.contains("M  c.txt"),
        "the unstaged change is in the unstaged section:\n{text}"
    );
    assert!(
        text.contains("untracked, in neither diff"),
        "an untracked file is named so the empty side is not read as clean:\n{text}"
    );
    assert!(text.contains("untracked.txt"), "the untracked path is named:\n{text}");
}

#[test]
fn commit_previews_the_exact_staged_files_and_commits_only_them() {
    let mut fixture = fixture();
    let before = fixture.head();
    write(fixture.path(), "a.txt", "one\ntwo\n");
    write(fixture.path(), "b.txt", "new\n");
    git(fixture.path(), &["add", "--", "a.txt", "b.txt"]);
    write(fixture.path(), "c.txt", "one\nthree\n");

    assert_eq!(type_line(&mut fixture.app, "/commit add the second file"), None);
    let preview = last_line(&fixture.app);
    assert!(
        preview.contains("staged, exactly these 2 file(s)"),
        "the set is counted:\n{preview}"
    );
    assert!(preview.contains("M  a.txt"), "the modified path is listed:\n{preview}");
    assert!(preview.contains("A  b.txt"), "the added path is listed:\n{preview}");
    assert!(
        !preview.contains("c.txt"),
        "an unstaged edit is not in the reviewed set:\n{preview}"
    );
    assert!(
        preview.contains("nothing was committed yet"),
        "the preview claims nothing was committed:\n{preview}"
    );
    assert!(
        preview.contains("tree "),
        "the preview names the tree the approval covers:\n{preview}"
    );
    assert!(
        preview.contains("hooks are not run"),
        "the preview says the commit is made without the repository's hooks:\n{preview}"
    );
    assert_eq!(fixture.head(), before, "the preview committed something");

    assert_eq!(type_line(&mut fixture.app, "/commit --yes"), None);
    let done = last_line(&fixture.app);
    assert!(
        done.contains("committed ") && done.contains("add the second file"),
        "the commit is reported with its subject:\n{done}"
    );
    assert!(done.contains("not pushed"), "the client never pushes:\n{done}");
    assert_ne!(fixture.head(), before, "the commit did not move HEAD");

    // Exactly the reviewed files are in the commit; the unstaged edit is not.
    let committed = git(fixture.path(), &["show", "--name-only", "--format=", "HEAD"]);
    let mut names: Vec<&str> = committed.lines().filter(|line| !line.is_empty()).collect();
    names.sort_unstable();
    assert_eq!(names, ["a.txt", "b.txt"], "the commit holds the reviewed set");
    assert_eq!(fixture.staged(), "", "nothing is left staged");
    assert_eq!(fixture.unstaged(), "c.txt", "the unstaged edit is still unstaged");
    assert_eq!(
        git(fixture.path(), &["log", "-1", "--format=%s"]),
        "add the second file",
        "the message is stored as reviewed"
    );
}

#[test]
fn a_file_staged_after_the_review_refuses_the_commit() {
    let mut fixture = fixture();
    let before = fixture.head();
    write(fixture.path(), "a.txt", "one\ntwo\n");
    git(fixture.path(), &["add", "--", "a.txt"]);

    assert_eq!(type_line(&mut fixture.app, "/commit stage the second"), None);
    // Something else writes to the index between the two lines: a second
    // terminal, an editor's git plugin, or the user in another shell.
    write(fixture.path(), "b.txt", "new\n");
    git(fixture.path(), &["add", "--", "b.txt"]);

    assert_eq!(type_line(&mut fixture.app, "/commit --yes"), None);
    let refused = last_line(&fixture.app);
    assert!(
        refused.contains("the staged index changed after review"),
        "the refusal names what moved:\n{refused}"
    );
    assert!(
        refused.contains("/commit <message>") && refused.contains("/diff staged"),
        "the refusal shows the refresh path:\n{refused}"
    );
    assert_eq!(fixture.head(), before, "a refused commit moved HEAD");
    assert_eq!(
        fixture.staged(),
        "a.txt\nb.txt",
        "both staged paths are still staged, none of them committed"
    );
    // The review is spent: a second confirmation has nothing to confirm.
    assert_eq!(type_line(&mut fixture.app, "/commit --yes"), None);
    assert!(
        last_line(&fixture.app).contains("no staged set was reviewed"),
        "a stale review does not stay confirmable"
    );
    assert_eq!(fixture.head(), before);
}

#[test]
fn cancel_discards_the_review_and_commits_nothing() {
    let mut fixture = fixture();
    let before = fixture.head();
    write(fixture.path(), "a.txt", "one\ntwo\n");
    git(fixture.path(), &["add", "--", "a.txt"]);

    assert_eq!(type_line(&mut fixture.app, "/commit something"), None);
    assert_eq!(type_line(&mut fixture.app, "/commit --cancel"), None);
    assert!(
        last_line(&fixture.app).contains("review was discarded"),
        "cancel says what it did"
    );
    assert_eq!(type_line(&mut fixture.app, "/commit --yes"), None);
    assert!(
        last_line(&fixture.app).contains("no staged set was reviewed"),
        "a cancelled review cannot be confirmed"
    );
    assert_eq!(fixture.head(), before);
    assert_eq!(fixture.staged(), "a.txt", "the staged change is untouched");
}

#[test]
fn a_remote_session_never_reads_this_machines_git() {
    let mut fixture = fixture();
    write(fixture.path(), "a.txt", "one\ntwo\n");
    git(fixture.path(), &["add", "--", "a.txt"]);
    // An explicit socket or a remote bridge reports a directory that belongs
    // to the other machine.
    fixture.app.workspace_is_local = false;

    assert_eq!(type_line(&mut fixture.app, "/diff"), None);
    let refused = last_line(&fixture.app);
    assert!(
        refused.contains("remote bridge") && refused.contains("not this machine's"),
        "a remote workspace is refused before git runs:\n{refused}"
    );
    assert_eq!(type_line(&mut fixture.app, "/commit from elsewhere"), None);
    let refused = last_line(&fixture.app);
    assert!(
        refused.contains("remote bridge"),
        "the commit refuses a remote workspace too:\n{refused}"
    );
    assert!(fixture.app.commit_review.is_none(), "a remote review was kept");
    // A session that never named its directory leaves only the launch
    // directory, which is not known to be the workspace either.
    fixture.app.workspace_is_local = true;
    fixture.app.workspace_known = false;
    assert_eq!(type_line(&mut fixture.app, "/diff"), None);
    assert!(
        last_line(&fixture.app).contains("did not report the directory"),
        "a guessed directory is not read as the workspace"
    );
}

#[test]
fn a_path_that_reads_like_an_option_is_still_only_a_path() {
    let mut fixture = fixture();
    let before = fixture.head();
    // A leading dash and a space: git would read either as an argument if the
    // path were ever assembled into a command line.
    let name = "--amend me.txt";
    write(fixture.path(), name, "trap\n");
    git(fixture.path(), &["add", "--", name]);

    assert_eq!(type_line(&mut fixture.app, "/commit name the file"), None);
    let preview = last_line(&fixture.app);
    assert!(
        preview.contains("A  --amend me.txt"),
        "the path is listed as it is:\n{preview}"
    );
    assert_eq!(type_line(&mut fixture.app, "/commit --yes"), None);
    let done = last_line(&fixture.app);
    assert!(done.contains("name the file"), "the message survived the path:\n{done}");
    assert_ne!(fixture.head(), before);
    assert_eq!(
        git(fixture.path(), &["log", "-1", "--format=%s"]),
        "name the file",
        "the message is the one reviewed, not a flag the path supplied"
    );
    assert_eq!(
        git(fixture.path(), &["show", "--name-only", "--format=", "HEAD"]),
        name,
        "the file is committed under its own name"
    );
}

#[test]
fn a_review_works_in_a_repository_with_no_commit_yet() {
    let mut fixture = empty_fixture();
    let dir = fixture.path().to_path_buf();
    write(&dir, "first.txt", "hello\n");
    git(&dir, &["add", "--", "first.txt"]);

    assert_eq!(type_line(&mut fixture.app, "/commit the first commit"), None);
    let preview = last_line(&fixture.app);
    assert!(
        preview.contains("as the first commit"),
        "the preview names the missing parent:\n{preview}"
    );
    assert_eq!(type_line(&mut fixture.app, "/commit --yes"), None);
    assert!(last_line(&fixture.app).contains("committed "));
    assert_eq!(git(&dir, &["log", "--oneline"]).lines().count(), 1);
    assert_eq!(
        git(&dir, &["rev-list", "--parents", "HEAD"]).split_whitespace().count(),
        1,
        "the commit has no parent"
    );
}

#[test]
fn a_long_diff_is_cut_instead_of_filling_the_transcript() {
    let mut fixture = fixture();
    let mut lines = String::new();
    for number in 0..600 {
        lines.push_str(&format!("line {number}\n"));
    }
    write(fixture.path(), "long.txt", &lines);
    git(fixture.path(), &["add", "--", "long.txt"]);

    assert_eq!(type_line(&mut fixture.app, "/diff staged"), None);
    let text = last_line(&fixture.app);
    assert!(
        text.contains("… diff truncated"),
        "the cut is named:\n{}",
        text.lines().take(4).collect::<Vec<&str>>().join("\n")
    );
    assert!(
        text.lines().count() < 300,
        "the diff is bounded to a pane, not to the file: {} lines",
        text.lines().count()
    );
    // The file list stays exact even when the patch is cut.
    assert!(text.contains("A  long.txt"));
}

#[test]
fn a_commit_without_a_message_reviews_nothing() {
    let mut fixture = fixture();
    write(fixture.path(), "a.txt", "one\ntwo\n");
    git(fixture.path(), &["add", "--", "a.txt"]);

    assert_eq!(type_line(&mut fixture.app, "/commit"), None);
    let usage = last_line(&fixture.app);
    assert!(
        usage.contains("previews the staged set"),
        "the usage is shown:\n{usage}"
    );
    assert!(
        fixture.app.commit_review.is_none(),
        "a message-less line opened a review"
    );
    // A flag nobody defined is not a commit message.
    assert_eq!(type_line(&mut fixture.app, "/commit --amend"), None);
    assert!(
        last_line(&fixture.app).contains("is not a flag"),
        "an unknown flag is refused rather than taken as a message"
    );
    assert!(fixture.app.commit_review.is_none());
}

#[test]
fn a_second_review_cannot_replace_an_open_one() {
    let mut fixture = fixture();
    write(fixture.path(), "a.txt", "one\ntwo\n");
    git(fixture.path(), &["add", "--", "a.txt"]);

    assert_eq!(type_line(&mut fixture.app, "/commit first pass"), None);
    assert_eq!(type_line(&mut fixture.app, "/commit second pass"), None);
    let refused = last_line(&fixture.app);
    assert!(
        refused.contains("already open"),
        "the open review is not silently replaced:\n{refused}"
    );
}

#[test]
fn a_repository_with_hooks_is_refused_rather_than_bypassed() {
    let mut fixture = fixture();
    write(fixture.path(), "a.txt", "one\ntwo\n");
    git(fixture.path(), &["add", "--", "a.txt"]);
    let before = fixture.head();
    // `pre-commit` is the hook this client could never run, and
    // `reference-transaction` is the one its `update-ref` would run by itself.
    for hook in ["pre-commit", "reference-transaction"] {
        let installed = fixture.hooks_path().join(hook);
        std::fs::write(&installed, "#!/bin/sh\nexit 0\n").expect("install a hook");
        assert_eq!(type_line(&mut fixture.app, "/commit with a hook"), None);
        let refused = last_line(&fixture.app);
        assert!(
            refused.contains("hook(s)") && refused.contains(hook),
            "the refusal names the hook it will not skip:\n{refused}"
        );
        assert!(
            refused.contains("git commit") && refused.contains("/diff staged"),
            "the refusal sends the user to a terminal commit after the same review:\n{refused}"
        );
        assert!(fixture.app.commit_review.is_none(), "a hook review was kept");
        assert_eq!(type_line(&mut fixture.app, "/commit --yes"), None);
        assert!(last_line(&fixture.app).contains("no staged set was reviewed"));
        std::fs::remove_file(&installed).expect("remove the hook");
    }
    assert_eq!(fixture.head(), before);
    assert_eq!(fixture.staged(), "a.txt", "the staged change is untouched");
}

#[test]
fn a_detached_head_has_no_branch_to_commit_to() {
    let mut fixture = fixture();
    git(fixture.path(), &["checkout", "-q", "--detach"]);
    write(fixture.path(), "a.txt", "one\ntwo\n");
    git(fixture.path(), &["add", "--", "a.txt"]);
    let before = fixture.head();

    assert_eq!(type_line(&mut fixture.app, "/commit detached"), None);
    let refused = last_line(&fixture.app);
    assert!(
        refused.contains("detached"),
        "a detached HEAD is refused with the reason:\n{refused}"
    );
    assert!(fixture.app.commit_review.is_none());
    assert_eq!(fixture.head(), before);
    assert_eq!(fixture.staged(), "a.txt");
}

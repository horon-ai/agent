//! Real-process QA for moving a session into a managed Git worktree, against a
//! real repository and an isolated HOME. Each case spawns the shipped binary.
#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_horon-agent");

struct Sandbox {
    _directory: tempfile::TempDir,
    /// Real path of the scratch root: the session store refuses symlinked components.
    root: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let directory = tempfile::Builder::new()
            .prefix(&format!("horon-move-{label}-"))
            .tempdir()
            .expect("temp dir");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).expect("private root");
        let root = directory.path().canonicalize().expect("real scratch root");
        for name in ["run", "config", "cache", "tmp"] {
            fs::create_dir_all(root.join(name)).expect("support dir");
        }
        Self {
            _directory: directory,
            root,
        }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn home(&self, name: &str) -> PathBuf {
        let home = self.path().join(name);
        fs::create_dir_all(&home).expect("home");
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("private home");
        home
    }

    fn run(&self, home: &Path, args: &[&str]) -> Output {
        Command::new(BINARY)
            .current_dir(self.path())
            .env_clear()
            .env("HOME", self.path())
            .env("HORON_HOME", home)
            .env("HORON_AGENT_RUNTIME_DIR", self.path().join("run"))
            .env("XDG_RUNTIME_DIR", self.path().join("run"))
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_CACHE_HOME", self.path().join("cache"))
            .env("TMPDIR", self.path().join("tmp"))
            .env("PATH", "/usr/bin:/bin")
            .arg("--no-update")
            .args(args)
            .output()
            .expect("spawn horon-agent")
    }

    fn migrate(&self, home: &Path, id: &str) -> Output {
        self.run(home, &["sessions", "migrate", id, "--apply"])
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn stdout(output: &Output) -> String {
    text(&output.stdout)
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn assert_refused(output: &Output, needle: &str) {
    assert!(!output.status.success(), "command unexpectedly succeeded");
    let combined = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(
        combined.contains(needle),
        "expected refusal containing {needle:?}, got: {combined}"
    );
}

fn message(id: &str, role: &str) -> Value {
    json!({"id":id,"role":role,"content":[{"type":"text","text":id}],
        "timestamp":"2020-01-01T01:00:00+01:00"})
}

fn event_log(home: &Path, id: &str) -> PathBuf {
    home.join("sessions").join(id).join("events.jsonl")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}: {}", text(&output.stderr));
    text(&output.stdout)
}

/// A repository with one commit, and a migrated session whose working directory is that repository.
fn worktree_fixture(sandbox: &Sandbox, home: &Path) -> PathBuf {
    let repo = sandbox.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("tracked.txt"), "one\n").expect("tracked");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let sessions = home.join("sessions");
    fs::create_dir_all(&sessions).expect("sessions");
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700)).expect("private sessions");
    let snapshot = json!({"id":"wt_one","created_at":"2020-01-01T00:00:00Z","updated_at":"2020-01-02T00:00:00Z",
        "title":"worktree","provider_key":"openai","model":"test","route_api_method":"openai-api",
        "messages":[message("one","user"),message("two","assistant")],"parent_id":null,
        "working_dir":repo.to_str().unwrap(),"status":"completed","last_pid":null});
    fs::write(sessions.join("wt_one.json"), serde_json::to_vec(&snapshot).unwrap()).expect("snapshot");
    assert_ok(&sandbox.migrate(home, "wt_one"));
    repo
}

fn worktree_json(sandbox: &Sandbox, home: &Path, args: &[&str]) -> Value {
    let mut full = vec!["sessions"];
    full.extend_from_slice(args);
    full.push("--json");
    let output = sandbox.run(home, &full);
    assert_ok(&output);
    serde_json::from_str(&stdout(&output)).expect("json report")
}

fn working_dir_of(sandbox: &Sandbox, home: &Path) -> Value {
    worktree_json(sandbox, home, &["inspect", "wt_one"])["working_dir"].clone()
}

#[test]
fn a_session_moves_into_a_worktree_and_back_with_its_history_intact() {
    let sandbox = Sandbox::new("worktree-move");
    let home = sandbox.home("a");
    let repo = worktree_fixture(&sandbox, &home);
    let planned = sandbox.path().join("repo-worktrees").join("feat-x");
    fs::write(repo.join("scratch.txt"), "uncommitted\n").unwrap();
    let log = event_log(&home, "wt_one");
    let before = fs::read(&log).unwrap();

    let preview = worktree_json(&sandbox, &home, &["move-to-worktree", "wt_one", "--branch", "feat-x"]);
    assert_eq!(preview["applied"], json!(false));
    assert_eq!(preview["uncommitted_paths_left_behind"], json!(1));
    assert_eq!(preview["worktree"], json!(planned.to_str().unwrap()));
    assert!(!planned.exists(), "a preview creates nothing");
    assert_eq!(working_dir_of(&sandbox, &home), json!(repo.to_str().unwrap()));

    let moved = worktree_json(
        &sandbox,
        &home,
        &["move-to-worktree", "wt_one", "--branch", "feat-x", "--apply"],
    );
    assert_eq!(moved["applied"], json!(true));
    assert!(planned.join("tracked.txt").exists());
    assert!(
        !planned.join("scratch.txt").exists(),
        "uncommitted changes are not copied"
    );
    assert!(repo.join("scratch.txt").exists(), "uncommitted changes are not touched");
    assert_eq!(working_dir_of(&sandbox, &home), json!(planned.to_str().unwrap()));
    let after = fs::read(&log).unwrap();
    assert!(after.starts_with(&before), "the original events are kept byte for byte");

    let again = sandbox.run(
        &home,
        &["sessions", "move-to-worktree", "wt_one", "--branch", "other", "--apply"],
    );
    assert_refused(&again, "already works in a managed worktree");

    worktree_json(&sandbox, &home, &["move-to-worktree", "wt_one", "--undo", "--apply"]);
    assert_eq!(working_dir_of(&sandbox, &home), json!(repo.to_str().unwrap()));
    assert!(planned.exists(), "undo keeps the worktree");
}

#[test]
fn removal_only_touches_a_recorded_clean_worktree_that_no_session_uses() {
    let sandbox = Sandbox::new("worktree-remove");
    let home = sandbox.home("a");
    let repo = worktree_fixture(&sandbox, &home);
    let planned = sandbox.path().join("repo-worktrees").join("feat-x");
    let foreign = sandbox.path().join("foreign");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "foreign", foreign.to_str().unwrap()],
    );
    worktree_json(
        &sandbox,
        &home,
        &["move-to-worktree", "wt_one", "--branch", "feat-x", "--apply"],
    );

    let output = sandbox.run(
        &home,
        &["-C", repo.to_str().unwrap(), "sessions", "worktrees", "--json"],
    );
    assert_ok(&output);
    let rows: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let managed: Vec<bool> = rows["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["managed"].as_bool().unwrap())
        .collect();
    assert_eq!(managed.iter().filter(|managed| **managed).count(), 1, "{rows}");

    let remove = |path: &Path, apply: bool| {
        let mut args = vec!["sessions", "remove-worktree", path.to_str().unwrap()];
        if apply {
            args.push("--apply");
        }
        sandbox.run(&home, &args)
    };
    assert_refused(&remove(&foreign, true), "was not created by");
    assert!(foreign.exists());
    assert_refused(&remove(&planned, true), "still works in");

    worktree_json(&sandbox, &home, &["move-to-worktree", "wt_one", "--undo", "--apply"]);
    fs::write(planned.join("dirty.txt"), "work\n").unwrap();
    assert_refused(&remove(&planned, true), "has uncommitted changes");
    assert!(planned.join("dirty.txt").exists());

    fs::remove_file(planned.join("dirty.txt")).unwrap();
    assert_ok(&remove(&planned, false));
    assert!(planned.exists(), "a preview removes nothing");
    assert_ok(&remove(&planned, true));
    assert!(!planned.exists());
    assert!(repo.join("tracked.txt").exists() && foreign.exists());
}

#[test]
fn worktree_lifecycle_retains_history_filters_undone_canonicalizes_remove_and_guards_lock() {
    let sandbox = Sandbox::new("worktree-history");
    let home = sandbox.home("a");
    let repo = worktree_fixture(&sandbox, &home);
    let tree_a = sandbox.path().join("repo-worktrees").join("feat-a");
    let tree_b = sandbox.path().join("repo-worktrees").join("feat-b");

    worktree_json(
        &sandbox,
        &home,
        &["move-to-worktree", "wt_one", "--branch", "feat-a", "--apply"],
    );
    worktree_json(&sandbox, &home, &["move-to-worktree", "wt_one", "--undo", "--apply"]);

    // After undo, `sessions worktrees` must no longer list the tree as managed.
    let listed = sandbox.run(
        &home,
        &["-C", repo.to_str().unwrap(), "sessions", "worktrees", "--json"],
    );
    assert_ok(&listed);
    let rows: Value = serde_json::from_str(&stdout(&listed)).unwrap();
    let row_a = rows["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"] == json!(tree_a.to_str().unwrap()))
        .expect("feat-a listed");
    assert_eq!(row_a["managed"], json!(false), "{row_a}");
    assert_eq!(row_a["sessions"], json!([]), "{row_a}");

    // Moving the same session into a second worktree must not orphan the first record.
    worktree_json(
        &sandbox,
        &home,
        &["move-to-worktree", "wt_one", "--branch", "feat-b", "--apply"],
    );
    assert!(tree_b.exists());

    // Relative removal path canonicalizes and still removes feat-a after move to feat-b.
    let rel_a = tree_a
        .strip_prefix(sandbox.path())
        .expect("under sandbox")
        .to_str()
        .unwrap()
        .to_owned();
    assert_ok(&sandbox.run(&home, &["sessions", "remove-worktree", &rel_a, "--apply"]));
    assert!(!tree_a.exists());

    // Active `.writer.lock` must block `--undo --apply` and forward `--apply`.
    let lock_path = home.join("sessions").join("wt_one").join(".writer.lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock_file.lock().unwrap();

    let locked_undo = sandbox.run(&home, &["sessions", "move-to-worktree", "wt_one", "--undo", "--apply"]);
    assert_refused(&locked_undo, "live writer");
    drop(lock_file);

    worktree_json(&sandbox, &home, &["move-to-worktree", "wt_one", "--undo", "--apply"]);
    let tree_c = sandbox.path().join("repo-worktrees").join("feat-c");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock_file.lock().unwrap();
    let locked_move = sandbox.run(
        &home,
        &[
            "sessions",
            "move-to-worktree",
            "wt_one",
            "--branch",
            "feat-c",
            "--apply",
        ],
    );
    assert_refused(&locked_move, "live writer");
    assert!(!tree_c.exists(), "must not create worktree when writer lock is active");
}

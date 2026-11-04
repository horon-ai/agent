use super::*;

struct Repo {
    _root: tempfile::TempDir,
    path: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("a temp repository root");
        let path = root.path().join("repo");
        std::fs::create_dir_all(&path).expect("the repository directory");
        run(&path, &["init", "-q", "."]);
        run(
            &path,
            &[
                "-c",
                "user.email=t@example.test",
                "-c",
                "user.name=T",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        std::fs::write(path.join("file.txt"), "one\n").expect("a tracked file");
        run(&path, &["add", "file.txt"]);
        run(
            &path,
            &[
                "-c",
                "user.email=t@example.test",
                "-c",
                "user.name=T",
                "commit",
                "-q",
                "-m",
                "file",
            ],
        );
        Self { _root: root, path }
    }
}

fn run(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn link_worktree(repo: &Repo, path: &Path) {
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .arg("worktree")
        .arg("add")
        .arg("-q")
        .arg(path)
        .arg("-b")
        .arg("linked-branch")
        .output()
        .expect("git worktree add runs");
    assert!(
        output.status.success(),
        "worktree add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stash_is_refused_in_the_shared_main_checkout() {
    let repo = Repo::new();
    assert_eq!(
        review("git stash", &repo.path),
        Some(
            "git stash is blocked in the shared main checkout: other sessions may have uncommitted work here. \
             Use a linked worktree (communicate spawn with isolation=worktree) or commit your changes first. \
             Set [tools] git_guard = false to disable this guard."
                .to_string()
        )
    );
    assert!(
        review("cd sub && git stash pop", &repo.path).is_some(),
        "a chained segment is reviewed on its own"
    );
    for allowed in [
        "git stash list",
        "git stash show",
        "git stash create",
        "git status",
        "git log --oneline",
        "git diff",
        "ls",
    ] {
        assert_eq!(review(allowed, &repo.path), None, "{allowed:?} must not be refused");
    }
}

#[test]
fn history_rewrites_are_classified() {
    let repo = Repo::new();
    for refused in [
        "git reset --hard HEAD",
        "git reset --soft HEAD~1",
        "git reset HEAD~1",
        "git checkout -b feature",
        "git checkout -B feature",
        "git checkout --detach HEAD",
        "git checkout -f",
        "git checkout -- .",
        "git checkout .",
        "git switch main",
        "git restore .",
        "git restore --worktree .",
    ] {
        assert!(review(refused, &repo.path).is_some(), "{refused:?} must be refused");
    }
    for allowed in [
        "git reset HEAD",
        "git reset -- file.txt",
        "git reset --soft HEAD",
        "git checkout",
        "git checkout -- file.txt",
        "git checkout file.txt",
        "git restore --staged .",
        "git restore file.txt",
        "git switch",
        "git commit -m x",
    ] {
        assert_eq!(review(allowed, &repo.path), None, "{allowed:?} must not be refused");
    }
}

#[test]
fn a_linked_worktree_is_private_and_allowed() {
    let repo = Repo::new();
    let linked = repo._root.path().join("linked");
    link_worktree(&repo, &linked);
    for command in [
        "git stash",
        "git reset --hard HEAD",
        "git switch main",
        "git checkout -f",
    ] {
        assert_eq!(
            review(command, &linked),
            None,
            "{command:?} must be allowed in a linked worktree"
        );
    }
    assert!(
        review("git stash", &repo.path).is_some(),
        "the main checkout is still shared"
    );
}

#[test]
fn git_dir_overrides_are_refused_without_resolving_a_checkout() {
    let repo = Repo::new();
    for command in [
        "git --git-dir=/tmp/elsewhere/.git stash",
        "git --git-dir /tmp/elsewhere/.git stash",
        "git --work-tree=/tmp/elsewhere stash",
        "git --work-tree /tmp/elsewhere stash",
    ] {
        assert_eq!(
            review(command, &repo.path),
            Some(
                "git with --git-dir/--work-tree is blocked while [tools] git_guard is on: \
                 the guard cannot verify which checkout it changes."
                    .to_string()
            ),
            "{command:?}"
        );
    }
}

#[test]
fn a_dash_c_directory_is_the_checkout_that_gets_reviewed() {
    let repo = Repo::new();
    let subdir = repo.path.join("sub");
    std::fs::create_dir_all(&subdir).expect("a subdirectory");
    // From the subdirectory the main checkout is still shared.
    assert!(review("git stash", &subdir).is_some());
    // `-C` pointing back at the checkout must resolve the same place.
    assert!(
        review(&format!("git -C {} stash", repo.path.display()), &subdir).is_some(),
        "git -C must be honoured before deciding the place"
    );
}

#[test]
fn a_non_repository_is_not_guarded() {
    let dir = tempfile::tempdir().expect("a plain temp directory");
    assert_eq!(review("git stash", dir.path()), None);
}

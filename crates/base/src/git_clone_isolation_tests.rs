use super::*;
use std::path::Path;
use tempfile::TempDir;

/// A temporary directory whose `path()` is the canonical spelling, so the
/// paths Git records (which resolve symlinks such as macOS `/var`) match the
/// paths these tests compare against.
struct CanonTemp {
    _dir: TempDir,
    path: PathBuf,
}

impl CanonTemp {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn tempdir() -> CanonTemp {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    CanonTemp { _dir: dir, path }
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Fixture repository with one commit and a `sub/` directory.
fn repo() -> (CanonTemp, String) {
    let temp = tempdir();
    let path = temp.path().join("project");
    std::fs::create_dir(&path).unwrap();
    std::fs::create_dir(path.join("sub")).unwrap();
    git(&path, &["init", "-b", "main"]);
    git(&path, &["config", "user.name", "Clone Test"]);
    git(&path, &["config", "user.email", "clone-test@example.invalid"]);
    std::fs::write(path.join("tracked.txt"), "committed\n").unwrap();
    std::fs::write(path.join("sub").join("nested.txt"), "nested\n").unwrap();
    std::fs::write(path.join(".gitignore"), "target/\n").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "-m", "initial"]);
    let path = path.to_str().unwrap().to_string();
    (temp, path)
}

fn clone_root(repo_path: &str, branch: &str) -> PathBuf {
    let main = PathBuf::from(repo_path);
    let parent = main.parent().unwrap();
    let name = main.file_name().unwrap().to_str().unwrap();
    parent
        .join(format!("{name}-clones"))
        .join(git_worktree::safe_name(branch))
}

#[test]
fn create_copies_head_and_maps_the_working_directory() {
    let (_temp, path) = repo();
    let repo_path = Path::new(&path);
    std::fs::write(repo_path.join("tracked.txt"), "dirty edit\n").unwrap();
    std::fs::write(repo_path.join("untracked.txt"), "junk\n").unwrap();
    std::fs::create_dir_all(repo_path.join("target").join("debug")).unwrap();
    std::fs::write(repo_path.join("target").join("debug").join("cache.bin"), "cache\n").unwrap();

    let child = create(&path, "swarm/clone_test").unwrap();

    assert_eq!(child.branch, "swarm/clone_test");
    assert_eq!(child.base_head, git_worktree::head(&path).unwrap());
    assert_eq!(Path::new(&child.root), clone_root(&path, "swarm/clone_test"));
    assert!(Path::new(&child.root).join(".git").is_dir());
    // HEAD content, not the parent's dirty working tree.
    assert_eq!(
        std::fs::read_to_string(Path::new(&child.root).join("tracked.txt")).unwrap(),
        "committed\n"
    );
    // Untracked files are cleaned; ignored build output stays.
    assert!(!Path::new(&child.root).join("untracked.txt").exists());
    assert!(
        Path::new(&child.root)
            .join("target")
            .join("debug")
            .join("cache.bin")
            .exists()
    );
    assert_eq!(
        git_stdout(Path::new(&child.root), &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
        "swarm/clone_test"
    );

    let marker = read_owner_marker(Path::new(&child.root)).expect("marker written");
    assert_eq!(marker.kind, "clone");
    assert_eq!(marker.base_head, child.base_head);
    assert_eq!(marker.pid, std::process::id());

    // A nested working directory maps to the same relative path in the clone.
    let nested = create(&format!("{path}/sub"), "swarm/clone_nested").unwrap();
    assert_eq!(Path::new(&nested.working_dir), Path::new(&nested.root).join("sub"));
    assert_eq!(nested.working_dir, format!("{}/sub", nested.root));
}

#[test]
fn create_refuses_an_existing_destination() {
    let (_temp, path) = repo();
    let first = create(&path, "swarm/duplicate").unwrap();
    let error = create(&path, "swarm/duplicate").unwrap_err().to_string();
    assert!(error.contains("already exists"), "{error}");
    assert!(Path::new(&first.root).is_dir());
}

#[test]
fn remove_refuses_a_dirty_clone() {
    let (_temp, path) = repo();
    let child = create(&path, "swarm/dirty_clone").unwrap();
    std::fs::write(Path::new(&child.root).join("tracked.txt"), "changed\n").unwrap();
    let error = remove(Path::new(&child.root)).unwrap_err().to_string();
    assert!(error.contains("uncommitted changes"), "{error}");
    assert!(Path::new(&child.root).join("tracked.txt").exists());
}

#[test]
fn fetch_branch_makes_the_clone_branch_available_in_the_parent() {
    let (_temp, path) = repo();
    let child = create(&path, "swarm/fetched").unwrap();
    std::fs::write(Path::new(&child.root).join("tracked.txt"), "from clone\n").unwrap();
    git(Path::new(&child.root), &["add", "-A"]);
    git(Path::new(&child.root), &["commit", "-m", "clone change"]);
    fetch_branch(&path, Path::new(&child.root), &child.branch).unwrap();
    assert_eq!(
        git_stdout(Path::new(&path), &["rev-parse", "swarm/fetched"]).unwrap(),
        git_stdout(Path::new(&child.root), &["rev-parse", "HEAD"]).unwrap()
    );
}

/// A PID that is certainly not running: a child that has already exited.
fn dead_pid() -> u32 {
    let mut child = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn overwrite_marker_pid(checkout: &Path, pid: u32) {
    let mut marker = read_owner_marker(checkout).unwrap();
    marker.pid = pid;
    std::fs::write(marker_path(checkout), serde_json::to_string(&marker).unwrap()).unwrap();
}

#[test]
fn sweep_reaps_dead_clean_checkouts_and_keeps_live_or_changed_ones() {
    let (_temp, path) = repo();
    let clean = create(&path, "swarm/dead_clean").unwrap();
    let dirty = create(&path, "swarm/dead_dirty").unwrap();
    let live = create(&path, "swarm/live").unwrap();
    std::fs::write(Path::new(&dirty.root).join("tracked.txt"), "changed\n").unwrap();
    overwrite_marker_pid(Path::new(&clean.root), dead_pid());
    overwrite_marker_pid(Path::new(&dirty.root), dead_pid());

    let reaped = sweep_orphans(&path).unwrap();

    assert_eq!(reaped, vec![clean.root.clone()]);
    assert!(!Path::new(&clean.root).exists());
    assert!(!marker_path(Path::new(&clean.root)).exists());
    assert!(Path::new(&dirty.root).exists());
    assert!(Path::new(&live.root).exists());
}

#[test]
fn sweep_keeps_checkouts_with_commits_beyond_the_base() {
    let (_temp, path) = repo();
    let child = create(&path, "swarm/dead_committed").unwrap();
    std::fs::write(Path::new(&child.root).join("tracked.txt"), "child work\n").unwrap();
    git(Path::new(&child.root), &["add", "-A"]);
    git(Path::new(&child.root), &["commit", "-m", "child work"]);
    overwrite_marker_pid(Path::new(&child.root), dead_pid());

    assert!(sweep_orphans(&path).unwrap().is_empty());
    assert!(Path::new(&child.root).exists());
}

#[test]
fn sweep_ignores_checkouts_without_a_marker() {
    let (_temp, path) = repo();
    let child = create(&path, "swarm/no_marker").unwrap();
    std::fs::remove_file(marker_path(Path::new(&child.root))).unwrap();
    assert!(sweep_orphans(&path).unwrap().is_empty());
    assert!(Path::new(&child.root).exists());
}

#[test]
fn create_from_a_linked_worktree_copies_the_main_repository() {
    let (_temp, path) = repo();
    let linked = git_worktree::create(&path, "feature/linked").unwrap();
    let before = git_worktree::head(&linked.path).unwrap();

    let child = create(&linked.path, "swarm/from_linked").unwrap();

    // The clone owns a real `.git` directory; copying a linked worktree's
    // gitfile would have pointed every clone command back at the original.
    assert!(Path::new(&child.root).join(".git").is_dir());
    assert_eq!(child.working_dir, child.root);
    std::fs::write(Path::new(&child.root).join("tracked.txt"), "clone only\n").unwrap();
    git(Path::new(&child.root), &["add", "-A"]);
    git(Path::new(&child.root), &["commit", "-m", "clone only"]);
    assert_eq!(git_worktree::head(&linked.path).unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(Path::new(&linked.path).join("tracked.txt")).unwrap(),
        "committed\n"
    );
}

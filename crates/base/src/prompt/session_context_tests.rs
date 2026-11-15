//! The `session_context` measurement and the tests of the two builders it
//! measures: the git lines and the GPU line of a session context.
//!
//! A session context is built when a session is created or restored. Its git
//! lines and its GPU line come from external commands, so each one is a fork
//! on the path that opens a session. The measurement runs the two builders in
//! three fixed situations and reports how many processes each one started.
//! The other tests hold the builders to the lines they produced when every
//! situation cost a fork.

use std::cell::Cell;
use std::path::Path;
use std::process::Command;

use super::git_context::{
    Route, StatusFailed, branch_from_header, environment_selects_repository, get_git_info, git_info_by_three_calls,
    git_info_from_status, route,
};
use super::hardware::{gpu_summary, gpu_summary_under, lists_display_controller, lspci_display_controllers};

fn git_spawns() -> u64 {
    horon_agent_perf_counters::snapshot().session_context_git_spawns
}

fn gpu_probe_spawns() -> u64 {
    horon_agent_perf_counters::snapshot().session_context_gpu_probe_spawns
}

/// The git lines for `dir`, and the `git` processes started to produce them.
fn git_info_and_spawns(dir: &Path) -> (Option<String>, u64) {
    let before = git_spawns();
    let info = get_git_info(Some(dir));
    (info, git_spawns() - before)
}

/// Runs `git` in `dir` to build a fixture. The identity is passed on the
/// command line so a commit works on a machine that has none configured.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=horon", "-c", "user.email=horon@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .status();
    assert!(
        status.is_ok_and(|status| status.success()),
        "git {args:?} failed in {}",
        dir.display()
    );
}

/// A work tree on branch `trunk` that has no commit yet.
fn unborn_repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory for a repository");
    git(dir.path(), &["init", "--quiet"]);
    git(dir.path(), &["symbolic-ref", "HEAD", "refs/heads/trunk"]);
    dir
}

/// A clean work tree on branch `trunk` with one commit.
fn repository() -> tempfile::TempDir {
    let dir = unborn_repository();
    std::fs::write(dir.path().join("tracked.txt"), "tracked\n").expect("a tracked file");
    git(dir.path(), &["add", "tracked.txt"]);
    git(dir.path(), &["commit", "--quiet", "-m", "first"]);
    dir
}

/// Holds `dir` to the one-call path: it is routed as a work tree, the status
/// call answers, and the answer is the three-call path's, byte for byte.
/// Returns those lines.
fn one_call_lines(dir: &Path) -> Option<String> {
    assert_eq!(
        route(Some(dir), false),
        Route::WorkTree,
        "{} is not read as a work tree",
        dir.display()
    );
    let three_calls = git_info_by_three_calls(Some(dir));
    assert_eq!(
        git_info_from_status(Some(dir)),
        Ok(three_calls.clone()),
        "one status call and three calls disagree in {}",
        dir.display()
    );
    assert_eq!(get_git_info(Some(dir)), three_calls);
    three_calls
}

/// How many child processes a session context costs: `git` for a directory
/// outside any repository, `git` for a fresh repository, and the GPU probe on
/// a machine whose PCI device list holds no display controller.
///
/// Attempts are counted, so a machine without `lspci` still measures the fork
/// it tried. The numbers are reported, not asserted: the structural gate owns
/// the bounds. What is asserted is that each situation is the one its key
/// names, because a count taken somewhere else would read as a measurement.
/// `gpu_probe_spawns_here` and `gpu_reported` describe the machine running the
/// test (the session path caches the GPU line for the life of the process, so
/// the probe is called directly); the gate reads neither.
#[test]
fn session_context_child_processes_are_measured() {
    // The counters are process-wide; the lock keeps another test of this
    // crate from starting `git` between two readings.
    let _env = crate::storage::lock_test_env();
    assert!(
        horon_agent_perf_counters::enabled(),
        "the counters are off, so the spawns below measure nothing"
    );

    let outside = tempfile::tempdir().expect("a directory outside any repository");
    let (outside_info, outside_repo) = git_info_and_spawns(outside.path());
    assert_eq!(
        outside_info, None,
        "the temporary directory is inside a repository, so this is not the outside-repository count"
    );

    let repo = unborn_repository();
    let (repo_info, in_repo) = git_info_and_spawns(repo.path());
    assert!(
        repo_info.is_some_and(|info| info.starts_with("Git:")),
        "a fresh repository reported no git lines"
    );

    let no_display = tempfile::tempdir().expect("a PCI device list without a display controller");
    let before = gpu_probe_spawns();
    let without_display = gpu_summary_under(no_display.path(), lspci_display_controllers);
    let gpu_probe = gpu_probe_spawns() - before;
    assert_eq!(
        without_display, None,
        "a device list without a display controller reported a GPU"
    );

    let before = gpu_probe_spawns();
    let gpu = gpu_summary();
    let gpu_probe_here = gpu_probe_spawns() - before;

    println!(
        "STRUCTURAL_PERF section=session_context git_spawns_outside_repo={outside_repo} \
         git_spawns_in_repo={in_repo} gpu_probe_spawns_without_display={gpu_probe} \
         gpu_probe_spawns_here={gpu_probe_here} gpu_reported={}",
        u8::from(gpu.is_some()),
    );
}

#[test]
fn git_context_matches_three_spawn_output() {
    // Holds the counter lock: this test starts counted `git` processes, which
    // would land in the measurement above if the two ran at the same time.
    let _env = crate::storage::lock_test_env();
    let repo = repository();
    assert_eq!(one_call_lines(repo.path()), Some("Git:\n  Branch: trunk".to_string()));

    // A linked work tree keeps a `.git` file, not a directory.
    let elsewhere = tempfile::tempdir().expect("a directory for a linked work tree");
    let linked = elsewhere.path().join("linked");
    let linked_arg = linked.to_str().expect("a UTF-8 temporary path");
    git(repo.path(), &["worktree", "add", "--quiet", "-b", "side", linked_arg]);
    assert!(linked.join(".git").is_file(), "the linked work tree has no .git file");
    assert_eq!(one_call_lines(&linked), Some("Git:\n  Branch: side".to_string()));

    // Seven changed paths: the count, the first five, and the marker.
    for name in ["a", "b", "c", "d", "e", "f", "g"] {
        std::fs::write(repo.path().join(format!("{name}.txt")), name).expect("an untracked file");
    }
    let seven = "Git:\n  Branch: trunk\n  Modified: 7 files\n    ?? a.txt\n    ?? b.txt\n    ?? c.txt\n    ?? d.txt\n    \
                 ?? e.txt\n    ...";
    assert_eq!(one_call_lines(repo.path()), Some(seven.to_string()));

    // A directory below the root is read through the same repository.
    let nested = repo.path().join("nested").join("deeper");
    std::fs::create_dir_all(&nested).expect("a nested directory");
    assert_eq!(one_call_lines(&nested), Some(seven.to_string()));
}

#[test]
fn git_context_edge_states() {
    let _env = crate::storage::lock_test_env();
    // Outside any repository: no route to git at all.
    let outside = tempfile::tempdir().expect("a directory outside any repository");
    assert_eq!(route(Some(outside.path()), false), Route::NoRepository);
    assert_eq!(get_git_info(Some(outside.path())), None);
    assert_eq!(git_info_by_three_calls(Some(outside.path())), None);

    // A branch without a commit is still named.
    let unborn = unborn_repository();
    assert_eq!(one_call_lines(unborn.path()), Some("Git:\n  Branch: trunk".to_string()));

    // A detached HEAD has no branch line, and nothing at all when clean.
    let detached = repository();
    git(detached.path(), &["checkout", "--quiet", "--detach"]);
    assert_eq!(one_call_lines(detached.path()), None);
    std::fs::write(detached.path().join("new.txt"), "new").expect("an untracked file");
    assert_eq!(
        one_call_lines(detached.path()),
        Some("Git:\n  Modified: 1 files\n    ?? new.txt".to_string())
    );

    // A bare repository and the inside of a git directory have no work tree
    // for `git status`, so git is asked the way it always was.
    let bare = tempfile::tempdir().expect("a directory for a bare repository");
    git(bare.path(), &["init", "--quiet", "--bare"]);
    assert_eq!(route(Some(bare.path()), false), Route::AskGit);
    let bare_lines = get_git_info(Some(bare.path()));
    assert_eq!(bare_lines, git_info_by_three_calls(Some(bare.path())));
    assert!(
        bare_lines.is_some_and(|lines| lines.starts_with("Git:\n  Branch: ")),
        "a bare repository lost its branch line"
    );
    let git_dir = detached.path().join(".git");
    assert_eq!(route(Some(&git_dir), false), Route::AskGit);
    assert_eq!(get_git_info(Some(&git_dir)), git_info_by_three_calls(Some(&git_dir)));

    // A repository named by the environment is not the one the walk finds.
    assert!(environment_selects_repository(|name| name == "GIT_DIR"));
    assert!(environment_selects_repository(|name| name == "GIT_WORK_TREE"));
    assert!(!environment_selects_repository(|_| false));
    assert_eq!(route(Some(outside.path()), true), Route::AskGit);
    assert_eq!(route(Some(unborn.path()), true), Route::AskGit);

    // A directory that does not exist is left to git, which reports nothing.
    let missing = outside.path().join("missing");
    assert_eq!(route(Some(&missing), false), Route::AskGit);
    assert_eq!(get_git_info(Some(&missing)), None);

    // A status call that cannot run is a failure, not an empty answer.
    assert_eq!(git_info_from_status(Some(&missing)), Err(StatusFailed));

    // The header texts git prints, old and new.
    assert_eq!(branch_from_header("trunk"), Some("trunk"));
    assert_eq!(
        branch_from_header("trunk...origin/trunk [ahead 1, behind 2]"),
        Some("trunk")
    );
    assert_eq!(
        branch_from_header("feature/a.b...origin/feature/a.b"),
        Some("feature/a.b")
    );
    assert_eq!(branch_from_header("No commits yet on trunk"), Some("trunk"));
    assert_eq!(
        branch_from_header("No commits yet on trunk...origin/trunk [gone]"),
        Some("trunk")
    );
    assert_eq!(branch_from_header("Initial commit on trunk"), Some("trunk"));
    assert_eq!(branch_from_header("HEAD (no branch)"), None);
}

/// Writes one PCI device directory with the given `class` file content.
fn pci_device(devices: &Path, address: &str, class: &str) {
    let device = devices.join(address);
    std::fs::create_dir_all(&device).expect("a PCI device directory");
    std::fs::write(device.join("class"), class).expect("a PCI class file");
}

/// The GPU line for `devices` with a probe that answers `probe GPU`, and how
/// often that probe ran.
fn gpu_line_and_probe_runs(devices: &Path) -> (Option<String>, u32) {
    let runs = Cell::new(0);
    let line = gpu_summary_under(devices, || {
        runs.set(runs.get() + 1);
        Some("probe GPU".to_string())
    });
    (line, runs.get())
}

#[test]
fn gpu_probe_runs_when_display_class_exists() {
    let _env = crate::storage::lock_test_env();
    for class in ["0x030000\n", "0x030200\n", "0x038000\n"] {
        let devices = tempfile::tempdir().expect("a PCI device list");
        pci_device(devices.path(), "network", "0x020000\n");
        pci_device(devices.path(), "display", class);
        assert!(
            lists_display_controller(devices.path()),
            "class {class:?} is a display controller"
        );
        assert_eq!(
            gpu_line_and_probe_runs(devices.path()),
            (Some("probe GPU".to_string()), 1),
            "class {class:?} did not reach the probe exactly once"
        );
    }
}

#[test]
fn gpu_probe_skipped_without_display_class() {
    let _env = crate::storage::lock_test_env();
    // Devices, none of them a display controller: network, storage, bridge,
    // and a device whose class file is missing.
    let devices = tempfile::tempdir().expect("a PCI device list");
    pci_device(devices.path(), "network", "0x020000\n");
    pci_device(devices.path(), "storage", "0x010802\n");
    pci_device(devices.path(), "bridge", "0x060000\n");
    std::fs::create_dir_all(devices.path().join("classless")).expect("a PCI device directory");
    assert!(!lists_display_controller(devices.path()));
    assert_eq!(gpu_line_and_probe_runs(devices.path()), (None, 0));

    // An empty list, and a system that has no such directory at all.
    let empty = tempfile::tempdir().expect("an empty PCI device list");
    assert_eq!(gpu_line_and_probe_runs(empty.path()), (None, 0));
    let absent = empty.path().join("absent");
    assert!(!lists_display_controller(&absent));
    assert_eq!(gpu_line_and_probe_runs(&absent), (None, 0));
}

//! The `session_context` measurement: child processes started to build a
//! session context.
//!
//! A session context is built when a session is created or restored. Its git
//! lines and its GPU line come from external commands, so each one is a fork
//! on the path that opens a session. The measurement runs the two builders in
//! three fixed situations and reports how many processes each one started.

use std::path::Path;
use std::process::Command;

use super::git_context::get_git_info;
use super::hardware::gpu_summary;

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

/// How many child processes a session context costs: `git` for a directory
/// outside any repository, `git` for a fresh repository, and the GPU probe on
/// its own (the session path caches it for the life of the process, so it is
/// called directly here).
///
/// Attempts are counted, so a machine without `lspci` still measures the fork
/// it tried. The numbers are reported, not asserted: the structural gate owns
/// the bounds. What is asserted is that each situation is the one its key
/// names, because a count taken somewhere else would read as a measurement.
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

    let repo = tempfile::tempdir().expect("a directory for a fresh repository");
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repo.path())
        .status();
    assert!(
        init.is_ok_and(|status| status.success()),
        "git init failed, so the in-repository count would measure a missing git"
    );
    let (repo_info, in_repo) = git_info_and_spawns(repo.path());
    assert!(
        repo_info.is_some_and(|info| info.starts_with("Git:")),
        "a fresh repository reported no git lines"
    );

    let before = gpu_probe_spawns();
    let gpu = gpu_summary();
    let gpu_probe = gpu_probe_spawns() - before;

    println!(
        "STRUCTURAL_PERF section=session_context git_spawns_outside_repo={outside_repo} \
         git_spawns_in_repo={in_repo} gpu_probe_spawns_without_display={gpu_probe} gpu_reported={}",
        u8::from(gpu.is_some()),
    );
}

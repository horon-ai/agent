//! Kernel change waits shared by the real-process suites: a file or directory
//! write, and a process exit.
//!
//! Both waits go through `horon_agent_platform`, so the OS event source (inotify
//! where the adapter has one, a plain sleep where it does not) never appears
//! here. This module only decides how long one wait blocks before the caller
//! re-checks its own predicate.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long one wait blocks before the caller re-checks its own predicate.
///
/// The platform wait reports an event or the timeout, and this helper arms
/// nothing before the caller scans. Capping the slice bounds what a change that
/// happens between the scan and the wait can cost: at most one slice, after
/// which the caller re-scans and sees it. Small enough that a probe adds no
/// measurable latency to a local daemon start, large enough not to spin a core.
const WAIT_SLICE: Duration = Duration::from_millis(25);

/// Whether the process table reports `pid` as gone.
///
/// A missing entry means the child was reaped; a zombie is the exit a process
/// event reports before the parent waits for it, so both count as exited.
fn process_exited(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| matches!(rest.trim_start().chars().next(), Some('Z' | 'X'))),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        !horon_agent_platform::is_process_running(pid as u32)
    }
}

/// Waits for process `pid` to exit, checking the process table once per slice.
///
/// A pid that is already gone reports success, because the exit the caller waits
/// for has happened.
pub fn wait_for_pid_exit(pid: i32, deadline: Duration) -> Result<(), String> {
    let end = Instant::now() + deadline;
    loop {
        if process_exited(pid) {
            return Ok(());
        }
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("process {pid} still runs after {deadline:?}"));
        }
        std::thread::sleep(remaining.min(WAIT_SLICE));
    }
}

/// A wait on the paths a probe watches.
///
/// The caller arms the watch, scans, and then waits, so a write that lands
/// between the scan and the wait costs one slice rather than the whole
/// deadline.
pub struct ChangeWatch {
    paths: Vec<PathBuf>,
}

impl ChangeWatch {
    /// Registers a wait on every path the caller wants to notice a change to.
    pub fn arm(paths: &[&Path]) -> Self {
        Self {
            paths: paths.iter().map(|path| path.to_path_buf()).collect(),
        }
    }

    /// Blocks until one watched path changes or the slice expires.
    pub fn wait(&self, timeout: Duration) {
        // A path the platform cannot watch, or a wait that fails, costs this
        // slice only: the caller's deadline and predicate still decide the
        // outcome.
        drop(horon_agent_platform::watch::wait_for_any_change(
            &self.paths,
            timeout.min(WAIT_SLICE),
        ));
    }
}

//! POSIX primitives shared by the macOS and Linux adapters.
//!
//! `platform-linux` owns this file and compiles it directly; `platform-macos`
//! pulls the same source in with `#[path = ...]`, so the two Unix adapters
//! cannot drift apart. The file never refers to `crate::` items: it is a
//! self-contained module compiled into whichever adapter includes it.

use std::io;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use horon_agent_logging as logging;

fn desired_nofile_soft_limit(current: u64, hard: u64, minimum: u64) -> Option<u64> {
    let desired = current.max(minimum).min(hard);
    (desired > current).then_some(desired)
}

/// Check if a process is running by PID.
///
/// `kill(pid, 0)` performs the permission and existence check without sending
/// a signal. Only `ESRCH` means the process is gone; `EPERM` still proves it
/// exists.
pub(crate) fn is_process_running(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    let err = io::Error::last_os_error();
    !matches!(err.raw_os_error(), Some(code) if code == libc::ESRCH)
}

/// Send a signal to an entire detached process group/session led by `pid`.
///
/// Detached tasks are spawned with `setsid()`, so the leader PID is also the
/// process-group/session ID. Signaling `-pid` reaches the full tree.
pub(crate) fn signal_detached_process_group(pid: u32, signal: i32) -> io::Result<()> {
    let rc = unsafe { libc::kill(-(pid as i32), signal) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Stop a process group gracefully: send SIGTERM to the group led by `pid`,
/// wait up to `grace` for the leader to exit, then SIGKILL the group.
///
/// `pid` must be a process-group leader (spawned with `setpgid`/`setsid`), which
/// is how `bash` children are started. Returns whether the group is gone when
/// this returns.
pub(crate) fn terminate_process_group(pid: u32, grace: Duration) -> bool {
    if !is_process_running(pid) {
        return true;
    }
    // Best effort: the poll below decides whether the group actually exits.
    drop(signal_detached_process_group(pid, libc::SIGTERM));
    let deadline = Instant::now() + grace;
    loop {
        if !is_process_running(pid) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50).min(deadline - now));
    }
    drop(signal_detached_process_group(pid, libc::SIGKILL));
    // SIGKILL is immediate for the kernel, but the process entry lingers
    // until it is reaped. Poll briefly instead of claiming success blindly.
    for _ in 0..50 {
        if !is_process_running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    !is_process_running(pid)
}

/// Best-effort non-blocking reap for a child process owned by the current process.
///
/// Returns:
/// - `Ok(Some(exit_code))` if the child exited and was reaped now
/// - `Ok(None)` if it is still running or is not our child
pub(crate) fn try_reap_child_process(pid: u32) -> io::Result<Option<i32>> {
    let mut status = 0;
    let rc = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
    if rc == 0 {
        return Ok(None);
    }
    if rc == -1 {
        let err = io::Error::last_os_error();
        if matches!(err.raw_os_error(), Some(code) if code == libc::ECHILD) {
            return Ok(None);
        }
        return Err(err);
    }

    if libc::WIFEXITED(status) {
        Ok(Some(libc::WEXITSTATUS(status)))
    } else if libc::WIFSIGNALED(status) {
        Ok(Some(128 + libc::WTERMSIG(status)))
    } else {
        Ok(Some(-1))
    }
}

/// Atomically swap a symlink by creating a temp symlink and renaming.
pub(crate) fn atomic_symlink_swap(src: &Path, dst: &Path, temp: &Path) -> io::Result<()> {
    // A leftover staging entry is expected on the first call and harmless.
    drop(std::fs::remove_file(temp));
    std::os::unix::fs::symlink(src, temp)?;
    std::fs::rename(temp, dst)
}

/// Spawn a process detached from the current client session.
///
/// This is used for launching new terminal windows (for `/resume`, `/split`,
/// crash restore, etc.) so the new client survives if the invoking horon-agent
/// process exits or its terminal closes.
pub(crate) fn spawn_detached(cmd: &mut Command) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;

    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn()
}

/// Spawn a fire-and-forget child that must never flash a console window.
///
/// Consoles are a Windows concept, so on Unix this is exactly
/// [`spawn_detached`].
pub(crate) fn spawn_detached_without_console_window(cmd: &mut Command) -> io::Result<Child> {
    spawn_detached(cmd)
}

/// Reap a detached child without blocking the caller.
pub(crate) fn reap_detached(child: Child) {
    let mut child = child;
    // A thread that cannot start only leaves the child unreaped, which the OS
    // handles when this process exits.
    drop(
        std::thread::Builder::new()
            .name("horon-agent-detached-child".to_string())
            .spawn(move || {
                drop(child.wait());
            }),
    );
}

/// Replace the current process with a new command (exec on Unix).
///
/// On success this never returns.
pub(crate) fn replace_process(cmd: &mut Command) -> io::Error {
    use std::os::unix::process::CommandExt;

    let err = cmd.exec();
    logging::error(&format!(
        "replace_process failed: {} ({})",
        err,
        process_fd_diagnostic_snapshot()
    ));
    err
}

/// Best-effort increase of the current process soft `RLIMIT_NOFILE`.
///
/// This helps horon-agent survive short-lived reload/connect spikes even when it was
/// launched from a shell with a conservative `ulimit -n` like 1024.
pub(crate) fn raise_nofile_limit_best_effort(minimum_soft_limit: u64) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        logging::warn(&format!("Failed to read RLIMIT_NOFILE: {}", io::Error::last_os_error()));
        return;
    }

    // `rlim_cur`/`rlim_max` are `u64` on Linux/macOS but `i64` on some
    // platforms (e.g. FreeBSD), so cast explicitly to keep builds portable.
    // The cast is a no-op (and clippy-flagged) where the field is already
    // `u64`, hence the allow.
    #[allow(clippy::unnecessary_cast)]
    let current: u64 = limit.rlim_cur as u64;
    #[allow(clippy::unnecessary_cast)]
    let hard: u64 = limit.rlim_max as u64;
    let Some(desired) = desired_nofile_soft_limit(current, hard, minimum_soft_limit) else {
        return;
    };

    let updated = libc::rlimit {
        rlim_cur: desired as libc::rlim_t,
        rlim_max: limit.rlim_max,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &updated) } == 0 {
        logging::info(&format!(
            "Raised RLIMIT_NOFILE soft limit from {} to {} (hard={})",
            current, desired, hard
        ));
    } else {
        logging::warn(&format!(
            "Failed to raise RLIMIT_NOFILE from {} toward {} (hard={}): {}",
            current,
            desired,
            hard,
            io::Error::last_os_error()
        ));
    }
}

/// Create a symlink.
pub(crate) fn symlink_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// Set file permissions to owner read/write/execute (0o755).
pub(crate) fn set_permissions_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let perms = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(path, perms)
}

/// Summarize the current process's file-descriptor usage for debugging reload or
/// connect failures such as EMFILE/`Too many open files`.
fn process_fd_diagnostic_snapshot() -> String {
    let pid = std::process::id();
    let fd_dir = Path::new("/proc/self/fd");
    let mut total = 0usize;
    let mut sockets = 0usize;
    let mut pipes = 0usize;
    let mut anon = 0usize;
    let mut chars = 0usize;
    let mut regs = 0usize;
    let mut dirs = 0usize;
    let mut other = 0usize;

    if let Ok(entries) = std::fs::read_dir(fd_dir) {
        for entry in entries.flatten() {
            total += 1;
            let target = match std::fs::read_link(entry.path()) {
                Ok(path) => path.to_string_lossy().into_owned(),
                Err(_) => String::new(),
            };
            if target.starts_with("socket:") {
                sockets += 1;
            } else if target.starts_with("pipe:") {
                pipes += 1;
            } else if target.starts_with("anon_inode:") {
                anon += 1;
            } else if target.starts_with("/dev/") {
                chars += 1;
            } else if target.starts_with('/') {
                match std::fs::metadata(&target) {
                    Ok(meta) if meta.is_file() => regs += 1,
                    Ok(meta) if meta.is_dir() => dirs += 1,
                    Ok(_) | Err(_) => other += 1,
                }
            } else {
                other += 1;
            }
        }
    }

    let (soft_limit, hard_limit) =
        read_max_open_files_limits().unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));

    format!(
        "pid={} fds={} soft_limit={} hard_limit={} kinds={{socket:{}, pipe:{}, anon_inode:{}, char:{}, file:{}, dir:{}, other:{}}}",
        pid, total, soft_limit, hard_limit, sockets, pipes, anon, chars, regs, dirs, other
    )
}

fn read_max_open_files_limits() -> Option<(String, String)> {
    let contents = match std::fs::read_to_string("/proc/self/limits") {
        Ok(contents) => contents,
        Err(_) => return None,
    };
    contents.lines().find_map(|line| {
        let parts: Vec<_> = line.split_whitespace().collect();
        (parts.len() >= 5 && parts[0] == "Max" && parts[1] == "open" && parts[2] == "files")
            .then(|| (parts[3].to_string(), parts[4].to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_nofile_soft_limit_only_raises_when_possible() {
        assert_eq!(desired_nofile_soft_limit(1024, 524_288, 8192), Some(8192));
        assert_eq!(desired_nofile_soft_limit(8192, 524_288, 8192), None);
        assert_eq!(desired_nofile_soft_limit(1024, 4096, 8192), Some(4096));
    }

    #[test]
    fn spawn_detached_creates_new_session() {
        let parent_sid = unsafe { libc::getsid(0) };
        assert!(parent_sid > 0, "the test process should belong to a session");

        // The child must outlive the check: `getsid(pid)` answers only while the
        // process exists. `trap '' HUP` keeps this heredoc-free `sh` alive for the
        // check without making it depend on the terminal it just left.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("trap '' HUP; sleep 5")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let mut child = spawn_detached(&mut cmd).expect("spawn detached child");

        // `spawn_detached` calls `setsid()` in the pre-exec hook, so the child is
        // its own session leader: its session id equals its pid, and never the
        // parent's session.
        let child_sid = Instant::now();
        let mut observed = unsafe { libc::getsid(child.id() as libc::pid_t) };
        while observed == -1 && child_sid.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(5));
            observed = unsafe { libc::getsid(child.id() as libc::pid_t) };
        }
        let _ = child.kill();
        // The kill above makes the exit status deliberate; the spawn itself already
        // succeeded, which is what this test asserts.
        let _ = child.wait();

        assert_eq!(
            observed,
            child.id() as i32,
            "detached child should lead its own session"
        );
        assert_ne!(observed, parent_sid, "detached child should not share parent session");
    }

    #[test]
    fn is_process_running_false_for_reaped_pid() {
        // `wait()` reaps the child, so the pid no longer names any process and
        // `kill(pid, 0)` answers ESRCH. The pid is not reused while the
        // assertion runs, because nothing on this host allocates pids that fast.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a short-lived child");
        let pid = child.id();
        child.wait().expect("reap the child");

        assert!(!is_process_running(pid), "a reaped pid must not be reported as running");
    }
}

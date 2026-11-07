//! Process primitives: group signalling, child placement and diagnostics.

use std::io;
use std::process::Command;

use horon_agent_platform_api::log::{self, PlatformLogLevel};
pub use horon_agent_platform_api::{ReadyChild, StopRequest};

use crate::Current;
use crate::PlatformAdapter;

/// Place `command`'s child in its own process group so the whole tree can be
/// signalled as a unit.
pub fn isolate_process_group(command: &mut Command) {
    Current::isolate_process_group(command);
}

/// Summarize this process's file-descriptor usage for debugging `EMFILE` and
/// similar failures.
pub fn fd_diagnostic_snapshot() -> String {
    Current::fd_diagnostic_snapshot()
}

/// Tune the system memory allocator for a long-running server.
///
/// `use_system_allocator` is false when the binary linked a different global
/// allocator, where the platform's own tuning has nothing to act on. The Linux
/// implementation sets glibc's arena count and mmap threshold; every other
/// platform does nothing.
pub fn configure_system_allocator(use_system_allocator: bool) {
    Current::configure_system_allocator(use_system_allocator);
}

/// The main-thread stack size the entry point must request.
///
/// `None` means the platform default is big enough and the entry point runs on
/// the main thread directly. Windows asks for a larger stack because its
/// default overflows during CLI bootstrap.
pub fn main_thread_stack_size() -> Option<usize> {
    Current::main_thread_stack_size()
}

/// Arm this platform's parent-death behaviour for the current process and report
/// whether the parent is already gone.
///
/// Only Linux can tie a child's death to its parent's; the other platforms
/// report `false` and leave shutdown to the caller.
pub fn bind_process_lifetime_to_parent() -> bool {
    Current::bind_process_lifetime_to_parent()
}

/// Ask the process group led by `pid` to stop.
///
/// Unix sends `SIGTERM` and reports [`StopRequest::Signaled`], which a forced
/// kill can still escalate. Windows has no catchable termination signal: it
/// terminates the tree and reports [`StopRequest::Terminated`].
pub fn request_graceful_stop(pid: u32) -> io::Result<StopRequest> {
    Current::request_graceful_stop(pid)
}

/// Force the process group led by `pid` to exit now.
pub fn force_stop_process_group(pid: u32) -> io::Result<()> {
    Current::force_stop_process_group(pid)
}

/// Spawn `cmd` with a readiness pipe the child inherits where the platform can
/// pass a descriptor.
///
/// The child writes one byte to the descriptor named by `ready_env_var` when it
/// can serve requests; the parent reads it. Windows cannot pass that descriptor
/// through process creation, so its child is spawned plainly and `ready` is
/// `None`.
pub fn spawn_with_ready_pipe(cmd: &mut Command, ready_env_var: &str) -> io::Result<ReadyChild> {
    Current::spawn_with_ready_pipe(cmd, ready_env_var)
}

/// Suppress the console window a spawned child would otherwise get.
pub fn hide_console_window(command: &mut Command) {
    Current::hide_console_window(command);
}

/// The command that runs `command` through this platform's shell.
///
/// `temp_dir` points the shell's temporary-directory variables at it on Unix
/// and is ignored on Windows.
pub fn shell_command(command: &str, temp_dir: Option<&std::path::Path>) -> Command {
    Current::shell_command(command, temp_dir)
}

/// [`shell_command`], but the command text is passed as a normal argument.
///
/// Windows re-encodes such an argument for `CommandLineToArgvW`, so nested
/// quotes can change; the process monitor has always spawned its configured
/// command this way.
pub fn shell_command_escaped(command: &str, temp_dir: Option<&std::path::Path>) -> Command {
    Current::shell_command_escaped(command, temp_dir)
}

/// Working directories of the processes this platform can inspect, used to
/// keep maintenance from deleting a directory a live process still sits in.
pub fn process_working_dirs() -> Vec<std::path::PathBuf> {
    Current::process_working_dirs()
}

/// This process's memory facts for the diagnostic snapshot.
pub fn process_memory_facts() -> horon_agent_platform_api::ProcessMemoryFacts {
    Current::process_memory_facts()
}

/// Set the short process name the operating system reports for this process.
pub fn set_process_name(name: &str) {
    Current::set_process_name(name);
}

/// Kill the process group led by `pid` immediately.
pub fn kill_process_group(pid: u32) {
    Current::kill_process_group(pid);
}

/// Stop a child process and reap it.
///
/// Unix asks politely with `SIGTERM`, waits up to three seconds, then kills;
/// Windows terminates at once. The child never survives this call.
pub fn terminate_child(child: &mut std::process::Child) {
    Current::terminate_child(child);
}

/// Whether this platform has POSIX process groups that a signal can reach as a
/// unit.
pub fn process_groups_supported() -> bool {
    cfg!(unix)
}

/// Whether this platform can replace the running process image with another
/// program (`exec`).
pub fn supports_process_replacement() -> bool {
    cfg!(unix)
}

/// Keeps a child's process group alive only while the guard is armed.
///
/// The shell tool arms the guard with the child's pid so that dropping the
/// future that owns the child (a cancelled call, a panicking sibling task)
/// takes the whole process tree down instead of orphaning it. Dropping the
/// guard never blocks: the polite-then-forced sequence runs on its own thread.
///
/// On a platform without process groups the guard is inert, because there is no
/// group to reach and the child is not this process's to kill.
pub struct ProcessGroupGuard {
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    /// Arm the guard for `pid`, or produce an inert guard when there is none.
    ///
    /// A `pid` on a platform without process groups produces an inert guard.
    pub fn new(pid: Option<u32>) -> Self {
        Self {
            pid: if process_groups_supported() { pid } else { None },
        }
    }

    /// Stop guarding: the child is expected to outlive this scope.
    pub fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            // Give the group a chance to run its TERM handler before the hard
            // kill, but never block the dropping task or runtime thread for the
            // grace period: the wait happens on its own thread.
            let spawned = std::thread::Builder::new()
                .name(format!("horon-agent-term-{pid}"))
                .spawn(move || {
                    crate::terminate_process_group(pid, std::time::Duration::from_secs(1));
                });
            if let Err(err) = spawned {
                log::log(
                    PlatformLogLevel::Warn,
                    &format!("process-group guard could not start a terminator for pid {pid}: {err}"),
                );
                // SIGKILL is 9 on every supported Unix.
                if let Err(kill_err) = crate::signal_detached_process_group(pid, 9) {
                    log::log(
                        PlatformLogLevel::Warn,
                        &format!("process-group guard could not kill pid {pid}: {kill_err}"),
                    );
                }
            }
        }
    }
}

/// Whether `error` reports that this process has run out of file descriptors.
///
/// The codes are the platform's own: `EMFILE` is 24 on every supported Unix, and
/// Windows reports `ERROR_TOO_MANY_OPEN_FILES` (4) or `WSAEMFILE` (10024)
/// depending on the call that failed.
pub fn is_too_many_open_files(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(24)
    }
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(4) | Some(10024))
    }
}

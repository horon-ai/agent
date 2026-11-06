//! Terminal and stdin-state primitives.

pub use horon_agent_platform_api::StdinState;

use crate::Current;
use crate::PlatformAdapter;

#[cfg(all(unix, not(target_os = "macos")))]
use horon_agent_platform_linux::term as os;
#[cfg(target_os = "macos")]
use horon_agent_platform_macos::term as os;
#[cfg(windows)]
use horon_agent_platform_windows::term as os;

/// Whether `pid` is blocked reading its standard input.
pub fn stdin_state(pid: u32) -> StdinState {
    Current::stdin_state(pid)
}

/// Whether `pid` or a descendant sharing its standard input is blocked reading
/// it.
pub fn process_tree_stdin_state(pid: u32) -> StdinState {
    Current::process_tree_stdin_state(pid)
}

/// The immediate children of `pid`, empty where the platform does not expose the
/// parent link cheaply.
pub fn direct_children(pid: u32) -> Vec<u32> {
    Current::direct_children(pid)
}

pub use os::Pty;

/// Whether this platform can allocate a POSIX pseudo-terminal.
pub fn pty_supported() -> bool {
    os::pty_supported()
}

/// A command that runs `command` text detached from the current session and
/// records its final working directory in `cwd_file` when one is given.
///
/// `None` means this platform has no local shell that can perform the capture,
/// so the caller must not take the detached path at all.
pub fn detached_shell_command(command: &str, cwd_file: Option<&std::path::Path>) -> Option<std::process::Command> {
    os::detached_shell_command(command, cwd_file)
}

/// Whether the default shell reports POSIX `sh` syntax.
pub fn shell_is_posix() -> bool {
    os::shell_is_posix()
}

/// Extra environment entries the platform shell needs beyond the shared set.
pub fn shell_env_overrides() -> Vec<(&'static str, String)> {
    os::shell_env_overrides()
}

/// Whether the platform passes a scratch directory to child shells through
/// `TMPDIR`.
pub fn passes_scratch_dir_to_shell() -> bool {
    os::passes_scratch_dir_to_shell()
}

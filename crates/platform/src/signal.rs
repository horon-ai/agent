//! Signal decoding, process identity and termination requests.

use std::process::ExitStatus;

#[cfg(all(unix, not(target_os = "macos")))]
use horon_agent_platform_linux::signal as os;
#[cfg(target_os = "macos")]
use horon_agent_platform_macos::signal as os;
#[cfg(windows)]
use horon_agent_platform_windows::signal as os;

/// The signal that terminated the process, when a signal terminated it.
pub fn exit_signal(status: &ExitStatus) -> Option<i32> {
    os::exit_signal(status)
}

/// The conventional name of `signal` for a command's exit status, or `SIG<n>`
/// when the platform's table has no name for it.
pub fn signal_name(signal: i32) -> String {
    os::signal_name(signal)
}

/// A sentence describing a process that `signal` terminated.
pub fn signal_crash_reason(signal: i32) -> String {
    os::signal_crash_reason(signal)
}

/// The parent process id, when the platform exposes it.
pub fn parent_pid() -> Option<u32> {
    os::parent_pid()
}

/// Wait until the process is asked to terminate gracefully.
pub async fn wait_for_termination() {
    os::wait_for_termination().await
}

/// Spawn a watcher that calls `on_signal` for every termination signal this
/// platform delivers to the process.
///
/// Unix watches `SIGHUP`, `SIGTERM`, `SIGINT` and `SIGQUIT` in one task each, so
/// the callback runs on the runtime and can start a clean shutdown. Windows
/// delivers these through the console instead and installs nothing, so the
/// callback never runs there.
pub fn spawn_termination_signal_watchers<F>(on_signal: F)
where
    F: Fn(i32) + Send + Sync + 'static,
{
    os::spawn_termination_signal_watchers(on_signal);
}

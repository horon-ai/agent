//! Signals and process identity on Windows.
//!
//! Windows has no catchable termination signal and no parent process id in the
//! standard library, so the answering functions report absence and the wait
//! never resolves.

use std::process::ExitStatus;

/// A Windows exit status carries no terminating signal.
pub fn exit_signal(_status: &ExitStatus) -> Option<i32> {
    None
}

/// Windows delivers termination through the console, not as POSIX signal
/// numbers, so no number has a name here.
pub fn signal_name(_signal: i32) -> String {
    "unknown".to_string()
}

/// Windows never reports a POSIX signal as a crash cause; the exit code carries
/// the reason instead.
pub fn signal_crash_reason(signal: i32) -> String {
    format!("Terminated by signal {signal}")
}

/// Windows exposes no parent process id through the standard library.
pub fn parent_pid() -> Option<u32> {
    None
}

/// Windows has no graceful termination request this process can receive, so
/// the future never resolves.
pub async fn wait_for_termination() {
    std::future::pending::<()>().await;
}

/// Windows installs no POSIX signal watcher, so the callback never runs.
pub fn spawn_termination_signal_watchers<F>(_on_signal: F)
where
    F: Fn(i32) + Send + Sync + 'static,
{
}

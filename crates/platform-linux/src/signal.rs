//! Signals and process identity on Unix.
//!
//! `platform-linux` owns this file and compiles it directly; `platform-macos`
//! pulls it in with `#[path = ...]`, so both Unix adapters share one
//! implementation.

use std::process::ExitStatus;

use horon_agent_platform_api::log::{PlatformLogLevel as Level, log as report};

/// The signal that terminated the process, when a signal terminated it.
pub fn exit_signal(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

/// The conventional name of `signal` for a command's exit status, or `SIG<n>`
/// when this table does not name it.
///
/// This is the table the `bash` tool reports with, so it covers the signals a
/// command dies from and nothing else.
pub fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGABRT => "SIGABRT",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGUSR2 => "SIGUSR2",
        other => return format!("SIG{other}"),
    };
    name.to_string()
}

/// The name a session-ending signal is reported with, or `"unknown"`.
///
/// The crash path names only the four signals it can actually receive plus the
/// five a user is likely to send, and says `"unknown"` for the rest; unlike
/// [`signal_name`] this is not a command-exit table.
fn session_signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        _ => "unknown",
    }
}

/// A sentence describing a process that `signal` terminated.
pub fn signal_crash_reason(signal: i32) -> String {
    match signal {
        libc::SIGHUP => "Terminal or window closed (SIGHUP)".to_string(),
        libc::SIGTERM => "Terminated (SIGTERM)".to_string(),
        libc::SIGINT => "Interrupted (SIGINT)".to_string(),
        libc::SIGQUIT => "Quit signal (SIGQUIT)".to_string(),
        _ => format!("Terminated by signal {} ({})", session_signal_name(signal), signal),
    }
}

/// The parent process id, when the platform exposes it.
pub fn parent_pid() -> Option<u32> {
    let ppid = unsafe { libc::getppid() };
    (ppid > 0).then_some(ppid as u32)
}

/// Wait until the process is asked to terminate gracefully.
///
/// Resolves once SIGTERM has been delivered. Registration failures leave the
/// future pending rather than resolving immediately: a caller that cannot
/// receive the signal has nothing to do on this turn.
pub async fn wait_for_termination() {
    use tokio::signal::unix::{SignalKind, signal};

    match signal(SignalKind::terminate()) {
        Ok(mut sigterm) => {
            sigterm.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}

/// Watch the signals that end a CLI session and report each arrival to
/// `on_signal`.
///
/// One task per signal, so a signal this process cannot watch costs only its
/// own task; the log line names it.
pub fn spawn_termination_signal_watchers<F>(on_signal: F)
where
    F: Fn(i32) + Send + Sync + 'static,
{
    use std::sync::Arc;
    use tokio::signal::unix::{SignalKind, signal};

    fn spawn_one<F>(signal_number: i32, kind: SignalKind, on_signal: Arc<F>)
    where
        F: Fn(i32) + Send + Sync + 'static,
    {
        tokio::spawn(async move {
            let mut stream = match signal(kind) {
                Ok(stream) => stream,
                Err(error) => {
                    report(
                        Level::Error,
                        &format!("Failed to install {} handler: {error}", signal_name(signal_number)),
                    );
                    return;
                }
            };
            if stream.recv().await.is_some() {
                on_signal(signal_number);
            }
        });
    }

    let on_signal = Arc::new(on_signal);
    spawn_one(libc::SIGHUP, SignalKind::hangup(), Arc::clone(&on_signal));
    spawn_one(libc::SIGTERM, SignalKind::terminate(), Arc::clone(&on_signal));
    spawn_one(libc::SIGINT, SignalKind::interrupt(), Arc::clone(&on_signal));
    spawn_one(libc::SIGQUIT, SignalKind::quit(), on_signal);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_status_signal_names_match_the_bash_tool_table() {
        assert_eq!(signal_name(libc::SIGHUP), "SIGHUP");
        assert_eq!(signal_name(libc::SIGTERM), "SIGTERM");
        assert_eq!(signal_name(libc::SIGKILL), "SIGKILL");
        assert_eq!(signal_name(libc::SIGUSR1), "SIGUSR1");
        // A signal outside the table keeps its number, exactly as the tool's
        // exit-status metadata always reported it.
        assert_eq!(signal_name(libc::SIGILL), format!("SIG{}", libc::SIGILL));
        assert_eq!(signal_name(libc::SIGTRAP), format!("SIG{}", libc::SIGTRAP));
    }

    #[test]
    fn crash_reasons_name_the_session_signals_and_say_unknown_otherwise() {
        assert_eq!(signal_crash_reason(libc::SIGHUP), "Terminal or window closed (SIGHUP)");
        assert_eq!(signal_crash_reason(libc::SIGTERM), "Terminated (SIGTERM)");
        assert_eq!(signal_crash_reason(libc::SIGINT), "Interrupted (SIGINT)");
        assert_eq!(signal_crash_reason(libc::SIGQUIT), "Quit signal (SIGQUIT)");
        assert_eq!(signal_crash_reason(libc::SIGKILL), "Terminated by signal SIGKILL (9)");
        // The crash text uses its own narrower table, not the exit-status one.
        assert_eq!(
            signal_crash_reason(libc::SIGTRAP),
            format!("Terminated by signal unknown ({})", libc::SIGTRAP)
        );
    }
}

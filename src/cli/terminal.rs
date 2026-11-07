//! Session lifecycle + panic/signal handling for the headless CLI.
//!
//! The interactive TUI's terminal runtime (raw mode, alternate screen, mouse
//! and keyboard-enhancement modes, kitty stack ownership, theme detection) lived
//! here in the upstream tree. This headless fork keeps only the parts the server
//! and CLI still need: current-session tracking, panic/signal crash labeling,
//! and the resume hints printed to stderr.

use std::io::{self, Write};
use std::panic;

use crate::{id, session};

pub fn set_current_session(session_id: &str) {
    crate::set_current_session(session_id);
}

pub fn get_current_session() -> Option<String> {
    crate::get_current_session()
}

/// Whether a panic in this process should relabel the on-disk session as crashed.
///
/// Only an `Active` session can legitimately make that transition. A dying
/// client (closed terminal window, dropped SSH) must not relabel a session that
/// the shared server still owns, nor write its stale snapshot over the server's
/// newer one. See #599; `mark_current_session_crashed` already had this guard.
fn should_record_panic_as_crash(status: &session::SessionStatus) -> bool {
    matches!(status, session::SessionStatus::Active)
}

pub fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        default_hook(info);

        if let Some(session_id) = get_current_session() {
            print_session_resume_hint(&session_id);

            if std::env::var_os("HORON_AGENT_SSH_REMOTE").is_none()
                && let Ok(mut session) = session::Session::load(&session_id)
                && should_record_panic_as_crash(&session.status)
            {
                session.mark_crashed(Some(format!("Panic: {}", info)));
                let _ = session.save();
            }
        }
    }));
}

pub fn mark_current_session_crashed(message: String) {
    if std::env::var_os("HORON_AGENT_SSH_REMOTE").is_some() {
        return;
    }
    if let Some(session_id) = get_current_session()
        && let Ok(mut session) = session::Session::load(&session_id)
        && matches!(session.status, session::SessionStatus::Active)
    {
        session.mark_crashed(Some(message));
        let _ = session.save();
    }
}

pub fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

pub fn show_crash_resume_hint() {
    let crashed = session::find_recent_crashed_sessions();
    if crashed.is_empty() {
        return;
    }

    // Crash hints print outside any UI, possibly on a console that never had VT
    // processing enabled (issue #498), so gate the color codes.
    let ansi = crate::console::stderr_supports_ansi();
    let (yellow, bold, reset) = if ansi {
        ("\x1b[33m", "\x1b[1m", "\x1b[0m")
    } else {
        ("", "", "")
    };

    for line in crash_resume_hint_lines(&crashed, yellow, bold, reset) {
        eprintln!("{}", crate::output_style::terminal_text(&line));
    }
    eprintln!();
}

/// Build the crash-resume hint lines for `crashed`, newest first.
///
/// Pure so the wording is testable: the lines are printed to stderr outside any
/// UI, where nothing asserts on them, and the bug in issue #690 was purely
/// about wording (the single-session form never mentioned that bare
/// `horon-agent --resume` opens a searchable picker, so it read as "memorize this ID
/// or lose the session").
fn crash_resume_hint_lines(crashed: &[(String, String)], yellow: &str, bold: &str, reset: &str) -> Vec<String> {
    let Some((id, name)) = crashed.first() else {
        return Vec::new();
    };
    let session_label = id::extract_session_name(id).unwrap_or(name.as_str());

    if crashed.len() == 1 {
        vec![
            format!(
                "{yellow}💥 Session {bold}{session_label}{reset}{yellow} crashed. Resume with:{reset}  horon-agent --resume {id}"
            ),
            // Always mention the picker. Showing only the ID form reads as
            // "write this down or lose the session", when bare
            // `horon-agent --resume` opens a searchable list (issue #690).
            format!("{yellow}   Or browse all:{reset} horon-agent --resume"),
        ]
    } else {
        vec![
            format!(
                "{yellow}💥 {} sessions crashed recently. Most recent: {bold}{session_label}{reset}",
                crashed.len()
            ),
            format!("{yellow}   Resume with:{reset}  horon-agent --resume {id}"),
            format!("{yellow}   List all:{reset}     horon-agent --resume"),
        ]
    }
}

#[cfg(test)]
mod crash_resume_hint_tests {
    use super::crash_resume_hint_lines;

    fn session(id: &str, name: &str) -> (String, String) {
        (id.to_string(), name.to_string())
    }

    #[test]
    fn no_crashed_sessions_produces_no_hint() {
        assert!(crash_resume_hint_lines(&[], "", "", "").is_empty());
    }

    /// Issue #690: a user who cannot memorize the ID must still be told how to
    /// get to the picker.
    #[test]
    fn single_session_hint_also_points_at_the_picker() {
        let lines = crash_resume_hint_lines(&[session("ses_koala_123", "koala")], "", "", "");
        let joined = lines.join("\n");

        assert!(
            joined.contains("horon-agent --resume ses_koala_123"),
            "the direct resume command must still be offered: {joined}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Or browse all: horon-agent --resume")),
            "the picker form (bare --resume) must be mentioned too: {joined}"
        );
    }

    #[test]
    fn multiple_sessions_hint_lists_recent_and_the_picker() {
        let lines = crash_resume_hint_lines(
            &[session("ses_koala_123", "koala"), session("ses_otter_456", "otter")],
            "",
            "",
            "",
        );
        let joined = lines.join("\n");

        assert!(joined.contains("2 sessions crashed"), "{joined}");
        assert!(joined.contains("horon-agent --resume ses_koala_123"), "{joined}");
        assert!(joined.contains("List all:"), "{joined}");
    }
}

pub fn print_session_resume_hint(session_id: &str) {
    let _ = write_session_resume_hint(io::stderr().lock(), session_id);
}

fn write_session_resume_hint(mut writer: impl Write, session_id: &str) -> io::Result<()> {
    let session_name = id::extract_session_name(session_id).unwrap_or(session_id);
    writeln!(writer)?;
    writeln!(
        writer,
        "\x1b[33mSession \x1b[1m{}\x1b[0m\x1b[33m - to resume:\x1b[0m",
        session_name
    )?;
    if let Some(command) = super::ssh::resume_hint(session_id) {
        writeln!(writer, "  {command}")?;
    } else {
        writeln!(writer, "  horon-agent --resume {}", session_id)?;
    }
    writeln!(writer)?;
    Ok(())
}

/// The crash reason recorded when a signal terminates this process.
fn signal_crash_reason(sig: i32) -> String {
    horon_agent_platform::signal::signal_crash_reason(sig)
}

fn handle_termination_signal(sig: i32) -> ! {
    // Record the crash before exiting so the session is not left `Active` on
    // disk. The interactive terminal-mode restoration the upstream handler also
    // performed is a presentation concern this headless build does not carry.
    mark_current_session_crashed(signal_crash_reason(sig));

    if let Some(session_id) = get_current_session() {
        print_session_resume_hint(&session_id);
    }

    std::process::exit(128 + sig);
}

/// Turn the signals that end a session into a recorded crash and an exit.
///
/// The platform installs the watchers it has: Unix watches SIGHUP, SIGTERM,
/// SIGINT and SIGQUIT, Windows delivers these through the console and installs
/// none.
pub fn spawn_session_signal_watchers() {
    horon_agent_platform::signal::spawn_termination_signal_watchers(|sig| {
        crate::logging::info(&format!(
            "Received {} in horon-agent process",
            horon_agent_platform::signal::signal_name(sig)
        ));
        handle_termination_signal(sig);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_SESSION_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_session_recovery_tracking() {
        let _guard = TEST_SESSION_LOCK.lock().unwrap();
        set_current_session("test_session_123");

        let stored = get_current_session();
        assert_eq!(stored.as_deref(), Some("test_session_123"));
    }

    #[test]
    fn test_session_recovery_message_format() {
        let _guard = TEST_SESSION_LOCK.lock().unwrap();
        let test_session = "session_format_test_12345";
        set_current_session(test_session);

        if let Some(session_id) = get_current_session() {
            let mut output = Vec::new();
            write_session_resume_hint(&mut output, &session_id).unwrap();
            let output = String::from_utf8(output).unwrap();
            let expected_cmd = format!("horon-agent --resume {}", session_id);
            assert!(output.contains(&expected_cmd));
            assert!(output.contains("to resume"));
            assert!(!session_id.is_empty());
        } else {
            panic!("Session ID should be set");
        }
    }

    #[test]
    fn session_resume_hint_writer_reports_closed_stderr_without_panicking() {
        struct ClosedWriter;

        impl Write for ClosedWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "stderr closed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let error = write_session_resume_hint(ClosedWriter, "session_closed_pipe")
            .expect_err("closed stderr should be reported as an I/O error");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}

#[cfg(test)]
mod panic_crash_labeling_tests {
    //! Regression coverage for #599.
    //!
    //! Closing a terminal (or dropping SSH) makes `ratatui::restore()`'s
    //! internal `eprintln!` panic with EIO. The panic hook then relabeled the
    //! session as `Crashed` and saved the dying client's stale snapshot over the
    //! server's newer one. Only an `Active` session may be relabeled.
    use super::*;

    #[test]
    fn active_session_is_still_labeled_crashed_on_panic() {
        assert!(should_record_panic_as_crash(&session::SessionStatus::Active));
    }

    #[test]
    fn already_crashed_session_is_not_relabeled() {
        assert!(!should_record_panic_as_crash(&session::SessionStatus::Crashed {
            message: Some("earlier crash".to_string())
        }));
    }

    #[test]
    fn completed_session_is_not_relabeled_by_a_dying_client() {
        // The exact #599 shape: the session lives on in the shared server and is
        // no longer Active locally, so a dead-terminal panic must leave it alone.
        for status in [
            session::SessionStatus::Closed,
            session::SessionStatus::Reloaded,
            session::SessionStatus::Compacted,
            session::SessionStatus::RateLimited,
            session::SessionStatus::Error {
                message: "unrelated".to_string(),
            },
        ] {
            assert!(
                !should_record_panic_as_crash(&status),
                "non-active status {status:?} must not be relabeled as crashed"
            );
        }
    }
}

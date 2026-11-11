//! Hand the bare `horon` command over to the interactive client.
//!
//! `horon` with no subcommand used to end at "the interactive client is
//! unavailable in this headless build" once the server was up. The interactive
//! client is now a separate binary (`horon-tui`) that speaks the harness API, so
//! a terminal launch replaces this process with that sibling instead of stopping
//! at the message. A pipe, a disabled switch, or a missing sibling keeps the
//! headless path and its message, which is what scripts and CI rely on.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Set to any value to keep the headless path on a terminal.
pub(crate) const DISABLE_ENV: &str = "HORON_NO_TUI";

/// File name of the interactive client shipped beside this binary.
const SIBLING_STEM: &str = "horon-tui";

/// One line telling an operator how to get the interactive client.
pub(crate) const INSTALL_HINT: &str =
    "install `horon-tui` next to this binary (`horon-agent install-release`) for the interactive client";

/// What the bare command should do after the server is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// Replace this process with the client at this path.
    Exec(PathBuf),
    /// Keep this process and explain why the client was not started.
    Headless(HeadlessReason),
}

/// Why the interactive client was not started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadlessReason {
    /// stdin or stdout is a pipe, a file, or a closed stream.
    NotATerminal,
    /// [`DISABLE_ENV`] is set.
    DisabledByEnv,
    /// No `horon-tui` next to the running binary.
    MissingSibling,
}

impl HeadlessReason {
    /// The condition, phrased for the operator.
    pub(crate) fn detail(self) -> &'static str {
        match self {
            Self::NotATerminal => "stdin and stdout are not both terminals",
            Self::DisabledByEnv => "HORON_NO_TUI is set",
            Self::MissingSibling => "no horon-tui beside this binary",
        }
    }
}

/// The decision on its own, without reading the process's terminal or
/// environment, so every condition is testable.
pub(crate) fn decide(
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    disabled_by_env: bool,
    sibling: Option<&Path>,
) -> Handoff {
    if !stdin_is_terminal || !stdout_is_terminal {
        return Handoff::Headless(HeadlessReason::NotATerminal);
    }
    if disabled_by_env {
        return Handoff::Headless(HeadlessReason::DisabledByEnv);
    }
    match sibling {
        Some(sibling) => Handoff::Exec(sibling.to_path_buf()),
        None => Handoff::Headless(HeadlessReason::MissingSibling),
    }
}

/// The decision for this process: its own terminal state, environment, and
/// sibling binary.
pub(crate) fn decide_for_process() -> Handoff {
    // Without a resolvable executable path there is no sibling to find; the
    // headless message then says the client is missing, and the log keeps why.
    let sibling = match std::env::current_exe() {
        // The raw path is checked first; a launcher that is itself a symlink
        // (macOS reports the unresolved path) is retried at its real location.
        Ok(exe) => sibling_tui(Some(&exe)).or_else(|| match std::fs::canonicalize(&exe) {
            Ok(real) => sibling_tui(Some(&real)),
            Err(error) => {
                crate::logging::info(&format!("tui handoff: cannot resolve {}: {error}", exe.display()));
                None
            }
        }),
        Err(error) => {
            crate::logging::info(&format!("tui handoff: cannot resolve current_exe: {error}"));
            None
        }
    };
    decide(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        std::env::var_os(DISABLE_ENV).is_some(),
        sibling.as_deref(),
    )
}

/// Path of the interactive client beside `current_exe`, when one is installed.
pub(crate) fn sibling_tui(current_exe: Option<&Path>) -> Option<PathBuf> {
    let directory = current_exe?.parent()?;
    let candidate = directory.join(format!("{SIBLING_STEM}{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

/// Act on the decision: start the client, or fail with the headless sentence.
pub(crate) fn run(handoff: Handoff, resume: Option<&str>) -> anyhow::Result<()> {
    match handoff {
        Handoff::Exec(sibling) => {
            let cwd =
                std::env::current_dir().map_err(|error| anyhow::anyhow!("working directory unreadable: {error}"))?;
            hand_off(&sibling, resume, Some(cwd.as_path()))
        }
        Handoff::Headless(reason) => anyhow::bail!(
            "the interactive client is unavailable in this headless build ({}); {}; use `horon-agent serve` or `horon-agent run`",
            reason.detail(),
            INSTALL_HINT
        ),
    }
}

/// The arguments the client is started with.
///
/// A resumed session is passed as `--session <id>`, which is the client's
/// "attach to this session" spelling; without it the client would open a fresh
/// session and the resumed one would be left behind. The daemon socket is not
/// passed: it is already exported as `HORON_AGENT_SOCKET`, and the client
/// resolves the harness API socket itself.
fn client_args(resume: Option<&str>, cwd: Option<&Path>) -> Vec<std::ffi::OsString> {
    let mut args = Vec::new();
    if let Some(resume) = resume.filter(|resume| !resume.is_empty()) {
        args.push(std::ffi::OsString::from("--session"));
        args.push(std::ffi::OsString::from(resume));
    }
    if let Some(cwd) = cwd {
        args.push(std::ffi::OsString::from("--cwd"));
        args.push(cwd.as_os_str().to_owned());
    }
    args
}

/// Replace this process with the interactive client and return only if that
/// failed.
pub(crate) fn hand_off(sibling: &Path, resume: Option<&str>, cwd: Option<&Path>) -> anyhow::Result<()> {
    let mut command = std::process::Command::new(sibling);
    command.args(client_args(resume, cwd));
    let error = horon_agent_platform::replace_process(&mut command);
    Err(anyhow::anyhow!(
        "could not start the interactive client {}: {error}",
        sibling.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_with_sibling_execs() {
        let sibling = Path::new("/opt/horon-tui");
        assert_eq!(
            decide(true, true, false, Some(sibling)),
            Handoff::Exec(sibling.to_path_buf())
        );
    }

    #[test]
    fn a_pipe_keeps_the_headless_path() {
        assert_eq!(
            decide(false, true, false, Some(Path::new("/opt/horon-tui"))),
            Handoff::Headless(HeadlessReason::NotATerminal)
        );
        assert_eq!(
            decide(true, false, false, Some(Path::new("/opt/horon-tui"))),
            Handoff::Headless(HeadlessReason::NotATerminal)
        );
    }

    #[test]
    fn the_env_switch_wins_over_an_installed_client() {
        assert_eq!(
            decide(true, true, true, Some(Path::new("/opt/horon-tui"))),
            Handoff::Headless(HeadlessReason::DisabledByEnv)
        );
    }

    #[test]
    fn a_missing_sibling_keeps_the_headless_path() {
        assert_eq!(
            decide(true, true, false, None),
            Handoff::Headless(HeadlessReason::MissingSibling)
        );
    }

    #[test]
    fn a_resumed_session_is_passed_as_the_session_flag() {
        let cwd = Path::new("/work/project");
        assert_eq!(
            client_args(Some("sess-42"), Some(cwd)),
            vec![
                std::ffi::OsString::from("--session"),
                std::ffi::OsString::from("sess-42"),
                std::ffi::OsString::from("--cwd"),
                std::ffi::OsString::from("/work/project"),
            ]
        );
    }

    #[test]
    fn a_fresh_launch_passes_only_the_cwd() {
        assert_eq!(
            client_args(None, Some(Path::new("/work/project"))),
            vec![
                std::ffi::OsString::from("--cwd"),
                std::ffi::OsString::from("/work/project"),
            ]
        );
        // An empty id is what the flag parser rejects before this point; it must
        // never become `--session ""`.
        assert!(
            client_args(Some(""), Some(Path::new("/work")))
                .iter()
                .all(|arg| arg != "--session")
        );
    }

    #[test]
    fn the_hint_names_the_interactive_client() {
        assert!(INSTALL_HINT.contains(SIBLING_STEM));
        assert_eq!(
            HeadlessReason::NotATerminal.detail(),
            "stdin and stdout are not both terminals"
        );
    }

    #[test]
    fn the_sibling_is_looked_up_beside_the_running_binary() {
        let directory = std::env::temp_dir().join(format!("horon-tui-handoff-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("temp dir");
        let agent = directory.join(format!("horon-agent{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&agent, b"").expect("agent placeholder");
        assert_eq!(
            sibling_tui(Some(&agent)),
            None,
            "a client that is not installed must not be found"
        );

        let sibling = directory.join(format!("{SIBLING_STEM}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&sibling, b"").expect("sibling placeholder");
        assert_eq!(sibling_tui(Some(&agent)), Some(sibling));
        assert_eq!(sibling_tui(None), None);

        let _ = std::fs::remove_dir_all(&directory);
    }
}

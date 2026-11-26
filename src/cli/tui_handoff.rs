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
///
/// Only [`HeadlessReason::MissingSibling`] is answered by installing: the other
/// two reasons happen with the client already in place, and naming an install
/// there told the operator to fix something that was not broken.
pub(crate) const INSTALL_HINT: &str =
    "install `horon-tui` next to this binary (`horon install-release`) for the interactive client";

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

    /// What restores the handoff for this reason, phrased for the operator.
    ///
    /// Only a missing client is answered by installing one. A pipe and the
    /// environment switch happen with the client already installed, so their
    /// remedy names the condition's own switch instead: an install instruction
    /// there reads as "the install is broken" when it is not.
    pub(crate) fn remedy(self) -> &'static str {
        match self {
            Self::NotATerminal => "run it on a terminal instead of a pipe or file",
            Self::DisabledByEnv => "unset `HORON_NO_TUI` to use the interactive client",
            Self::MissingSibling => INSTALL_HINT,
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
    decide(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        std::env::var_os(DISABLE_ENV).is_some(),
        locate_tui().as_deref(),
    )
}

/// The interactive client this process hands over to.
///
/// The sibling of the running binary comes first, at its raw path and then at
/// its real location (macOS reports a symlinked launcher unresolved). A build
/// that has no sibling, such as a repo build in `target/debug` or a version
/// directory a source install left without one, falls back to the client of
/// the installed `current` and `stable` channels and then to the `horon-tui`
/// on `PATH`, so bare `horon` still opens the client.
fn locate_tui() -> Option<PathBuf> {
    let beside = match std::env::current_exe() {
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
    beside
        .or_else(installed_tui)
        .or_else(|| tui_on_path(std::env::var_os("PATH").as_deref()))
}

/// The client beside the binary an installed channel points at.
fn installed_tui() -> Option<PathBuf> {
    [crate::build::current_binary_path(), crate::build::stable_binary_path()]
        .into_iter()
        .find_map(|channel| match channel.map(std::fs::canonicalize) {
            Ok(Ok(real)) => sibling_tui(Some(&real)),
            // An unset or dangling channel has no client to offer.
            Ok(Err(_)) | Err(_) => None,
        })
}

/// The first `horon-tui` file in the directories of `path`.
pub(crate) fn tui_on_path(path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let name = format!("{SIBLING_STEM}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path?)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&name))
        .find(|candidate| candidate.is_file())
}

/// Path of the interactive client beside `current_exe`, when one is installed.
pub(crate) fn sibling_tui(current_exe: Option<&Path>) -> Option<PathBuf> {
    let directory = current_exe?.parent()?;
    let candidate = directory.join(format!("{SIBLING_STEM}{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

/// Act on the decision: start the client, or fail with the headless sentence.
pub(crate) fn run(
    handoff: Handoff,
    resume: Option<&str>,
    goal: Option<(&str, u64)>,
    add_dirs: &[String],
    plan: bool,
) -> anyhow::Result<()> {
    match handoff {
        Handoff::Exec(sibling) => {
            let cwd =
                std::env::current_dir().map_err(|error| anyhow::anyhow!("working directory unreadable: {error}"))?;
            hand_off(&sibling, resume, Some(cwd.as_path()), goal, add_dirs, plan)
        }
        Handoff::Headless(reason) => anyhow::bail!("{}", headless_message(reason)),
    }
}

/// The sentence a headless launch ends with.
///
/// The condition, the one remedy that answers it, and the headless fallback, in
/// an order scripts can match on: the condition text is unchanged from when the
/// install hint was appended to every reason, and only [`HeadlessReason::MissingSibling`]
/// still names an install.
fn headless_message(reason: HeadlessReason) -> String {
    format!(
        "the interactive client is unavailable in this headless build ({}); {}; use `horon serve` or `horon run`",
        reason.detail(),
        reason.remedy()
    )
}

/// The arguments the client is started with.
///
/// A resumed session is passed as `--session <id>`, which is the client's
/// "attach to this session" spelling; without it the client would open a fresh
/// session and the resumed one would be left behind. The daemon socket is not
/// passed: it is already exported as `HORON_AGENT_SOCKET`, and the client
/// resolves the harness API socket itself.
fn client_args(
    resume: Option<&str>,
    cwd: Option<&Path>,
    goal: Option<(&str, u64)>,
    add_dirs: &[String],
    plan: bool,
) -> Vec<std::ffi::OsString> {
    let mut args = Vec::new();
    if let Some(resume) = resume.filter(|resume| !resume.is_empty()) {
        args.push(std::ffi::OsString::from("--session"));
        args.push(std::ffi::OsString::from(resume));
    }
    if let Some(cwd) = cwd {
        args.push(std::ffi::OsString::from("--cwd"));
        args.push(cwd.as_os_str().to_owned());
    }
    if let Some((title, budget)) = goal {
        args.extend([
            "--goal".into(),
            title.into(),
            "--goal-budget".into(),
            budget.to_string().into(),
        ]);
    }
    for directory in add_dirs {
        args.push("--add-dir".into());
        args.push(directory.into());
    }
    if plan {
        args.push("--plan".into());
    }
    args
}

/// Replace this process with the interactive client and return only if that
/// failed.
fn hand_off(
    sibling: &Path,
    resume: Option<&str>,
    cwd: Option<&Path>,
    goal: Option<(&str, u64)>,
    add_dirs: &[String],
    plan: bool,
) -> anyhow::Result<()> {
    let mut command = std::process::Command::new(sibling);
    command.args(client_args(resume, cwd, goal, add_dirs, plan));
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
            client_args(Some("sess-42"), Some(cwd), None, &[], false),
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
            client_args(None, Some(Path::new("/work/project")), None, &[], false),
            vec![
                std::ffi::OsString::from("--cwd"),
                std::ffi::OsString::from("/work/project"),
            ]
        );
        // An empty id is what the flag parser rejects before this point; it must
        // never become `--session ""`.
        assert!(
            client_args(Some(""), Some(Path::new("/work")), None, &[], false)
                .iter()
                .all(|arg| arg != "--session")
        );
    }

    #[test]
    fn the_headless_sentence_names_the_remedy_for_its_reason() {
        // A pipe and the environment switch happen with the client already
        // installed, so the install hint must not appear: it told the operator
        // to fix an install that was not broken.
        for reason in [HeadlessReason::NotATerminal, HeadlessReason::DisabledByEnv] {
            let message = headless_message(reason);
            assert!(message.contains(reason.detail()), "{message}");
            assert!(!message.contains("horon install-release"), "{message}");
            assert!(!message.contains(INSTALL_HINT), "{message}");
            assert!(message.contains("`horon serve` or `horon run`"), "{message}");
        }
        assert!(headless_message(HeadlessReason::NotATerminal).contains("run it on a terminal"));
        assert!(headless_message(HeadlessReason::DisabledByEnv).contains("`HORON_NO_TUI`"));

        // A missing client is the one reason installing answers.
        let missing = headless_message(HeadlessReason::MissingSibling);
        assert!(missing.contains(INSTALL_HINT), "{missing}");
        assert!(missing.contains("`horon serve` or `horon run`"), "{missing}");
    }

    #[test]
    fn a_headless_handoff_fails_the_command() {
        for reason in [
            HeadlessReason::NotATerminal,
            HeadlessReason::DisabledByEnv,
            HeadlessReason::MissingSibling,
        ] {
            let error = run(Handoff::Headless(reason), None, None, &[], false)
                .expect_err("a headless handoff must not succeed");
            assert_eq!(error.to_string(), headless_message(reason));
        }
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

    #[test]
    fn a_build_without_a_sibling_falls_back_to_the_client_on_path() {
        let root = std::env::temp_dir().join(format!("horon-tui-path-{}", std::process::id()));
        let empty = root.join("empty");
        let installed = root.join("bin");
        std::fs::create_dir_all(&empty).expect("empty dir");
        std::fs::create_dir_all(&installed).expect("bin dir");
        let client = installed.join(format!("{SIBLING_STEM}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&client, b"").expect("client placeholder");

        let path = std::env::join_paths([empty.clone(), installed.clone()]).expect("path");
        assert_eq!(tui_on_path(Some(&path)), Some(client));
        let without = std::env::join_paths([empty]).expect("path");
        assert_eq!(tui_on_path(Some(&without)), None);
        assert_eq!(tui_on_path(None), None);

        let _ = std::fs::remove_dir_all(&root);
    }
}

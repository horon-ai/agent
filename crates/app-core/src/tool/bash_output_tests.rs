//! Formatting, timeout and environment behavior of the `bash` tool.
//!
//! These are the checks that used to sit inline in `bash.rs`; they live here so
//! that file stays inside its code-size budget. Every path below resolves
//! through `super::` exactly as before, because this module is a child of the
//! same `bash` module.

use std::time::Duration;

use super::build_shell_command;
use super::format_command_output;

#[test]
fn format_command_output_keeps_head_and_tail_of_long_output() {
    // The minimizer test flips a process-wide override under this lock.
    let _guard = crate::storage::lock_test_env();
    let input = format!("{}é", "a".repeat(29_999));
    let output = format_command_output("test-session", "cargo build", input, None);
    assert!(output.starts_with(&"a".repeat(12_000)), "the head keeps its 40% budget");
    assert!(
        !output.starts_with(&"a".repeat(12_001)),
        "the head must not exceed its budget"
    );
    assert!(output.contains("omitted from `bash` output"), "the cut must be marked");
    assert!(output.contains('é'), "the tail must survive the cut");
    assert!(output.contains("artifact://"), "the full output is saved: {output}");
    assert!(output.len() <= 30_000, "output stayed within the budget");
}

#[test]
fn resolved_timeout_zero_disables_the_deadline() {
    let zero = super::resolve_bash_timeout(Some(0));
    assert!(zero.duration.is_none());
    assert!(super::timeout_clamp_note(zero.clamped_from).is_empty());

    let omitted = super::resolve_bash_timeout(None);
    assert_eq!(omitted.duration, Some(Duration::from_millis(super::DEFAULT_TIMEOUT_MS)));

    let clamped = super::resolve_bash_timeout(Some(900_000));
    assert_eq!(clamped.duration, Some(Duration::from_millis(600_000)));
    assert_eq!(
        super::timeout_clamp_note(clamped.clamped_from),
        "\n\n[Timeout clamped to 600000ms (requested 900000ms; allowed range 1-600000ms).]"
    );

    let background = super::resolve_background_timeout(None);
    assert!(background.duration.is_none());
    let background_zero = super::resolve_background_timeout(Some(0));
    assert!(background_zero.duration.is_none());
}

#[test]
fn non_interactive_env_disables_pagers_and_prompts() {
    let env = super::non_interactive_env(false);
    let find = |name: &str| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(find("PAGER"), Some("cat"));
    assert_eq!(find("HOMEBREW_PAGER"), Some("cat"));
    assert_eq!(find("AWS_PAGER"), Some(""));
    assert_eq!(find("LESS"), Some("FRX"));
    assert_eq!(find("TERM"), Some("dumb"));
    assert_eq!(find("NO_COLOR"), Some("1"));
    assert_eq!(find("GIT_EDITOR"), Some("true"));
    assert_eq!(find("EDITOR"), Some("true"));
    assert_eq!(find("GIT_TERMINAL_PROMPT"), Some("0"));
    assert_eq!(find("AGENT"), Some("1"));
    assert_eq!(find("npm_config_yes"), Some("true"));
    let askpass = find("SSH_ASKPASS").expect("SSH_ASKPASS is set");
    assert!(askpass == "false" || std::path::Path::new(askpass).is_absolute());

    // A terminal keeps colors, its own TERM, and no forced CI marker.
    let env = super::non_interactive_env(true);
    let find = |name: &str| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(find("TERM"), Some("xterm-256color"));
    assert_eq!(find("NO_COLOR"), None);
    assert_eq!(find("CI"), None);
}

#[test]
fn format_command_output_appends_the_exit_code_after_the_cut() {
    let output = format_command_output(
        "test-session",
        "cargo build",
        format!("{}\nlast-line", "x".repeat(40_000)),
        Some(3),
    );
    assert!(output.ends_with("Exit code: 3"));
    assert!(output.contains("last-line"));
}

#[tokio::test]
async fn build_shell_command_uses_cmd_and_executes_command() {
    // Only the `cmd.exe` path can run this; a POSIX shell has no `echo`
    // built-in difference to check here.
    if horon_agent_platform::term::shell_is_posix() {
        return;
    }
    let output = build_shell_command("echo hello-from-cmd", false)
        .output()
        .await
        .expect("run cmd command");
    assert!(output.status.success(), "cmd command should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_ascii_lowercase().contains("hello-from-cmd"),
        "unexpected stdout: {}",
        stdout
    );

    let probe_path = std::env::temp_dir().join(format!("horon-agent-cmd-quoting-probe-{}.cmd", std::process::id()));
    std::fs::write(
        &probe_path,
        concat!(
            "@echo off\r\n",
            "if \"%~1\"==\"text with spaces\" if \"%~2\"==\"\" (\r\n",
            "  echo quoted-argument-ok\r\n",
            "  exit /b 0\r\n",
            ")\r\n",
            "echo first=[%~1] second=[%~2]\r\n",
            "exit /b 1\r\n",
        ),
    )
    .expect("write cmd quoting probe");

    let quoted_command = format!("call \"{}\" \"text with spaces\"", probe_path.display());
    let quoted_output = build_shell_command(&quoted_command, false)
        .output()
        .await
        .expect("run cmd quoting probe");
    let _ = std::fs::remove_file(&probe_path);
    let quoted_stdout = String::from_utf8_lossy(&quoted_output.stdout);
    let quoted_stderr = String::from_utf8_lossy(&quoted_output.stderr);
    assert!(
        quoted_output.status.success(),
        "quoted argument should remain one child-process argument; stdout={quoted_stdout:?} stderr={quoted_stderr:?}"
    );
    assert!(
        quoted_stdout.contains("quoted-argument-ok"),
        "unexpected quoted-command stdout: {quoted_stdout}"
    );
}

#[test]
fn build_shell_command_uses_disk_backed_scratch_directory() {
    // Only a platform that hands its shells a scratch directory can check
    // this; elsewhere the variables are deliberately absent.
    if !horon_agent_platform::term::passes_scratch_dir_to_shell() {
        return;
    }
    // Keep HORON_HOME stable until the final directory assertion. Running
    // synchronously avoids holding the environment MutexGuard across await.
    let _env_lock = crate::storage::lock_test_env();
    let expected = super::tool_scratch_dir().expect("horon-agent scratch directory");
    let output = build_shell_command("printf '%s\\n%s\\n' \"$TMPDIR\" \"$HORON_AGENT_SCRATCH_DIR\"", false)
        .as_std_mut()
        .output()
        .expect("run bash command");
    assert!(output.status.success(), "bash command should succeed");
    let stdout = String::from_utf8(output.stdout).expect("utf-8 scratch paths");
    let paths = stdout.lines().collect::<Vec<_>>();
    let expected = expected.to_string_lossy().into_owned();
    assert_eq!(paths, vec![expected.as_str(), expected.as_str()]);
    assert!(std::path::Path::new(&expected).is_dir());
}

//! Terminal identity helpers shared by clients and hook launchers.
//!
//! This crate does not launch terminals: horon-agent never opens a terminal
//! window. What remains is the surface hooks and clients actually use: the
//! client terminal-env snapshot and its re-export, shell escaping for hook
//! command construction, and the POSIX-style word splitter for configured hook
//! command lines.

use anyhow::Result;
use std::process::Command;

/// Terminal/window-manager environment variables that identify *which*
/// terminal, multiplexer, or display a client is attached to.
///
/// The horon-agent server process is long-lived and captures these at *its* startup,
/// so once a client connects from a different terminal/tmux/zellij session the
/// server's copies are stale. Hook launchers then target the wrong terminal
/// (see issue #405). To fix this, clients snapshot these vars from their own
/// environment and send them to the server, which re-exports them to hooks so
/// the hook acts in the terminal the user is actually looking at.
///
/// This intentionally covers terminal multiplexers (tmux, screen, zellij),
/// terminal emulators (kitty, wezterm, ghostty, iTerm, ...), and the display
/// server (X11 `DISPLAY`, Wayland `WAYLAND_DISPLAY`) so routing follows the
/// connecting client.
pub const CLIENT_TERMINAL_ENV_VARS: &[&str] = &[
    // Terminal multiplexers
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    "TMUX",
    "TMUX_PANE",
    "STY",
    // herdr terminal multiplexer (https://herdr.dev), see issue #405
    "HERDR_ENV",
    "HERDR_SOCKET_PATH",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_BIN_PATH",
    "HERDR_SESSION",
    "HERDR_AGENT",
    // Terminal emulators
    "TERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "COLORTERM",
    "KITTY_PID",
    "KITTY_WINDOW_ID",
    "KITTY_LISTEN_ON",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "ITERM_SESSION_ID",
    "WINDOWID",
    "HANDTERM_SESSION",
    "HANDTERM_PID",
    "WT_SESSION",
    "WT_PROFILE_ID",
    // Display / window manager
    "DISPLAY",
    "WAYLAND_DISPLAY",
];

/// Snapshot the current process's terminal-identifying env vars (see
/// [`CLIENT_TERMINAL_ENV_VARS`]). Only vars that are actually set are included,
/// so the map is empty when nothing identifies the terminal.
pub fn snapshot_client_terminal_env() -> Vec<(String, String)> {
    CLIENT_TERMINAL_ENV_VARS
        .iter()
        .filter_map(|&key| std::env::var(key).ok().map(|value| (key.to_string(), value)))
        .collect()
}

/// Replace inherited terminal identity with an authoritative client snapshot.
///
/// Removing every known key first is important for a shared server: an empty
/// client snapshot must not leak the pane that happened to start the server.
/// Aliases let integrations explicitly distinguish client values from other
/// process environment while native names preserve existing hook behavior.
pub fn apply_client_terminal_env(cmd: &mut Command, env: &[(String, String)]) {
    for key in CLIENT_TERMINAL_ENV_VARS {
        cmd.env_remove(key);
        cmd.env_remove(format!("HORON_AGENT_CLIENT_{key}"));
    }
    for (key, value) in env {
        if CLIENT_TERMINAL_ENV_VARS.contains(&key.as_str()) {
            cmd.env(key, value);
            cmd.env(format!("HORON_AGENT_CLIENT_{key}"), value);
        }
    }
}

pub fn sh_escape(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

/// Parse a configured hook command line into argv parts.
///
/// Supports basic POSIX-style word splitting: whitespace separates arguments,
/// single and double quotes group words, and backslash escapes the next
/// character (outside single quotes). Errors on empty input, unterminated
/// quotes, and trailing escapes.
pub fn parse_hook_command(raw: &str) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut token_started = false;

    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            token_started = true;
            escaped = false;
            continue;
        }

        if let Some(quote_ch) = quote {
            if ch == quote_ch {
                quote = None;
            } else if ch == '\\' && quote_ch == '"' {
                escaped = true;
            } else {
                current.push(ch);
                token_started = true;
            }
            continue;
        }

        match ch {
            '\\' => {
                escaped = true;
                token_started = true;
            }
            '\'' | '"' => {
                quote = Some(ch);
                token_started = true;
            }
            ch if ch.is_whitespace() => {
                if token_started {
                    parts.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            ch => {
                current.push(ch);
                token_started = true;
            }
        }
    }

    if escaped {
        anyhow::bail!("hook command ends with an escape character");
    }
    if quote.is_some() {
        anyhow::bail!("hook command has an unterminated quote");
    }
    if token_started {
        parts.push(current);
    }
    if parts.is_empty() {
        anyhow::bail!("hook command is empty");
    }

    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Serializes the tests that read or mutate the process environment.
    ///
    /// EnvGuard takes the shared lock, clears the named keys, and restores their
    /// previous values when it drops - on panic too - so a failing test cannot
    /// leak its fixture into the tests running beside it in the same process. A
    /// panic while the lock is held poisons it, and a poisoned lock must not
    /// cascade into unrelated tests, so acquiring the guard recovers the inner
    /// guard instead of unwrapping the poison.
    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let saved = keys.iter().map(|key| (*key, std::env::var_os(key))).collect::<Vec<_>>();
            for key in keys {
                // SAFETY: ENV_LOCK is held for the whole mutation, and every key
                // is restored when the guard drops.
                unsafe { std::env::remove_var(key) };
            }
            Self { _lock: lock, saved }
        }

        fn set(&self, key: &str, value: &str) {
            // SAFETY: ENV_LOCK is held for the whole mutation, and every key is
            // restored when the guard drops.
            unsafe { std::env::set_var(key, value) };
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                // SAFETY: ENV_LOCK is held until the guard is dropped.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    #[test]
    fn client_terminal_env_replaces_inherited_identity_and_exports_aliases() {
        let mut command = Command::new("hook");
        command.env("HERDR_PANE_ID", "stale-pane");
        command.env("TMUX_PANE", "stale-tmux");
        apply_client_terminal_env(
            &mut command,
            &[
                ("HERDR_PANE_ID".to_string(), "client-pane".to_string()),
                ("UNTRUSTED_CLIENT_VAR".to_string(), "ignored".to_string()),
            ],
        );
        let env = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(env["HERDR_PANE_ID"].as_deref(), Some("client-pane"));
        assert_eq!(env["HORON_AGENT_CLIENT_HERDR_PANE_ID"].as_deref(), Some("client-pane"));
        assert_eq!(env["TMUX_PANE"], None);
        assert!(!env.contains_key("UNTRUSTED_CLIENT_VAR"));
        assert!(!env.contains_key("HORON_AGENT_CLIENT_UNTRUSTED_CLIENT_VAR"));
    }

    #[test]
    #[cfg(unix)]
    fn snapshot_client_terminal_env_captures_set_vars_only() {
        // Clear every client terminal marker first so the "only set vars are
        // captured" assertion does not depend on the shell this test runs in.
        let guard = EnvGuard::new(CLIENT_TERMINAL_ENV_VARS);
        guard.set("ZELLIJ_SESSION_NAME", "snapshot-test");
        let snapshot = snapshot_client_terminal_env();
        assert!(
            snapshot
                .iter()
                .any(|(k, v)| k == "ZELLIJ_SESSION_NAME" && v == "snapshot-test")
        );
        assert!(!snapshot.iter().any(|(k, _)| k == "TMUX"));
    }

    #[test]
    fn parse_hook_command_splits_words_and_quotes() {
        assert_eq!(
            parse_hook_command("tmux new-window --").unwrap(),
            vec!["tmux", "new-window", "--"]
        );
        assert_eq!(
            parse_hook_command("my-hook --label 'two words'").unwrap(),
            vec!["my-hook", "--label", "two words"]
        );
        assert_eq!(
            parse_hook_command(r#"hook "a \"b\" c""#).unwrap(),
            vec!["hook", r#"a "b" c"#]
        );
    }

    #[test]
    fn parse_hook_command_rejects_bad_input() {
        assert!(parse_hook_command("").is_err());
        assert!(parse_hook_command("   ").is_err());
        assert!(parse_hook_command("hook 'unterminated").is_err());
        assert!(parse_hook_command("hook trailing\\").is_err());
    }
}

//! Windows stdin-state detection.
//!
//! There is no `/proc` and no `libproc`; answering needs thread wait-reason
//! inspection that this adapter does not implement, so every query is
//! `Unknown` rather than a guess.

use horon_agent_platform_api::StdinState;

pub(crate) fn stdin_state(_pid: u32) -> StdinState {
    StdinState::Unknown
}

pub(crate) fn process_tree_stdin_state(_pid: u32) -> StdinState {
    StdinState::Unknown
}

pub(crate) fn direct_children(_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// Windows has no POSIX pseudo-terminal.
pub fn pty_supported() -> bool {
    false
}

/// A terminal handle on a platform that has none.
///
/// Every method reports the same "not supported" error: callers can hold the
/// type unconditionally and only see a failure when they ask for a terminal,
/// which the `pty_supported` check keeps out of reach.
pub struct Pty;

impl Pty {
    /// Always reports that this platform has no terminal to allocate.
    pub fn open(_columns: u16, _rows: u16) -> std::io::Result<Pty> {
        Err(unsupported())
    }

    /// Always reports that this platform has no terminal to configure.
    pub fn configure(&mut self, _command: &mut std::process::Command) -> std::io::Result<()> {
        Err(unsupported())
    }

    /// Always reports that this platform has no terminal to read.
    pub fn reader(&self) -> std::io::Result<std::fs::File> {
        Err(unsupported())
    }

    /// Always reports that this platform has no terminal to write.
    pub fn writer(&self) -> std::io::Result<std::fs::File> {
        Err(unsupported())
    }
}

fn unsupported() -> std::io::Error {
    std::io::Error::other("this platform has no POSIX terminal to allocate")
}

/// Windows runs `cmd.exe`, not a POSIX shell.
pub fn shell_is_posix() -> bool {
    false
}

/// Windows has no shell that can record a command's final directory here.
pub fn detached_shell_command(_command: &str, _cwd_file: Option<&std::path::Path>) -> Option<std::process::Command> {
    None
}

/// `cmd.exe` children otherwise decode tool output as the OEM code page.
///
/// The locale is only filled in when the parent left it unset, so a caller's
/// own locale still wins.
pub fn shell_env_overrides() -> Vec<(&'static str, String)> {
    let mut env: Vec<(&'static str, String)> = vec![
        ("PYTHONIOENCODING", "utf-8".to_string()),
        ("PYTHONUTF8", "1".to_string()),
    ];
    let has_locale =
        std::env::vars_os().any(|(key, _)| key.to_string_lossy() == "LANG" || key.to_string_lossy().starts_with("LC_"));
    if !has_locale {
        env.push(("LANG", "C.UTF-8".to_string()));
        env.push(("LC_ALL", "C.UTF-8".to_string()));
    }
    env
}

/// Windows has no scratch directory convention for child shells here.
pub fn passes_scratch_dir_to_shell() -> bool {
    false
}

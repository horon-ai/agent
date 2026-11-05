//! Terminal allocation for the `bash` tool.
//!
//! `pty: "auto"` (the default) allocates a terminal only for commands that need
//! one, `"always"` forces one, and `"off"` never allocates. Every descriptor and
//! `unsafe` operation involved in allocating a terminal lives in this module so
//! the rest of the tool only deals with [`PtyPlan`].

use crate::config::BashPtyMode;
#[cfg(unix)]
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

/// Programs whose interactive behavior depends on a terminal on stdin.
///
/// `pty: "auto"` allocates a terminal only when a command's argv0 is one of
/// these, or when `python`/`node` is started with `-i`.
pub(crate) const TERMINAL_COMMANDS: &[&str] = &[
    "ssh", "su", "sudo", "top", "htop", "less", "more", "man", "vim", "nano", "psql", "mysql",
];

const PTY_COLUMNS: u16 = 80;
const PTY_ROWS: u16 = 24;

pub(crate) type BoxedReader = Box<dyn tokio::io::AsyncRead + Unpin + Send>;
pub(crate) type BoxedWriter = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

#[cfg(not(unix))]
const NO_TERMINAL_NOTE: &str = "Note: this command normally needs a terminal, but this platform \
has no POSIX terminal to allocate, so it ran with pipes.";

pub(crate) enum PtyPlan {
    Pipe { note: Option<&'static str> },
    Terminal(PtyHandle),
}

pub(crate) fn plan(mode: BashPtyMode, command: &str) -> anyhow::Result<PtyPlan> {
    match mode {
        BashPtyMode::Off => Ok(PtyPlan::Pipe { note: None }),
        BashPtyMode::Always => required(),
        BashPtyMode::Auto => {
            if command_wants_terminal(command) {
                preferred()
            } else {
                Ok(PtyPlan::Pipe { note: None })
            }
        }
    }
}

#[cfg(unix)]
fn required() -> anyhow::Result<PtyPlan> {
    Ok(PtyPlan::Terminal(PtyHandle::open()?))
}

#[cfg(unix)]
fn preferred() -> anyhow::Result<PtyPlan> {
    required()
}

#[cfg(not(unix))]
fn required() -> anyhow::Result<PtyPlan> {
    Err(unsupported().into())
}

#[cfg(not(unix))]
fn preferred() -> anyhow::Result<PtyPlan> {
    Ok(PtyPlan::Pipe {
        note: Some(NO_TERMINAL_NOTE),
    })
}

/// Whether `auto` should allocate a terminal for this command string.
///
/// The command is scanned as shell text: quoted words are ignored as command
/// names, and each simple command (split on `;`, newlines, `|`, `&`, `&&`,
/// `||`) is checked on its own, so `ssh host` and `ls | less` both match while
/// `echo "ssh"` does not.
pub(crate) fn command_wants_terminal(command: &str) -> bool {
    split_segments(command)
        .iter()
        .any(|segment| segment_wants_terminal(segment))
}

fn segment_wants_terminal(words: &[String]) -> bool {
    let mut index = 0;
    while index < words.len() && is_assignment(&words[index]) {
        index += 1;
    }
    let Some(argv0) = words.get(index) else {
        return false;
    };
    let base = basename(argv0);
    if TERMINAL_COMMANDS.contains(&base) {
        return true;
    }
    let interactive = words[index + 1..]
        .iter()
        .any(|word| word == "-i" || (word.starts_with('-') && !word.starts_with("--") && word.contains('i')));
    let interpreter = base.starts_with("python") || base == "node" || base == "nodejs";
    interpreter && interactive
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'),
        None => false,
    }
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Split shell text into simple-command segments of words.
///
/// This is a small scanner, not a shell parser. It honors single quotes, double
/// quotes, and backslash escapes so quoted text cannot pass for a command name.
fn split_segments(command: &str) -> Vec<Vec<String>> {
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                word.push(ch);
            }
            continue;
        }
        if in_double {
            match ch {
                '\\' => escaped = true,
                '"' => in_double = false,
                _ => word.push(ch),
            }
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '\'' => in_single = true,
            '"' => in_double = true,
            ' ' | '\t' | '\r' => push_word(&mut words, &mut word),
            ';' | '\n' | '&' | '|' => push_segment(&mut segments, &mut words, &mut word),
            _ => word.push(ch),
        }
    }
    push_segment(&mut segments, &mut words, &mut word);
    segments
}

fn push_word(words: &mut Vec<String>, word: &mut String) {
    if !word.is_empty() {
        words.push(std::mem::take(word));
    }
}

fn push_segment(segments: &mut Vec<Vec<String>>, words: &mut Vec<String>, word: &mut String) {
    push_word(words, word);
    if !words.is_empty() {
        segments.push(std::mem::take(words));
    }
}

/// A freshly allocated terminal.
///
/// The slave end is consumed by [`PtyHandle::configure`] so the parent keeps no
/// copy of it open: a lingering slave descriptor would keep the master end from
/// reaching end-of-file after the child exits, and the reader would hang.
pub(crate) struct PtyHandle {
    #[cfg(unix)]
    master: std::os::fd::OwnedFd,
    #[cfg(unix)]
    slave: Option<std::os::fd::OwnedFd>,
}

impl PtyHandle {
    /// Point `command`'s stdin, stdout, and stderr at the terminal and make it
    /// the child's controlling terminal.
    pub(crate) fn configure(&mut self, command: &mut TokioCommand) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let slave = self.take_slave()?;
            command
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            // SAFETY: `pre_exec` runs in the forked child before `exec`, so every
            // call below only changes the child's own session, terminal, signal
            // mask, and dispositions; none can affect the parent.
            //
            // `setsid` detaches the child from the daemon's process group so a
            // terminal interrupt cannot reach the daemon. `TIOCSCTTY` then makes
            // the terminal this session's controlling terminal, which is what lets
            // the line discipline generate SIGINT for `^C` at all (an open slave
            // descriptor alone never does) and what interactive programs such as
            // ssh and vim require. The interrupt dispositions are reset because a
            // long-lived agent process commonly ignores or blocks SIGINT, and the
            // child would otherwise inherit that and ignore `^C`.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let mut set: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut set);
                    libc::sigaddset(&mut set, libc::SIGINT);
                    libc::sigaddset(&mut set, libc::SIGQUIT);
                    libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                    Ok(())
                });
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = command;
            Err(unsupported())
        }
    }

    pub(crate) fn reader(&self) -> std::io::Result<tokio::fs::File> {
        #[cfg(unix)]
        {
            Ok(tokio::fs::File::from_std(std::fs::File::from(self.master.try_clone()?)))
        }
        #[cfg(not(unix))]
        {
            Err(unsupported())
        }
    }

    pub(crate) fn writer(&self) -> std::io::Result<tokio::fs::File> {
        #[cfg(unix)]
        {
            Ok(tokio::fs::File::from_std(std::fs::File::from(self.master.try_clone()?)))
        }
        #[cfg(not(unix))]
        {
            Err(unsupported())
        }
    }

    #[cfg(unix)]
    fn take_slave(&mut self) -> std::io::Result<std::os::fd::OwnedFd> {
        self.slave
            .take()
            .ok_or_else(|| std::io::Error::other("terminal slave end already consumed"))
    }

    #[cfg(unix)]
    fn open() -> std::io::Result<Self> {
        use std::os::fd::{FromRawFd, OwnedFd};

        let mut winsize = libc::winsize {
            ws_row: PTY_ROWS,
            ws_col: PTY_COLUMNS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut master_fd: libc::c_int = -1;
        let mut slave_fd: libc::c_int = -1;
        // SAFETY: `openpty` writes two descriptors on success; `name` is unused
        // (null), the termios pointer is null for kernel defaults, and `winsize`
        // points at an initialized struct. glibc declares the window-size
        // parameter as `*const`, macOS as `*mut`, so the raw pointer is built
        // from a mutable borrow that is valid for both ABIs.
        let rc = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut winsize,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: on success both descriptors are valid, distinct, and owned by
        // nothing else, so each `OwnedFd` is the unique owner.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master_fd), OwnedFd::from_raw_fd(slave_fd)) };
        Ok(Self {
            master,
            slave: Some(slave),
        })
    }
}

#[cfg(not(unix))]
fn unsupported() -> std::io::Error {
    std::io::Error::other("this platform has no POSIX terminal to allocate")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlisted_commands_request_a_terminal() {
        for command in [
            "ssh host",
            "sudo apt install jq",
            "ls | less",
            "man bash",
            "/usr/bin/vim notes.txt",
            "FOO=1 psql -d demo",
            "cat file | htop",
            "python3 -i",
            "node -i",
        ] {
            assert!(command_wants_terminal(command), "{command}");
        }
    }

    #[test]
    fn ordinary_commands_stay_on_pipes() {
        for command in [
            "echo hello",
            "ls -la",
            "cargo test -p horon-agent-app-core",
            "echo \"ssh\"",
            "printf 'less'",
            "grep -i foo file",
            "python3 -m pytest",
            "node script.js",
        ] {
            assert!(!command_wants_terminal(command), "{command}");
        }
    }
}

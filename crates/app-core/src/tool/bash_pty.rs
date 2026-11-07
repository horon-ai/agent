//! Terminal allocation for the `bash` tool.
//!
//! `pty: "auto"` (the default) allocates a terminal only for commands that need
//! one, `"always"` forces one, and `"off"` never allocates. Every descriptor and
//! `unsafe` operation involved in allocating a terminal lives in this module so
//! the rest of the tool only deals with [`PtyPlan`].

use crate::config::BashPtyMode;
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

fn required() -> anyhow::Result<PtyPlan> {
    Ok(PtyPlan::Terminal(PtyHandle::open()?))
}

fn preferred() -> anyhow::Result<PtyPlan> {
    if !horon_agent_platform::term::pty_supported() {
        return Ok(PtyPlan::Pipe {
            note: Some(NO_TERMINAL_NOTE),
        });
    }
    required()
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
/// The descriptor handling lives in the platform adapter; this wrapper only
/// adapts its blocking handles to the async ones the tool needs. The slave end
/// is consumed by [`PtyHandle::configure`] so the parent keeps no copy of it
/// open: a lingering slave descriptor would keep the master end from reaching
/// end-of-file after the child exits, and the reader would hang.
pub(crate) struct PtyHandle {
    pty: horon_agent_platform::term::Pty,
}

impl PtyHandle {
    /// Point `command`'s stdin, stdout, and stderr at the terminal and make it
    /// the child's controlling terminal.
    pub(crate) fn configure(&mut self, command: &mut TokioCommand) -> std::io::Result<()> {
        self.pty.configure(command.as_std_mut())
    }

    /// The terminal's output, as an async reader.
    pub(crate) fn reader(&self) -> std::io::Result<tokio::fs::File> {
        Ok(tokio::fs::File::from_std(self.pty.reader()?))
    }

    /// The terminal's input, as an async writer.
    pub(crate) fn writer(&self) -> std::io::Result<tokio::fs::File> {
        Ok(tokio::fs::File::from_std(self.pty.writer()?))
    }

    fn open() -> std::io::Result<Self> {
        Ok(PtyHandle {
            pty: horon_agent_platform::term::Pty::open(PTY_COLUMNS, PTY_ROWS)?,
        })
    }
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

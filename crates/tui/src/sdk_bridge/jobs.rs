//! The background work one session is still running, and the one stop this
//! client offers: a single background terminal.
//!
//! The daemon lists live work only, so every row this module builds is
//! running. Output arrives as base64 of the last 64 KiB; a row keeps only the
//! last few visible characters, because the preview pane shows eight lines and
//! a job's log is not the request's transcript. Everything here is read-only
//! and scoped to the session the worker follows, which is what makes a
//! reattach a fresh listing rather than a replay of output notices.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use horon_agent_sdk::api::BackgroundWorkItem;
use horon_agent_sdk::{Error, HoronAgentClient};

use super::Incoming;

/// Bytes of a job's output tail a row keeps, after decoding.
///
/// The daemon already caps the tail at 64 KiB; a preview pane shows four lines,
/// so the rest would only cost memory and a wider filter string.
const OUTPUT_TAIL_BYTES: usize = 2 * 1024;

/// Output lines a row keeps for its preview.
const OUTPUT_LINES: usize = 4;

/// Longest output line, in characters.
const OUTPUT_LINE_CHARS: usize = 160;

/// Longest command, directory or id a row keeps.
const FIELD_CHARS: usize = 160;

/// One background work item, as the picker draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundJobRow {
    /// Tool call that started the work; the id a cancellation names.
    pub call_id: String,
    /// Background task id the daemon assigned.
    pub task_id: String,
    /// `terminal` or `tool_call`.
    pub kind: String,
    /// Command line of a terminal, when the daemon reported one.
    pub command: Option<String>,
    /// Working directory of a terminal, when the daemon reported one.
    pub cwd: Option<String>,
    /// The last visible output lines, already sanitized and bounded.
    pub output: Vec<String>,
}

impl BackgroundJobRow {
    /// Whether the daemon will stop this job as a background terminal.
    ///
    /// Only a bash job is cancelled through `cancel_background_terminal`; any
    /// other tool the daemon moved to the background answers `not_cancellable`.
    pub fn cancellable(&self) -> bool {
        self.kind == "terminal"
    }
}

/// The list of running background work for `session_id`.
pub(super) fn read(client: &HoronAgentClient, session_id: &str) -> Result<Incoming, Error> {
    Ok(Incoming::BackgroundJobs(rows(client.list_background_work(session_id)?)))
}

/// Stop one background terminal and report the list as it stands after.
///
/// The cancel reply only says a stop was started; the refreshed list is what
/// says whether the job is still running. A refresh that fails is not the
/// cancellation's failure, so its sentence travels beside the verdict and the
/// client reports it without hiding either.
pub(super) fn cancel(client: &HoronAgentClient, session_id: &str, call_id: &str) -> Result<Incoming, Error> {
    let outcome = client.cancel_background_terminal(session_id, call_id)?;
    let jobs = client.list_background_work(session_id).map(rows);
    Ok(Incoming::BackgroundJobCancelled {
        call_id: outcome.call_id,
        accepted: outcome.accepted,
        reason: outcome.reason,
        jobs,
    })
}

/// The rows of one list answer, in the daemon's order.
pub(super) fn rows(items: Vec<BackgroundWorkItem>) -> Vec<BackgroundJobRow> {
    items.into_iter().map(row).collect()
}

/// One item, with every field it draws sanitized and cut to a pane's width.
fn row(item: BackgroundWorkItem) -> BackgroundJobRow {
    BackgroundJobRow {
        // The id a cancellation names is kept byte for byte: a display cut or
        // a stripped control character would make the request name another
        // call. The app cuts it only where it draws it.
        call_id: item.call_id,
        task_id: field(&item.task_id),
        kind: field(&item.kind),
        command: item.command.as_deref().map(field).filter(|text| !text.is_empty()),
        cwd: item.cwd.as_deref().map(field).filter(|text| !text.is_empty()),
        output: match item.output.as_deref() {
            Some(encoded) => output_lines(encoded),
            None => Vec::new(),
        },
    }
}

/// The last visible lines of a base64 output tail.
///
/// Decoding a value the daemon wrote cannot fail for a payload this build
/// produced, and one that does is an empty tail rather than an error the list
/// would show on every row.
fn output_lines(encoded: &str) -> Vec<String> {
    let Ok(bytes) = STANDARD.decode(encoded) else {
        return Vec::new();
    };
    let mut start = bytes.len().saturating_sub(OUTPUT_TAIL_BYTES);
    // The cut is by bytes; moving to the next code point keeps the first line
    // from opening with a replacement glyph.
    while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
        start += 1;
    }
    let text = String::from_utf8_lossy(&bytes[start..]);
    let mut lines: Vec<String> = text
        .lines()
        .rev()
        .map(sanitized)
        .filter(|line| !line.is_empty())
        .take(OUTPUT_LINES)
        .collect();
    lines.reverse();
    lines
}

/// A command, directory or id as one visible line.
fn field(text: &str) -> String {
    let Some(first) = text.lines().next() else {
        return String::new();
    };
    sanitized(first).chars().take(FIELD_CHARS).collect()
}

/// One line with escape sequences and control characters removed.
///
/// The command, the directory and the output are daemon values drawn as text.
/// A raw escape in one could move the terminal's cursor or change a colour, so
/// the sequence is read and dropped here instead of reaching the backend.
fn sanitized(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(if character.is_control() { ' ' } else { character });
            continue;
        }
        // A CSI sequence ends at its final byte, an OSC at BEL or ST, and any
        // other escape is one character of its own.
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out.trim_end().chars().take(OUTPUT_LINE_CHARS).collect()
}

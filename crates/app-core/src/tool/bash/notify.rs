//! Opt-in live notices for a finite background command's output (#466).
//!
//! A caller that waits on a milestone or a warning can pass `notify_pattern` and
//! receive each matching output line while the job keeps running. The design
//! follows oh-my-pi's `OutputNotifier`
//! (`packages/coding-agent/src/async/notify.ts`, MIT, © can1357): compile once,
//! examine each new line once, keep a per-job budget, and leave completion
//! delivery to its own path.
//!
//! Two rules shape this file. The matcher allocates nothing until a caller opts
//! in, because every background command would otherwise pay for a feature it did
//! not ask for. And a notice is derived from process output, so it is bounded,
//! stripped of terminal escapes, and prefixed with the job and the stream: a
//! client renders it as data, never as instruction.

use super::*;

/// The longest `notify_pattern` accepted, in bytes.
const MAX_PATTERN_BYTES: usize = 512;

/// The size bound for the compiled pattern, in bytes. The `regex` crate turns a
/// pattern into an automaton whose size follows its syntax, and the pattern is
/// caller-supplied text, so both the source and the program are capped.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// How many notices one job may deliver. A command that prints ten thousand
/// matching lines still yields at most this many, so a chatty child cannot flood
/// the owner's transcript.
const MAX_NOTICES_PER_JOB: usize = 20;

/// The most notice text one job may deliver, in bytes, whatever the count. The
/// two caps bound a job whichever of them it reaches first.
const MAX_NOTICE_BYTES_PER_JOB: usize = 20 * 1024;

/// The longest matched line a notice shows, in characters. The prefix and an
/// elision marker add a few bytes on top.
const MAX_NOTICE_TEXT_CHARS: usize = 200;

/// The longest escape sequence dropped as one unit, so a malformed introducer
/// cannot swallow the rest of a line.
const MAX_ESCAPE_CHARS: usize = 32;

/// Which pipe a matched line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Refuse a `notify_pattern` that could never be honored, before anything is
/// spawned: an empty or oversized pattern, and the two combinations that have no
/// meaning — a foreground call, which reports its output when it returns, and a
/// named service, which runs until cancelled and is out of scope for live
/// notices.
pub(super) fn validate(pattern: Option<&str>, run_in_background: bool, service: Option<&str>) -> Result<()> {
    let Some(pattern) = pattern else {
        return Ok(());
    };
    // A name of spaces is no name at all, which is how the background path reads it.
    let service = service.map(str::trim).filter(|service| !service.is_empty());
    anyhow::ensure!(
        !pattern.is_empty(),
        "notify_pattern must not be empty; omit it to disable live notices"
    );
    anyhow::ensure!(
        service.is_none(),
        "notify_pattern does not apply to a named service (`name`); it delivers live notices for a finite background job"
    );
    anyhow::ensure!(
        run_in_background,
        "notify_pattern needs `run_in_background: true`; a foreground call reports its output when it returns"
    );
    anyhow::ensure!(
        pattern.len() <= MAX_PATTERN_BYTES,
        "notify_pattern is {} bytes; the limit is {MAX_PATTERN_BYTES}",
        pattern.len()
    );
    Ok(())
}

/// Compile the opt-in pattern for one job's output loop.
///
/// The pattern itself is compiled here — a caller-supplied regular expression is
/// untrusted text, so it is bounded rather than compiled as given — and a
/// pattern that does not compile is still refused before a process exists.
pub(super) fn compile(
    pattern: Option<&str>,
    run_in_background: bool,
    service: Option<&str>,
) -> Result<Option<regex::Regex>> {
    validate(pattern, run_in_background, service)?;
    let Some(pattern) = pattern else {
        return Ok(None);
    };
    let regex = regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|error| anyhow::anyhow!("invalid notify_pattern: {error}"))?;
    Ok(Some(regex))
}

/// The sentence a started background call adds when live notices are on. The
/// leading newline continues the line above it in the started-call message.
pub(super) fn status_note(pattern: Option<&str>) -> &'static str {
    match pattern {
        Some(_) => "\nLive notices: each matching output line arrives while the job runs (bounded per job).",
        None => "",
    }
}

/// One job's live-notice matcher and its remaining budget.
///
/// Built once per background call and owned by that call's output loop. Without
/// an opted-in pattern this is a single `None`, so nothing is allocated and each
/// line costs one branch.
pub(super) struct Notifier {
    matcher: Option<Matcher>,
}

struct Matcher {
    session_id: String,
    task_id: String,
    regex: regex::Regex,
    notices: usize,
    bytes: usize,
}

impl Notifier {
    /// The notifier for the job writing `output_path`.
    ///
    /// The job identifier comes from the output file the background manager
    /// already named, which is the same task id every other control surface
    /// uses; no separate registry is involved.
    pub(super) fn for_job(session_id: &str, output_path: &Path, pattern: Option<regex::Regex>) -> Notifier {
        let Some(regex) = pattern else {
            return Notifier { matcher: None };
        };
        Notifier {
            matcher: Some(Matcher {
                session_id: session_id.to_string(),
                task_id: task_id_from_output_path(output_path).unwrap_or("?").to_string(),
                regex,
                notices: 0,
                bytes: 0,
            }),
        }
    }

    /// Offer one output line, publishing a bounded notice when it matches.
    ///
    /// This never awaits, so a slow reader cannot hold up the output file the
    /// command is writing to; and a spent budget turns into one comparison per
    /// line rather than a growing backlog.
    pub(super) fn observe(&mut self, stream: Stream, line: &str) {
        let Some(matcher) = self.matcher.as_mut() else {
            return;
        };
        if matcher.notices >= MAX_NOTICES_PER_JOB || matcher.bytes >= MAX_NOTICE_BYTES_PER_JOB {
            return;
        }
        if !matcher.regex.is_match(line) {
            return;
        }
        let text = format!(
            "bash task {} {}: {}",
            matcher.task_id,
            stream.label(),
            notice_text(line)
        );
        if matcher.bytes + text.len() > MAX_NOTICE_BYTES_PER_JOB {
            return;
        }
        matcher.notices += 1;
        matcher.bytes += text.len();
        crate::bus::Bus::global().publish(crate::bus::BusEvent::BackgroundOutput {
            session_id: matcher.session_id.clone(),
            text,
        });
    }
}

/// One output line as a notice shows it: terminal escapes dropped, tabs folded to
/// spaces, surrounding whitespace trimmed, and the text cut at
/// [`MAX_NOTICE_TEXT_CHARS`] with an elision marker.
fn notice_text(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.trim().chars();
    let mut written = 0;
    let mut cut = false;
    while let Some(ch) = chars.next() {
        if written >= MAX_NOTICE_TEXT_CHARS {
            cut = true;
            break;
        }
        if ch == '\u{1b}' {
            for _ in 0..escape_width(chars.as_str()) {
                chars.next();
            }
            continue;
        }
        let shown = match ch {
            '\t' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        };
        if let Some(shown) = shown {
            out.push(shown);
            written += 1;
        }
    }
    if cut {
        out.push('…');
    }
    out
}

/// How many characters after an `ESC` belong to the same escape sequence.
///
/// `rest` starts right after the `ESC`. A CSI sequence runs to a final byte in
/// `0x40..=0x7E`, an OSC sequence runs to `BEL` or `ST`, and anything else is a
/// two-character escape. The scan is capped so a malformed introducer cannot
/// consume a whole line.
fn escape_width(rest: &str) -> usize {
    let mut chars = rest.chars().take(MAX_ESCAPE_CHARS);
    match chars.next() {
        Some('[') => {
            let mut width = 1;
            for ch in chars {
                width += 1;
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    break;
                }
            }
            width
        }
        Some(']') => {
            let mut width = 1;
            let mut previous = '\0';
            for ch in chars {
                width += 1;
                if ch == '\u{7}' || (previous == '\u{1b}' && ch == '\\') {
                    break;
                }
                previous = ch;
            }
            width
        }
        Some(_) => 1,
        None => 0,
    }
}

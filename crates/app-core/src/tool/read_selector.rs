//! Line selectors for `read`: `path:10-40`, `path:10-`, `path:-50`, `path:raw`.
//!
//! The selector is the comma-separated tail of a read path. Splitting is
//! conservative: a path is only split when the text after a colon parses
//! completely as a selector, so a file whose own name contains a colon keeps
//! working. Rendering lives here too, because `artifact://` reads and
//! `history://current` render in-memory text through the same rules.

use anyhow::Result;
use std::path::Path;

/// Longest line rendered before it is cut. An anchored (hashline) cut is
/// annotated with [`TRUNCATED_TAG`] because nothing can vouch for the bytes
/// that were dropped.
pub(crate) const MAX_LINE_LEN: usize = 2000;

/// Default line count for a read without an explicit limit, and for the
/// open-ended selector `:N-`.
pub(crate) const DEFAULT_LIMIT: usize = 5000;

/// Hard ceiling on the bytes one page may return, so a file of very long lines
/// cannot spend a whole context window on a single page.
pub(crate) const READ_BYTE_BUDGET: usize = 256 * 1024;

/// Anchor rendered for a line cut at [`MAX_LINE_LEN`]. It names no content, so
/// an anchored edit has nothing to validate against and must refuse it.
pub(crate) const TRUNCATED_TAG: &str = "----";

/// Lines of context added after each explicit range end (and one leading line
/// before each explicit range start) when a selector names explicit ranges.
const TRAILING_CONTEXT: usize = 3;
const LEADING_CONTEXT: usize = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selector {
    /// No selector: the offset/limit page of the whole document.
    None,
    /// `:raw` — every line, no numbers, no per-line cut.
    Raw,
    /// Explicit 1-based inclusive ranges. `None` as an end means "to EOF".
    Lines {
        ranges: Vec<(usize, Option<usize>)>,
        raw: bool,
    },
    /// `:-N` — the last N lines, optionally combined with explicit ranges.
    Tail {
        count: usize,
        ranges: Vec<(usize, Option<usize>)>,
        raw: bool,
    },
}

/// One rendered page.
#[derive(Debug, Default, Clone)]
pub(crate) struct Rendered {
    pub body: String,
    pub total_lines: usize,
    /// Last line number that reached `body` (0 when nothing was rendered).
    pub last_line: usize,
    /// Lines cut at [`MAX_LINE_LEN`].
    pub truncated_lines: usize,
    /// More lines remain in the document beyond `last_line`.
    pub more: bool,
}

/// Splits a read path into the path itself and its selector.
///
/// The whole string wins when it names an existing file. Otherwise the first
/// colon whose suffix parses as a selector separates the two parts, and a
/// suffix that looks like a selector but does not parse is an error rather than
/// a file named `something:junk`.
pub(crate) fn split_path_and_selector(raw: &str, cwd: &Path) -> Result<(String, Selector)> {
    if raw.is_empty() {
        return Ok((String::new(), Selector::None));
    }
    let candidate = Path::new(raw);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    };
    match std::fs::symlink_metadata(&resolved) {
        Ok(_) => return Ok((raw.to_string(), Selector::None)),
        Err(err) if err.kind() != std::io::ErrorKind::NotFound && err.kind() != std::io::ErrorKind::NotADirectory => {
            // A permission error (or anything else that is not "no such path")
            // resolves to a literal path: never guess a split from a failed lookup.
            return Ok((raw.to_string(), Selector::None));
        }
        Err(_) => {}
    }

    if raw.contains("://") {
        return Ok((raw.to_string(), Selector::None));
    }

    for (idx, _) in raw.match_indices(':') {
        let sel = &raw[idx + 1..];
        if !looks_like_selector(sel) {
            continue;
        }
        let selector = parse_selector(sel)?;
        return Ok((raw[..idx].to_string(), selector));
    }
    Ok((raw.to_string(), Selector::None))
}

/// Splits a selector off a URI-shaped path whose `//` must not be read as a
/// separator: `artifact://12:-3` becomes `("artifact://12", Tail { count: 3 })`.
///
/// The URI readers (`artifact://`, `history://current`) use this to reuse the
/// file selector grammar; no filesystem reader calls it.
pub(crate) fn split_uri_and_selector(raw: &str) -> Result<(String, Selector)> {
    let Some(scheme_end) = raw.find("://").map(|idx| idx + 3) else {
        return Ok((raw.to_string(), Selector::None));
    };
    for (idx, _) in raw.match_indices(':') {
        if idx < scheme_end {
            continue;
        }
        let sel = &raw[idx + 1..];
        if !looks_like_selector(sel) {
            continue;
        }
        let selector = parse_selector(sel)?;
        return Ok((raw[..idx].to_string(), selector));
    }
    Ok((raw.to_string(), Selector::None))
}

/// Whether a colon suffix could be a selector at all.
///
/// Only the characters a selector can be made of count; anything else (a port
/// number in a URL, a drive letter, a plain filename) leaves the colon alone.
fn looks_like_selector(sel: &str) -> bool {
    !sel.is_empty()
        && sel
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, ',' | '+' | '-' | ':' | 'r' | 'a' | 'w'))
}

pub(crate) fn parse_selector(sel: &str) -> Result<Selector> {
    let invalid = || {
        anyhow::anyhow!(
            "Invalid selector ':{sel}'. Use :N, :N-M, :N+K, :N- (open-ended), :-N (last N lines), \
             a comma-separated list of ranges, :raw, or a range combined with raw (e.g. :raw:50-100)."
        )
    };

    let chunks: Vec<&str> = sel.split(':').collect();
    if chunks.len() > 2 {
        return Err(invalid());
    }

    let mut raw = false;
    let mut ranges: Vec<(usize, Option<usize>)> = Vec::new();
    let mut tail: Option<usize> = None;
    for chunk in chunks {
        if chunk.eq_ignore_ascii_case("raw") {
            if raw {
                return Err(invalid());
            }
            raw = true;
            continue;
        }
        if chunk.is_empty() {
            return Err(invalid());
        }
        for part in chunk.split(',') {
            match parse_part(part) {
                Some(Part::Range(range)) => ranges.push(range),
                Some(Part::Tail(count)) => tail = Some(tail.map_or(count, |current: usize| current.max(count))),
                None => return Err(invalid()),
            }
        }
    }

    if ranges.is_empty() && tail.is_none() {
        if raw {
            return Ok(Selector::Raw);
        }
        return Err(invalid());
    }
    match tail {
        Some(count) => Ok(Selector::Tail { count, ranges, raw }),
        None => Ok(Selector::Lines { ranges, raw }),
    }
}

enum Part {
    Range((usize, Option<usize>)),
    Tail(usize),
}

fn parse_part(part: &str) -> Option<Part> {
    if part.is_empty() {
        return None;
    }
    let digits = |text: &str| -> Option<usize> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse::<usize>().ok()
    };

    // `-N`: the last N lines.
    if let Some(rest) = part.strip_prefix('-') {
        let count = digits(rest)?;
        return (count >= 1).then_some(Part::Tail(count));
    }
    // `N+K`: N through N+K-1.
    if let Some((left, right)) = part.split_once('+') {
        let start = digits(left)?;
        let extra = digits(right)?;
        if start < 1 || extra < 1 {
            return None;
        }
        return Some(Part::Range((start, Some(start.checked_add(extra - 1)?))));
    }
    // `N-M` and `N-`.
    if let Some((left, right)) = part.split_once('-') {
        let start = digits(left)?;
        if start < 1 {
            return None;
        }
        if right.is_empty() {
            return Some(Part::Range((start, None)));
        }
        let end = digits(right)?;
        return (end >= start).then_some(Part::Range((start, Some(end))));
    }
    let single = digits(part)?;
    (single >= 1).then_some(Part::Range((single, Some(single))))
}

/// Renders `text` through `selector`.
///
/// Callers that hold in-memory documents (`artifact://`, `history://current`)
/// use this so their pages look exactly like a file read.
pub(crate) fn render_with_selector(text: &str, selector: &Selector, hashline: bool) -> Result<Rendered> {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Ok(Rendered {
            total_lines: 0,
            ..Rendered::default()
        });
    }

    match selector {
        Selector::None => Ok(render_spans(
            &lines,
            &[Span {
                start: 1,
                end: total,
                limited: false,
                context: false,
            }],
            total,
            hashline,
        )),
        Selector::Raw => Ok(render_raw_spans(
            &lines,
            &[Span {
                start: 1,
                end: total,
                limited: false,
                context: false,
            }],
            total,
        )),
        Selector::Lines { ranges, raw } => render_selected(&lines, ranges, &[], *raw, hashline),
        Selector::Tail { count, ranges, raw } => {
            // `:-N` names the last N lines: start at `total - N + 1`.
            let start = total.saturating_sub(*count) + 1;
            render_selected(&lines, ranges, &[(start, total)], *raw, hashline)
        }
    }
}

/// Renders the offset/limit page of `text` (the `start_line`/`offset` style).
pub(crate) fn render_range(text: &str, offset: usize, limit: usize, hashline: bool) -> Rendered {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 || limit == 0 || offset >= total {
        return Rendered {
            total_lines: total,
            last_line: offset.min(total),
            more: offset < total,
            ..Rendered::default()
        };
    }
    let start = offset + 1;
    let end = limit.saturating_add(offset).min(total);
    let mut rendered = render_spans(
        &lines,
        &[Span {
            start,
            end,
            limited: false,
            context: false,
        }],
        total,
        hashline,
    );
    rendered.more |= end < total;
    rendered
}

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
    /// The requested range extends past `end`, so a continuation exists.
    limited: bool,
    /// Whether the explicit-range context rules apply to this span.
    context: bool,
}

fn render_selected(
    lines: &[&str],
    ranges: &[(usize, Option<usize>)],
    extra: &[(usize, usize)],
    raw: bool,
    hashline: bool,
) -> Result<Rendered> {
    let total = lines.len();
    let mut spans: Vec<Span> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for &(start, end) in ranges {
        if start > total {
            if ranges.len() == 1 && extra.is_empty() {
                return Err(anyhow::anyhow!(
                    "Line {start} is beyond end of file ({total} lines total). \
                     Use :1 to read from the start, or :{total} to read the last line."
                ));
            }
            let described_end = end.unwrap_or(start);
            skipped.push(format!(
                "[Range {start}-{described_end} is beyond end of file ({total} lines total); skipped]"
            ));
            continue;
        }
        let (end, limited) = match end {
            Some(end) => (end.min(total), false),
            None => {
                let capped = start.saturating_add(DEFAULT_LIMIT - 1).min(total);
                (capped, capped < total)
            }
        };
        spans.push(Span {
            start,
            end,
            limited,
            context: !raw,
        });
    }
    // The tail span is implicit: `:-2` means exactly the last two lines, so no
    // context is added around it. Only ranges the caller spelled out get it.
    for &(start, end) in extra {
        spans.push(Span {
            start,
            end,
            limited: false,
            context: false,
        });
    }

    let mut rendered = if raw {
        render_raw_spans(lines, &merged(spans, total), total)
    } else {
        let merged = merged(spans, total);
        let mut rendered = render_spans(lines, &merged, total, hashline);
        rendered.more |= merged.iter().any(|span| span.limited && span.end <= rendered.last_line);
        rendered
    };
    if !skipped.is_empty() {
        if !rendered.body.is_empty() && !rendered.body.ends_with('\n') {
            rendered.body.push('\n');
        }
        for notice in skipped {
            rendered.body.push_str(&notice);
            rendered.body.push('\n');
        }
    }
    Ok(rendered)
}

/// Expands explicit spans with their context and merges overlapping or
/// adjacent spans, ascending.
fn merged(mut spans: Vec<Span>, total: usize) -> Vec<Span> {
    for span in &mut spans {
        if span.context {
            span.start = span.start.saturating_sub(LEADING_CONTEXT).max(1);
            span.end = span.end.saturating_add(TRAILING_CONTEXT).min(total);
        }
    }
    spans.retain(|span| span.start <= span.end);
    spans.sort_by_key(|span| span.start);
    let mut out: Vec<Span> = Vec::new();
    for span in spans {
        match out.last_mut() {
            Some(current) if span.start <= current.end.saturating_add(1) => {
                current.end = current.end.max(span.end);
                current.limited |= span.limited;
                current.context |= span.context;
            }
            _ => out.push(span),
        }
    }
    out
}

fn render_spans(lines: &[&str], spans: &[Span], total: usize, hashline: bool) -> Rendered {
    let mut out = String::new();
    let mut last_line = 0usize;
    let mut truncated_lines = 0usize;
    let mut stopped_early = false;

    'spans: for (index, span) in spans.iter().enumerate() {
        if index > 0 {
            if out.len() >= READ_BYTE_BUDGET {
                stopped_early = true;
                break;
            }
            out.push_str("…\n");
        }
        for line_no in span.start..=span.end.min(total) {
            if out.len() >= READ_BYTE_BUDGET {
                stopped_early = true;
                break 'spans;
            }
            let line = lines[line_no - 1];
            if push_numbered_line(&mut out, line_no, line, hashline) {
                truncated_lines += 1;
            }
            last_line = line_no;
        }
    }

    let more = stopped_early || spans.iter().any(|span| span.limited && span.end <= last_line);
    Rendered {
        body: out,
        total_lines: total,
        last_line,
        truncated_lines,
        more,
    }
}

fn render_raw_spans(lines: &[&str], spans: &[Span], total: usize) -> Rendered {
    let mut out = String::new();
    let mut last_line = 0usize;
    let mut stopped_early = false;

    'spans: for (index, span) in spans.iter().enumerate() {
        if index > 0 {
            if out.len() >= READ_BYTE_BUDGET {
                stopped_early = true;
                break;
            }
            out.push_str("…\n");
        }
        for line_no in span.start..=span.end.min(total) {
            if out.len() >= READ_BYTE_BUDGET {
                stopped_early = true;
                break 'spans;
            }
            out.push_str(lines[line_no - 1]);
            out.push('\n');
            last_line = line_no;
        }
    }

    let more = stopped_early || spans.iter().any(|span| span.limited && span.end <= last_line);
    Rendered {
        body: out,
        total_lines: total,
        last_line,
        truncated_lines: 0,
        more,
    }
}

pub(crate) fn push_numbered_line(out: &mut String, line_no: usize, line: &str, hashline: bool) -> bool {
    use std::fmt::Write;

    if line.len() > MAX_LINE_LEN {
        let truncated = crate::util::truncate_str(line, MAX_LINE_LEN);
        if hashline {
            let _ = writeln!(out, "{:>5}#{}:{}...", line_no, TRUNCATED_TAG, truncated);
        } else {
            let _ = writeln!(out, "{:>5}\t{}...", line_no, truncated);
        }
        return true;
    }
    if hashline {
        let _ = writeln!(
            out,
            "{}",
            horon_agent_edit_safety::format_line(line_no, horon_agent_edit_safety::tag_for(line.as_bytes()), line)
        );
    } else {
        let _ = writeln!(out, "{:>5}\t{}", line_no, line);
    }
    false
}

#[cfg(test)]
#[path = "read_selector_tests.rs"]
mod tests;

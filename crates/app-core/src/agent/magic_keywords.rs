//! Per-turn effort keyword scanner.
//!
//! Recognizes standalone `ultrathink` (highest effort) and `chillax` (lowest
//! effort) keywords in user-authored prompt text. Keywords inside code spans,
//! fenced blocks, HTML comments, tags, or URLs are ignored. The message text is
//! never modified; only the detected keyword is returned.

/// An effort keyword found in user-authored text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffortKeyword {
    /// `ultrathink` — raise to the highest reasoning effort for this turn.
    Ultrathink,
    /// `chillax` — lower to the lowest reasoning effort for this turn.
    Chillax,
}

const ULTRATHINK: &str = "ultrathink";
const CHILLAX: &str = "chillax";

/// Scan `text` for a standalone effort keyword.
///
/// `chillax` wins when both keywords appear, matching omp precedence. Matching is
/// exact lowercase; the keyword must stand alone in prose.
pub(crate) fn scan(text: &str) -> Option<EffortKeyword> {
    let excluded = exclusion_mask(text);
    let mut found_ultrathink = false;
    let mut found_chillax = false;

    for (keyword, flag) in [(ULTRATHINK, &mut found_ultrathink), (CHILLAX, &mut found_chillax)] {
        let mut from = 0;
        while let Some(rel) = text[from..].find(keyword) {
            let start = from + rel;
            let end = start + keyword.len();
            if !excluded[start..end].iter().any(|&b| b) && standalone(text, start, end) {
                *flag = true;
                break;
            }
            from = end;
        }
    }

    if found_chillax {
        Some(EffortKeyword::Chillax)
    } else if found_ultrathink {
        Some(EffortKeyword::Ultrathink)
    } else {
        None
    }
}

/// True when the keyword at `start..end` stands alone in the original text.
///
/// The characters immediately before and after must not continue a word, path,
/// flag, or dotted name. Quotes and sentence punctuation may touch it.
fn standalone(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();
    let before_ok = start == 0 || !is_continuation(bytes[start - 1]);
    let after_ok = end == bytes.len() || !is_continuation_after(bytes, end);
    before_ok && after_ok
}

fn is_continuation(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'\\' | b'-' | b':' | b'(' | b'@' | b'#' | b'$')
}

fn is_continuation_after(bytes: &[u8], i: usize) -> bool {
    let b = bytes[i];
    if b == b'.' || b == b',' {
        // `ultrathink.` at a sentence end matches; `ultrathink.ts` does not.
        return bytes
            .get(i + 1)
            .is_some_and(|next| next.is_ascii_alphanumeric() || *next == b'_');
    }
    is_continuation(b)
}

/// Mark every byte that belongs to an excluded region.
///
/// Excluded regions: fenced code blocks (``` / ~~~, to the matching fence or
/// end), inline code spans (backtick runs of equal length), HTML/XML comments
/// (`<!-- … -->`), tags (`<…>`), and URLs (tokens containing `://` or starting
/// with `www.`).
fn exclusion_mask(text: &str) -> Vec<bool> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut mask = vec![false; n];
    let mut i = 0;

    while i < n {
        // Fenced code block.
        if let Some(fence_len) = fence_run(bytes, i) {
            let fence_char = bytes[i];
            let start = i;
            i += fence_len;
            while i < n {
                if bytes[i] == fence_char
                    && let Some(len) = fence_run(bytes, i)
                    && len >= fence_len
                {
                    i += len;
                    break;
                }
                i += 1;
            }
            mask[start..i].fill(true);
            continue;
        }

        // Inline code span.
        if bytes[i] == b'`' {
            let run = backtick_run(bytes, i);
            let start = i;
            i += run;
            while i < n {
                if bytes[i] == b'`' {
                    let close = backtick_run(bytes, i);
                    if close == run {
                        i += close;
                        break;
                    }
                    i += close;
                } else {
                    i += 1;
                }
            }
            mask[start..i].fill(true);
            continue;
        }

        // HTML/XML comment.
        if bytes[i..].starts_with(b"<!--") {
            let start = i;
            i += 4;
            while i < n && !bytes[i..].starts_with(b"-->") {
                i += 1;
            }
            i = (i + 3).min(n);
            mask[start..i].fill(true);
            continue;
        }

        // Tag.
        if bytes[i] == b'<' {
            if let Some(rel) = bytes[i..].iter().position(|&b| b == b'>') {
                let start = i;
                i += rel + 1;
                mask[start..i].fill(true);
                continue;
            }
        }

        // URL starting with `www.`.
        if bytes[i..].starts_with(b"www.") && url_boundary(bytes, i) {
            let start = i;
            i = url_end(bytes, i);
            mask[start..i].fill(true);
            continue;
        }

        // URL with a scheme: a word token followed by `://`.
        if is_word_byte(bytes[i]) && url_boundary(bytes, i) {
            let start = i;
            let mut end = i;
            while end < n && is_word_byte(bytes[end]) {
                end += 1;
            }
            if bytes[end..].starts_with(b"://") {
                i = url_end(bytes, start);
                mask[start..i].fill(true);
                continue;
            }
        }

        i += 1;
    }

    mask
}

/// True when `i` begins a token (not inside a longer word or path).
fn url_boundary(bytes: &[u8], i: usize) -> bool {
    i == 0 || !is_continuation(bytes[i - 1])
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

fn fence_run(bytes: &[u8], i: usize) -> Option<usize> {
    let n = bytes.len();
    let c = bytes[i];
    if c != b'`' && c != b'~' {
        return None;
    }
    let mut len = 0;
    while i + len < n && bytes[i + len] == c {
        len += 1;
    }
    (len >= 3).then_some(len)
}

fn backtick_run(bytes: &[u8], i: usize) -> usize {
    let mut len = 0;
    while i + len < bytes.len() && bytes[i + len] == b'`' {
        len += 1;
    }
    len
}

/// End offset of a URL token starting at `start`.
fn url_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && is_url_char(bytes[i]) {
        i += 1;
    }
    // Trailing prose punctuation is not part of the URL.
    while i > start && matches!(bytes[i - 1], b'.' | b',' | b';' | b')' | b'!' | b'?') {
        i -= 1;
    }
    i
}

fn is_url_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'_' | b'-' | b'.' | b'~' | b':' | b'/' | b'?' | b'#' | b'@' | b'!' | b'$' | b'&' | b'+' | b'=' | b'%'
        )
}

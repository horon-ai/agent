//! Tab completion for one workspace path token in the draft.
//!
//! A draft that names a file or directory fills that name in on Tab, as the
//! shell-independent form `"name with space"`; a token that starts with `@`
//! keeps the marker and quotes only the path, so a mention can be completed
//! again. Several names that share a prefix are filled to that prefix first,
//! and the next Tab walks the candidates, one per press, wrapping at the end; a
//! directory candidate is the exception, because the press after it belongs to
//! the listing inside it.
//!
//! The scan is bounded on purpose. One token is completed, from one directory,
//! in one pass that reads names and file kinds only: file contents are never
//! opened, globs are never expanded, and `..`, an absolute path and a leading
//! `~` are refused rather than resolved. The directory must canonicalize to a
//! path inside the workspace, so a symlink pointing out of it is not followed.
//!
//! Reading the workspace only makes sense when it is on this machine, and the
//! caller decides that: `App::workspace_is_local` is on only for the client's
//! own daemon, because a session behind an explicit socket or a remote bridge
//! reports paths that belong to another machine.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// The launch directory is a display fallback when the daemon reports none.
pub(crate) fn launch_directory(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    let Ok(dir) = std::env::current_dir() else { return None };
    Some(dir)
}

/// Entries one Tab may read before the directory is treated as unbounded.
///
/// A partial listing could fill a prefix no candidate actually shares, so past
/// this many entries the key does nothing instead.
const MAX_ENTRIES: usize = 4096;

/// The candidates one token was filled from, and the one it holds now.
pub struct Choice {
    /// Where the completed token starts in the draft, counted in bytes.
    start: usize,
    /// The full relative paths of the candidates, as they are inserted.
    candidates: Arc<Vec<String>>,
    /// The candidate the token holds, when it holds one.
    chosen: usize,
}

/// The draft after completing its last token, and the candidates it offers, or
/// `None` when there is nothing to fill in.
///
/// `draft` is the text up to the cursor. `previous` is the choice the last Tab
/// made: when the token still holds the candidate it filled, this press moves
/// to the next one instead of reading the directory again.
pub fn complete(draft: &str, workspace: Option<&str>, previous: Option<&Choice>) -> Option<(String, Choice)> {
    let start = token_start(draft);
    let form = Form::parse(&draft[start..])?;
    // A command line is the registry's, and an absolute path is the whole
    // filesystem's: neither belongs to the one workspace directory read here.
    if !form.relative() {
        return None;
    }
    if let Some(previous) = previous
        && previous.start == start
        && previous.candidates.len() > 1
        && previous
            .candidates
            .get(previous.chosen)
            .is_some_and(|chosen| !chosen.ends_with('/') && chosen.as_str() == form.path)
    {
        let next = (previous.chosen + 1) % previous.candidates.len();
        return Some((
            replace_rendered(draft, start, &form, &previous.candidates[next]),
            Choice {
                start,
                candidates: Arc::clone(&previous.candidates),
                chosen: next,
            },
        ));
    }
    let (directory, fragment) = split(form.path);
    let root = workspace_root(workspace)?;
    // Canonicalizing the one directory resolves every symlink in the path it
    // was written with, so the prefix check below is what keeps a link from
    // reading outside the workspace.
    let listing = if directory.is_empty() {
        root
    } else {
        let path = reachable(std::fs::canonicalize(root.join(directory)))?;
        if !path.starts_with(&root) || !path.is_dir() {
            return None;
        }
        path
    };
    let (candidates, prefix_len, common_len) = read_candidates(&listing, directory, fragment)?;
    let (mut next, mut chosen) = match candidates.as_slice() {
        [only] => (replace_rendered(draft, start, &form, only), 0),
        [first, ..] => {
            if common_len > fragment.len() {
                let prefix_end = prefix_len + common_len;
                let fill = &first[..prefix_end];
                let next = replace_rendered(draft, start, &form, fill);
                let chosen = candidates.iter().position(|candidate| candidate == fill).unwrap_or(0);
                (next, chosen)
            } else {
                (replace_rendered(draft, start, &form, first), 0)
            }
        }
        [] => return None,
    };
    // The first candidate can be exactly what is typed, as `foo` is beside
    // `foo.rs`: with more candidates the press means the next one rather than
    // nothing, or a typed name would shut the list it starts.
    if next == draft && candidates.len() > 1 {
        chosen = 1;
        next = replace_rendered(draft, start, &form, &candidates[1]);
    }
    if next == draft {
        return None;
    }
    Some((
        next,
        Choice {
            start,
            candidates: Arc::new(candidates),
            chosen,
        },
    ))
}

/// Where the token being completed starts, counted in bytes.
///
/// A quoted token spans the spaces inside its quotes: it is the form this
/// module writes, and completing it again must read the whole name rather than
/// its last word.
fn token_start(draft: &str) -> usize {
    let line_start = draft.rfind('\n').map_or(0, |index| index + 1);
    let closed = draft.ends_with('"');
    let quotes = draft[line_start..].matches('"').count();
    // A quote opens the token only where a token can begin: at the start of
    // the line, after whitespace, or right after the `@` marker. A quote
    // inside a word — the inch mark in `5" screen` — is text, and the token
    // after it still completes.
    let opens = |quote: usize| {
        draft[..quote]
            .chars()
            .next_back()
            .is_none_or(|previous| previous.is_whitespace() || previous == '@')
    };
    // A quote belongs to the token being completed when it is still open, and
    // when it was just closed by the fill this module wrote.
    let quote = match draft.rfind('"') {
        Some(closing) if closed => {
            let opening = draft[..closing].rfind('"').unwrap_or(closing);
            opens(opening).then_some(opening)
        }
        Some(opening) if quotes % 2 == 1 => opens(opening).then_some(opening),
        _ => None,
    };
    if let Some(opening) = quote {
        // Walk back over the marker in front of the quote, which is the token
        // that opened it.
        let mut start = opening;
        while let Some(previous) = draft[..start].chars().next_back() {
            if previous.is_whitespace() {
                break;
            }
            start -= previous.len_utf8();
        }
        return start;
    }
    draft[line_start..]
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map(|(index, character)| line_start + index + character.len_utf8())
        .unwrap_or(line_start)
}

/// The draft with the token that starts at `start` replaced by the rendered candidate.
fn replace_rendered(draft: &str, start: usize, form: &Form<'_>, path: &str) -> String {
    let needs_quotes = form.quoted || has_whitespace(path);
    let extra = usize::from(form.mention) + if needs_quotes { 2 } else { 0 };
    let mut next = String::with_capacity(start + extra + path.len());
    next.push_str(&draft[..start]);
    if form.mention {
        next.push('@');
    }
    if needs_quotes {
        next.push('"');
        next.push_str(path);
        next.push('"');
    } else {
        next.push_str(path);
    }
    next
}

fn has_whitespace(text: &str) -> bool {
    if text.is_ascii() {
        text.bytes().any(|b| b == b' ' || matches!(b, b'\t'..=b'\r'))
    } else {
        text.chars().any(char::is_whitespace)
    }
}

/// The directory part of a token and the fragment the next name must start
/// with. The directory keeps the spelling the draft used, minus a trailing
/// separator.
fn split(path: &str) -> (&str, &str) {
    let (directory, fragment) = match path.rfind('/') {
        Some(index) => (&path[..index], &path[index + 1..]),
        None => ("", path),
    };
    (directory.trim_end_matches('/'), fragment)
}

/// The token as the draft wrote it: the mention marker, the quotes and the path.
struct Form<'a> {
    mention: bool,
    quoted: bool,
    path: &'a str,
}

impl<'a> Form<'a> {
    /// Read one token, or nothing when it names no path at all.
    fn parse(token: &'a str) -> Option<Self> {
        let (mention, rest) = match token.strip_prefix('@') {
            Some(rest) => (true, rest),
            None => (false, token),
        };
        // A closing quote may still be missing while the name is being edited.
        let (quoted, path) = match rest.strip_prefix('"') {
            Some(inner) => (true, inner.strip_suffix('"').unwrap_or(inner)),
            None => (false, rest),
        };
        if path.is_empty() {
            return None;
        }
        Some(Self { mention, quoted, path })
    }

    /// Whether the token names a path inside the workspace.
    fn relative(&self) -> bool {
        !self.path.starts_with('~')
            && Path::new(self.path)
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
    }
}

/// What a filesystem call returned, or nothing when the path is unreachable.
///
/// Completion only offers names it could read: a directory this process may
/// not open, a path that vanished between the token and the press, or a
/// workspace that no longer resolves all end the press with no candidates
/// instead of a guess. That choice is made here once, rather than at each
/// call, so no call can drop a failure by accident.
fn reachable<T>(result: std::io::Result<T>) -> Option<T> {
    let Ok(value) = result else { return None };
    Some(value)
}

/// The workspace as this machine resolves it, or nothing when it is not here.
fn workspace_root(workspace: Option<&str>) -> Option<PathBuf> {
    let root = reachable(std::fs::canonicalize(workspace?))?;
    root.is_dir().then_some(root)
}

/// Formatted candidate paths in `directory` starting with `fragment`, plus the
/// byte offset of the entry name and the shared byte prefix length among names.
fn read_candidates(listing: &Path, directory: &str, fragment: &str) -> Option<(Vec<String>, usize, usize)> {
    let prefix_len = if directory.is_empty() { 0 } else { directory.len() + 1 };
    let mut candidates = Vec::new();
    let mut seen = 0;
    for entry in reachable(std::fs::read_dir(listing))? {
        seen += 1;
        if seen > MAX_ENTRIES {
            return None;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(fragment) || !safe_name(name) {
            continue;
        }
        // The kind is read without following a link: a symlink is offered by
        // its own name and not walked into, which is what keeps the next Tab
        // going through the workspace check again.
        let is_dir = match entry.file_type() {
            Ok(kind) => kind.is_dir(),
            Err(_) => false,
        };
        let mut full = String::with_capacity(prefix_len + name.len() + usize::from(is_dir));
        if !directory.is_empty() {
            full.push_str(directory);
            full.push('/');
        }
        full.push_str(name);
        if is_dir {
            full.push('/');
        }
        candidates.push(full);
    }
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_unstable_by(|a, b| sort_key(&a[prefix_len..]).cmp(&sort_key(&b[prefix_len..])));
    let common_len = if candidates.len() > 1 {
        let first = bare_name(&candidates[0][prefix_len..]);
        let last = bare_name(&candidates[candidates.len() - 1][prefix_len..]);
        common_prefix_len(first, last)
    } else {
        0
    };
    Some((candidates, prefix_len, common_len))
}

fn bare_name(entry: &str) -> &str {
    entry.strip_suffix('/').unwrap_or(entry)
}

fn sort_key(entry: &str) -> (&str, bool) {
    match entry.strip_suffix('/') {
        Some(name) => (name, true),
        None => (entry, false),
    }
}

/// Whether a name can go back into the draft as written.
///
/// A control character would draw as nothing, and a quote or a backslash has no
/// quoting form that every shell reads the same way, so such a name is left out
/// rather than written in a form the user cannot trust.
fn safe_name(name: &str) -> bool {
    if name.is_ascii() {
        !name.bytes().any(|b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\')
    } else {
        !name
            .chars()
            .any(|character| character.is_control() || character == '"' || character == '\\')
    }
}

/// Longest UTF-8 byte prefix shared by two sorted boundary names.
fn common_prefix_len(first: &str, last: &str) -> usize {
    if first.is_ascii() && last.is_ascii() {
        return first.bytes().zip(last.bytes()).take_while(|(a, b)| a == b).count();
    }
    first
        .chars()
        .zip(last.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum()
}

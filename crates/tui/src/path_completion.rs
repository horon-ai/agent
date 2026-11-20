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
    candidates: Vec<String>,
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
            replace(draft, start, &form.render(&previous.candidates[next])),
            Choice {
                start,
                candidates: previous.candidates.clone(),
                chosen: next,
            },
        ));
    }
    let (directory, fragment) = split(form.path);
    let root = workspace_root(workspace)?;
    // Canonicalizing the one directory resolves every symlink in the path it
    // was written with, so the prefix check below is what keeps a link from
    // reading outside the workspace.
    let listing = std::fs::canonicalize(root.join(directory)).ok()?;
    if !listing.starts_with(&root) || !listing.is_dir() {
        return None;
    }
    let names = read_names(&listing, fragment)?;
    let candidates: Vec<String> = names
        .iter()
        .map(|(name, is_dir)| entry(directory, name, *is_dir))
        .collect();
    let mut fill = match candidates.as_slice() {
        [only] => only.clone(),
        [first, ..] => {
            let prefix = common_prefix(&names);
            // A shared prefix no longer than what is already typed says the
            // candidates differ here, so the first one is offered and the next
            // press walks the rest.
            if prefix.len() > fragment.len() {
                entry(directory, &prefix, false)
            } else {
                first.clone()
            }
        }
        [] => return None,
    };
    let mut next = replace(draft, start, &form.render(&fill));
    // The first candidate can be exactly what is typed, as `foo` is beside
    // `foo.rs`: with more candidates the press means the next one rather than
    // nothing, or a typed name would shut the list it starts.
    if next == draft && candidates.len() > 1 {
        fill = candidates[1].clone();
        next = replace(draft, start, &form.render(&fill));
    }
    if next == draft {
        return None;
    }
    let chosen = candidates.iter().position(|candidate| *candidate == fill).unwrap_or(0);
    Some((
        next,
        Choice {
            start,
            candidates,
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

/// The draft with the token that starts at `start` replaced.
fn replace(draft: &str, start: usize, rendered: &str) -> String {
    let mut next = String::with_capacity(draft.len() + rendered.len());
    next.push_str(&draft[..start]);
    next.push_str(rendered);
    next
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

/// One candidate as it is inserted: the directory as typed, the name, and a
/// separator for a directory so the next press descends into it.
fn entry(directory: &str, name: &str, is_dir: bool) -> String {
    let separator = if is_dir { "/" } else { "" };
    if directory.is_empty() {
        format!("{name}{separator}")
    } else {
        format!("{directory}/{name}{separator}")
    }
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

    /// The token as it goes back into the draft.
    ///
    /// A path with a space is quoted so it stays one token, and a token that
    /// was already quoted keeps its quotes. The quotes are the same in every
    /// shell that reads the draft later, which a backslash escape would not
    /// be.
    fn render(&self, path: &str) -> String {
        let mut rendered = String::new();
        if self.mention {
            rendered.push('@');
        }
        if self.quoted || path.chars().any(char::is_whitespace) {
            rendered.push('"');
            rendered.push_str(path);
            rendered.push('"');
        } else {
            rendered.push_str(path);
        }
        rendered
    }
}

/// The workspace as this machine resolves it, or nothing when it is not here.
fn workspace_root(workspace: Option<&str>) -> Option<PathBuf> {
    let root = std::fs::canonicalize(workspace?).ok()?;
    root.is_dir().then_some(root)
}

/// The names in one directory that start with `fragment`, with their kind.
fn read_names(directory: &Path, fragment: &str) -> Option<Vec<(String, bool)>> {
    let mut names = Vec::new();
    let mut seen = 0;
    for entry in std::fs::read_dir(directory).ok()? {
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
        names.push((name.to_string(), is_dir));
    }
    if names.is_empty() {
        return None;
    }
    names.sort();
    Some(names)
}

/// Whether a name can go back into the draft as written.
///
/// A control character would draw as nothing, and a quote or a backslash has no
/// quoting form that every shell reads the same way, so such a name is left out
/// rather than written in a form the user cannot trust.
fn safe_name(name: &str) -> bool {
    for character in name.chars() {
        if character.is_control() || character == '"' || character == '\\' {
            return false;
        }
    }
    true
}

/// The longest prefix every name shares.
fn common_prefix(names: &[(String, bool)]) -> String {
    let mut prefix = String::new();
    for (position, character) in names[0].0.chars().enumerate() {
        if names[1..]
            .iter()
            .any(|(name, _)| name.chars().nth(position) != Some(character))
        {
            break;
        }
        prefix.push(character);
    }
    prefix
}

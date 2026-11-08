//! Expansion of `@path` import lines inside loaded context files.
//!
//! An `AGENTS.md` or `CLAUDE.md` line that consists only of `@<path>` is
//! replaced by the referenced file's contents. A relative path resolves against
//! the file holding the line and a `~/` path resolves under the user home.
//! Recursion stops at [`MAX_IMPORT_HOPS`], a file already on the current chain
//! becomes a one-line marker, and a missing or unreadable file keeps its
//! original line.
//!
//! A path that resolves outside the project directory and outside the user home
//! is refused and left literal, which keeps an instruction file from inlining
//! arbitrary host files. Fenced code blocks are copied verbatim because their
//! contents are examples, not imports.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Deepest import chain, counted from the file being loaded.
///
/// The file passed to [`expand_context_imports`] sits at depth 0, so an import
/// line in a depth-5 file would be the sixth hop and stays literal.
pub const MAX_IMPORT_HOPS: usize = 5;

/// Prefix of the line that replaces an import skipped as a cycle.
pub const CYCLE_MARKER_PREFIX: &str = "<!-- @import skipped (cycle): ";

/// Expand `@<path>` import lines in `content`, read from `source`.
///
/// `source` identifies the file the content came from, so relative imports
/// resolve next to it. `project_dir` is one of the two allowed roots; the other
/// is the user home.
///
/// Content without an `@` returns [`Cow::Borrowed`] without allocating, so the
/// common case costs the same as before imports existed.
pub fn expand_context_imports<'a>(content: &'a str, source: &Path, project_dir: &Path) -> Cow<'a, str> {
    // Two cheap gates before any allocation: most instruction files carry no
    // `@` at all, and an address like `dev@example.com` carries one without
    // being an import, so both keep the original buffer.
    if !content.contains('@') || !has_import_line(content) {
        return Cow::Borrowed(content);
    }

    let roots = AllowedRoots::new(project_dir, source);
    let mut chain = Vec::new();
    if let Some(canonical) = canonical_path(source) {
        chain.push(canonical);
    }
    match expand_text(content, source, &roots, &mut chain, 0) {
        Some(expanded) => Cow::Owned(expanded),
        None => Cow::Borrowed(content),
    }
}

/// Rewrite every import line in `content`.
///
/// Returns `None` when nothing was substituted, which lets the caller keep the
/// original buffer instead of a copy that differs by nothing.
fn expand_text(
    content: &str,
    source: &Path,
    roots: &AllowedRoots,
    chain: &mut Vec<PathBuf>,
    depth: usize,
) -> Option<String> {
    let mut out = String::with_capacity(content.len());
    let mut changed = false;
    let mut fence: Option<Fence> = None;

    for line in content.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);

        if let Some(marker) = fence_start(body) {
            match fence {
                None => fence = Some(marker),
                Some(open) if open.closes(marker) => fence = None,
                Some(_) => {}
            }
            out.push_str(line);
            continue;
        }

        if fence.is_some() {
            out.push_str(line);
            continue;
        }

        let Some(raw) = import_target(body) else {
            out.push_str(line);
            continue;
        };

        if depth >= MAX_IMPORT_HOPS {
            out.push_str(line);
            continue;
        }

        let candidate = match resolve_import(raw, source, roots) {
            Resolved::Path(path) => path,
            Resolved::Missing => {
                crate::logging::warn(&format!(
                    "context import skipped missing file {raw} in {}",
                    source.display()
                ));
                out.push_str(line);
                continue;
            }
            Resolved::Refused => {
                crate::logging::warn(&format!(
                    "context import refused path {raw} outside project and home roots"
                ));
                out.push_str(line);
                continue;
            }
        };

        // A chain visits each file at most once, so a simple two-file cycle
        // reaches this branch exactly once and emits exactly one marker.
        if chain.iter().any(|entry| entry == &candidate) {
            out.push_str(CYCLE_MARKER_PREFIX);
            out.push('@');
            out.push_str(raw);
            out.push_str(" -->");
            if line.ends_with('\n') {
                out.push('\n');
            }
            changed = true;
            continue;
        }

        let text = match std::fs::read_to_string(&candidate) {
            Ok(text) => text,
            Err(error) => {
                crate::logging::warn(&format!(
                    "context import skipped unreadable file {}: {error}",
                    candidate.display()
                ));
                out.push_str(line);
                continue;
            }
        };

        chain.push(candidate.clone());
        let nested = match expand_text(&text, &candidate, roots, chain, depth + 1) {
            Some(expanded) => expanded,
            None => text,
        };
        chain.pop();

        out.push_str(nested.trim_end());
        if line.ends_with('\n') {
            out.push('\n');
        }
        changed = true;
    }

    changed.then_some(out)
}

/// Whether any line could be an import, without building the output.
fn has_import_line(content: &str) -> bool {
    content.lines().any(|line| import_target(line).is_some())
}

/// The target of an import line, or `None` when the line is ordinary text.
fn import_target(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let target = trimmed.strip_prefix('@')?;
    if target.is_empty() || target.starts_with('@') || target.contains(char::is_whitespace) {
        return None;
    }
    Some(target)
}

/// Outcome of resolving one import target.
enum Resolved {
    /// Canonical path inside an allowed root.
    Path(PathBuf),
    /// Nothing exists at the resolved location.
    Missing,
    /// The path resolves outside both allowed roots.
    Refused,
}

/// Resolve an import target to a canonical path inside an allowed root.
fn resolve_import(raw: &str, source: &Path, roots: &AllowedRoots) -> Resolved {
    let joined = if let Some(rest) = raw.strip_prefix("~/") {
        match home_root() {
            Some(home) => home.join(rest),
            None => return Resolved::Missing,
        }
    } else {
        let path = Path::new(raw);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            match source.parent() {
                Some(parent) => parent.join(path),
                None => path.to_path_buf(),
            }
        }
    };

    let Some(canonical) = canonical_path(&joined) else {
        return Resolved::Missing;
    };
    if roots.allows(&canonical) {
        Resolved::Path(canonical)
    } else {
        Resolved::Refused
    }
}

/// The root a `~/` path resolves under.
///
/// This follows [`crate::storage::user_home_path`] so a sandboxed run
/// (`HORON_AGENT_HOME`) never reaches the real home directory.
fn home_root() -> Option<PathBuf> {
    let Ok(path) = crate::storage::user_home_path("") else {
        return None;
    };
    Some(path)
}

fn canonical_path(path: &Path) -> Option<PathBuf> {
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return None;
    };
    Some(canonical)
}

/// The directories an import may read from.
struct AllowedRoots {
    project: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl AllowedRoots {
    fn new(project_dir: &Path, source: &Path) -> Self {
        // A project root that cannot be canonicalized still has a readable
        // context file in it, so fall back to that file's directory rather than
        // refusing every relative import.
        let project = canonical_path(project_dir).or_else(|| source.parent().and_then(canonical_path));
        Self {
            project,
            home: home_root().as_deref().and_then(canonical_path),
        }
    }

    fn allows(&self, candidate: &Path) -> bool {
        [self.project.as_deref(), self.home.as_deref()]
            .into_iter()
            .flatten()
            .any(|root| candidate.starts_with(root))
    }
}

/// An opening code fence and the marker that closes it.
#[derive(Clone, Copy)]
struct Fence {
    marker: char,
    len: usize,
}

impl Fence {
    fn closes(self, other: Self) -> bool {
        self.marker == other.marker && other.len >= self.len
    }
}

fn fence_start(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    (len >= 3).then_some(Fence { marker, len })
}

#[cfg(test)]
#[path = "context_import_tests.rs"]
mod context_import_tests;

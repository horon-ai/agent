//! Extra workspace roots a session may reach beyond its working directory.
//!
//! A root is admitted only after it resolves to an existing directory, so the
//! list never carries a spelling that names nothing on this machine. Resolution
//! also collapses the aliases a person can type for one directory (a symlink,
//! a relative path, a trailing separator), which keeps the stored list a set.
//!
//! The list itself still lives in the session's inert extension map
//! ([`crate::session::Session::additional_dirs`]), and these helpers are the only
//! writers that resolve and deduplicate before storing.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Why a path could not become an extra workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirError {
    /// The argument was empty or whitespace only.
    Empty,
    /// The path does not exist, or it could not be resolved.
    NotFound(String),
    /// The path exists but names something that is not a directory.
    NotADirectory(String),
}

impl fmt::Display for DirError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a directory path is required"),
            Self::NotFound(path) => write!(formatter, "{path}: no such directory"),
            Self::NotADirectory(path) => write!(formatter, "{path}: not a directory"),
        }
    }
}

impl std::error::Error for DirError {}

/// Resolve one extra workspace root.
///
/// Symlinks, `.` and `..` are resolved and the result must be a directory, so
/// the stored value is the canonical path a later comparison can match.
pub fn canonical_root(raw: &str) -> Result<PathBuf, DirError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(DirError::Empty);
    }
    let resolved = std::fs::canonicalize(raw).map_err(|_| DirError::NotFound(raw.to_string()))?;
    if !resolved.is_dir() {
        return Err(DirError::NotADirectory(raw.to_string()));
    }
    Ok(resolved)
}

/// Resolve a launch list of roots, dropping later spellings of one directory.
///
/// The first spelling of a directory keeps its position, so the stored list
/// reads in the order the roots were given.
pub fn canonical_roots(raw: &[String]) -> Result<Vec<String>, DirError> {
    let mut roots: Vec<String> = Vec::with_capacity(raw.len());
    for value in raw {
        let root = canonical_root(value)?.to_string_lossy().into_owned();
        if !roots.iter().any(|existing| existing == &root) {
            roots.push(root);
        }
    }
    Ok(roots)
}

/// Whether two spellings name the same directory.
///
/// Both sides are resolved when they exist, which is what makes a symlink, a
/// relative path and a trailing separator collapse to one entry. A root that
/// has since been deleted still has to be removable, so an unresolvable side
/// falls back to a lexical comparison of the path components.
pub fn same_directory(left: &str, right: &str) -> bool {
    match (std::fs::canonicalize(left.trim()), canonical_root(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => lexical(left) == lexical(right),
    }
}

fn matches_canonical(existing: &str, canonical_right: &Path, right_str: &str) -> bool {
    match std::fs::canonicalize(existing.trim()) {
        Ok(left) => left == canonical_right,
        Err(_) => lexical(existing) == lexical(right_str),
    }
}

/// The path with redundant separators, `.` and resolvable `..` components normalized.
fn lexical(raw: &str) -> PathBuf {
    let mut components: Vec<Component<'_>> = Vec::new();
    for component in Path::new(raw.trim()).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                } else if !matches!(components.last(), Some(Component::RootDir)) {
                    components.push(component);
                }
            }
            other => components.push(other),
        }
    }
    let mut path = PathBuf::new();
    for component in components {
        path.push(component.as_os_str());
    }
    path
}

impl crate::session::Session {
    /// Add one extra workspace root to this session.
    ///
    /// The path is resolved (symlinks and `..` included) and must name an
    /// existing directory. A directory already on the list under any spelling
    /// is not added twice. Answers whether the list changed.
    pub fn add_additional_dir(&mut self, raw: &str) -> Result<bool, DirError> {
        let canonical = canonical_root(raw)?;
        let root = canonical.to_string_lossy().into_owned();
        let mut directories = self.additional_dirs();
        if directories
            .iter()
            .any(|existing| existing == &root || matches_canonical(existing, &canonical, &root))
        {
            return Ok(false);
        }
        directories.push(root);
        self.set_additional_dirs(directories);
        Ok(true)
    }

    /// Remove one extra workspace root from this session's list.
    ///
    /// Only the list changes; the directory itself is never touched. A root
    /// that no longer exists is still removable by the spelling that was stored
    /// or by its canonical path, because removal resolves both sides when it
    /// can and compares path components when it cannot. Answers whether the
    /// list changed.
    pub fn remove_additional_dir(&mut self, raw: &str) -> bool {
        let mut directories = self.additional_dirs();
        if directories.is_empty() {
            return false;
        }
        let trimmed_raw = raw.trim();
        let resolved_raw = if trimmed_raw.is_empty() {
            None
        } else {
            match std::fs::canonicalize(trimmed_raw) {
                Ok(path) if path.is_dir() => Some(path),
                Ok(_) | Err(_) => None,
            }
        };
        let resolved_str = resolved_raw.as_ref().map(|path| path.to_string_lossy());
        let mut lexical_raw: Option<PathBuf> = None;
        let before = directories.len();
        directories.retain(|existing| {
            let matches = match (&resolved_raw, &resolved_str) {
                (Some(target), Some(target_str)) => {
                    existing == target_str.as_ref()
                        || existing.trim() == trimmed_raw
                        || match std::fs::canonicalize(existing.trim()) {
                            Ok(left) => left == *target,
                            Err(_) => {
                                let lex_raw = lexical_raw.get_or_insert_with(|| lexical(trimmed_raw));
                                lexical(existing) == *lex_raw
                            }
                        }
                }
                _ => {
                    existing.trim() == trimmed_raw || {
                        let lex_raw = lexical_raw.get_or_insert_with(|| lexical(trimmed_raw));
                        lexical(existing) == *lex_raw
                    }
                }
            };
            !matches
        });
        if directories.len() == before {
            return false;
        }
        self.set_additional_dirs(directories);
        true
    }
}

#[cfg(test)]
#[path = "workspace_dirs_tests.rs"]
mod tests;

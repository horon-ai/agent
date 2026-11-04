//! Suffix resolution for a `read` path that does not exist.
//!
//! Agents often name a file by a path fragment they remember — `src/x.rs` for
//! `crates/k/src/x.rs` — and a plain "not found" wastes a turn on `find`.
//! Resolving the fragment by suffix is safe only when exactly one file matches:
//! guessing between two candidates would be worse than the error.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Finds the single file under `cwd` whose relative path ends with `authored`.
///
/// The match is component-aligned, so `src/x.rs` matches `crates/k/src/x.rs`
/// but never `mysrc/x.rs`. Hidden files are searched; `.git` is skipped. The
/// walk gives up after `deadline` and returns `None`, as it does when a second
/// candidate appears — an ambiguous fragment must not silently pick one.
pub(crate) fn unique_suffix_match(cwd: &Path, authored: &str, deadline: Duration) -> Option<PathBuf> {
    let needle = normalize(authored);
    if needle.is_empty() || needle == "." {
        return None;
    }
    let suffix = format!("/{needle}");
    let started = Instant::now();
    let mut found: Option<PathBuf> = None;

    let walker = ignore::WalkBuilder::new(cwd)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();

    for entry in walker {
        if started.elapsed() > deadline {
            return None;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(cwd) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative == needle || relative.ends_with(&suffix) {
            if found.is_some() {
                return None;
            }
            found = Some(entry.path().to_path_buf());
        }
    }
    found
}

/// The `relative` rendering of `path` under `cwd`, for the note a caller
/// prepends to the resolved read.
pub(crate) fn relative_display(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn normalize(authored: &str) -> String {
    let text = authored.replace('\\', "/");
    let mut rest = text.as_str();
    while let Some(stripped) = rest.strip_prefix("./") {
        rest = stripped;
    }
    rest.trim_end_matches('/').to_string()
}

#[cfg(test)]
#[path = "path_suffix_tests.rs"]
mod tests;

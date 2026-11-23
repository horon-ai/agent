//! Stable project keys for project-scoped memory.
//!
//! A project's memories live under a key derived from the project directory
//! rather than from the caller's current directory spelling. The key is the
//! SHA-256 hex prefix of the canonical path, so two spellings of one directory
//! (a symlink, `/var` against `/private/var`, a trailing `.`) land on one key,
//! two different directories with the same basename never collide, and moving
//! the whole user volume keeps the keys: the key describes the project path
//! inside the volume, not the volume.
//!
//! The key never leaves the user's memory directory: no marker file is written
//! into the project, and the project itself is only read (`canonicalize`).
//!
//! Earlier versions keyed a project by `DefaultHasher` over the caller's
//! `PathBuf`. That hash is not a stable on-disk contract (its algorithm is
//! unspecified and has changed across Rust releases) and it cannot be inverted,
//! so it survives only as the name of a migration source. [`pre_wiki_project_hash`]
//! reproduces it with the same spelling the old code hashed, which is how an
//! existing `memory/projects/<hash>.json` is found again.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Hex characters kept from the SHA-256 digest: 128 bits, 32 lowercase hex
/// characters. That is short enough to read and long enough that two project
/// paths in one user volume cannot collide by accident.
const KEY_HEX_CHARS: usize = 32;

/// The project key for `project_dir`.
///
/// The directory is canonicalised first, which resolves symlinks and the
/// platform's own aliasing. A path that cannot be canonicalised (a project
/// directory that is not there right now) falls back to its absolute form, so a
/// key exists for every caller; the fallback is documented because it is the
/// one case where two spellings can differ.
pub(crate) fn stable_project_key(project_dir: &Path) -> String {
    let resolved = std::fs::canonicalize(project_dir)
        .or_else(|_| std::path::absolute(project_dir))
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(resolved.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut digest_hex = hex::encode(digest);
    digest_hex.truncate(KEY_HEX_CHARS);
    digest_hex
}

/// The pre-0.2 project key: `DefaultHasher` over the caller's `PathBuf`.
///
/// This is reproduced exactly as the old storage code computed it so an
/// existing `memory/projects/<hash>.json` is still found. It is used only to
/// locate a migration source and is never written as a live key: the hash
/// algorithm is unspecified across Rust releases, so it must not become part of
/// the on-disk contract again.
pub(crate) fn pre_wiki_project_hash(project_dir: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    project_dir.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The directory a migrated source file is filed under when no live project key
/// is known yet: `pre-wiki-<hash>`.
///
/// This is a valid project slug (lowercase letters, digits and `-`), and the
/// `pre-wiki-` prefix keeps it apart from every real key, which is plain hex. It
/// exists so `memory migrate` can move every pre-wiki file into the store without
/// guessing a project path; the first time that project is opened, the pages
/// move to their stable key because the page id is unique.
pub(crate) fn pre_wiki_namespace(project_hash: &str) -> String {
    format!("pre-wiki-{project_hash}")
}

/// The pre-wiki notes file a project hash may still have beside its memory file.
pub(crate) fn pre_wiki_notes_path(root: &Path, project_hash: &str) -> PathBuf {
    root.join("notes").join(format!("{project_hash}.json"))
}

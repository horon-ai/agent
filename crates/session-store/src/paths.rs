//! Store root paths: one validated root per session directory.
//!
//! A root travels with the durability level original writes under it, so every
//! writer, cache and migration opened from the same paths flushes originals the
//! way the operator configured.

use crate::schema::{is_lower_hex_64, validate_session_id_text};
use crate::{SessionId, StoreError, StoreErrorCode};
use horon_agent_platform::SyncMode;
use std::path::{Path, PathBuf};

/// The mode bits a directory carries, for a refusal that names what to change.
///
/// The operator repairs a permissive directory with `chmod`, so the message that
/// refuses it has to state the mode it found. A platform without mode bits, where
/// privacy comes from an inherited ACL, adds nothing.
fn mode_suffix(metadata: &std::fs::Metadata) -> String {
    match horon_agent_platform::fs::mode_bits(metadata) {
        Some(mode) => format!(" (mode {mode:04o})"),
        None => String::new(),
    }
}

pub(crate) fn ensure_safe_component(root: &Path, candidate: &Path) -> Result<(), StoreError> {
    // Every refusal names the path it refused: a reason alone leaves an operator
    // hunting through the store for the one entry that blocks it.
    let refuse = |message: String| StoreError::invalid_request(StoreErrorCode::UnsafePath, message);
    // The root may itself be reached through a symlink (macOS /tmp, a linked HOME).
    // Resolving the root once is what makes the boundary the root itself, while
    // every component below it is still checked for symlinks.
    let canonical_root = root
        .canonicalize()
        .map_err(|err| refuse(format!("store root {} is not usable: {err}", root.display())))?;
    if !canonical_root.is_dir() {
        return Err(refuse(format!(
            "store root {} is not a directory",
            canonical_root.display()
        )));
    }
    let relative = candidate
        .strip_prefix(root)
        .or_else(|_| candidate.strip_prefix(&canonical_root))
        .map_err(|_| {
            refuse(format!(
                "path {} is outside the store root {}",
                candidate.display(),
                root.display()
            ))
        })?
        .to_path_buf();
    if relative.as_os_str().is_empty() {
        return Err(refuse(format!(
            "session path {} is the store root itself",
            candidate.display()
        )));
    }
    let mut current = canonical_root;
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        current.push(component);
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            break;
        };
        if metadata.file_type().is_symlink() {
            return Err(refuse(format!("path component {} is a symlink", current.display())));
        }
        let is_last = index + 1 == component_count;
        if is_last && !metadata.is_dir() {
            return Err(refuse(format!(
                "path {} exists and is not a directory",
                current.display()
            )));
        }
        if !is_last && !metadata.is_dir() {
            return Err(refuse(format!(
                "path component {} is not a directory",
                current.display()
            )));
        }
        // Match Disk::directory even when a clean cache avoids the writer.
        if !horon_agent_platform::fs::is_private_dir(&metadata) {
            return Err(refuse(format!(
                "path component {} is not owner-private{}",
                current.display(),
                mode_suffix(&metadata)
            )));
        }
    }
    Ok(())
}

/// Paths for one store root (HOME/sessions). Every accessor re-validates the
/// session id and refuses a symlinked, non-directory or non-private component under the root,
/// so a valid id can never address a path outside the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPaths {
    root: PathBuf,
    sync: SyncMode,
}

impl SessionPaths {
    /// Wrap a store root with the strongest durability level.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            sync: SyncMode::Full,
        }
    }

    /// The same root, confirming every original write with `mode`.
    ///
    /// The level travels with the paths so every writer, cache and migration
    /// opened from them flushes originals the same way the operator configured.
    pub fn with_sync_mode(mut self, mode: SyncMode) -> Self {
        self.sync = mode;
        self
    }

    /// The durability level original writes under this root use.
    pub fn sync_mode(&self) -> SyncMode {
        self.sync
    }

    /// The configured root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The session directory {root}/{session_id}.
    pub fn session_dir(&self, session_id: &SessionId) -> Result<PathBuf, StoreError> {
        validate_session_id_text(session_id.as_str())?;
        let candidate = self.root.join(session_id.as_str());
        ensure_safe_component(self.root.as_path(), &candidate)?;
        Ok(candidate)
    }

    /// The canonical event log {root}/{session_id}/events.jsonl.
    pub fn events_log(&self, session_id: &SessionId) -> Result<PathBuf, StoreError> {
        Ok(self.session_dir(session_id)?.join("events.jsonl"))
    }

    /// The blob directory {root}/{session_id}/blobs.
    pub fn blobs_dir(&self, session_id: &SessionId) -> Result<PathBuf, StoreError> {
        let dir = self.session_dir(session_id)?.join("blobs");
        ensure_safe_component(self.root.as_path(), &dir)?;
        Ok(dir)
    }

    /// The blob path for a verified lowercase sha256.
    pub fn blob_file(&self, session_id: &SessionId, sha256: &str) -> Result<PathBuf, StoreError> {
        if !is_lower_hex_64(sha256) {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidPayloadRef,
                "blob name must be a lowercase hex sha256",
            ));
        }
        Ok(self.blobs_dir(session_id)?.join(format!("{sha256}.json")))
    }
}

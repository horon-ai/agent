//! Store root paths: one validated root per session directory.
//!
//! A root travels with the durability level original writes under it, so every
//! writer, cache and migration opened from the same paths flushes originals the
//! way the operator configured.

use crate::schema::{ensure_safe_component, is_lower_hex_64, validate_session_id_text};
use crate::{SessionId, StoreError, StoreErrorCode};
use horon_agent_platform::SyncMode;
use std::path::{Path, PathBuf};

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

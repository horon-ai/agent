//! Build, publish, and migration records shared by the update pipeline.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A snapshot of the working tree a binary was built from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceState {
    pub repo_scope: String,
    pub worktree_scope: String,
    pub short_hash: String,
    pub full_hash: String,
    pub dirty: bool,
    pub fingerprint: String,
    pub version_label: String,
    pub changed_paths: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedBuild {
    pub version: String,
    pub source_fingerprint: String,
    pub versioned_path: PathBuf,
    pub current_link: PathBuf,
    pub launcher_link: PathBuf,
    pub previous_current_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DevBinarySourceMetadata {
    pub version_label: String,
    pub source_fingerprint: String,
    pub short_hash: String,
    pub full_hash: String,
    pub dirty: bool,
    pub changed_paths: usize,
}

impl From<&SourceState> for DevBinarySourceMetadata {
    fn from(source: &SourceState) -> Self {
        Self {
            version_label: source.version_label.clone(),
            source_fingerprint: source.fingerprint.clone(),
            short_hash: source.short_hash.clone(),
            full_hash: source.full_hash.clone(),
            dirty: source.dirty,
            changed_paths: source.changed_paths,
        }
    }
}

/// Version identity reported by a built binary.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BinaryVersionReport {
    pub version: Option<String>,
    pub git_hash: Option<String>,
}

/// Information about a specific build version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildInfo {
    /// Git commit hash (short)
    pub hash: String,
    /// Git commit hash (full)
    pub full_hash: String,
    /// Build timestamp
    pub built_at: DateTime<Utc>,
    /// Git commit message (first line)
    pub commit_message: Option<String>,
    /// Whether build is from dirty working tree
    pub dirty: bool,
    /// Stable fingerprint of the source state used to produce the build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fingerprint: Option<String>,
    /// Immutable published version label, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_label: Option<String>,
}

/// Context saved before migrating a session to a different build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationContext {
    pub session_id: String,
    pub from_version: String,
    pub to_version: String,
    pub change_summary: Option<String>,
    pub diff: Option<String>,
    pub timestamp: DateTime<Utc>,
}

//! Typed package-management contract between a management client, a running
//! daemon, the API bridge and the catalog the daemon publishes.
//!
//! The daemon owns every catalog mutation while it is running: one request is
//! applied once, serialized against the inbox pass, and answered only after the
//! resulting generation is on disk and announced to subscribers. The CLI uses
//! the same request when a daemon is reachable and falls back to the store's own
//! file lock when one is not, so both paths publish through the same store.
//!
//! Types here are the wire projection, not the on-disk catalog: source paths,
//! content digests and declared files stay in the daemon's `installed.json`.
//! A client needs identity, kind, version, scope, state and generation.

use serde::{Deserialize, Serialize};

use crate::PackageKind;

/// Version of this request/receipt shape.
pub const PACKAGE_CONTROL_VERSION: u32 = 1;

/// Where a package is enabled, as it travels on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageControlScope {
    /// The user's shared catalog in `$HORON_HOME/packages`.
    Global,
    /// One canonical project root.
    Project { root: String },
}

/// What a management client asks the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageControlOperation {
    /// Install one package directory. Validation and publication are the same
    /// ones `horon package install` performs without a daemon.
    Install {
        /// Canonical or relative directory containing the manifest.
        directory: String,
        scope: PackageControlScope,
        /// Permit replacing the record that already holds this package's ID.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        replace: bool,
    },
    /// Stop loading a package without touching its copy.
    Disable { id: String },
    /// Load a disabled package again.
    Enable { id: String },
    /// Remove a package; its ID stays reserved and its record stays as a
    /// tombstone.
    Remove { id: String },
    /// Install every complete directory under `inbox/`.
    SyncInbox,
    /// Read the published catalog without changing it.
    Status,
}

/// One catalog request, identified by a caller-chosen operation id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageControlRequest {
    /// Stable identity of this operation. A retry that reuses it receives the
    /// recorded result instead of applying the operation a second time.
    pub operation_id: String,
    pub operation: PackageControlOperation,
}

/// What the host may do with a recorded package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageControlState {
    /// The package loads and its executable may be started.
    Installed,
    /// The package stays on disk but does not load.
    Disabled,
    /// The package was removed. Its record stays so the ID cannot be claimed
    /// without an explicit replace, and it names no executable.
    Revoked,
}

/// One catalog record as a management client sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageSummary {
    pub kind: PackageKind,
    pub id: String,
    pub version: String,
    pub name: String,
    pub scope: PackageControlScope,
    pub state: PackageControlState,
    /// Catalog generation this record was last written at.
    pub generation: u64,
}

/// One inbox entry a pass could not install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInboxSkip {
    /// Directory name under `inbox/`.
    pub name: String,
    /// Why it was left in place.
    pub reason: String,
}

/// The authoritative answer to one [`PackageControlRequest`].
///
/// `generation` and `catalog` describe the catalog *now*, after the operation
/// landed: exactly the generation subscribers receive. A replayed receipt keeps
/// the original outcome and the current catalog, so a client retrying after a
/// lost reply learns both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageControlReceipt {
    pub operation_id: String,
    /// The catalog generation this reply is authoritative for.
    pub generation: u64,
    /// True when this reply replays an operation id recorded earlier.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replayed: bool,
    /// IDs whose record this operation wrote or changed, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
    /// Inbox entries an `install`-free or `sync_inbox` pass left in place.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<PackageInboxSkip>,
    /// Complete catalog at `generation`, sorted by ID. Removed records stay
    /// with [`PackageControlState::Revoked`].
    pub catalog: Vec<PackageSummary>,
}

impl PackageControlReceipt {
    /// The record the operation changed, when it changed exactly one.
    pub fn single(&self) -> Option<&PackageSummary> {
        match self.changed.as_slice() {
            [id] => self.catalog.iter().find(|record| &record.id == id),
            _ => None,
        }
    }
}

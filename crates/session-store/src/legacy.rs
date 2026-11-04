//! Offline, non-destructive legacy migration. No process-global HOME is read.
pub use crate::legacy_completion::{IndexCompletion, complete_index_migration, complete_index_migration_with_hook};
pub use crate::legacy_runtime::{LegacyIndexStamp, RuntimeSources, require_index_migration, runtime_sources};
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Explicit legacy application home (the directory containing `sessions`).
#[derive(Debug, Clone)]
pub struct LegacySource {
    pub(crate) home: PathBuf,
    pub(crate) id: SessionId,
    pub(crate) runtime: bool,
}

/// Diagnostics describe preservation/recovery, never silent success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyDiagnostic {
    BackupRecovered,
    JournalDamage { line: usize, salvaged: usize },
    DuplicateMessages { count: usize },
    StaleJournalEntry { line: usize },
    DiagnosticFile { name: String },
    CompactionPreservedInert { reason: String },
}

/// Syscall boundaries on the actual migration backend, not mock storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationPoint {
    SourceCaptured,
    SourceCopied,
    EventsStaged,
    OriginalsSync,
    ManifestSync,
    StageDirectorySync,
    BeforePublish,
    Renamed,
    ParentSync,
    ParentSynced,
    ReceiptSync,
    ReceiptDirectorySync,
    UpgradeAppended,
    UpgradeManifestPublished,
    IndexInventoryPublished,
    IndexHeaderPublish,
    RuntimeSourcesPublished,
}

/// Per-call instrumentation. Returning an error aborts without a success receipt.
pub type MigrationHook = Box<dyn FnMut(MigrationPoint) -> std::io::Result<()> + Send>;

/// Successful publication versus a verified repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    Published,
    AlreadyPublished,
}

/// Local-only receipt; raw byte fingerprints never enter replicated events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReceipt {
    pub version: u32,
    pub session_id: SessionId,
    pub source_files: BTreeMap<String, String>,
    pub source_digest: String,
    pub logical_source_digest: String,
    pub origin_id: OriginId,
    pub events: Vec<(EventId, String)>,
    pub index_imported: bool,
    #[serde(default)]
    pub display_recovery_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_session_ids: Option<Vec<SessionId>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub index_stamps: BTreeMap<String, LegacyIndexStamp>,
    pub diagnostics: Vec<LegacyDiagnostic>,
}

/// A successful operation, including explicit recovery diagnostics.
#[derive(Debug)]
pub struct MigrationReport {
    pub outcome: MigrationOutcome,
    pub receipt: MigrationReceipt,
}

/// Replayed legacy state. Raw unknown values remain data, not runtime authority.
#[derive(Debug)]
pub struct LegacyRead {
    pub metadata: SessionMetadata,
    pub messages: Vec<horon_agent_session_types::StoredMessage>,
    pub auxiliary: serde_json::Value,
    pub diagnostics: Vec<LegacyDiagnostic>,
}

impl LegacySource {
    /// Bind an existing home and safe session identity without opening SQLite.
    ///
    /// # Errors
    /// Refuses symlinked directory components and missing homes.
    pub fn new(home: &Path, id: SessionId) -> Result<Self, StoreError> {
        crate::legacy_bundle::safe_path(home)?;
        if !home.is_dir() {
            return Err(StoreError::unknown_session("legacy home does not exist"));
        }
        Ok(Self {
            home: home.to_path_buf(),
            id,
            runtime: false,
        })
    }

    /// Read legacy JSON without writes. An existing DB requires offline migration.
    ///
    /// # Errors
    /// Refuses live ownership, malformed sources, source races and index metadata loss.
    pub fn read(&self) -> Result<LegacyRead, StoreError> {
        let bundle = crate::legacy_bundle::Bundle::capture(self)?;
        if bundle.has_index() {
            return Err(migration_required());
        }
        let replay = crate::legacy_replay::replay(self, &bundle)?;
        bundle.verify(self)?;
        replay.into_read()
    }

    /// Publish into an explicit event-store root, retaining all original bytes.
    ///
    /// # Errors
    /// Refuses changed sources, live legacy owners, corrupt input and failed durability.
    pub fn migrate(&self, destination: &SessionPaths) -> Result<MigrationReport, StoreError> {
        self.migrate_with_hook(destination, Box::new(|_| Ok(())))
    }

    /// Runtime-only migration of files. An index is never opened, even when the
    /// offline import feature is enabled in this process.
    pub fn migrate_for_runtime(&self, destination: &SessionPaths) -> Result<(), StoreError> {
        self.migrate_for_runtime_with_hook(destination, Box::new(|_| Ok(())))
    }

    /// Instrument the existing initial file-only migration transaction.
    pub fn migrate_for_runtime_with_hook(
        &self,
        destination: &SessionPaths,
        hook: MigrationHook,
    ) -> Result<(), StoreError> {
        require_index_migration(destination)?;
        let has_index = !crate::legacy_runtime::index_stamps(&self.home)?.is_empty();
        if destination.events_log(&self.id)?.try_exists()? {
            match runtime_sources(destination, &self.id, ReadProbe::default()) {
                Ok(_) => return Ok(()),
                Err(error) if error.code == StoreErrorCode::MigrationIncomplete && !has_index => {}
                Err(error) => return Err(error),
            }
        }
        if has_index {
            if !self
                .home
                .join("sessions")
                .join(format!("{}.json", self.id.as_str()))
                .try_exists()?
            {
                return Err(StoreError::unknown_session("session original absent"));
            }
            return Err(migration_required());
        }
        let mut source = self.clone();
        source.runtime = true;
        source.migrate_with_hook(destination, hook)?;
        Ok(())
    }

    /// Same real migration with observable failure/interruption boundaries.
    ///
    /// # Errors
    /// Returns the same errors as `migrate`, including injected I/O errors.
    pub fn migrate_with_hook(
        &self,
        destination: &SessionPaths,
        hook: MigrationHook,
    ) -> Result<MigrationReport, StoreError> {
        crate::legacy_publish::migrate(self, destination, hook)
    }
}

/// Read a completed receipt and validate its original events before runtime wiring.
///
/// # Errors
/// Missing/incomplete receipts, damaged originals and invalid receipt identities fail.
pub fn read_receipt(paths: &SessionPaths, id: &SessionId) -> Result<MigrationReceipt, StoreError> {
    crate::legacy_publish::receipt(paths, id)
}

/// Bounded raw bytes of a preserved legacy original.
#[derive(Debug, Clone)]
pub struct LegacyOriginals {
    pub snapshot: Vec<u8>,
    pub journal: Option<Vec<u8>>,
}

fn bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, StoreError> {
    use std::io::Read;
    crate::legacy_bundle::safe_path(path)?;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let length = file.metadata()?.len();
    if length > limit {
        return Err(corrupt("legacy original exceeds the bounded read limit"));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(corrupt("legacy original exceeds the bounded read limit"));
    }
    Ok(Some(bytes))
}

/// Read the preserved legacy snapshot and its journal tail without touching them.
///
/// Every consumer that still has to read an unmigrated original asks here, so the
/// legacy path rules stay in the module that also replays and migrates it.
///
/// # Errors
/// A missing snapshot is `UnknownSession`; oversized, symlinked or unreadable
/// originals are refused instead of being silently truncated.
pub fn originals(sessions_dir: &Path, id: &SessionId, limit: u64) -> Result<LegacyOriginals, StoreError> {
    let snapshot = sessions_dir.join(format!("{}.json", id.as_str()));
    let snapshot =
        bounded(&snapshot, limit)?.ok_or_else(|| StoreError::unknown_session("preserved legacy snapshot absent"))?;
    let journal = sessions_dir.join(format!("{}.journal.jsonl", id.as_str()));
    let journal = bounded(&journal, limit.saturating_sub(snapshot.len() as u64))?;
    Ok(LegacyOriginals { snapshot, journal })
}

pub(crate) fn corrupt(message: impl Into<String>) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::CorruptLegacy, message)
}
pub(crate) fn conflict() -> StoreError {
    StoreError::invalid_request(StoreErrorCode::SourceConflict, "legacy source bundle changed")
}
pub(crate) fn migration_required() -> StoreError {
    StoreError::invalid_request(
        StoreErrorCode::MigrationRequired,
        "offline migration with legacy-session-index-import is required",
    )
}

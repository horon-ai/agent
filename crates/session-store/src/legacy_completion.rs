//! Offline HOME completion and bounded, history-free runtime authorization.
use crate::blobs::{Disk, checked_file, new_file};
use crate::cache_io::{Stamp, publish};
use crate::legacy::{LegacyIndexStamp, MigrationHook, MigrationPoint, MigrationReceipt, migration_required};
use crate::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

const HEADER: &str = ".legacy-index-completed.json";
const HEADER_MAX: u64 = 4096;
const ROW_MAX: u64 = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexCompletion {
    version: u32,
    source: BTreeMap<String, LegacyIndexStamp>,
    source_hashes: BTreeMap<String, String>,
    pub session_count: u64,
    pub inventory_digest: String,
    inventory_bytes: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    digest: String,
    value: IndexCompletion,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    session_id: SessionId,
    manifest: Stamp,
    completion_hash: String,
}

fn hash_valid(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn bounded(probe: &ReadProbe, path: &Path, limit: u64) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    probe.file(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| migration_required())? > limit {
        return Err(migration_required());
    }
    Ok(bytes)
}

fn private_root(paths: &SessionPaths) -> Result<(), StoreError> {
    crate::legacy_bundle::safe_path(paths.root())?;
    let metadata = fs::symlink_metadata(paths.root())?;
    if !metadata.is_dir() {
        return Err(crate::blobs::unsafe_path(paths.root()));
    }
    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(crate::blobs::unsafe_path(paths.root()));
        }
    }
    Ok(())
}

pub(crate) fn check(paths: &SessionPaths, probe: &ReadProbe) -> Result<(), StoreError> {
    let home = paths.root().parent().ok_or_else(migration_required)?;
    let source = crate::legacy_runtime::index_stamps(home)?;
    if source.is_empty() {
        return Ok(());
    }
    if !paths.root().try_exists()? {
        return Err(migration_required());
    }
    private_root(paths)?;
    let raw = bounded(probe, &paths.root().join(HEADER), HEADER_MAX).map_err(|_| migration_required())?;
    let header: Header = serde_json::from_slice(&raw).map_err(|_| migration_required())?;
    let canonical = serde_json::to_vec(&header.value).map_err(crate::cache_io::json_error)?;
    if !hash_valid(&header.digest)
        || sha256_hex(&canonical) != header.digest
        || header.value.version != 1
        || header.value.source != source
        || !header.value.source_hashes.keys().eq(source.keys())
        || header.value.source_hashes.values().any(|hash| !hash_valid(hash))
        || !hash_valid(&header.value.inventory_digest)
    {
        return Err(migration_required());
    }
    let inventory = paths.root().join(format!(
        ".legacy-index-inventory-{}.jsonl",
        header.value.inventory_digest
    ));
    let file = probe.file(&inventory).map_err(|_| migration_required())?;
    let mut reader = BufReader::with_capacity(1024, file);
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut previous: Option<SessionId> = None;
    for _ in 0..header.value.session_count {
        let mut line = Vec::new();
        (&mut reader).take(ROW_MAX + 1).read_until(b'\n', &mut line)?;
        let length = u64::try_from(line.len()).map_err(|_| migration_required())?;
        if length == 0 || length > ROW_MAX || line.last() != Some(&b'\n') {
            return Err(migration_required());
        }
        bytes = bytes.checked_add(length).ok_or_else(migration_required)?;
        if bytes > header.value.inventory_bytes {
            return Err(migration_required());
        }
        digest.update(&line);
        let row: Row = serde_json::from_slice(&line).map_err(|_| migration_required())?;
        if previous.as_ref().is_some_and(|id| id >= &row.session_id) || !hash_valid(&row.completion_hash) {
            return Err(migration_required());
        }
        let directory = paths.session_dir(&row.session_id)?;
        if Stamp::read(&directory.join("legacy-source-manifest.json"))? != row.manifest
            || bounded(probe, &directory.join("legacy-completed.sha256"), 64)? != row.completion_hash.as_bytes()
        {
            return Err(migration_required());
        }
        let original = fs::symlink_metadata(paths.events_log(&row.session_id)?)?;
        if !original.is_file() {
            return Err(migration_required());
        }
        #[cfg(unix)]
        {
            if original.nlink() != 1 || original.mode() & 0o777 != 0o600 {
                return Err(migration_required());
            }
        }
        previous = Some(row.session_id);
    }
    let mut trailing = [0];
    if reader.read(&mut trailing)? != 0
        || bytes != header.value.inventory_bytes
        || format!("{:x}", digest.finalize()) != header.value.inventory_digest
    {
        return Err(migration_required());
    }
    Ok(())
}

fn index_hashes(receipt: &MigrationReceipt) -> BTreeMap<String, String> {
    receipt
        .source_files
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "session-metadata-v1.sqlite3" | "session-metadata-v1.sqlite3-wal" | "session-metadata-v1.sqlite3-shm"
            )
        })
        .map(|(name, hash)| (name.clone(), hash.clone()))
        .collect()
}

fn publish_complete(
    paths: &SessionPaths,
    ids: &[SessionId],
    source: BTreeMap<String, LegacyIndexStamp>,
    hashes: &BTreeMap<String, String>,
    hook: &mut MigrationHook,
) -> Result<IndexCompletion, StoreError> {
    private_root(paths)?;
    let lock_path = paths.root().join(".legacy-index-completion.lock");
    let lock = match new_file(&lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => checked_file(&lock_path, true)?,
        Err(error) => return Err(error.into()),
    };
    crate::writer::lock_exclusive(&lock)?;
    // Do not replay already-complete histories for each partial row migration.
    for id in ids {
        if !paths.session_dir(id)?.join("legacy-completed.sha256").try_exists()? {
            return Err(migration_required());
        }
    }
    let mut inventory = Vec::new();
    let mut previous: Option<&SessionId> = None;
    for id in ids {
        if previous.is_some_and(|old| old >= id) {
            return Err(migration_required());
        }
        let receipt = crate::legacy::read_receipt(paths, id).map_err(|_| migration_required())?;
        if !receipt.index_imported || receipt.display_recovery_version != 1 || index_hashes(&receipt) != *hashes {
            return Err(migration_required());
        }
        let directory = paths.session_dir(id)?;
        let completion = bounded(&ReadProbe::default(), &directory.join("legacy-completed.sha256"), 64)?;
        let completion_hash = String::from_utf8(completion).map_err(|_| migration_required())?;
        crate::legacy_runtime::publish_runtime_sources(paths, &receipt, &completion_hash)?;
        if crate::legacy_runtime::runtime_sources(paths, id, ReadProbe::default())?.is_none() {
            return Err(migration_required());
        }
        let row = Row {
            session_id: id.clone(),
            manifest: Stamp::read(&directory.join("legacy-source-manifest.json"))?,
            completion_hash,
        };
        let mut line = serde_json::to_vec(&row).map_err(crate::cache_io::json_error)?;
        line.push(b'\n');
        if u64::try_from(line.len()).map_err(|_| migration_required())? > ROW_MAX {
            return Err(migration_required());
        }
        inventory.extend(line);
        previous = Some(id);
    }
    let value = IndexCompletion {
        version: 1,
        source,
        source_hashes: hashes.clone(),
        session_count: u64::try_from(ids.len()).map_err(|_| migration_required())?,
        inventory_digest: sha256_hex(&inventory),
        inventory_bytes: u64::try_from(inventory.len()).map_err(|_| migration_required())?,
    };
    let raw = serde_json::to_vec(&Header {
        digest: sha256_hex(&serde_json::to_vec(&value).map_err(crate::cache_io::json_error)?),
        value: value.clone(),
    })
    .map_err(crate::cache_io::json_error)?;
    if bounded(&ReadProbe::default(), &paths.root().join(HEADER), HEADER_MAX)
        .ok()
        .as_deref()
        == Some(raw.as_slice())
        && check(paths, &ReadProbe::default()).is_ok()
    {
        return Ok(value);
    }
    let mut disk = Disk::new(Box::new(|_| Ok(())));
    publish(
        &mut disk,
        &paths
            .root()
            .join(format!(".legacy-index-inventory-{}.jsonl", value.inventory_digest)),
        &inventory,
        &ReadProbe::default(),
    )?;
    hook(MigrationPoint::IndexInventoryPublished)?;
    hook(MigrationPoint::IndexHeaderPublish)?;
    publish(&mut disk, &paths.root().join(HEADER), &raw, &ReadProbe::default())?;
    crate::blobs::sync_directory(paths.root(), SyncMode::Full)?;
    Ok(value)
}

/// Complete an explicitly offline HOME inventory, including a genuine zero-row DB.
/// # Errors
/// Requires the opt-in index importer, unchanged guarded sources and every nonempty row's receipt.
pub fn complete_index_migration(home: &Path, destination: &SessionPaths) -> Result<IndexCompletion, StoreError> {
    complete_index_migration_with_hook(home, destination, Box::new(|_| Ok(())))
}

/// Same offline operation with real durable-publication interruption boundaries.
/// # Errors
/// Returns the same failures as `complete_index_migration` and injected I/O failures.
pub fn complete_index_migration_with_hook(
    home: &Path,
    destination: &SessionPaths,
    mut hook: MigrationHook,
) -> Result<IndexCompletion, StoreError> {
    if destination.root().parent() != Some(home) {
        return Err(crate::blobs::unsafe_path(destination.root()));
    }
    crate::legacy_bundle::guard_home(home)?;
    let source = crate::legacy_runtime::index_stamps(home)?;
    let bundle = crate::legacy_bundle::capture_index(home)?;
    if !bundle.has_index() {
        return Err(StoreError::unknown_session("legacy index absent"));
    }
    let mut disk = Disk::new(Box::new(|_| Ok(())));
    disk.directory(destination.root())?;
    let stage = destination
        .root()
        .join(format!(".legacy-index-stage-{}", uuid::Uuid::new_v4()));
    disk.directory(&stage)?;
    let ids = crate::legacy_index::inventory(&bundle, &stage)?;
    crate::legacy_bundle::guard_home(home)?;
    if bundle.hashes() != crate::legacy_bundle::capture_index(home)?.hashes()
        || source != crate::legacy_runtime::index_stamps(home)?
    {
        return Err(crate::legacy::conflict());
    }
    let result = publish_complete(destination, &ids, source.clone(), &bundle.hashes(), &mut hook)?;
    if bundle.hashes() != crate::legacy_bundle::capture_index(home)?.hashes()
        || source != crate::legacy_runtime::index_stamps(home)?
    {
        return Err(crate::legacy::conflict());
    }
    Ok(result)
}

pub(crate) fn after_session(paths: &SessionPaths, receipt: &MigrationReceipt) -> Result<(), StoreError> {
    if !receipt.index_imported {
        return Ok(());
    }
    let Some(ids) = &receipt.index_session_ids else {
        return Ok(());
    };
    let home = paths.root().parent().ok_or_else(migration_required)?;
    if let Err(error) = crate::legacy_bundle::guard_home(home) {
        if error.code == StoreErrorCode::Busy {
            return Ok(());
        }
        return Err(error);
    }
    // The inventory belongs to the snapshot captured by this migration. Never
    // combine it with a newer database identity observed after receipt publication.
    let stamps = receipt.index_stamps.clone();
    let hashes = index_hashes(receipt);
    if stamps.is_empty()
        || crate::legacy_runtime::index_stamps(home)? != stamps
        || crate::legacy_bundle::capture_index(home)?.hashes() != hashes
    {
        return Err(crate::legacy::conflict());
    }
    let mut hook: MigrationHook = Box::new(|_| Ok(()));
    let completion = publish_complete(paths, ids, stamps.clone(), &hashes, &mut hook);
    if crate::legacy_runtime::index_stamps(home)? != stamps
        || crate::legacy_bundle::capture_index(home)?.hashes() != hashes
    {
        return Err(crate::legacy::conflict());
    }
    match completion {
        Ok(_) => Ok(()),
        Err(error) if error.code == StoreErrorCode::MigrationRequired => Ok(()),
        Err(error) => Err(error),
    }
}

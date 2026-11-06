//! Runtime authorization from offline receipts, without opening any SQLite file.
use crate::legacy::migration_required;
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::BufReader;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyIndexStamp {
    #[serde(default)]
    pub device: u64,
    #[serde(default)]
    pub inode: u64,
    #[serde(default)]
    pub uid: u32,
    #[serde(default)]
    pub gid: u32,
    pub length: u64,
    pub modified_seconds: u64,
    pub modified_nanos: u32,
}

/// Stat only: changing read permissions does not invalidate a completed import.
pub(crate) fn index_stamps(home: &Path) -> Result<BTreeMap<String, LegacyIndexStamp>, StoreError> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = home;
        return Err(crate::blobs::unsupported());
    }
    let mut result = BTreeMap::new();
    for suffix in ["", "-wal", "-shm"] {
        let name = format!("session-metadata-v1.sqlite3{suffix}");
        let path = home.join(&name);
        crate::legacy_bundle::safe_path(&path)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() {
            return Err(crate::blobs::unsafe_path(&path));
        }
        let modified = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| migration_required())?;
        // A Windows stamp carries only what the platform exposes: the file's
        // length and modification time. The identity fields stay at their
        // defaults, so a completed import stays authorized until the database
        // file itself changes, which is the property this check needs.
        #[cfg(unix)]
        let (device, inode, uid, gid) = (metadata.dev(), metadata.ino(), metadata.uid(), metadata.gid());
        #[cfg(not(unix))]
        let (device, inode, uid, gid) = (0, 0, 0, 0);
        result.insert(
            name,
            LegacyIndexStamp {
                device,
                inode,
                uid,
                gid,
                length: metadata.len(),
                modified_seconds: modified.as_secs(),
                modified_nanos: modified.subsec_nanos(),
            },
        );
    }
    Ok(result)
}

/// Authorize runtime activation only after every indexed row was migrated offline.
///
/// This reads small local manifests and completion markers, never database bytes,
/// preserved database copies, or original transcripts. Event integrity remains the
/// normal store reader's responsibility.
pub fn require_index_migration(paths: &SessionPaths) -> Result<(), StoreError> {
    crate::legacy_completion::check(paths, &ReadProbe::default())
}

/// Runtime source inventory; original event IDs/history are deliberately absent.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSources {
    pub source_files: BTreeMap<String, String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSourceReceipt {
    version: u32,
    session_id: SessionId,
    manifest: crate::cache_io::Stamp,
    completion_hash: String,
    sources: RuntimeSources,
}

pub(crate) fn publish_runtime_sources(
    paths: &SessionPaths,
    receipt: &crate::legacy::MigrationReceipt,
    completion_hash: &str,
) -> Result<(), StoreError> {
    let directory = paths.session_dir(&receipt.session_id)?;
    let manifest = directory.join("legacy-source-manifest.json");
    let value = RuntimeSourceReceipt {
        version: 1,
        session_id: receipt.session_id.clone(),
        manifest: crate::cache_io::Stamp::read(&manifest)?,
        completion_hash: completion_hash.to_owned(),
        sources: RuntimeSources {
            source_files: receipt.source_files.clone(),
        },
    };
    let bytes = crate::cache_io::encode(&value)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(migration_required());
    }
    let target = directory.join("legacy-runtime-sources.json");
    if ReadProbe::default().bounded(&target).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    crate::cache_io::publish(
        &mut crate::blobs::Disk::new(Box::new(|_| Ok(()))),
        &target,
        &bytes,
        &ReadProbe::default(),
    )
}

/// Read a bounded immutable-source inventory, never the historical event manifest.
/// This does not replace the separate HOME/index authorization guard.
/// # Errors
/// Missing/stale local derivatives require explicit offline receipt completion.
pub fn runtime_sources(
    paths: &SessionPaths,
    id: &SessionId,
    probe: ReadProbe,
) -> Result<Option<RuntimeSources>, StoreError> {
    use std::io::Read;
    let directory = paths.session_dir(id)?;
    let manifest = directory.join("legacy-source-manifest.json");
    match fs::symlink_metadata(&manifest) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            for name in [
                "legacy-completed.sha256",
                "legacy-runtime-sources.json",
                "legacy-originals",
                ".legacy-first-file.json",
            ] {
                if directory.join(name).symlink_metadata().is_ok() {
                    return Err(migration_required());
                }
            }
            let home = paths.root().parent().ok_or_else(migration_required)?;
            // Only a canonical log can be admitted here; a legacy-only home has no
            // runtime source decision to make yet.
            if paths.events_log(id)?.try_exists()? && local_source_applicable(home, id)? {
                return Err(migration_required());
            }
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }
    if first_file_incomplete(paths, id, &probe)? {
        return Err(StoreError::invalid_request(
            StoreErrorCode::MigrationIncomplete,
            "initial file-only migration can be resumed with the same source",
        ));
    }
    let bytes = probe
        .bounded(&directory.join("legacy-runtime-sources.json"))
        .map_err(|_| migration_required())?;
    let value: RuntimeSourceReceipt = crate::cache_io::decode(&bytes)?;
    let mut marker = Vec::new();
    probe
        .file(&directory.join("legacy-completed.sha256"))?
        .take(65)
        .read_to_end(&mut marker)?;
    if value.version != 1
        || value.session_id != *id
        || value.manifest != crate::cache_io::Stamp::read(&manifest)?
        || marker.len() != 64
        || marker != value.completion_hash.as_bytes()
        || !value
            .completion_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || value.sources.source_files.is_empty()
        || value.sources.source_files.iter().any(|(path, hash)| {
            let name = Path::new(path);
            let transcript = name.parent() == Some(Path::new("sessions"))
                && name
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&format!("{}.", id.as_str())));
            let index = [
                "session-metadata-v1.sqlite3",
                "session-metadata-v1.sqlite3-wal",
                "session-metadata-v1.sqlite3-shm",
            ]
            .contains(&path.as_str());
            let mutable = path == "sdk-archive.json"
                || ["", "-goals", "-plan", "-gate-observations", "-review-state"]
                    .iter()
                    .any(|suffix| path == &format!("todos/{}{suffix}.json", id.as_str()))
                || [".json", ".lock"]
                    .iter()
                    .any(|suffix| path == &format!("sessions/edit-stats/{}{suffix}", id.as_str()))
                || path == &format!("session_leases/{}.json", id.as_str())
                || ["active_pids", "streaming_pids", "internal_pids"]
                    .iter()
                    .any(|dir| path == &format!("{dir}/{}", id.as_str()));
            !(transcript || index || mutable)
                || Path::new(path).is_absolute()
                || Path::new(path)
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
                || hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(migration_required());
    }
    Ok(Some(value.sources))
}

/// The primary legacy snapshot `sessions/<id>.json` is the only file the offline
/// replay can apply for this id. A missing file, another session's body, or bytes
/// that do not parse as a snapshot cannot be migrated for this id, so they are not
/// applicable local sources and must not mask an already valid canonical log.
fn local_source_applicable(home: &Path, id: &SessionId) -> Result<bool, StoreError> {
    let path = home.join("sessions").join(format!("{}.json", id.as_str()));
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(migration_required()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    // Only the identity is deserialized; other fields are skipped while streaming,
    // so a large historical snapshot is never buffered as a whole.
    match serde_json::from_reader::<_, LocalSourceIdentity>(BufReader::new(fs::File::open(&path)?)) {
        Ok(identity) => Ok(identity.id == id.as_str()),
        Err(_) => Ok(false),
    }
}

#[derive(serde::Deserialize)]
struct LocalSourceIdentity {
    id: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FirstFileTransaction {
    version: u32,
    session_id: SessionId,
    manifest: crate::cache_io::Stamp,
}

pub(crate) fn mark_first_file(paths: &SessionPaths, id: &SessionId) -> Result<(), StoreError> {
    let directory = paths.session_dir(id)?;
    let value = FirstFileTransaction {
        version: 1,
        session_id: id.clone(),
        manifest: crate::cache_io::Stamp::read(&directory.join("legacy-source-manifest.json"))?,
    };
    crate::cache_io::publish(
        &mut crate::blobs::Disk::new(Box::new(|_| Ok(()))),
        &directory.join(".legacy-first-file.json"),
        &crate::cache_io::encode(&value)?,
        &ReadProbe::default(),
    )
}

fn first_file_incomplete(paths: &SessionPaths, id: &SessionId, probe: &ReadProbe) -> Result<bool, StoreError> {
    let directory = paths.session_dir(id)?;
    if directory.join("legacy-completed.sha256").symlink_metadata().is_ok() {
        return Ok(false);
    }
    let pending = directory.join(".legacy-first-file.json");
    match fs::symlink_metadata(&pending) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let value: FirstFileTransaction = crate::cache_io::decode(&probe.bounded(&pending)?)?;
    if value.version != 1
        || value.session_id != *id
        || value.manifest != crate::cache_io::Stamp::read(&directory.join("legacy-source-manifest.json"))?
    {
        return Err(migration_required());
    }
    Ok(true)
}

//! Wire-only whole-record transfer. Never an event payload_ref or rewritten log.
use crate::blob_exchange::{BLOB_CHUNK_BYTES, BlobTransferStatus, SessionBlobChunk};
use crate::blobs::{checked_file, unsafe_path, unsupported};
use crate::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Seek, SeekFrom};

pub use crate::record_reference::CanonicalRecordRef;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordBlobChunk {
    pub reference: CanonicalRecordRef,
    pub chunk: SessionBlobChunk,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordBlobStatus {
    pub reference: CanonicalRecordRef,
    pub status: BlobTransferStatus,
}

fn invalid(message: &str) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::InvalidRecord, message)
}

// Separate role/namespace, while reusing the proven blob engine and the OUTER lock.
pub(crate) fn transfer_paths(paths: &SessionPaths, id: &SessionId) -> Result<SessionPaths, StoreError> {
    // The transfer directory is trusted only when its owner can be verified
    // against the session directory; a platform that cannot prove ownership
    // keeps the original refusal instead of running the exchange unverified.
    if !horon_agent_platform::fs::supports_record_exchange() {
        return Err(unsupported());
    }
    let session = paths.session_dir(id)?;
    let root = session.join(".record-transfer");
    match fs::symlink_metadata(&root) {
        Ok(meta) => {
            if !horon_agent_platform::fs::is_private_dir_owned_by(&meta, &fs::metadata(&session)?) {
                return Err(unsafe_path(&root));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(SessionPaths::new(root))
}

pub(crate) fn hydrate(
    paths: &SessionPaths,
    id: &SessionId,
    r: &CanonicalRecordRef,
) -> Result<CanonicalEventRecord, StoreError> {
    r.validate()?;
    let transfer = transfer_paths(paths, id)?;
    if !transfer.root().try_exists()? {
        return Err(StoreError::invalid_request(
            StoreErrorCode::MissingDependency,
            "whole-record blob not published",
        ));
    }
    let path = transfer.blob_file(id, &r.sha256)?;
    let mut file = match checked_file(&path, false) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::invalid_request(
                StoreErrorCode::MissingDependency,
                "whole-record blob not published",
            ));
        }
        Err(e) => return Err(e.into()),
    };
    crate::blob_staging::verify(&mut file, &r.sha256, r.size)?;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(r.size + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| invalid("record length"))? != r.size || sha256_hex(&bytes) != r.sha256 {
        return Err(StoreError::invalid_request(
            StoreErrorCode::PayloadDigestMismatch,
            "record changed while reading",
        ));
    }
    let record = CanonicalEventRecord {
        event_id: r.event_id.clone(),
        content_hash: r.content_hash.clone(),
        canonical_json: String::from_utf8(bytes).map_err(|_| invalid("record is not UTF-8"))?,
    };
    record.validate()?;
    Ok(record)
}

/// Derive exact record bytes from the original log; this export creates no files.
pub fn export_record_blob(
    paths: &SessionPaths,
    id: &SessionId,
    r: &CanonicalRecordRef,
    offset: u64,
) -> Result<RecordBlobChunk, StoreError> {
    r.validate()?;
    if offset >= r.size || !offset.is_multiple_of(BLOB_CHUNK_BYTES) {
        return Err(StoreError::invalid_request(
            StoreErrorCode::MissingOffset,
            "invalid record chunk boundary",
        ));
    }
    let graph = EventGraph::open(paths, id)?;
    let event = graph
        .event(&r.event_id)
        .ok_or_else(|| StoreError::invalid_request(StoreErrorCode::MissingDependency, "record source absent"))?;
    let record = event.canonical_record()?;
    if CanonicalRecordRef::from_record(&record, r.position)? != *r {
        return Err(StoreError::invalid_request(
            StoreErrorCode::PayloadDigestMismatch,
            "record descriptor differs from source",
        ));
    }
    let end = offset + (r.size - offset).min(BLOB_CHUNK_BYTES);
    let start = usize::try_from(offset).map_err(|_| invalid("record offset"))?;
    let stop = usize::try_from(end).map_err(|_| invalid("record offset"))?;
    Ok(RecordBlobChunk {
        reference: r.clone(),
        chunk: SessionBlobChunk {
            sha256: r.sha256.clone(),
            offset,
            data_base64: crate::replication_codec::encode(&record.canonical_json.as_bytes()[start..stop], false),
            next_offset: end,
            total_size: r.size,
            complete: end == r.size,
        },
    })
}

/// Read one verified whole-record blob, for an offline export.
///
/// # Errors
/// Refuses an unpublished or changed record blob.
pub fn read_record_blob(paths: &SessionPaths, id: &SessionId, r: &CanonicalRecordRef) -> Result<Vec<u8>, StoreError> {
    Ok(hydrate(paths, id, r)?.canonical_json.into_bytes())
}

/// Publish one whole canonical record through the session's exclusive guard.
///
/// # Errors
/// Refuses a length or hash that does not match the descriptor, or a busy session.
pub fn write_record_blob(
    paths: &SessionPaths,
    id: &SessionId,
    r: &CanonicalRecordRef,
    bytes: &[u8],
) -> Result<RecordBlobStatus, StoreError> {
    r.validate()?;
    if u64::try_from(bytes.len()).map_err(|_| invalid("record length overflow"))? != r.size
        || sha256_hex(bytes) != r.sha256
    {
        return Err(StoreError::invalid_request(
            StoreErrorCode::PayloadDigestMismatch,
            "record bytes differ from their descriptor",
        ));
    }
    let mut writer =
        BlockingWriter::lock_for_exchange(paths.clone(), id.clone(), (Box::new(|_| Ok(())), ReadProbe::default()))?;
    let mut offset = 0u64;
    let mut status = writer.record_blob_status(r)?;
    while offset < r.size {
        let end = offset + (r.size - offset).min(BLOB_CHUNK_BYTES);
        let start = usize::try_from(offset).map_err(|_| invalid("record offset"))?;
        let stop = usize::try_from(end).map_err(|_| invalid("record offset"))?;
        status = writer.import_record_blob(&RecordBlobChunk {
            reference: r.clone(),
            chunk: SessionBlobChunk {
                sha256: r.sha256.clone(),
                offset,
                data_base64: crate::replication_codec::encode(&bytes[start..stop], false),
                next_offset: end,
                total_size: r.size,
                complete: end == r.size,
            },
        })?;
        offset = end;
    }
    Ok(status)
}

pub fn import_record_blob(
    paths: &SessionPaths,
    id: &SessionId,
    chunk: &RecordBlobChunk,
) -> Result<RecordBlobStatus, StoreError> {
    let mut writer =
        BlockingWriter::lock_for_exchange(paths.clone(), id.clone(), (Box::new(|_| Ok(())), ReadProbe::default()))?;
    writer.import_record_blob(chunk)
}

pub fn record_blob_status(
    paths: &SessionPaths,
    id: &SessionId,
    r: &CanonicalRecordRef,
) -> Result<RecordBlobStatus, StoreError> {
    let mut writer =
        BlockingWriter::lock_for_exchange(paths.clone(), id.clone(), (Box::new(|_| Ok(())), ReadProbe::default()))?;
    writer.record_blob_status(r)
}

impl BlockingWriter {
    pub fn import_record_blob(&mut self, chunk: &RecordBlobChunk) -> Result<RecordBlobStatus, StoreError> {
        self.check_lock()?;
        chunk.reference.validate()?;
        if chunk.reference.sha256 != chunk.chunk.sha256 || chunk.reference.size != chunk.chunk.total_size {
            return Err(invalid("record chunk descriptor mismatch"));
        }
        let paths = transfer_paths(&self.paths, &self.id)?;
        self.disk.directory(paths.root())?;
        self.disk.directory(&paths.session_dir(&self.id)?)?;
        let status = crate::blob_exchange::import_blob_at(&paths, &self.id, &chunk.chunk, &mut self.disk)?;
        Ok(RecordBlobStatus {
            reference: chunk.reference.clone(),
            status,
        })
    }

    pub fn record_blob_status(&mut self, r: &CanonicalRecordRef) -> Result<RecordBlobStatus, StoreError> {
        self.check_lock()?;
        r.validate()?;
        let paths = transfer_paths(&self.paths, &self.id)?;
        if !paths.root().try_exists()? {
            return Ok(RecordBlobStatus {
                reference: r.clone(),
                status: BlobTransferStatus {
                    sha256: r.sha256.clone(),
                    received: 0,
                    total_size: r.size,
                    complete: false,
                },
            });
        }
        let status = crate::blob_exchange::blob_status_at(&paths, &self.id, &r.payload_transport(), &mut self.disk)?;
        Ok(RecordBlobStatus {
            reference: r.clone(),
            status,
        })
    }
}

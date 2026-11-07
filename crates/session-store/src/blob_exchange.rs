//! One-MiB contiguous original-blob exchange with resumable durable staging.
use crate::blob_staging as stage;
use crate::blobs::{Disk, checked_file};
use crate::replication_codec::{decode, encode};
use crate::*;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};

pub const BLOB_CHUNK_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBlobChunk {
    pub sha256: String,
    pub offset: u64,
    /// RFC 4648 standard base64, with padding.
    pub data_base64: String,
    pub next_offset: u64,
    pub total_size: u64,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobTransferStatus {
    pub sha256: String,
    pub received: u64,
    pub total_size: u64,
    pub complete: bool,
}

fn reference(paths: &SessionPaths, id: &SessionId, r: &PayloadRef) -> Result<(), StoreError> {
    paths.blob_file(id, &r.sha256)?;
    if r.encoding != PayloadEncoding::CanonicalJson || r.size == 0 {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidPayloadRef,
            "invalid original blob reference",
        ));
    }
    Ok(())
}

/// Read one original chunk. The complete source's hash and size are verified first.
pub fn export_blob(
    paths: &SessionPaths,
    id: &SessionId,
    r: &PayloadRef,
    offset: u64,
) -> Result<SessionBlobChunk, StoreError> {
    reference(paths, id, r)?;
    if offset >= r.size || !offset.is_multiple_of(BLOB_CHUNK_BYTES) {
        return Err(StoreError::invalid_request(
            StoreErrorCode::MissingOffset,
            "invalid chunk boundary",
        ));
    }
    let path = paths.blob_file(id, &r.sha256)?;
    let mut file = checked_file(&path, false)?;
    stage::verify(&mut file, &r.sha256, r.size)?;
    file.seek(SeekFrom::Start(offset))?;
    let size = (r.size - offset).min(BLOB_CHUNK_BYTES);
    let mut bytes = vec![0; usize::try_from(size).map_err(|_| stage::conflict("chunk length overflow"))?];
    file.read_exact(&mut bytes)?;
    Ok(SessionBlobChunk {
        sha256: r.sha256.clone(),
        offset,
        data_base64: encode(&bytes, false),
        next_offset: offset + size,
        total_size: r.size,
        complete: offset + size == r.size,
    })
}

/// Read one verified original blob in full, for an offline export.
///
/// # Errors
/// Refuses an invalid reference, a missing blob, or bytes that do not match it.
pub fn read_blob(paths: &SessionPaths, id: &SessionId, r: &PayloadRef) -> Result<Vec<u8>, StoreError> {
    reference(paths, id, r)?;
    let path = paths.blob_file(id, &r.sha256)?;
    let mut file = checked_file(&path, false)?;
    stage::verify(&mut file, &r.sha256, r.size)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| stage::conflict("blob length overflow"))? != r.size {
        return Err(stage::conflict("blob length changed"));
    }
    Ok(bytes)
}

/// Publish one whole original blob through the session's exclusive guard.
///
/// # Errors
/// Refuses a length or hash that does not match the reference, or a busy session.
pub fn write_blob(
    paths: &SessionPaths,
    id: &SessionId,
    r: &PayloadRef,
    bytes: &[u8],
) -> Result<BlobTransferStatus, StoreError> {
    reference(paths, id, r)?;
    if u64::try_from(bytes.len()).map_err(|_| stage::conflict("blob length overflow"))? != r.size {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidPayloadRef,
            "blob length differs from its reference",
        ));
    }
    let mut writer =
        BlockingWriter::lock_for_exchange(paths.clone(), id.clone(), (Box::new(|_| Ok(())), ReadProbe::default()))?;
    let mut offset = 0u64;
    let mut status = writer.blob_status(r)?;
    while offset < r.size {
        let end = (offset + BLOB_CHUNK_BYTES).min(r.size);
        let slice = &bytes[usize::try_from(offset).map_err(|_| stage::conflict("blob offset"))?
            ..usize::try_from(end).map_err(|_| stage::conflict("blob offset"))?];
        status = writer.import_blob(&SessionBlobChunk {
            sha256: r.sha256.clone(),
            offset,
            data_base64: encode(slice, false),
            next_offset: end,
            total_size: r.size,
            complete: end == r.size,
        })?;
        offset = end;
    }
    Ok(status)
}

pub fn import_blob(
    paths: &SessionPaths,
    id: &SessionId,
    chunk: &SessionBlobChunk,
) -> Result<BlobTransferStatus, StoreError> {
    let mut writer =
        BlockingWriter::lock_for_exchange(paths.clone(), id.clone(), (Box::new(|_| Ok(())), ReadProbe::default()))?;
    writer.import_blob(chunk)
}

/// Read resumable status under the same exclusive guard. A complete original is
/// synchronized before reporting completion, including after a rename interruption.
pub fn blob_status(paths: &SessionPaths, id: &SessionId, r: &PayloadRef) -> Result<BlobTransferStatus, StoreError> {
    let mut writer =
        BlockingWriter::lock_for_exchange(paths.clone(), id.clone(), (Box::new(|_| Ok(())), ReadProbe::default()))?;
    writer.blob_status(r)
}

fn published(paths: &SessionPaths, id: &SessionId, r: &PayloadRef, disk: &mut Disk) -> Result<bool, StoreError> {
    let path = paths.blob_file(id, &r.sha256)?;
    match checked_file(&path, false) {
        Ok(mut file) => {
            stage::verify(&mut file, &r.sha256, r.size)?;
            disk.sync_file(&file, IoPoint::BlobFileSync)?;
            disk.sync_dir(&paths.blobs_dir(id)?, IoPoint::BlobDirectorySync)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

impl BlockingWriter {
    /// Stage a payload chunk through the session's existing exclusive guard.
    pub fn import_blob(&mut self, chunk: &SessionBlobChunk) -> Result<BlobTransferStatus, StoreError> {
        self.check_lock()?;
        import_blob_at(&self.paths, &self.id, chunk, &mut self.disk)
    }
    pub fn blob_status(&mut self, r: &PayloadRef) -> Result<BlobTransferStatus, StoreError> {
        self.check_lock()?;
        blob_status_at(&self.paths, &self.id, r, &mut self.disk)
    }
}

pub(crate) fn import_blob_at(
    paths: &SessionPaths,
    id: &SessionId,
    chunk: &SessionBlobChunk,
    disk: &mut Disk,
) -> Result<BlobTransferStatus, StoreError> {
    let r = PayloadRef {
        sha256: chunk.sha256.clone(),
        size: chunk.total_size,
        encoding: PayloadEncoding::CanonicalJson,
    };
    reference(paths, id, &r)?;
    let bytes = decode(&chunk.data_base64, false, 1024 * 1024)?;
    let len = u64::try_from(bytes.len()).map_err(|_| stage::conflict("chunk length overflow"))?;
    if chunk.offset >= r.size
        || !chunk.offset.is_multiple_of(BLOB_CHUNK_BYTES)
        || len != (r.size - chunk.offset).min(BLOB_CHUNK_BYTES)
        || chunk.next_offset != chunk.offset + len
        || chunk.complete != (chunk.next_offset == r.size)
    {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidRecord,
            "invalid blob chunk bounds",
        ));
    }
    let directory = stage::stage_path(paths, id, &r.sha256)?;
    if published(paths, id, &r, disk)? {
        let mut file = checked_file(&paths.blob_file(id, &r.sha256)?, false)?;
        file.seek(SeekFrom::Start(chunk.offset))?;
        let mut previous = vec![0; bytes.len()];
        file.read_exact(&mut previous)?;
        if previous != bytes {
            return Err(stage::conflict("different bytes at published offset"));
        }
        stage::cleanup(disk, &directory)?;
    } else {
        let previous = match stage::manifest(&directory)? {
            Some(size) if size != r.size => {
                return Err(stage::conflict("blob total size changed"));
            }
            Some(_) => stage::received(&directory, r.size)?,
            None => 0,
        };
        if chunk.offset > previous {
            return Err(StoreError::invalid_request(
                StoreErrorCode::MissingOffset,
                "missing contiguous blob prefix",
            ));
        }
        stage::prepare(disk, &directory, r.size)?;
        stage::put(disk, &directory, chunk.offset, &bytes)?;
        if stage::received(&directory, r.size)? == r.size {
            let path = paths.blob_file(id, &r.sha256)?;
            stage::publish(disk, &directory, &path, &r.sha256, r.size)?;
            stage::cleanup(disk, &directory)?;
        }
    }
    blob_status_at(paths, id, &r, disk)
}

pub(crate) fn blob_status_at(
    paths: &SessionPaths,
    id: &SessionId,
    r: &PayloadRef,
    disk: &mut Disk,
) -> Result<BlobTransferStatus, StoreError> {
    reference(paths, id, r)?;
    let directory = stage::stage_path(paths, id, &r.sha256)?;
    let complete = published(paths, id, r, disk)?;
    let received = if complete {
        r.size
    } else {
        match stage::manifest(&directory)? {
            Some(size) if size != r.size => {
                return Err(stage::conflict("blob total size changed"));
            }
            Some(_) => {
                disk.sync_dir(&directory, IoPoint::BlobDirectorySync)?;
                stage::received(&directory, r.size)?
            }
            None => 0,
        }
    };
    Ok(BlobTransferStatus {
        sha256: r.sha256.clone(),
        received,
        total_size: r.size,
        complete,
    })
}

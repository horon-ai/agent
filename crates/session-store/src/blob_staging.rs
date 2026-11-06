//! Durable per-offset chunk files. Only renamed, synced chunks advance status.
use crate::blob_exchange::BLOB_CHUNK_BYTES;
use crate::blobs::{Disk, checked_file, new_file, read_file, unsafe_path};
use crate::*;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(crate) fn stage_path(paths: &SessionPaths, id: &SessionId, hash: &str) -> Result<PathBuf, StoreError> {
    paths.blob_file(id, hash)?;
    let parent = paths.session_dir(id)?.join(".blob-staging");
    let stage = parent.join(hash);
    for path in [&parent, &stage] {
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_dir() => return Err(unsafe_path(path)),
            Ok(meta) => {
                #[cfg(unix)]
                if meta.permissions().mode() & 0o777 != 0o700
                    || meta.uid() != fs::metadata(paths.session_dir(id)?)?.uid()
                {
                    return Err(unsafe_path(path));
                }
                #[cfg(not(unix))]
                {
                    let _ = &meta;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(stage)
}

pub(crate) fn conflict(message: &str) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::Conflict, message)
}

pub(crate) fn temporary(path: &Path) -> Result<File, StoreError> {
    let file = match new_file(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => checked_file(path, true)?,
        Err(e) => return Err(e.into()),
    };
    file.set_len(0)?;
    Ok(file)
}

pub(crate) fn manifest(stage: &Path) -> Result<Option<u64>, StoreError> {
    let path = stage.join("size");
    let mut file = match checked_file(&path, false) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut text = String::new();
    (&mut file).take(32).read_to_string(&mut text)?;
    let size = text.parse().map_err(|_| conflict("invalid staging size"))?;
    Ok(Some(size))
}

pub(crate) fn prepare(disk: &mut Disk, stage: &Path, total: u64) -> Result<(), StoreError> {
    match manifest(stage)? {
        Some(size) if size != total => return Err(conflict("blob total size changed")),
        Some(_) => return Ok(()),
        None => {}
    }
    disk.directory(stage)?;
    let temp = stage.join(".size.tmp");
    let mut file = temporary(&temp)?;
    file.write_all(total.to_string().as_bytes())?;
    disk.sync_file(&file, IoPoint::BlobFileSync)?;
    fs::rename(temp, stage.join("size"))?;
    disk.sync_dir(stage, IoPoint::BlobDirectorySync)
}

pub(crate) fn received(stage: &Path, total: u64) -> Result<u64, StoreError> {
    let mut offset = 0;
    while offset < total {
        let path = stage.join(format!("{offset}.chunk"));
        let file = match checked_file(&path, false) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(offset),
            Err(e) => return Err(e.into()),
        };
        let len = (total - offset).min(BLOB_CHUNK_BYTES);
        if file.metadata()?.len() != len {
            return Err(conflict("invalid staged chunk length"));
        }
        offset += len;
    }
    Ok(offset)
}

pub(crate) fn put(disk: &mut Disk, stage: &Path, offset: u64, bytes: &[u8]) -> Result<(), StoreError> {
    let path = stage.join(format!("{offset}.chunk"));
    match checked_file(&path, false) {
        Ok(file) => {
            if read_file(&path)? != bytes {
                return Err(conflict("different bytes at existing offset"));
            }
            disk.sync_file(&file, IoPoint::BlobFileSync)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let temp = stage.join(format!(".{offset}.tmp"));
            let mut file = temporary(&temp)?;
            disk.hit(IoPoint::BlobWrite)?;
            file.write_all(bytes)?;
            disk.sync_file(&file, IoPoint::BlobFileSync)?;
            if read_file(&temp)? != bytes {
                return Err(conflict("staged chunk verification failed"));
            }
            fs::rename(temp, path)?;
            disk.hit(IoPoint::BlobRenamed)?;
        }
        Err(e) => return Err(e.into()),
    }
    disk.sync_dir(stage, IoPoint::BlobDirectorySync)
}

pub(crate) fn verify(file: &mut File, hash: &str, size: u64) -> Result<(), StoreError> {
    use std::io::{Seek, SeekFrom};
    if file.metadata()?.len() != size {
        return Err(StoreError::invalid_request(
            StoreErrorCode::PayloadDigestMismatch,
            "blob size mismatch",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    if format!("{:x}", digest.finalize()) != hash {
        return Err(StoreError::invalid_request(
            StoreErrorCode::PayloadDigestMismatch,
            "blob hash mismatch",
        ));
    }
    Ok(())
}

pub(crate) fn publish(
    disk: &mut Disk,
    stage: &Path,
    destination: &Path,
    hash: &str,
    total: u64,
) -> Result<(), StoreError> {
    let parent = destination.parent().ok_or_else(|| unsafe_path(destination))?;
    disk.directory(parent)?;
    let temp = stage.join(".complete.tmp");
    let mut file = temporary(&temp)?;
    let mut offset = 0;
    while offset < total {
        let bytes = read_file(&stage.join(format!("{offset}.chunk")))?;
        file.write_all(&bytes)?;
        offset += u64::try_from(bytes.len()).map_err(|_| conflict("chunk length overflow"))?;
    }
    disk.sync_file(&file, IoPoint::BlobFileSync)?;
    verify(&mut file, hash, total)?;
    disk.hit(IoPoint::BlobVerified)?;
    // Destination is only replaced after a no-follow existence check, under the
    // session writer lock. Caller handles an already-published original separately.
    match fs::symlink_metadata(destination) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
        Ok(_) => return Err(conflict("blob destination appeared during publication")),
    }
    fs::rename(temp, destination)?;
    disk.hit(IoPoint::BlobRenamed)?;
    disk.sync_dir(parent, IoPoint::BlobDirectorySync)?;
    disk.sync_dir(stage, IoPoint::BlobDirectorySync)?;
    disk.hit(IoPoint::BlobPublished)
}

pub(crate) fn cleanup(disk: &mut Disk, stage: &Path) -> Result<(), StoreError> {
    if !stage.exists() {
        return Ok(());
    }
    // The stage contains only this transfer's regular private files. Refuse any
    // unknown entry or symlink rather than recursively removing arbitrary paths.
    for entry in fs::read_dir(stage)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| unsafe_path(&entry.path()))?;
        let numeric = name
            .strip_suffix(".chunk")
            .or_else(|| name.strip_prefix('.').and_then(|s| s.strip_suffix(".tmp")));
        if name != "size"
            && name != ".size.tmp"
            && name != ".complete.tmp"
            && !numeric.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(unsafe_path(&entry.path()));
        }
        checked_file(&entry.path(), false)?;
    }
    for entry in fs::read_dir(stage)? {
        fs::remove_file(entry?.path())?;
    }
    fs::remove_dir(stage)?;
    disk.sync_dir(
        stage.parent().ok_or_else(|| unsafe_path(stage))?,
        IoPoint::BlobDirectorySync,
    )
}

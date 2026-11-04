//! Private cache publication and per-reader observations of actual reads.
use crate::blobs::{Disk, checked_file, new_file};
use crate::{IoPoint, StoreError, sha256_hex};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
pub struct ReadProbe {
    reads: Arc<Mutex<Vec<ReadReceipt>>>,
    writes: Arc<Mutex<Vec<ReadReceipt>>>,
    publications: Arc<Mutex<Vec<ReadReceipt>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadReceipt {
    pub path: std::path::PathBuf,
    pub offset: u64,
    pub bytes: usize,
}

impl ReadProbe {
    pub fn receipts(&self) -> Result<Vec<ReadReceipt>, StoreError> {
        self.reads
            .lock()
            .map(|v| v.clone())
            .map_err(|_| crate::recovery::corrupt("read observation lock poisoned"))
    }
    pub fn writes(&self) -> Result<Vec<ReadReceipt>, StoreError> {
        self.writes
            .lock()
            .map(|v| v.clone())
            .map_err(|_| crate::recovery::corrupt("write observation lock poisoned"))
    }

    /// Whole-file derived publications: cache, index, summary and catalog rows.
    ///
    /// Separate from [`Self::writes`] on purpose. `writes` is the original log's
    /// append stream, where every offset is the true file position; a derived
    /// publication replaces its file, so its bytes are reported here instead of
    /// being mixed into that append stream.
    pub fn publications(&self) -> Result<Vec<ReadReceipt>, StoreError> {
        self.publications
            .lock()
            .map(|v| v.clone())
            .map_err(|_| crate::recovery::corrupt("publication observation lock poisoned"))
    }
    pub(crate) fn append(&self, file: &mut File, data: (&Path, &[u8])) -> Result<(), StoreError> {
        let (path, mut bytes) = data;
        while !bytes.is_empty() {
            let count = file.write(bytes)?;
            if count == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
            }
            let end = file.stream_position()?;
            let written = u64::try_from(count).map_err(|_| crate::recovery::corrupt("write length"))?;
            self.record_write(path, end.saturating_sub(written), count)?;
            bytes = &bytes[count..];
        }
        Ok(())
    }

    /// Records bytes that a whole-file publication wrote at `offset`.
    ///
    /// Cache and catalog files are published by replacing the previous file, so
    /// their write cost is the published length, not a size delta. Callers report
    /// the write only after the publication succeeded.
    pub(crate) fn note_write(&self, path: &Path, offset: u64, bytes: usize) -> Result<(), StoreError> {
        horon_agent_perf_counters::record_io(0, 0, bytes as u64);
        self.publications
            .lock()
            .map_err(|_| crate::recovery::corrupt("publication observation lock poisoned"))?
            .push(ReadReceipt {
                path: path.to_path_buf(),
                offset,
                bytes,
            });
        Ok(())
    }

    fn record_write(&self, path: &Path, offset: u64, bytes: usize) -> Result<(), StoreError> {
        horon_agent_perf_counters::record_io(0, bytes as u64, 0);
        self.writes
            .lock()
            .map_err(|_| crate::recovery::corrupt("write observation lock poisoned"))?
            .push(ReadReceipt {
                path: path.to_path_buf(),
                offset,
                bytes,
            });
        Ok(())
    }
    pub(crate) fn file(&self, path: &Path) -> Result<ObservedFile, StoreError> {
        Ok(ObservedFile {
            file: checked_file(path, false)?,
            path: path.to_path_buf(),
            probe: self.clone(),
            offset: 0,
        })
    }
    pub(crate) fn read(&self, path: &Path) -> Result<Vec<u8>, StoreError> {
        let mut bytes = Vec::new();
        self.file(path)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    pub(crate) fn bounded(&self, path: &Path) -> Result<Vec<u8>, StoreError> {
        let limit =
            u64::try_from(crate::MAX_RECORD_BYTES).map_err(|_| crate::recovery::corrupt("cache size overflow"))?;
        let mut bytes = Vec::new();
        self.file(path)?.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() > crate::MAX_RECORD_BYTES {
            return Err(StoreError::invalid_request(
                crate::StoreErrorCode::RecordTooLarge,
                "derived record exceeds bound",
            ));
        }
        Ok(bytes)
    }
}

pub(crate) struct ObservedFile {
    file: File,
    path: std::path::PathBuf,
    probe: ReadProbe,
    offset: u64,
}
impl Read for ObservedFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.file.read(buffer)?;
        horon_agent_perf_counters::record_io(count as u64, 0, 0);
        horon_agent_perf_counters::record_store_bytes_read(count as u64);
        self.probe
            .reads
            .lock()
            .map_err(|_| std::io::Error::other("probe poisoned"))?
            .push(ReadReceipt {
                path: self.path.clone(),
                offset: self.offset,
                bytes: count,
            });
        self.offset = self.offset.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        Ok(count)
    }
}
impl Seek for ObservedFile {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.offset = self.file.seek(position)?;
        Ok(self.offset)
    }
}

/// Host-local identity/epoch. A same-size rewrite changes mtime or ctime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stamp {
    device: u64,
    inode: u64,
    pub len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    pub fn optional(path: &Path) -> Result<Option<Self>, StoreError> {
        match fs::symlink_metadata(path) {
            Ok(_) => Self::read(path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn read(path: &Path) -> Result<Self, StoreError> {
        let m = checked_file(path, false)?.metadata()?;
        #[cfg(not(unix))]
        {
            let _ = &m;
            return Err(crate::blobs::unsupported());
        }
        #[cfg(unix)]
        Ok(Self {
            device: m.dev(),
            inode: m.ino(),
            len: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Checked<T> {
    digest: String,
    value: T,
}
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    let bytes = serde_json::to_vec(value).map_err(json_error)?;
    serde_json::to_vec(&Checked {
        digest: sha256_hex(&bytes),
        value,
    })
    .map_err(json_error)
}
pub(crate) fn decode<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T, StoreError> {
    let checked: Checked<T> = serde_json::from_slice(bytes).map_err(json_error)?;
    if checked.digest != sha256_hex(&serde_json::to_vec(&checked.value).map_err(json_error)?) {
        return Err(crate::recovery::corrupt("derived cache checksum mismatch"));
    }
    Ok(checked.value)
}
pub(crate) fn json_error(error: serde_json::Error) -> StoreError {
    StoreError::invalid_request(crate::StoreErrorCode::InvalidRecord, error.to_string())
}

pub(crate) fn dirty_present(paths: &crate::SessionPaths, id: Option<&crate::SessionId>) -> Result<bool, StoreError> {
    #[cfg(not(unix))]
    {
        let _ = (paths, id);
        return Err(crate::blobs::unsupported());
    }
    #[cfg(unix)]
    {
        let directory = paths.root().join(".catalog.dirty");
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
                    return Err(crate::blobs::unsafe_path(&directory));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        match id {
            Some(id) => match checked_file(&directory.join(id.as_str()), false) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.into()),
            },
            None => Ok(fs::read_dir(directory)?.next().transpose()?.is_some()),
        }
    }
}

pub(crate) fn publish(disk: &mut Disk, path: &Path, bytes: &[u8], probe: &ReadProbe) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| crate::recovery::corrupt("cache parent absent"))?;
    match fs::symlink_metadata(path) {
        Ok(_) => {
            checked_file(path, false)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let temp = parent.join(format!(".cache-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = new_file(&temp)?;
    file.write_all(bytes)?;
    disk.hit(IoPoint::CacheFileSync)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    disk.hit(IoPoint::CacheRenamed)?;
    probe.note_write(path, 0, bytes.len())?;
    disk.sync_dir(parent, IoPoint::CacheDirectorySync)
}

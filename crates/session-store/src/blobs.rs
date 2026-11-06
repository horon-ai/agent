//! Original blob publication and strict filesystem primitives (not cache I/O).
use crate::{EventEnvelope, IoHook, IoPoint, SessionId, SessionPaths, StoreError, StoreErrorCode};
use horon_agent_platform::SyncMode;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

/// The durability policy every original this store publishes is confirmed with.
///
/// Derived files are never flushed, so the mode reaches only original writes and
/// the dirty marker that protects them.
pub(crate) struct Disk {
    pub hook: IoHook,
    pub mode: SyncMode,
}

impl Disk {
    pub fn new(hook: IoHook, mode: SyncMode) -> Self {
        Self { hook, mode }
    }

    pub fn hit(&mut self, point: IoPoint) -> Result<(), StoreError> {
        observation::note(point, self.mode);
        (self.hook)(point).map_err(io_error)
    }

    /// Flush an original file's contents, observing `point` first.
    pub fn sync_file(&mut self, file: &File, point: IoPoint) -> Result<(), StoreError> {
        self.hit(point)?;
        sync_file(file, self.mode)
    }

    /// Flush a directory entry, observing `point` first.
    ///
    /// The leaf must be a directory: a file path is refused before the platform
    /// call, and Windows then has nothing left to flush because NTFS journals
    /// directory entries itself.
    pub fn sync_dir(&mut self, path: &Path, point: IoPoint) -> Result<(), StoreError> {
        self.hit(point)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            return Err(unsafe_path(path));
        }
        sync_directory(path, self.mode)
    }

    /// Ensure `path` is a private directory, flushing its entry only when this
    /// call creates it.
    ///
    /// An existing directory needs no flush: its entry was confirmed when it was
    /// created, and a crash that lost that entry also lost the directory. This is
    /// the difference between the dirty marker's directory costing one flush per
    /// session and one flush per append.
    pub fn ensure_dir(&mut self, path: &Path) -> Result<(), StoreError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if horon_agent_platform::fs::is_private_dir(&metadata) {
                    Ok(())
                } else {
                    Err(unsafe_path(path))
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.directory(path),
            Err(error) => Err(error.into()),
        }
    }

    pub fn directory(&mut self, path: &Path) -> Result<(), StoreError> {
        // Windows has no owner-only mode to set, so the directory is created with
        // the inheritable ACL of its parent and only its type is verified there.
        // The recursion creates one level at a time so every level gets the mode;
        // on Windows it is redundant work that `create_dir_all` repeats safely.
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !horon_agent_platform::fs::is_private_dir(&metadata) {
                    return Err(unsafe_path(path));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = path.parent().ok_or_else(|| unsafe_path(path))?;
                if !parent.exists() {
                    self.directory(parent)?;
                }
                match horon_agent_platform::fs::create_private_dir(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        return self.directory(path);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
        // Also sync on retry: an earlier create may have failed at parent sync.
        let parent = path.parent().ok_or_else(|| unsafe_path(path))?;
        self.sync_dir(parent, IoPoint::DirectorySync)
    }

    pub fn publish_blob(&mut self, path: &Path, event: (&EventEnvelope, &[u8])) -> Result<(), StoreError> {
        let (envelope, bytes) = event;
        envelope.hydrate_from_blob(bytes)?;
        let parent = path.parent().ok_or_else(|| unsafe_path(path))?;
        self.directory(parent)?;
        match checked_file(path, false) {
            Ok(existing) => {
                let existing_bytes = read_file(path)?;
                envelope.hydrate_from_blob(&existing_bytes)?;
                self.sync_file(&existing, IoPoint::BlobFileSync)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let temp = parent.join(format!(".blob-{}.tmp", uuid::Uuid::new_v4()));
                let mut file = new_file(&temp)?;
                self.hit(IoPoint::BlobWrite)?;
                file.write_all(bytes)?;
                self.sync_file(&file, IoPoint::BlobFileSync)?;
                envelope.hydrate_from_blob(&read_file(&temp)?)?;
                self.hit(IoPoint::BlobVerified)?;
                fs::rename(&temp, path)?;
                self.hit(IoPoint::BlobRenamed)?;
            }
            Err(error) => return Err(error.into()),
        }
        self.sync_dir(parent, IoPoint::BlobDirectorySync)?;
        self.hit(IoPoint::BlobPublished)
    }
}

pub(crate) fn io_error(error: std::io::Error) -> StoreError {
    match error.kind() {
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput => unsupported(),
        _ => error.into(),
    }
}

/// Flush an original file's contents through the platform adapter.
pub(crate) fn sync_file(file: &File, mode: SyncMode) -> Result<(), StoreError> {
    horon_agent_platform::sync_file(file, mode).map_err(io_error)
}

/// Flush a directory entry so a file published in it survives a crash.
///
/// Windows cannot open a directory for flushing without backup semantics and
/// NTFS journals directory metadata itself, so the platform adapter returns
/// `Ok(())` there and the published file's own flush is the boundary.
pub(crate) fn sync_directory(path: &Path, mode: SyncMode) -> Result<(), StoreError> {
    horon_agent_platform::sync_dir(path, mode).map_err(io_error)
}

pub(crate) fn unsupported() -> StoreError {
    StoreError::internal(
        StoreErrorCode::DurabilityUnsupported,
        "filesystem durability is unsupported",
    )
}

pub(crate) fn unsafe_path(path: &Path) -> StoreError {
    StoreError::invalid_request(
        StoreErrorCode::UnsafePath,
        format!("unsafe or non-private path: {}", path.display()),
    )
}

/// Open options for a file this store reads or creates.
///
/// The leaf is opened without following a symlink and, where the platform has
/// mode bits, created owner-only. Windows has no `O_NOFOLLOW` and no mode bits to
/// request at open time: the leaf is checked after opening by [`check_file`], and
/// the owner-only property comes from the agent home's inheritable ACL.
fn options() -> OpenOptions {
    let mut options = horon_agent_platform::fs::no_follow_open_options();
    horon_agent_platform::fs::request_owner_only_mode(&mut options);
    options
}

/// Open a single-link, owner-private regular file without following its leaf.
pub(crate) fn checked_file(path: &Path, writable: bool) -> Result<File, std::io::Error> {
    let file = options().read(true).write(writable).open(path)?;
    check_file(path, &file)?;
    Ok(file)
}

pub(crate) fn new_file(path: &Path) -> Result<File, std::io::Error> {
    let file = options().read(true).write(true).create_new(true).open(path)?;
    check_file(path, &file)?;
    Ok(file)
}

pub(crate) fn log_file(path: &Path) -> Result<(File, bool), std::io::Error> {
    let (file, created) = match options().read(true).append(true).create_new(true).open(path) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            (options().read(true).append(true).open(path)?, false)
        }
        Err(error) => return Err(error),
    };
    check_file(path, &file)?;
    Ok((file, created))
}

/// Verify that `file` is the private regular file `path` names.
pub(crate) fn check_file(path: &Path, file: &File) -> Result<(), std::io::Error> {
    horon_agent_platform::fs::verify_private_file(path, file)
}

pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    checked_file(path, false)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn verify_blob(paths: &SessionPaths, id: &SessionId, event: &EventEnvelope) -> Result<(), StoreError> {
    if let Some(reference) = &event.payload_ref {
        let path = paths.blob_file(id, &reference.sha256)?;
        event.hydrate_from_blob(&read_file(&path)?)?;
    }
    Ok(())
}

#[cfg(not(test))]
mod observation {
    use super::{IoPoint, SyncMode};

    pub(crate) fn note(_point: IoPoint, _mode: SyncMode) {}
}

/// Test-only record of the durability level each observed boundary used.
///
/// A writer runs on the test's own thread, so a thread-local log needs no
/// locks, cannot be polluted by a concurrent test, and keeps the production
/// `Disk` free of instrumentation.
#[cfg(test)]
pub(crate) mod observation {
    use super::{IoPoint, SyncMode};
    use std::cell::RefCell;

    thread_local! {
        static OBSERVED: RefCell<Vec<(IoPoint, SyncMode)>> = const { RefCell::new(Vec::new()) };
    }

    /// Record the level this boundary is about to confirm with.
    pub(crate) fn note(point: IoPoint, mode: SyncMode) {
        OBSERVED.with(|log| log.borrow_mut().push((point, mode)));
    }

    /// Take the boundaries observed on this thread since the last call.
    pub(crate) fn take() -> Vec<(IoPoint, SyncMode)> {
        OBSERVED.with(|log| std::mem::take(&mut *log.borrow_mut()))
    }
}

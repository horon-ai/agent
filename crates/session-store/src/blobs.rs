//! Original blob publication and strict filesystem primitives (not cache I/O).
use crate::{EventEnvelope, IoHook, IoPoint, SessionId, SessionPaths, StoreError, StoreErrorCode};
use horon_agent_platform::SyncMode;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

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

#[cfg(not(test))]
mod observation {
    use super::{IoPoint, SyncMode};

    pub(crate) fn note(_point: IoPoint, _mode: SyncMode) {}
}

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
            Ok(metadata) => check_private_dir(path, &metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.directory(path),
            Err(error) => Err(error.into()),
        }
    }

    pub fn directory(&mut self, path: &Path) -> Result<(), StoreError> {
        // Windows has no owner-only mode to set, so the directory is created with
        // the inheritable ACL of its parent and only its type and leaf identity
        // are verified. `create_dir_all` follows an existing intermediate link,
        // so the leaf is re-checked after creation.
        #[cfg(windows)]
        {
            match fs::symlink_metadata(path) {
                Ok(metadata) => return check_private_dir(path, &metadata),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir_all(path)?;
                    check_private_dir(path, &fs::symlink_metadata(path)?)?;
                }
                Err(error) => return Err(error.into()),
            }
            Ok(())
        }
        #[cfg(unix)]
        {
            match fs::symlink_metadata(path) {
                Ok(metadata) => {
                    check_private_dir(path, &metadata)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let parent = path.parent().ok_or_else(|| unsafe_path(path))?;
                    if !parent.exists() {
                        self.directory(parent)?;
                    }
                    match fs::DirBuilder::new().mode(0o700).create(path) {
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
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(unsupported())
        }
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

/// Refuse a path that is not the private directory this store requires.
///
/// Windows carries the owner-only property in the inherited ACL of the agent
/// home, so only the type is checked there.
#[cfg(any(unix, windows))]
fn check_private_dir(path: &Path, metadata: &fs::Metadata) -> Result<(), StoreError> {
    if !metadata.is_dir() {
        return Err(unsafe_path(path));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(unsafe_path(path));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn check_private_dir(_path: &Path, _metadata: &fs::Metadata) -> Result<(), StoreError> {
    Err(unsupported())
}

fn options() -> Result<OpenOptions, std::io::Error> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut options = OpenOptions::new();
        #[cfg(target_os = "macos")]
        options.custom_flags(0x100); // O_NOFOLLOW
        #[cfg(target_os = "linux")]
        options.custom_flags(0x20000); // O_NOFOLLOW
        options.mode(0o600);
        Ok(options)
    }
    // Windows has no `O_NOFOLLOW` and no mode bits to request at open time. The
    // leaf is checked after opening by `check_file`, and the owner-only property
    // comes from the agent home's inheritable ACL rather than from this call.
    #[cfg(windows)]
    {
        Ok(OpenOptions::new())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

/// Open a single-link, owner-private regular file without following its leaf.
pub(crate) fn checked_file(path: &Path, writable: bool) -> Result<File, std::io::Error> {
    let file = options()?.read(true).write(writable).open(path)?;
    check_file(path, &file)?;
    Ok(file)
}

pub(crate) fn new_file(path: &Path) -> Result<File, std::io::Error> {
    let file = options()?.read(true).write(true).create_new(true).open(path)?;
    check_file(path, &file)?;
    Ok(file)
}

pub(crate) fn log_file(path: &Path) -> Result<(File, bool), std::io::Error> {
    let (file, created) = match options()?.read(true).append(true).create_new(true).open(path) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            (options()?.read(true).append(true).open(path)?, false)
        }
        Err(error) => return Err(error),
    };
    check_file(path, &file)?;
    Ok((file, created))
}

pub(crate) fn check_file(path: &Path, file: &File) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        let actual = file.metadata()?;
        let named = fs::symlink_metadata(path)?;
        let parent = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
        if !actual.is_file()
            || !named.is_file()
            || actual.nlink() != 1
            || actual.permissions().mode() & 0o777 != 0o600
            || actual.uid() != fs::metadata(parent)?.uid()
            || actual.ino() != named.ino()
            || actual.dev() != named.dev()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unsafe file identity or permissions",
            ));
        }
        Ok(())
    }
    // Windows exposes no mode bits, link count or inode on the stable standard
    // library surface, and `OpenOptions` has no `O_NOFOLLOW`, so this is the
    // strongest check available: the leaf must be a regular file whose creation
    // time matches the opened handle, so a file swapped in between the open and
    // this check is refused. Size and the last write time are deliberately not
    // compared: NTFS may defer both in the directory entry while a writing handle
    // is still open, and this check also runs in that window. The owner-only
    // property is carried by the agent home's inheritable ACL.
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        let actual = file.metadata()?;
        let named = fs::symlink_metadata(path)?;
        if !actual.is_file() || !named.is_file() || actual.creation_time() != named.creation_time() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unsafe file identity or permissions",
            ));
        }
        Ok(())
    }
    // Every other platform keeps the original refusal: the store has no
    // durability contract it can verify there.
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, file);
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
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

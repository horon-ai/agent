//! Original blob publication and strict filesystem primitives (not cache I/O).
use crate::{EventEnvelope, IoHook, IoPoint, SessionId, SessionPaths, StoreError, StoreErrorCode};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(crate) struct Disk {
    pub hook: IoHook,
}

impl Disk {
    pub fn hit(&mut self, point: IoPoint) -> Result<(), StoreError> {
        (self.hook)(point).map_err(io_error)
    }

    pub fn sync_dir(&mut self, path: &Path, point: IoPoint) -> Result<(), StoreError> {
        self.hit(point)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            return Err(unsafe_path(path));
        }
        // A directory cannot be opened for `sync_all` on Windows without backup
        // semantics, and NTFS journals directory entries itself. The file's own
        // `sync_all` is what makes its bytes durable there, so this flush keeps
        // the identity check above and then has nothing left to do.
        #[cfg(windows)]
        {
            Ok(())
        }
        #[cfg(not(windows))]
        {
            File::open(path).map_err(io_error)?.sync_all().map_err(io_error)
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
                Ok(metadata) => {
                    if !metadata.is_dir() {
                        return Err(unsafe_path(path));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir_all(path)?;
                    if !fs::symlink_metadata(path)?.is_dir() {
                        return Err(unsafe_path(path));
                    }
                }
                Err(error) => return Err(error.into()),
            }
            Ok(())
        }
        #[cfg(unix)]
        {
            match fs::symlink_metadata(path) {
                Ok(metadata) => {
                    if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
                        return Err(unsafe_path(path));
                    }
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
                self.hit(IoPoint::BlobFileSync)?;
                existing.sync_all().map_err(io_error)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let temp = parent.join(format!(".blob-{}.tmp", uuid::Uuid::new_v4()));
                let mut file = new_file(&temp)?;
                self.hit(IoPoint::BlobWrite)?;
                file.write_all(bytes)?;
                self.hit(IoPoint::BlobFileSync)?;
                file.sync_all().map_err(io_error)?;
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

/// Flush a directory entry so a file published in it survives a crash.
#[cfg(not(windows))]
pub(crate) fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path).map_err(io_error)?.sync_all().map_err(io_error)
}

/// Flush a directory entry, where the platform can.
///
/// Windows cannot open a directory for flushing without backup semantics, and
/// NTFS journals directory metadata itself, so the published file's own
/// `sync_all` is the durability boundary there.
#[cfg(windows)]
pub(crate) fn sync_directory(_path: &Path) -> Result<(), StoreError> {
    Ok(())
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

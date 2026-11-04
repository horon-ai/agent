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
        File::open(path).map_err(io_error)?.sync_all().map_err(io_error)
    }

    pub fn directory(&mut self, path: &Path) -> Result<(), StoreError> {
        #[cfg(not(unix))]
        {
            let _ = path;
            return Err(unsupported());
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
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
    #[cfg(not(unix))]
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

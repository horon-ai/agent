//! POSIX filesystem primitives shared by the macOS and Linux adapters.
//!
//! `platform-linux` owns this file and compiles it directly; `platform-macos`
//! pulls the same source in with `#[path = ...]`, so the two Unix adapters
//! cannot drift apart. The file never refers to `crate::` items: it is a
//! self-contained module compiled into whichever adapter includes it.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use horon_agent_platform_api::{FileIds, FileStamp};

pub(crate) fn set_file_permissions_owner_only(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

pub(crate) fn set_dir_permissions_owner_only(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

pub(crate) fn harden_credential_file_permissions(path: &Path) {
    // Read-time repair is opportunistic: a filesystem that cannot chmod must not
    // fail the credential read that triggered it.
    std::mem::drop(set_file_permissions_owner_only(path));
}

pub(crate) fn harden_credential_dir_permissions(path: &Path) {
    std::mem::drop(set_dir_permissions_owner_only(path));
}

pub(crate) fn harden_dir_permissions(path: &Path) {
    std::mem::drop(set_dir_permissions_owner_only(path));
}

pub(crate) fn is_private_dir(metadata: &Metadata) -> bool {
    metadata.is_dir() && metadata.permissions().mode() & 0o777 == 0o700
}

pub(crate) fn is_private_dir_owned_by(metadata: &Metadata, owner: &Metadata) -> bool {
    is_private_dir(metadata) && metadata.uid() == owner.uid()
}

pub(crate) fn is_private_file(metadata: &Metadata) -> bool {
    metadata.is_file() && metadata.nlink() == 1 && metadata.permissions().mode() & 0o777 == 0o600
}

pub(crate) fn is_executable(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

pub(crate) fn no_follow_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.custom_flags(libc::O_NOFOLLOW);
    options
}

pub(crate) fn request_owner_only_mode(options: &mut OpenOptions) {
    options.mode(0o600);
}

pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

pub(crate) fn verify_private_file(path: &Path, file: &File) -> io::Result<()> {
    let actual = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    if !actual.is_file()
        || !named.is_file()
        || actual.nlink() != 1
        || actual.permissions().mode() & 0o777 != 0o600
        || actual.uid() != fs::metadata(parent)?.uid()
        || actual.ino() != named.ino()
        || actual.dev() != named.dev()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe file identity or permissions",
        ));
    }
    Ok(())
}

pub(crate) fn file_stamp(metadata: &Metadata) -> FileStamp {
    FileStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        len: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    }
}

pub(crate) fn file_ids(metadata: &Metadata) -> FileIds {
    FileIds {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        gid: metadata.gid(),
    }
}

pub(crate) fn mtime_ns(metadata: &Metadata) -> i128 {
    (metadata.mtime() as i128) * 1_000_000_000 + (metadata.mtime_nsec() as i128)
}

pub(crate) fn rotate_backup(primary: &Path, backup: &Path) -> io::Result<()> {
    // Replacing a hard link leaves the primary in place, so a concurrent reader
    // never observes a missing path.
    drop(fs::remove_file(backup));
    fs::hard_link(primary, backup)
}

pub(crate) fn sync_parent_directory(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => File::open(parent)?.sync_all(),
        None => Ok(()),
    }
}

pub(crate) fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
    // `rename` replaces the destination entry in one step, so a reader sees
    // either the old file or the new one.
    fs::rename(from, to)
}

pub(crate) fn symlink_file(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

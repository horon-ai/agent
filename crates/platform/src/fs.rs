//! Filesystem primitives for callers that must not branch on the operating
//! system.
//!
//! Every function dispatches to the adapter selected for this build, so the
//! caller states what it needs - a private directory, a verified regular file, a
//! cache stamp - and the adapter picks the syscall.

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

use horon_agent_platform_api::{FileIds, FileStamp};

use crate::Current;
use crate::PlatformAdapter;

/// Restrict a file to read/write by its owner (`0o600`, or an owner-only DACL
/// on Windows).
pub fn set_file_permissions_owner_only(path: &Path) -> io::Result<()> {
    Current::set_file_permissions_owner_only(path)
}

/// Restore owner write access without changing other Unix permission bits.
pub fn make_owner_writable(path: &Path, mut permissions: std::fs::Permissions) -> io::Result<()> {
    if !permissions.readonly() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)
}

/// Restrict a directory to read/write/execute by its owner (`0o700`, or an
/// inheritable owner-only DACL on Windows).
pub fn set_dir_permissions_owner_only(path: &Path) -> io::Result<()> {
    Current::set_dir_permissions_owner_only(path)
}

/// Narrow a secret-bearing file to its owner, best effort.
///
/// Unix chmods synchronously; Windows defers the ACL replacement so a
/// credential read never inherits its cost.
pub fn harden_credential_file_permissions(path: &Path) {
    Current::harden_credential_file_permissions(path);
}

/// Narrow a secret-bearing directory to its owner, best effort.
pub fn harden_credential_dir_permissions(path: &Path) {
    Current::harden_credential_dir_permissions(path);
}

/// Narrow a directory to its owner where the platform needs to, ignoring
/// failure.
pub fn harden_dir_permissions(path: &Path) {
    Current::harden_dir_permissions(path);
}

/// Whether `metadata` describes a directory only its owner can enter.
pub fn is_private_dir(metadata: &Metadata) -> bool {
    Current::is_private_dir(metadata)
}

/// Whether `metadata` describes a directory only its owner can enter and that
/// `owner` names the same user as.
pub fn is_private_dir_owned_by(metadata: &Metadata, owner: &Metadata) -> bool {
    Current::is_private_dir_owned_by(metadata, owner)
}

/// The POSIX mode bits of `metadata`, when this platform has them.
///
/// A refusal that names a directory which is not owner-private has to state the
/// mode the operator must change, and only Unix has a mode to name: Windows keeps
/// a directory private through an inherited ACL.
pub fn mode_bits(_metadata: &Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(_metadata.mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Whether `metadata` describes a single-link regular file only its owner can
/// read or write.
pub fn is_private_file(metadata: &Metadata) -> bool {
    Current::is_private_file(metadata)
}

/// Whether `path` names a file this process may execute.
pub fn is_executable(path: &Path) -> bool {
    Current::is_executable(path)
}

/// Whether metadata from `symlink_metadata` describes a path indirection.
pub fn is_path_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// The package target that matches this agent's OS, architecture and ABI.
pub fn native_package_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "aarch64") if cfg!(target_env = "gnu") => Some("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Some("x86_64-unknown-linux-gnu"),
        ("windows", "aarch64") if cfg!(target_env = "msvc") => Some("aarch64-pc-windows-msvc"),
        ("windows", "x86_64") if cfg!(target_env = "msvc") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// Open a package leaf without following its symlink or reparse point.
pub fn package_open_options() -> OpenOptions {
    let mut options = no_follow_open_options();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.read(true);
    options
}

/// Open options that refuse to follow a symlink at the leaf.
pub fn no_follow_open_options() -> OpenOptions {
    Current::no_follow_open_options()
}

/// Request owner-only permissions for a file these options create.
pub fn request_owner_only_mode(options: &mut OpenOptions) {
    Current::request_owner_only_mode(options);
}

/// Create a directory only its owner can enter.
///
/// On Unix the mode is requested at creation and an existing leaf is an error
/// the caller decides about; on Windows the whole path is created with the
/// parent's inherited ACL.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    Current::create_private_dir(path)
}

/// Verify that `file` is the private regular file `path` names.
pub fn verify_private_file(path: &Path, file: &File) -> io::Result<()> {
    Current::verify_private_file(path, file)
}

/// The identity and timestamps of `metadata`, for a cache stamp.
pub fn file_stamp(metadata: &Metadata) -> FileStamp {
    Current::file_stamp(metadata)
}

/// The ownership identity of `metadata`.
pub fn file_ids(metadata: &Metadata) -> FileIds {
    Current::file_ids(metadata)
}

/// `metadata`'s modification time in nanoseconds since the Unix epoch.
pub fn mtime_ns(metadata: &Metadata) -> i128 {
    Current::mtime_ns(metadata)
}

/// Keep `primary`'s current contents at `backup` while leaving `primary` in
/// place where the platform allows it.
pub fn rotate_backup(primary: &Path, backup: &Path) -> io::Result<()> {
    Current::rotate_backup(primary, backup)
}

/// Flush the directory entry of `path`'s parent, if any.
pub fn sync_parent_directory(path: &Path) -> io::Result<()> {
    Current::sync_parent_directory(path)
}

/// How `metadata` describes the file's type to a user.
pub fn describe_file_kind(metadata: &Metadata) -> &'static str {
    Current::describe_file_kind(metadata)
}

/// Prepare `options` to open the private credential file at `path`.
///
/// The leaf cannot be a symlink followed elsewhere, and the file is created
/// owner-only where the platform has POSIX modes.
pub fn harden_credential_open(options: &mut OpenOptions, path: &Path) -> io::Result<()> {
    Current::harden_credential_open(options, path)
}

/// Repair the permissions of the already-open `file` to owner-only.
pub fn set_permissions_owner_only_file(file: &File) -> io::Result<()> {
    Current::set_permissions_owner_only_file(file)
}

/// Verify `metadata` describes a private regular file: a regular file owned by
/// this user with a single link.
pub fn verify_private_file_metadata(metadata: &Metadata) -> io::Result<()> {
    Current::verify_private_file_metadata(metadata)
}

/// Verify `metadata` describes a directory owned by this user.
pub fn verify_private_dir_metadata(metadata: &Metadata) -> io::Result<()> {
    Current::verify_private_dir_metadata(metadata)
}

/// Whether this platform can verify that a private directory belongs to this
/// user, which the whole-record exchange requires before it trusts the transfer
/// directory it finds on disk.
///
/// Unix compares the directory's owner with the session directory's. The
/// Windows metadata surface exposes no owner, so the store refuses the exchange
/// there rather than run it against a directory it cannot verify.
pub fn supports_record_exchange() -> bool {
    cfg!(unix)
}

/// Copy the directory `src` to `dst` with the filesystem's copy-on-write
/// primitive. `dst` must not exist.
pub fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
    Current::clone_tree(src, dst)
}

/// Replace `to` with `from` in one step, so a reader sees either file.
pub fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
    Current::atomic_replace(from, to)
}

/// Create `dst` as a symbolic link to `src`.
pub fn symlink_file(src: &Path, dst: &Path) -> io::Result<()> {
    Current::symlink_file(src, dst)
}

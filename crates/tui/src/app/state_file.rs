//! The file operations the client's own state files share.
//!
//! The notes file and the pins file are both JSON written by this client alone,
//! both replaced whole rather than edited in place, and both kept owner-only.
//! The sequence that makes that safe — create a private temporary file beside
//! the target, write it, flush it to the disk, rename it over the target — is
//! written once here, so the two files cannot drift into different durability
//! rules. Their locks differ (one is broken when its owner dies, the other is an
//! OS lock released with the handle), so each keeps its own.
//!
//! Every sentence a failure produces is built here and handed back as text: the
//! two callers wrap it in their own fault type, and the reader sees the same
//! words either way.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Write `bytes` beside `path` and rename them over it.
///
/// The temporary file is created owner-only and flushed before the rename, so a
/// reader sees either the old file or the whole new one, never a half-written
/// one, and a crash cannot leave the new name pointing at an empty file. A
/// failed write removes its temporary file rather than leaving it behind.
///
/// `fallback` names the temporary file when `path` has no file name of its own.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], fallback: &str) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    fs::create_dir_all(directory).map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let temp = directory.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map_or(fallback, |name| name.to_str().unwrap_or(fallback)),
        std::process::id()
    ));
    if let Err(error) = write_temp(&temp, bytes) {
        return Err(with_cleanup(error, &temp));
    }
    fs::rename(&temp, path).map_err(|error| with_cleanup(format!("cannot replace {}: {error}", path.display()), &temp))
}

/// Add a failed temp-file cleanup to a write error, so neither goes unreported.
fn with_cleanup(error: String, temp: &Path) -> String {
    match discard(temp) {
        Some(cleanup) => format!("{error}; {cleanup}"),
        None => error,
    }
}

/// Remove a file that may not exist, reporting a failure that matters.
pub(crate) fn discard(path: &Path) -> Option<String> {
    match fs::remove_file(path) {
        Ok(()) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => Some(format!("{} could not be removed: {error}", path.display())),
    }
}

/// Create `temp` owner-only, write `bytes` and flush them.
fn write_temp(temp: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    horon_agent_sdk::private_fs::harden_credential_open(&mut options, temp)
        .map_err(|error| format!("cannot prepare {}: {error}", temp.display()))?;
    let mut file = options
        .open(temp)
        .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
    horon_agent_sdk::private_fs::set_permissions_owner_only_file(&file)
        .map_err(|error| format!("cannot make {} private: {error}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
    Ok(())
}

/// `<path>.corrupt`: where an unreadable state file is kept.
pub(crate) fn corrupt_path(path: &Path, fallback: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| std::ffi::OsString::from(fallback), |name| name.to_os_string());
    name.push(".corrupt");
    path.with_file_name(name)
}

/// `<path>.lock`: the file that keeps two savers apart.
pub(crate) fn lock_path(path: &Path, fallback: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| std::ffi::OsString::from(fallback), |name| name.to_os_string());
    name.push(".lock");
    path.with_file_name(name)
}

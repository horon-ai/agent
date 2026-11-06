//! Filesystem primitives whose meaning depends on the operating system.
//!
//! Windows has no POSIX permission bits: access is controlled by inherited
//! ACLs on the file, and a directory entry is journaled by NTFS rather than
//! flushed by the caller. The owner-only helpers therefore check what Windows
//! can check (that the path is not a reparse point) and leave the ACL to the
//! user's profile.

use std::fs::Metadata;
use std::io;
use std::path::Path;

/// Replace `to` with `from` in one step.
///
/// `MoveFileExW` with REPLACE_EXISTING is the only replacement that cannot
/// leave both names present or both absent, and WRITE_THROUGH waits for the
/// move to reach the device.
pub fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Create `dst` as a symbolic link to `src`.
///
/// Creating a link needs Developer Mode or the symlink privilege; the error is
/// reported rather than worked around, because a copy would stop tracking the
/// file it points at.
pub fn symlink_file(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_replace_swaps_two_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let from = directory.path().join("from");
        let to = directory.path().join("to");
        std::fs::write(&from, b"new").expect("write from");
        std::fs::write(&to, b"old").expect("write to");

        atomic_replace(&from, &to).expect("replace");

        assert_eq!(std::fs::read(&to).expect("read to"), b"new");
        assert!(!from.exists(), "the staged file is consumed");
    }
}

// ---------------------------------------------------------------------------
// Owner-only access control, file identity and durability boundaries.
//
// Windows has no mode bits: privacy is carried by a DACL, and replacing one is
// far more expensive than a Unix `chmod` - a security product can turn a single
// `SetNamedSecurityInfoW` into seconds. Credential paths are therefore hardened
// by a coalescing worker, while a caller that has already accepted the cost (a
// secret write, a directory it just created) uses the synchronous form.
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use horon_agent_platform_api::{FileIds, FileStamp};

pub(crate) fn set_file_permissions_owner_only(path: &Path) -> io::Result<()> {
    set_windows_acl_owner_only(path, 0)
}

pub(crate) fn set_dir_permissions_owner_only(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::Security::SUB_CONTAINERS_AND_OBJECTS_INHERIT;

    set_windows_acl_owner_only(path, SUB_CONTAINERS_AND_OBJECTS_INHERIT)
}

pub(crate) fn harden_credential_file_permissions(path: &Path) {
    schedule(path, false);
}

pub(crate) fn harden_credential_dir_permissions(path: &Path) {
    schedule(path, true);
}

pub(crate) fn harden_dir_permissions(_path: &Path) {
    // A directory created under the user-private tree already inherits that
    // ACL, and probing it here would cost an ACL call on a cheap path.
}

pub(crate) fn is_private_dir(metadata: &Metadata) -> bool {
    metadata.is_dir()
}

pub(crate) fn is_private_dir_owned_by(metadata: &Metadata, _owner: &Metadata) -> bool {
    metadata.is_dir()
}

pub(crate) fn is_private_file(metadata: &Metadata) -> bool {
    metadata.is_file()
}

pub(crate) fn is_executable(path: &Path) -> bool {
    path.is_file()
}

pub(crate) fn no_follow_open_options() -> OpenOptions {
    // There is no `O_NOFOLLOW`; the leaf identity is checked after opening by
    // `verify_private_file`.
    OpenOptions::new()
}

pub(crate) fn request_owner_only_mode(_options: &mut OpenOptions) {
    // The parent directory's inherited ACL decides the mode.
}

pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

pub(crate) fn verify_private_file(path: &Path, file: &File) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;

    // No mode bits, link count or inode on the stable standard library surface,
    // so this is the strongest check available: the leaf must be a regular file
    // whose creation time matches the opened handle, which refuses a file swapped
    // in between the open and the check. Size and last write time are
    // deliberately not compared: NTFS may defer both in the directory entry
    // while a writing handle is still open, and this check also runs in that
    // window.
    let actual = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    if !actual.is_file() || !named.is_file() || actual.creation_time() != named.creation_time() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe file identity or permissions",
        ));
    }
    Ok(())
}

pub(crate) fn file_stamp(metadata: &Metadata) -> FileStamp {
    use std::os::windows::fs::MetadataExt;

    // No device, inode or change time is exposed; the fields keep their wire
    // shape and lean on what the platform does expose.
    FileStamp {
        device: 0,
        inode: 0,
        len: metadata.file_size(),
        modified: (metadata.last_write_time() as i64, 0),
        changed: (metadata.creation_time() as i64, 0),
    }
}

pub(crate) fn file_ids(_metadata: &Metadata) -> FileIds {
    FileIds::default()
}

pub(crate) fn mtime_ns(metadata: &Metadata) -> i128 {
    let Ok(modified) = metadata.modified() else {
        return 0;
    };
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_nanos() as i128,
        Err(_) => 0,
    }
}

pub(crate) fn rotate_backup(primary: &Path, backup: &Path) -> io::Result<()> {
    // Renaming replaces the destination, so the old backup has to go first and
    // the primary is briefly absent; no hard-linked replace is available.
    drop(fs::remove_file(backup));
    fs::rename(primary, backup)
}

pub(crate) fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    // NTFS journals directory entries itself.
    Ok(())
}

/// How long a completed attempt suppresses the next one for the same path.
const SECRET_HARDEN_CACHE_TTL: Duration = Duration::from_secs(60);
/// How long a failed attempt suppresses the next one.
const SECRET_HARDEN_FAILURE_BACKOFF: Duration = Duration::from_secs(5);
/// How long the worker waits before the first ACL call, so first-frame latency
/// never inherits it.
const SECRET_HARDEN_DEFER_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum SecretHardenAttempt {
    InFlight,
    Succeeded(Instant),
    Failed(Instant),
}

#[derive(Default)]
struct SecretHardenState {
    directories: HashMap<PathBuf, SecretHardenAttempt>,
    files: HashMap<PathBuf, SecretHardenAttempt>,
    pending_directories: HashSet<PathBuf>,
    pending_files: HashSet<PathBuf>,
    worker_running: bool,
}

impl SecretHardenState {
    /// Queue a path for best-effort hardening. Returns true when the caller
    /// should start the single worker for this process.
    fn enqueue(&mut self, path: &Path, directory: bool, now: Instant) -> bool {
        let attempted = if directory { &self.directories } else { &self.files };
        let should_suppress = match attempted.get(path) {
            Some(SecretHardenAttempt::InFlight) => true,
            Some(SecretHardenAttempt::Succeeded(attempted_at)) => {
                now.saturating_duration_since(*attempted_at) < SECRET_HARDEN_CACHE_TTL
            }
            Some(SecretHardenAttempt::Failed(attempted_at)) => {
                now.saturating_duration_since(*attempted_at) < SECRET_HARDEN_FAILURE_BACKOFF
            }
            None => false,
        };
        if should_suppress {
            return false;
        }

        if directory {
            self.pending_directories.insert(path.to_path_buf());
        } else {
            self.pending_files.insert(path.to_path_buf());
        }
        if self.worker_running {
            false
        } else {
            self.worker_running = true;
            true
        }
    }
}

static SECRET_HARDEN_STATE: LazyLock<Mutex<SecretHardenState>> =
    LazyLock::new(|| Mutex::new(SecretHardenState::default()));

fn schedule(path: &Path, directory: bool) {
    let should_spawn = {
        let Ok(mut state) = SECRET_HARDEN_STATE.lock() else {
            return;
        };
        state.enqueue(path, directory, Instant::now())
    };

    if !should_spawn {
        return;
    }

    if std::thread::Builder::new()
        .name("horon-agent-windows-acl-harden".to_string())
        .spawn(|| {
            std::thread::sleep(SECRET_HARDEN_DEFER_DELAY);
            run_hardening_worker();
        })
        .is_err()
        && let Ok(mut state) = SECRET_HARDEN_STATE.lock()
    {
        state.worker_running = false;
    }
}

fn run_hardening_worker() {
    loop {
        let (directories, files) = {
            let Ok(mut state) = SECRET_HARDEN_STATE.lock() else {
                return;
            };
            if state.pending_directories.is_empty() && state.pending_files.is_empty() {
                state.worker_running = false;
                return;
            }
            let directories = std::mem::take(&mut state.pending_directories);
            let files = std::mem::take(&mut state.pending_files);
            // Mark attempts before releasing the lock. Otherwise render-time
            // probes can requeue the same paths while a slow ACL call is in
            // flight, keeping the worker in an endless hardening loop.
            for path in &directories {
                state.directories.insert(path.clone(), SecretHardenAttempt::InFlight);
            }
            for path in &files {
                state.files.insert(path.clone(), SecretHardenAttempt::InFlight);
            }
            (directories, files)
        };

        let mut directory_results = Vec::with_capacity(directories.len());
        for path in &directories {
            let succeeded = set_dir_permissions_owner_only(path).is_ok();
            directory_results.push((path.clone(), succeeded));
        }
        let mut file_results = Vec::with_capacity(files.len());
        for path in &files {
            let succeeded = !path.exists() || set_file_permissions_owner_only(path).is_ok();
            file_results.push((path.clone(), succeeded));
        }

        let Ok(mut state) = SECRET_HARDEN_STATE.lock() else {
            return;
        };
        let completed_at = Instant::now();
        for (path, succeeded) in directory_results {
            state.directories.insert(
                path,
                if succeeded {
                    SecretHardenAttempt::Succeeded(completed_at)
                } else {
                    SecretHardenAttempt::Failed(completed_at)
                },
            );
        }
        for (path, succeeded) in file_results {
            state.files.insert(
                path,
                if succeeded {
                    SecretHardenAttempt::Succeeded(completed_at)
                } else {
                    SecretHardenAttempt::Failed(completed_at)
                },
            );
        }
        if state.pending_directories.is_empty() && state.pending_files.is_empty() {
            state.worker_running = false;
            return;
        }
        // New paths arrived while the ACL calls were running. Process them in
        // this worker without another startup delay.
        drop(state);
    }
}

fn set_windows_acl_owner_only(path: &Path, inheritance: windows_sys::Win32::Security::ACE_FLAGS) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_ALL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
        TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let result = (|| {
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }

        // TOKEN_USER contains pointer-aligned data followed by its SID. A usize
        // buffer gives the cast the alignment required by the Windows ABI.
        let word = std::mem::size_of::<usize>();
        let mut token_user = vec![0usize; (needed as usize).div_ceil(word)];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_user.as_mut_ptr().cast::<c_void>(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*(token_user.as_ptr().cast::<TOKEN_USER>()) };

        let trustee = TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: user.User.Sid.cast::<u16>(),
        };
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: inheritance,
            Trustee: trustee,
        };
        let mut acl = null_mut();
        let acl_status = unsafe { SetEntriesInAclW(1, &access, null(), &mut acl) };
        if acl_status != 0 {
            return Err(io::Error::from_raw_os_error(acl_status as i32));
        }

        let mut wide_path = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let security_status = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null(),
            )
        };
        unsafe {
            LocalFree(acl.cast::<c_void>());
        }
        if security_status != 0 {
            return Err(io::Error::from_raw_os_error(security_status as i32));
        }
        Ok(())
    })();

    unsafe {
        CloseHandle(token);
    }
    result
}

#[cfg(test)]
mod hardening_state_tests {
    use super::*;

    #[test]
    fn first_path_starts_one_worker_and_repeated_paths_are_coalesced() {
        let mut state = SecretHardenState::default();
        let now = Instant::now();
        let directory = Path::new(r"C:\Users\test\.horon-agent");
        let file = directory.join("auth.json");

        assert!(state.enqueue(directory, true, now));
        assert!(!state.enqueue(directory, true, now));
        assert!(!state.enqueue(&file, false, now));
        assert!(state.worker_running);
        assert_eq!(state.pending_directories.len(), 1);
        assert_eq!(state.pending_files.len(), 1);
    }

    #[test]
    fn recently_attempted_paths_are_not_requeued() {
        let mut state = SecretHardenState::default();
        let attempted_at = Instant::now();
        let file = PathBuf::from(r"C:\Users\test\.horon-agent\auth.json");
        state
            .files
            .insert(file.clone(), SecretHardenAttempt::Succeeded(attempted_at));

        assert!(!state.enqueue(&file, false, attempted_at));
        assert!(!state.worker_running);
        assert!(state.pending_files.is_empty());
    }

    #[test]
    fn failed_paths_retry_after_shorter_backoff() {
        let mut state = SecretHardenState::default();
        let attempted_at = Instant::now();
        let file = PathBuf::from(r"C:\Users\test\.horon-agent\auth.json");
        state
            .files
            .insert(file.clone(), SecretHardenAttempt::Failed(attempted_at));

        assert!(!state.enqueue(&file, false, attempted_at));
        let retry_at = attempted_at + SECRET_HARDEN_FAILURE_BACKOFF;
        assert!(state.enqueue(&file, false, retry_at));
    }

    #[test]
    fn in_flight_paths_are_not_requeued() {
        let mut state = SecretHardenState::default();
        let now = Instant::now();
        let file = PathBuf::from(r"C:\Users\test\.horon-agent\auth.json");
        state.files.insert(file.clone(), SecretHardenAttempt::InFlight);

        assert!(!state.enqueue(&file, false, now));
        assert!(state.pending_files.is_empty());
    }
}

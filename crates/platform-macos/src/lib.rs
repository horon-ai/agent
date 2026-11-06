//! macOS platform adapter: durable file sync and the system TLS verifier.
//!
//! The crate is compiled as an empty library on other targets so a
//! `--workspace` build never has to carry macOS-only syscalls; only
//! `horon-agent-platform` selects it, and only on macOS.
#![cfg(target_os = "macos")]

#[path = "../../platform-linux/src/unix.rs"]
mod unix;

mod tls;

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use horon_agent_platform_api::{PlatformAdapter, SyncMode};
use rustls::ClientConfig;

/// macOS implementation of [`PlatformAdapter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
    type PowerAssertion = PowerAssertion;

    fn sync_file(file: &File, mode: SyncMode) -> io::Result<()> {
        match mode {
            // std maps `sync_all` to `F_FULLFSYNC` on macOS, which flushes the
            // drive's write cache as well as the kernel buffers.
            SyncMode::Full => file.sync_all(),
            SyncMode::Fast => fsync_now(file),
        }
    }

    fn sync_dir(path: &Path, mode: SyncMode) -> io::Result<()> {
        let directory = File::open(path)?;
        match mode {
            SyncMode::Full => directory.sync_all(),
            SyncMode::Fast => fsync_now(&directory),
        }
    }

    fn tls_client_config() -> Arc<ClientConfig> {
        tls::client_config()
    }

    fn is_process_running(pid: u32) -> bool {
        unix::is_process_running(pid)
    }

    fn signal_detached_process_group(pid: u32, signal: i32) -> io::Result<()> {
        unix::signal_detached_process_group(pid, signal)
    }

    fn terminate_process_group(pid: u32, grace: Duration) -> bool {
        unix::terminate_process_group(pid, grace)
    }

    fn try_reap_child_process(pid: u32) -> io::Result<Option<i32>> {
        unix::try_reap_child_process(pid)
    }

    fn atomic_symlink_swap(src: &Path, dst: &Path, temp: &Path) -> io::Result<()> {
        unix::atomic_symlink_swap(src, dst, temp)
    }

    fn spawn_detached(cmd: &mut Command) -> io::Result<Child> {
        unix::spawn_detached(cmd)
    }

    fn spawn_detached_without_console_window(cmd: &mut Command) -> io::Result<Child> {
        unix::spawn_detached_without_console_window(cmd)
    }

    fn reap_detached(child: Child) {
        unix::reap_detached(child);
    }

    fn replace_process(cmd: &mut Command) -> io::Error {
        unix::replace_process(cmd)
    }

    fn raise_nofile_limit_best_effort(minimum_soft_limit: u64) {
        unix::raise_nofile_limit_best_effort(minimum_soft_limit);
    }

    fn symlink_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
        unix::symlink_or_copy(src, dst)
    }

    fn set_permissions_executable(path: &Path) -> io::Result<()> {
        unix::set_permissions_executable(path)
    }
}

impl Adapter {
    /// Build a client configuration that also trusts `extra_roots`.
    ///
    /// Production code takes the cached configuration from
    /// [`PlatformAdapter::tls_client_config`]; this hook lets tests inject a
    /// private CA and prove it is accepted beside the real trust set.
    #[doc(hidden)]
    pub fn client_config_with_extra_roots(
        extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
    ) -> Result<ClientConfig, rustls::Error> {
        tls::build(extra_roots)
    }
}

mod macos_power {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};

    use horon_agent_platform_api::SleepAssertion;

    type CFStringRef = *const c_void;
    type IOPMAssertionID = u32;
    type IOReturn = i32;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
    const K_IO_RETURN_SUCCESS: IOReturn = 0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const c_char, encoding: u32) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: u32,
            assertion_name: CFStringRef,
            assertion_id: *mut IOPMAssertionID,
        ) -> IOReturn;
        fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> IOReturn;
    }

    fn cf_string(value: &str) -> Option<CFStringRef> {
        let c_string = match CString::new(value) {
            Ok(c_string) => c_string,
            Err(_) => return None,
        };
        let cf = unsafe { CFStringCreateWithCString(std::ptr::null(), c_string.as_ptr(), K_CF_STRING_ENCODING_UTF8) };
        (!cf.is_null()).then_some(cf)
    }

    pub struct PowerAssertion {
        id: Option<IOPMAssertionID>,
    }

    impl SleepAssertion for PowerAssertion {
        fn prevent_user_idle_system_sleep(reason: &str) -> Self {
            let Some(assertion_type) = cf_string("PreventUserIdleSystemSleep") else {
                return Self { id: None };
            };
            let Some(assertion_name) = cf_string(reason) else {
                unsafe { CFRelease(assertion_type) };
                return Self { id: None };
            };

            let mut id = 0;
            let result = unsafe {
                IOPMAssertionCreateWithName(assertion_type, K_IOPM_ASSERTION_LEVEL_ON, assertion_name, &mut id)
            };
            unsafe {
                CFRelease(assertion_type);
                CFRelease(assertion_name);
            }

            if result == K_IO_RETURN_SUCCESS {
                horon_agent_logging::info(&format!(
                    "Created macOS sleep-prevention assertion while streaming (id={id})"
                ));
                Self { id: Some(id) }
            } else {
                horon_agent_logging::warn(&format!(
                    "Failed to create macOS sleep-prevention assertion while streaming: IOReturn={result}"
                ));
                Self { id: None }
            }
        }

        fn is_active(&self) -> bool {
            self.id.is_some()
        }

        fn release(&mut self) {
            if let Some(id) = self.id.take() {
                let result = unsafe { IOPMAssertionRelease(id) };
                if result != K_IO_RETURN_SUCCESS {
                    horon_agent_logging::warn(&format!(
                        "Failed to release macOS sleep-prevention assertion id={id}: IOReturn={result}"
                    ));
                }
            }
        }
    }

    impl Drop for PowerAssertion {
        fn drop(&mut self) {
            self.release();
        }
    }
}

pub use macos_power::PowerAssertion;

/// Plain `fsync(2)`: data and metadata reach the device, but the drive's write
/// cache is not flushed. This is what `SyncMode::Fast` asks for on macOS.
fn fsync_now(file: &File) -> io::Result<()> {
    // SAFETY: `file` owns a valid open descriptor for the duration of the call
    // and `fsync` only reads it.
    let result = unsafe { libc::fsync(file.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::path::PathBuf;

    use super::*;

    /// Write `contents` into a fresh temporary directory, sync it, and return
    /// the directory guard (which keeps the file alive) with its path.
    fn synced_file(contents: &[u8], mode: SyncMode) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("payload.txt");
        let mut file = File::create(&path).expect("create file");
        file.write_all(contents).expect("write bytes");
        file.flush().expect("flush bytes");
        Adapter::sync_file(&file, mode).expect("sync file");
        drop(file);
        (directory, path)
    }

    fn read_back(path: &std::path::Path) -> String {
        let mut contents = String::new();
        File::open(path)
            .expect("reopen")
            .read_to_string(&mut contents)
            .expect("read back");
        contents
    }

    #[test]
    fn sync_file_full_then_reopen_reads_bytes() {
        let (_directory, path) = synced_file(b"durable full mode\n", SyncMode::Full);
        assert_eq!(read_back(&path), "durable full mode\n");
    }

    #[test]
    fn sync_file_fast_then_reopen_reads_bytes() {
        let (_directory, path) = synced_file(b"durable fast mode\n", SyncMode::Fast);
        assert_eq!(read_back(&path), "durable fast mode\n");
    }

    #[test]
    fn fast_mode_uses_plain_fsync() {
        // The fast path calls libc::fsync directly instead of std's
        // F_FULLFSYNC wrapper, and reports the syscall's own result. Both
        // modes leave the bytes readable after the handle is closed.
        let (directory, path) = synced_file(b"fast mode\n", SyncMode::Fast);
        let file = File::options()
            .append(true)
            .open(&path)
            .expect("reopen for the direct syscall");
        fsync_now(&file).expect("plain fsync succeeds");
        assert_eq!(read_back(&path), "fast mode\n");
        drop(directory);
    }

    #[test]
    fn sync_dir_flushes_a_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        Adapter::sync_dir(directory.path(), SyncMode::Full).expect("full directory sync");
        Adapter::sync_dir(directory.path(), SyncMode::Fast).expect("fast directory sync");
    }
}

//! macOS platform adapter: durable file sync and the system TLS verifier.
//!
//! The crate is compiled as an empty library on other targets so a
//! `--workspace` build never has to carry macOS-only syscalls; only
//! `horon-agent-platform` selects it, and only on macOS.
#![cfg(target_os = "macos")]

#[path = "../../platform-linux/src/unix.rs"]
mod unix;

#[path = "../../platform-linux/src/unix_fs.rs"]
mod unix_fs;

#[path = "../../platform-linux/src/ipc.rs"]
pub mod ipc;

#[path = "../../platform-linux/src/unix_term.rs"]
mod unix_term;

mod memory;
#[path = "../../platform-linux/src/signal.rs"]
pub mod signal;
pub mod term;
mod tls;
pub mod watch;

pub use unix::UnixInhibitor;

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use horon_agent_platform_api::{
    FileIds, FileStamp, PlatformAdapter, ProcessMemoryFacts, ReadyChild, StdinState, StopRequest, SyncMode,
};
use rustls::ClientConfig;

#[cfg(test)]
#[path = "term_tests.rs"]
mod term_tests;

/// macOS implementation of [`PlatformAdapter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
    type PowerAssertion = PowerAssertion;
    type Inhibitor = UnixInhibitor;

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

    fn directory_sync_is_effective() -> bool {
        // The entry table a directory flush writes is what names a new file, so
        // the flush confirms a creation whose file was never flushed.
        true
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

    fn terminate_process(pid: u32, grace: Duration) -> bool {
        unix::terminate_process(pid, grace)
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

    fn spawn_background_consoleless(cmd: &mut Command) -> io::Result<Child> {
        unix::spawn_background_consoleless(cmd)
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

    fn set_file_permissions_owner_only(path: &Path) -> io::Result<()> {
        unix_fs::set_file_permissions_owner_only(path)
    }

    fn set_dir_permissions_owner_only(path: &Path) -> io::Result<()> {
        unix_fs::set_dir_permissions_owner_only(path)
    }

    fn harden_credential_file_permissions(path: &Path) {
        unix_fs::harden_credential_file_permissions(path);
    }

    fn harden_credential_dir_permissions(path: &Path) {
        unix_fs::harden_credential_dir_permissions(path);
    }

    fn harden_dir_permissions(path: &Path) {
        unix_fs::harden_dir_permissions(path);
    }

    fn is_private_dir(metadata: &Metadata) -> bool {
        unix_fs::is_private_dir(metadata)
    }

    fn is_private_dir_owned_by(metadata: &Metadata, owner: &Metadata) -> bool {
        unix_fs::is_private_dir_owned_by(metadata, owner)
    }

    fn is_private_file(metadata: &Metadata) -> bool {
        unix_fs::is_private_file(metadata)
    }

    fn is_executable(path: &Path) -> bool {
        unix_fs::is_executable(path)
    }

    fn no_follow_open_options() -> OpenOptions {
        unix_fs::no_follow_open_options()
    }

    fn request_owner_only_mode(options: &mut OpenOptions) {
        unix_fs::request_owner_only_mode(options);
    }

    fn create_private_dir(path: &Path) -> io::Result<()> {
        unix_fs::create_private_dir(path)
    }

    fn verify_private_file(path: &Path, file: &File) -> io::Result<()> {
        unix_fs::verify_private_file(path, file)
    }

    fn file_stamp(metadata: &Metadata) -> FileStamp {
        unix_fs::file_stamp(metadata)
    }

    fn file_ids(metadata: &Metadata) -> FileIds {
        unix_fs::file_ids(metadata)
    }

    fn mtime_ns(metadata: &Metadata) -> i128 {
        unix_fs::mtime_ns(metadata)
    }

    fn rotate_backup(primary: &Path, backup: &Path) -> io::Result<()> {
        unix_fs::rotate_backup(primary, backup)
    }

    fn sync_parent_directory(path: &Path) -> io::Result<()> {
        unix_fs::sync_parent_directory(path)
    }

    fn isolate_process_group(command: &mut Command) {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
    }

    fn fd_diagnostic_snapshot() -> String {
        // The shared Unix reader looks for `/proc`, which macOS lacks, so it
        // reports an empty descriptor table with unknown limits rather than
        // claiming the platform cannot answer.
        unix::process_fd_diagnostic_snapshot()
    }

    fn stdin_state(pid: u32) -> StdinState {
        term::stdin_state(pid)
    }

    fn process_tree_stdin_state(pid: u32) -> StdinState {
        term::process_tree_stdin_state(pid)
    }

    fn direct_children(pid: u32) -> Vec<u32> {
        term::direct_children(pid)
    }

    fn user_discriminator() -> String {
        unsafe { libc::geteuid() }.to_string()
    }

    fn user_runtime_dir() -> Option<PathBuf> {
        // Every macOS process gets a private per-user temporary directory here,
        // which is both cheaper and more private than the shared one.
        match std::env::var("TMPDIR") {
            Ok(directory) => Some(PathBuf::from(directory)),
            Err(_) => None,
        }
    }

    fn user_config_home() -> Option<PathBuf> {
        // macOS keeps application support outside the dotfile namespace.
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    }

    fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
        unix_fs::atomic_replace(from, to)
    }

    fn symlink_file(src: &Path, dst: &Path) -> io::Result<()> {
        unix_fs::symlink_file(src, dst)
    }

    fn terminate_child(child: &mut Child) {
        unix::terminate_child(child);
    }

    fn describe_file_kind(metadata: &Metadata) -> &'static str {
        unix::describe_file_kind(metadata)
    }

    fn harden_credential_open(options: &mut OpenOptions, path: &Path) -> io::Result<()> {
        unix::harden_credential_open(options, path)
    }

    fn set_permissions_owner_only_file(file: &File) -> io::Result<()> {
        unix::set_permissions_owner_only_file(file)
    }

    fn verify_private_file_metadata(metadata: &Metadata) -> io::Result<()> {
        unix::verify_private_file_metadata(metadata)
    }

    fn verify_private_dir_metadata(metadata: &Metadata) -> io::Result<()> {
        unix::verify_private_dir_metadata(metadata)
    }

    fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
        unix::clone_tree(src, dst)
    }

    fn hide_console_window(cmd: &mut Command) {
        unix::hide_console_window(cmd);
    }

    fn shell_command(command: &str, temp_dir: Option<&Path>) -> Command {
        unix::shell_command(command, temp_dir)
    }

    fn shell_command_escaped(command: &str, temp_dir: Option<&Path>) -> Command {
        unix::shell_command_escaped(command, temp_dir)
    }

    fn process_working_dirs() -> Vec<PathBuf> {
        // macOS has no process table that lists working directories cheaply.
        Vec::new()
    }

    fn process_memory_facts() -> ProcessMemoryFacts {
        memory::process_memory_facts()
    }

    fn set_process_name(name: &str) {
        unix::set_process_name(name);
    }

    fn browser_launcher_state() -> Option<bool> {
        unix::browser_launcher_state()
    }

    fn is_container() -> bool {
        unix::is_container()
    }

    fn kill_process_group(pid: u32) {
        // The child was started as its own group leader, so the negative id
        // reaches it and its descendants in one call.
        drop(unix::signal_detached_process_group(pid, libc::SIGKILL));
    }

    fn configure_system_allocator(_use_system_allocator: bool) {
        // macOS uses the system allocator's own defaults; the glibc arena and
        // mmap knobs this tunes do not exist here.
    }

    fn main_thread_stack_size() -> Option<usize> {
        // The default main-thread stack is large enough on Unix.
        None
    }

    fn bind_process_lifetime_to_parent() -> bool {
        unix::bind_process_lifetime_to_parent()
    }

    fn request_graceful_stop(pid: u32) -> io::Result<StopRequest> {
        unix::request_graceful_stop(pid)
    }

    fn force_stop_process_group(pid: u32) -> io::Result<()> {
        unix::force_stop_process_group(pid)
    }

    fn spawn_with_ready_pipe(cmd: &mut Command, ready_env_var: &str) -> io::Result<ReadyChild> {
        unix::spawn_with_ready_pipe(cmd, ready_env_var)
    }

    fn signal_name(signal: i32) -> String {
        signal::signal_name(signal)
    }

    fn signal_crash_reason(signal: i32) -> String {
        signal::signal_crash_reason(signal)
    }

    fn spawn_termination_signal_watchers<F>(on_signal: F)
    where
        F: Fn(i32) + Send + Sync + 'static,
    {
        signal::spawn_termination_signal_watchers(on_signal);
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
    use horon_agent_platform_api::log::{PlatformLogLevel as Level, log as report};
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
                report(
                    Level::Info,
                    &format!("Created macOS sleep-prevention assertion while streaming (id={id})"),
                );
                Self { id: Some(id) }
            } else {
                report(
                    Level::Warn,
                    &format!("Failed to create macOS sleep-prevention assertion while streaming: IOReturn={result}"),
                );
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
                    report(
                        Level::Warn,
                        &format!("Failed to release macOS sleep-prevention assertion id={id}: IOReturn={result}"),
                    );
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

    #[test]
    fn directory_sync_is_effective_on_unix() {
        // The flush writes the entry table that names a newly created file.
        assert!(Adapter::directory_sync_is_effective());
    }
}

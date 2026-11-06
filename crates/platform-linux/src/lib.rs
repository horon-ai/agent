//! Linux and other Unix platform adapter: durable file sync and the system
//! root store for TLS.
//!
//! FreeBSD and the other Unix targets use this crate too: the calls below are
//! POSIX, and nothing here is Linux-specific beyond the name.

mod allocator;
pub mod ipc;
pub mod signal;
pub mod term;
mod tls;
mod unix;
mod unix_fs;
mod unix_term;
pub mod watch;

pub use unix::UnixInhibitor;

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use horon_agent_platform_api::{
    FileIds, FileStamp, PlatformAdapter, ProcessMemoryFacts, ReadyChild, SleepAssertion, StdinState, StopRequest,
    SyncMode,
};
use rustls::ClientConfig;

#[cfg(all(test, target_os = "linux"))]
#[path = "term_tests.rs"]
mod term_tests;

/// Linux implementation of [`PlatformAdapter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
    type PowerAssertion = PowerAssertion;
    type Inhibitor = UnixInhibitor;

    fn sync_file(file: &File, _mode: SyncMode) -> io::Result<()> {
        // `sync_data` is fdatasync on Linux: it flushes the file's data and the
        // metadata needed to read it, without the timestamps a plain fsync
        // also writes. There is no cheaper durable call, so both modes match.
        file.sync_data()
    }

    fn sync_dir(path: &Path, _mode: SyncMode) -> io::Result<()> {
        // A directory has no data blocks; fsync flushes its entry table.
        File::open(path)?.sync_all()
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

        // Group 0 means "a new group whose id is the child's pid".
        command.process_group(0);
    }

    fn fd_diagnostic_snapshot() -> String {
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
        // The effective uid separates users sharing one temporary directory.
        unsafe { libc::geteuid() }.to_string()
    }

    fn user_runtime_dir() -> Option<PathBuf> {
        // Linux reads XDG_RUNTIME_DIR in the caller, which owns the fallback
        // order; there is no platform-provided directory beyond it.
        None
    }

    fn user_config_home() -> Option<PathBuf> {
        // The XDG base-directory convention: `$XDG_CONFIG_HOME`, or `~/.config`
        // when it is unset.
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
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
        #[cfg(target_os = "linux")]
        {
            unix::process_working_dirs()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Vec::new()
        }
    }

    fn process_memory_facts() -> ProcessMemoryFacts {
        #[cfg(target_os = "linux")]
        {
            unix::process_memory_facts()
        }
        #[cfg(not(target_os = "linux"))]
        {
            ProcessMemoryFacts::default()
        }
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

    fn configure_system_allocator(use_system_allocator: bool) {
        allocator::configure(use_system_allocator);
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
/// Inert sleep-prevention assertion: only macOS can keep the machine awake.
pub struct PowerAssertion;

impl SleepAssertion for PowerAssertion {
    fn prevent_user_idle_system_sleep(_reason: &str) -> Self {
        Self
    }

    fn is_active(&self) -> bool {
        false
    }

    fn release(&mut self) {}
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
    fn sync_dir_flushes_a_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        Adapter::sync_dir(directory.path(), SyncMode::Full).expect("full directory sync");
        Adapter::sync_dir(directory.path(), SyncMode::Fast).expect("fast directory sync");
    }
}

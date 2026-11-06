//! Linux and other Unix platform adapter: durable file sync and the system
//! root store for TLS.
//!
//! FreeBSD and the other Unix targets use this crate too: the calls below are
//! POSIX, and nothing here is Linux-specific beyond the name.

mod tls;
mod unix;

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use horon_agent_platform_api::{PlatformAdapter, SleepAssertion, SyncMode};
use rustls::ClientConfig;

/// Linux implementation of [`PlatformAdapter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
    type PowerAssertion = PowerAssertion;

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

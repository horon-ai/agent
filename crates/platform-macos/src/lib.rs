//! macOS platform adapter: durable file sync and the system TLS verifier.
//!
//! The crate is compiled as an empty library on other targets so a
//! `--workspace` build never has to carry macOS-only syscalls; only
//! `horon-agent-platform` selects it, and only on macOS.
#![cfg(target_os = "macos")]

mod tls;

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Arc;

use horon_agent_platform_api::{PlatformAdapter, SyncMode};
use rustls::ClientConfig;

/// macOS implementation of [`PlatformAdapter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
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
}

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

//! Linux and other Unix platform adapter: durable file sync and the system
//! root store for TLS.

mod tls;

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use horon_agent_platform_api::{PlatformAdapter, SyncMode};
use rustls::ClientConfig;

/// Linux implementation of [`PlatformAdapter`].
///
/// FreeBSD and the other Unix targets use this crate too: the calls below are
/// POSIX, and nothing here is Linux-specific beyond the name.
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
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

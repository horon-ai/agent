//! Windows platform adapter: durable file sync and the system certificate
//! verifier for TLS.

mod tls;

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use horon_agent_platform_api::{PlatformAdapter, SyncMode};
use rustls::ClientConfig;

/// Windows implementation of [`PlatformAdapter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
    fn sync_file(file: &File, _mode: SyncMode) -> io::Result<()> {
        // `sync_all` is FlushFileBuffers; there is no cheaper durable call, so
        // both modes match.
        file.sync_all()
    }

    fn sync_dir(_path: &Path, _mode: SyncMode) -> io::Result<()> {
        // A directory cannot be opened for FlushFileBuffers without backup
        // semantics and NTFS journals its entries itself, so the durability
        // boundary is the file's own flush. The caller keeps the check that the
        // path is really a directory.
        Ok(())
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
        let (_directory, path) = synced_file(b"durable full mode\r\n", SyncMode::Full);
        assert_eq!(read_back(&path), "durable full mode\r\n");
    }

    #[test]
    fn sync_file_fast_then_reopen_reads_bytes() {
        let (_directory, path) = synced_file(b"durable fast mode\r\n", SyncMode::Fast);
        assert_eq!(read_back(&path), "durable fast mode\r\n");
    }

    #[test]
    fn sync_dir_is_a_no_op_on_a_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        Adapter::sync_dir(directory.path(), SyncMode::Full).expect("directory sync");
        Adapter::sync_dir(directory.path(), SyncMode::Fast).expect("directory sync");
    }
}

//! The platform adapter selected for this build.
//!
//! Core crates call these free functions and never branch on the operating
//! system themselves. `Current` is resolved at compile time (macOS, Windows,
//! and every other Unix target, where Linux handles FreeBSD as well), so no
//! dynamic dispatch is involved anywhere on this path.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use rustls::ClientConfig;

pub use horon_agent_platform_api::{PlatformAdapter, SyncMode};

#[cfg(target_os = "macos")]
pub type Current = horon_agent_platform_macos::Adapter;

#[cfg(windows)]
pub type Current = horon_agent_platform_windows::Adapter;

#[cfg(all(unix, not(target_os = "macos")))]
pub type Current = horon_agent_platform_linux::Adapter;

/// Flush `file`'s contents according to `mode`.
pub fn sync_file(file: &File, mode: SyncMode) -> io::Result<()> {
    Current::sync_file(file, mode)
}

/// Flush a directory entry so a file created or renamed inside it survives a
/// crash.
///
/// A path that is not a directory is an error on every platform, before any
/// operating-system call happens. Windows then has nothing left to flush and
/// returns `Ok(())`.
pub fn sync_dir(path: &Path, mode: SyncMode) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a directory"));
    }
    Current::sync_dir(path, mode)
}

static TLS_CONFIG_BUILDS: AtomicU64 = AtomicU64::new(0);
static TLS_CONFIG: LazyLock<Arc<ClientConfig>> = LazyLock::new(|| {
    TLS_CONFIG_BUILDS.fetch_add(1, Ordering::Relaxed);
    Current::tls_client_config()
});

/// The process-wide client TLS configuration.
///
/// The trust set is the bundled webpki roots plus the roots the operating
/// system trusts, the client offers ALPN `h2` then `http/1.1`, and the
/// aws-lc-rs provider is named explicitly. The configuration is built on the
/// first call and every later call returns the same allocation, so a process
/// pays the system-verifier setup once.
pub fn tls_client_config() -> Arc<ClientConfig> {
    Arc::clone(&TLS_CONFIG)
}

/// How many configurations this process has built.
///
/// A process that shares the cached configuration reports `1`; the `tls_probe`
/// example prints this as the evidence that a second client reuses the first
/// build.
pub fn tls_config_builds() -> u64 {
    TLS_CONFIG_BUILDS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn sync_file_full_then_reopen_reads_bytes() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        file.write_all(b"platform sync\n").expect("write bytes");
        file.flush().expect("flush bytes");
        sync_file(file.as_file(), SyncMode::Full).expect("full sync");
        sync_file(file.as_file(), SyncMode::Fast).expect("fast sync");

        let contents = fs::read(file.path()).expect("read back");
        assert_eq!(contents, b"platform sync\n");
    }

    #[test]
    fn sync_dir_accepts_a_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        sync_dir(directory.path(), SyncMode::Full).expect("full directory sync");
        sync_dir(directory.path(), SyncMode::Fast).expect("fast directory sync");
    }

    #[test]
    fn sync_dir_rejects_non_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("file.txt");
        fs::write(&path, b"data").expect("write file");

        let error = sync_dir(&path, SyncMode::Full).expect_err("a file path must be rejected");
        eprintln!("sync_dir(file) -> {error:?}");
        assert!(
            matches!(error.kind(), io::ErrorKind::NotADirectory | io::ErrorKind::InvalidInput),
            "unexpected error kind: {error:?}"
        );
    }

    #[test]
    fn sync_dir_rejects_a_missing_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing");
        let error = sync_dir(&missing, SyncMode::Full).expect_err("a missing path must be rejected");
        eprintln!("sync_dir(missing) -> {error:?}");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn tls_client_config_is_built_once_per_process() {
        let first = tls_client_config();
        let second = tls_client_config();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(tls_config_builds(), 1);
        assert_eq!(first.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[cfg(windows)]
    #[test]
    fn sync_dir_is_a_no_op_on_windows() {
        let directory = tempfile::tempdir().expect("temporary directory");
        // Windows keeps the identity check above and then has nothing to flush.
        sync_dir(directory.path(), SyncMode::Full).expect("windows directory sync is Ok");
        sync_dir(directory.path(), SyncMode::Fast).expect("windows directory sync is Ok");
    }
}

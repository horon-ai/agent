//! Fixture roots shared by the session-store integration targets.
//!
//! `std::env::temp_dir()` returns the OS spelling of the temporary directory.
//! On macOS that spelling is `/var/folders/...`, and `/var` is a symlink to
//! `private/var`. The store refuses any path that walks through a symlink
//! (`legacy_bundle::safe_path`), so a fixture has to hand it a canonical root.
//! Production resolves the same alias before it builds `SessionPaths`
//! (`horon-agent-base/src/session/storage_paths.rs::store_paths`), and these
//! fixtures do the same: resolve the parent once, then join a unique name so
//! concurrent tests still own separate directories.
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

/// A fresh, owner-private fixture root under the canonical temporary directory.
pub fn temp_root(prefix: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .canonicalize()
        .expect("the OS temporary directory must resolve")
        .join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("fixture root");
    root
}

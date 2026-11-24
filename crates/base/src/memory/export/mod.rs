//! The user volume's lifecycle: export, import, quiesce and deletion.
//!
//! A hosted user owns one volume whose root is `HORON_HOME`. Inside it, the
//! memory wiki is `memory/`: markdown pages as the truth and `index.sqlite` as
//! derived data. This module is everything an operator does to that volume as a
//! whole, and nothing else in the tree needs to know how a volume is moved:
//!
//! | Concern | Module | Durable artefact under the volume |
//! | --- | --- | --- |
//! | Tar export and import | [`archive`] | the tar the caller names |
//! | Manifest and per-file digests | [`manifest`] | `manifest.json` inside the tar |
//! | Quiesce and the write fence | [`fence`] | `memory/.write-fence.json` |
//! | User data key contract | [`key`] | `memory/key.json` |
//! | Central metadata client | [`control_plane`] | none, requests only |
//! | `delete_user` step machine | [`delete`] | `volume-state/deletion-job.json` |
//!
//! Two rules shape the whole module.
//!
//! * **The markdown is the only page content that leaves the volume.** The
//!   export carries page files verbatim plus a metadata line per page; the
//!   central metadata client carries identifiers, states and timestamps and
//!   never a page body, which a test proves by planting a marker body and
//!   scanning every request the client made.
//! * **Deletion is irreversible, so it is ordered and resumable.** A job file
//!   records each step, a failed step stays retryable, and the central row is
//!   marked deleted only after the write fence, key revocation, snapshot
//!   deletion and volume deletion have all finished.

use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

pub mod archive;
pub mod control_plane;
pub mod crypto;
pub mod delete;
pub mod fence;
pub mod key;
pub mod manifest;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// Directory under `HORON_HOME` that holds the memory store.
pub const STORE_DIRNAME: &str = "memory";

/// Directory under `HORON_HOME` that holds lifecycle state, kept outside the
/// store so deleting the store cannot lose the job that is deleting it.
pub const STATE_DIRNAME: &str = "volume-state";

/// The resolved locations of one user volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeLayout {
    home: PathBuf,
}

impl VolumeLayout {
    /// Resolve the layout of the volume whose root is `home`.
    pub fn for_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// The volume root, which is `HORON_HOME` for the process that serves it.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The memory store root (`<home>/memory`).
    pub fn store_root(&self) -> PathBuf {
        self.home.join(STORE_DIRNAME)
    }

    /// The lifecycle state directory (`<home>/volume-state`).
    pub fn state_dir(&self) -> PathBuf {
        self.home.join(STATE_DIRNAME)
    }

    /// Where a locally staged copy of the user data key would live.
    ///
    /// The control plane and its KMS own the key material; a volume never
    /// stores it. The path exists so revocation can wipe a staged copy a
    /// deployment created, and so the wipe is a no-op when nothing is there.
    pub fn key_material_dir(&self) -> PathBuf {
        self.home.join("keys")
    }
}

/// Write `bytes` to `path` atomically and owner-only: temp file beside the
/// target, fsync, rename, parent-directory fsync.
///
/// The parent directory's mode is left alone. Unlike the store's page writer
/// this must never tighten a directory the caller chose, such as the directory
/// holding an export tar.
pub(crate) fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Err(anyhow!("{} has no parent directory", path.display()));
    };
    std::fs::create_dir_all(parent).with_context(|| format!("cannot create the directory {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("{} has no UTF-8 file name", path.display()))?;
    let nonce: u64 = rand::random();
    let temp = parent.join(format!(".{file_name}.tmp.{}.{nonce}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        horon_agent_platform::fs::request_owner_only_mode(&mut options);
        let mut file = options
            .open(&temp)
            .with_context(|| format!("cannot create {}", temp.display()))?;
        use std::io::Write;
        file.write_all(bytes)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("cannot flush {}", temp.display()))?;
        horon_agent_platform::fs::set_file_permissions_owner_only(&temp)
            .with_context(|| format!("cannot restrict {}", temp.display()))?;
        std::fs::rename(&temp, path).with_context(|| format!("cannot move {} into place", path.display()))?;
        horon_agent_platform::fs::sync_parent_directory(path)
            .with_context(|| format!("cannot flush the directory of {}", path.display()))?;
        Ok(())
    })();
    if result.is_err()
        && let Err(cleanup) = std::fs::remove_file(&temp)
        && cleanup.kind() != std::io::ErrorKind::NotFound
    {
        return Err(anyhow!(cleanup).context(format!("cannot remove the temp file {}", temp.display())));
    }
    result
}

/// The current instant as an RFC 3339 string, for the records this module
/// writes.
pub(crate) fn now_rfc3339() -> String {
    horon_agent_time::Timestamp::now().to_rfc3339()
}

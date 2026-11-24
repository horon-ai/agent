//! The user data key contract: metadata in the volume, material in the KMS.
//!
//! A hosted volume is encrypted at rest by the platform, and the issue asks for
//! one more layer: a per-user data key that the control plane's key service
//! owns. This build implements the part the agent can honestly own.
//!
//! * **The volume carries metadata only.** [`KeyRecord`] holds the `key_id`, the
//!   algorithm id, `created_at` and `revoked_at`. There are no key bytes in it,
//!   and no field that could hold them.
//! * **The key service supplies material.** [`KeyProvider`] is the interface the
//!   control plane implements (a KMS call, a sealed local file, a hardware
//!   module). The agent calls it, never stores the result on the volume, and the
//!   [`KeyMaterial`] handle wipes its bytes when it drops.
//! * **A revoked key fails closed.** [`ensure_open`] refuses to open the store
//!   when the record names a revocation, so a volume whose key was destroyed for
//!   a deletion request cannot be served again by mistake, even if the data is
//!   still on disk.
//!
//! **Archive encryption and at-rest scope.** Sealed export archives use
//! `AES-256-GCM` per entry (`HME1`, see `crypto.rs`) when exported with a key,
//! while unencrypted exports record `encryption: none` and are rejected when
//! an archive carries any unrecognised or missing-key encryption header. Live
//! on-disk page files rely on platform at-rest volume encryption.

use super::{VolumeLayout, now_rfc3339, write_private_file};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// The key metadata file, inside the store root so it travels with the volume.
pub const KEY_FILE: &str = "key.json";

/// The key record version this build writes.
const KEY_VERSION: u32 = 1;

/// What the volume knows about its user data key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRecord {
    /// Always `1`; a future shape bumps it and an older reader refuses.
    pub version: u32,
    /// The key's identifier in the key service. Never the key itself.
    pub key_id: String,
    /// The algorithm id the key service named, e.g. `aes-256-gcm-envelope`.
    pub algorithm: String,
    /// When the key was created, RFC 3339.
    pub created_at: String,
    /// When the key was revoked, RFC 3339. `None` while the key is live.
    pub revoked_at: Option<String>,
}

impl KeyRecord {
    /// A freshly created record for `key_id` with `algorithm`, stamped now.
    pub fn created(key_id: impl Into<String>, algorithm: impl Into<String>) -> Self {
        Self {
            version: KEY_VERSION,
            key_id: key_id.into(),
            algorithm: algorithm.into(),
            created_at: now_rfc3339(),
            revoked_at: None,
        }
    }

    /// Whether the key was revoked.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// The record with `revoked_at` set to now. Revoking twice keeps the first
    /// instant: a retried step must not move the record's history.
    pub fn revoked(&self) -> Self {
        let mut next = self.clone();
        if next.revoked_at.is_none() {
            next.revoked_at = Some(now_rfc3339());
        }
        next
    }
}

/// Key bytes handed over by a [`KeyProvider`].
///
/// The value is redacted in `Debug` and wiped on drop, so a stray `{:?}` in a
/// log line or an early return cannot print or retain the material.
pub struct KeyMaterial {
    key_id: String,
    bytes: Vec<u8>,
}

impl KeyMaterial {
    /// Wrap `bytes` as the material of `key_id`.
    pub fn new(key_id: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            key_id: key_id.into(),
            bytes,
        }
    }

    /// The key this material belongs to.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Borrow the bytes. A provider performs its operation here and drops the
    /// handle; the caller must not clone them into a longer-lived value.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The material's length, which is all a log line may say.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether there is no material at all.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Hand the bytes to `use_it` and wipe them afterwards.
    pub fn consume<R>(mut self, use_it: impl FnOnce(&[u8]) -> R) -> R {
        let result = use_it(&self.bytes);
        self.wipe();
        result
    }

    fn wipe(&mut self) {
        self.bytes.fill(0);
        std::hint::black_box(&mut self.bytes);
        self.bytes.clear();
    }
}

impl Drop for KeyMaterial {
    fn drop(&mut self) {
        self.wipe();
    }
}

impl fmt::Debug for KeyMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KeyMaterial")
            .field("key_id", &self.key_id)
            .field("bytes", &format_args!("<redacted {} byte(s)>", self.bytes.len()))
            .finish()
    }
}

/// The interface a control plane implements to hand the agent a key.
///
/// The agent never creates, derives or persists key material; it asks for the
/// material of the `key_id` its volume records and lets the provider decide how
/// that is produced and protected.
pub trait KeyProvider {
    /// The material for `key_id`, or `None` when the service holds no such key.
    ///
    /// An error is a transport or policy failure and must not be read as "the
    /// key does not exist": a caller that cannot reach the service must not
    /// conclude that a volume was never encrypted.
    fn key_material(&self, key_id: &str) -> Result<Option<KeyMaterial>>;
}

impl VolumeLayout {
    /// The key metadata file of this volume.
    pub fn key_path(&self) -> std::path::PathBuf {
        self.store_root().join(KEY_FILE)
    }
}

/// Read the volume's key record, if it has one.
pub fn read_key(layout: &VolumeLayout) -> Result<Option<KeyRecord>> {
    read_key_at(&layout.store_root())
}

/// Read the key record at a store root, if any.
pub fn read_key_at(store_root: &Path) -> Result<Option<KeyRecord>> {
    let path = store_root.join(KEY_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow!(error).context(format!("cannot read the key record {}", path.display())));
        }
    };
    let record: KeyRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("the key record {} is not a valid key record", path.display()))?;
    if record.version != KEY_VERSION {
        return Err(anyhow!(
            "the key record {} carries version {} and this build writes {KEY_VERSION}",
            path.display(),
            record.version
        ));
    }
    Ok(Some(record))
}

/// Create the volume's key record, refusing to overwrite a live or revoked one.
pub fn create_key(layout: &VolumeLayout, key_id: &str, algorithm: &str) -> Result<KeyRecord> {
    if key_id.trim().is_empty() {
        return Err(anyhow!("a key id must not be empty"));
    }
    if algorithm.trim().is_empty() {
        return Err(anyhow!("a key algorithm id must not be empty"));
    }
    if let Some(existing) = read_key(layout)? {
        return Err(anyhow!(
            "the volume already holds key `{}` (revoked: {}); refusing to replace it",
            existing.key_id,
            existing.is_revoked()
        ));
    }
    let record = KeyRecord::created(key_id, algorithm);
    write_private_file(
        &layout.key_path(),
        &serde_json::to_vec_pretty(&record).context("cannot encode the key record")?,
    )?;
    Ok(record)
}

/// Revoke the volume's key: record the revocation and return the record.
///
/// Idempotent. A volume with no key record reports `None`, because there is no
/// key to revoke and a deletion job must be able to finish.
pub fn revoke_key(layout: &VolumeLayout) -> Result<Option<KeyRecord>> {
    let Some(existing) = read_key(layout)? else {
        return Ok(None);
    };
    if existing.is_revoked() {
        return Ok(Some(existing));
    }
    let revoked = existing.revoked();
    write_private_file(
        &layout.key_path(),
        &serde_json::to_vec_pretty(&revoked).context("cannot encode the key record")?,
    )?;
    crate::logging::info(&format!(
        "memory volume key {} revoked (algorithm {})",
        revoked.key_id, revoked.algorithm
    ));
    Ok(Some(revoked))
}

/// Wipe any key material a deployment staged inside the volume.
///
/// The contract puts material in the key service, so this directory is normally
/// absent; when a control plane seeded one, overwriting each file with zeros
/// before removing it is the most a portable Rust program can do, and the
/// platform's at-rest encryption plus the revocation are the real guarantees.
pub fn wipe_staged_material(layout: &VolumeLayout) -> Result<usize> {
    let directory = layout.key_material_dir();
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(anyhow!(error).context(format!("cannot read {}", directory.display())));
        }
    };
    let mut wiped = 0;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read an entry of {}", directory.display()))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).with_context(|| format!("cannot stat {}", path.display()))?;
        if metadata.is_dir() {
            continue;
        }
        overwrite_then_remove(&path, metadata.len())?;
        wiped += 1;
    }
    match std::fs::remove_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            // A non-empty directory (a subdirectory this function refuses to
            // walk) stays, and the removal step reports it rather than hiding it.
            return Err(anyhow!(error).context(format!("cannot remove {}", directory.display())));
        }
    }
    Ok(wiped)
}

/// Overwrite `path` with zeros, flush, then remove it.
fn overwrite_then_remove(path: &Path, len: u64) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let zeros = vec![0u8; 64 * 1024];
    let mut remaining = len;
    while remaining > 0 {
        let take = usize::try_from(remaining.min(zeros.len() as u64)).unwrap_or(zeros.len());
        file.write_all(&zeros[..take])
            .with_context(|| format!("cannot overwrite {}", path.display()))?;
        remaining -= take as u64;
    }
    file.sync_all()
        .with_context(|| format!("cannot flush {}", path.display()))?;
    drop(file);
    std::fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?;
    Ok(())
}

/// Refuse to serve a volume whose key was revoked.
///
/// Called before the store is opened for a hosted volume, so a deletion that
/// revoked the key cannot be undone by starting the Machine again.
pub fn ensure_open(layout: &VolumeLayout) -> Result<Option<KeyRecord>> {
    ensure_open_at(&layout.store_root())
}

/// [`ensure_open`] at a store root, which is where the store's own open calls it.
pub(crate) fn ensure_open_at(store_root: &Path) -> Result<Option<KeyRecord>> {
    let record = read_key_at(store_root)?;
    if let Some(record) = &record
        && let Some(revoked_at) = &record.revoked_at
    {
        return Err(anyhow!(
            "the user data key {} was revoked at {revoked_at}; this volume is closed for deletion",
            record.key_id
        ));
    }
    Ok(record)
}

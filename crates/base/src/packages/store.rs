//! Filesystem mechanics for the package store.
//!
//! Three things live here, all shared by the catalog:
//!
//! - the exclusive catalog lock, taken through the platform's portable
//!   instance lock so a second process waits instead of writing half a catalog;
//! - the validated copy: the declared files are copied byte by byte into a
//!   same-filesystem staging directory, hashed while they are written, then
//!   validated and hashed again from the published bytes before the directory
//!   is renamed under its content digest;
//! - reader references: a live session holds a lease directory whose lock file
//!   proves the holder is alive, so a replaced copy is collected only after its
//!   last reader is gone.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use horon_agent_extension_types::PackageKind;
use horon_agent_platform::fs::{
    is_path_link, make_owner_writable, package_open_options, request_owner_only_mode, set_dir_permissions_owner_only,
};
use horon_agent_platform::ipc::{InstanceLock, lock_instance_file};
use horon_agent_platform::{SyncMode, sync_dir, sync_file};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use super::catalog::StoredFile;
use super::manifest::{ValidatedPackage, open_package_file, validate_package};

pub(super) const CATALOG_FILE: &str = "installed.json";
const LOCK_FILE: &str = "catalog.lock";
const STORE_DIR: &str = "store";
const REFS_DIR: &str = "refs";
const INBOX_DIR: &str = "inbox";
pub(super) const STAGING_PREFIX: &str = ".staging-";
pub(super) const CATALOG_TEMP_PREFIX: &str = ".installed-";
const LEASE_LOCK: &str = "lease.lock";
const PIN_FILE: &str = "pinned.json";
const PIN_VERSION: u32 = 1;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
/// How long a writer waits for another process that holds the catalog lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RETRY: Duration = Duration::from_millis(5);

/// Directories of one horon home's package area.
pub(super) struct Layout {
    root: PathBuf,
}

impl Layout {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn catalog(&self) -> PathBuf {
        self.root.join(CATALOG_FILE)
    }

    pub(super) fn store(&self) -> PathBuf {
        self.root.join(STORE_DIR)
    }

    pub(super) fn refs(&self) -> PathBuf {
        self.root.join(REFS_DIR)
    }

    pub(super) fn inbox(&self) -> PathBuf {
        self.root.join(INBOX_DIR)
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// Create the package area and narrow every directory to its owner.
    pub(super) fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create the package directory {}", self.root.display()))?;
        let metadata = fs::symlink_metadata(&self.root)
            .with_context(|| format!("inspect the package directory {}", self.root.display()))?;
        ensure!(
            metadata.is_dir() && !is_path_link(&metadata),
            "package path is not a directory: {}",
            self.root.display()
        );
        set_dir_permissions_owner_only(&self.root)
            .with_context(|| format!("restrict {} to its owner", self.root.display()))?;
        ensure_private_dir(&self.store())?;
        ensure_private_dir(&self.refs())?;
        Ok(())
    }
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
            ensure!(
                metadata.is_dir() && !is_path_link(&metadata),
                "package path is not a directory without links: {}",
                path.display()
            );
        }
        Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
    }
    set_dir_permissions_owner_only(path).with_context(|| format!("restrict {} to its owner", path.display()))
}

/// The exclusive lock every catalog mutation holds.
///
/// The guard owns the stable lock file, never unlinks it, and releases the
/// operating-system lock when the file closes. A crashed holder therefore
/// leaves no held lock, and a waiter can never acquire an unlinked inode while
/// a second process acquires the replacement.
pub(super) struct StoreLock {
    _file: File,
}

/// Take the catalog lock, waiting for a live writer to finish.
pub(super) fn lock(layout: &Layout) -> Result<StoreLock> {
    layout.ensure()?;
    let path = layout.lock_path();
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    request_owner_only_mode(&mut options);
    let file = options
        .open(&path)
        .with_context(|| format!("open the package catalog lock at {}", path.display()))?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(StoreLock { _file: file }),
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    bail!(
                        "another process has held the package catalog lock at {} for more than {} seconds",
                        path.display(),
                        LOCK_TIMEOUT.as_secs()
                    );
                }
                std::thread::sleep(LOCK_RETRY);
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("lock the package catalog at {}", path.display()));
            }
        }
    }
}

/// A directory holding bytes that are published as one digest.
pub(super) struct PublishedCopy {
    pub digest: String,
    pub files: Vec<StoredFile>,
    pub executable: String,
}

/// Copy `validated`'s declared files out of `source` and publish them under
/// their content digest.
///
/// The bytes validated at the end are the copied bytes, not the source bytes:
/// the manifest and every declared file are read back from the staged copy, and
/// the recorded hashes are recomputed from the same copy.
///
/// Two renames are the publication points, and they fail differently. Before
/// the staging directory is renamed under its digest every error is local: the
/// staging directory is removed and the previous copy is untouched. After that
/// rename the copy is published even if the following directory flush fails,
/// because another process may already have observed it; the error is reported
/// and only collection decides the copy's fate. The catalog rename is the
/// second point: once it succeeds the new generation may be the one on disk, so
/// a later error never rolls back either copy.
pub(super) fn publish_copy(layout: &Layout, source: &Path, validated: &ValidatedPackage) -> Result<PublishedCopy> {
    let staging = layout.store().join(format!(
        "{STAGING_PREFIX}{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    fs::create_dir(&staging).with_context(|| format!("create {}", staging.display()))?;
    set_dir_permissions_owner_only(&staging).with_context(|| format!("restrict {} to its owner", staging.display()))?;
    match stage_copy(layout, source, validated, &staging) {
        Ok(published) => Ok(published),
        Err(error) => match remove_tree(&staging) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.context(format!(
                "could not remove the staged copy {}: {cleanup}",
                staging.display()
            ))),
        },
    }
}

/// Copy, validate and publish into `staging`; the caller removes it on error.
fn stage_copy(layout: &Layout, source: &Path, validated: &ValidatedPackage, staging: &Path) -> Result<PublishedCopy> {
    let mut copied: BTreeMap<PathBuf, String> = BTreeMap::new();
    for relative in &validated.files {
        let mut input = open_package_file(source, relative)?;
        let destination = staging.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        request_owner_only_mode(&mut options);
        let mut output = options
            .open(&destination)
            .with_context(|| format!("create {}", destination.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
        loop {
            let read = input
                .read(&mut buffer)
                .with_context(|| format!("read {} from {}", relative.display(), source.display()))?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .with_context(|| format!("write {}", destination.display()))?;
            hasher.update(&buffer[..read]);
        }
        sync_file(&output, SyncMode::Full).with_context(|| format!("flush {}", destination.display()))?;
        copied.insert(relative.clone(), hex::encode(hasher.finalize()));
    }
    let executable = slash_path(&validated.executable)?;
    horon_agent_platform::set_permissions_executable(&staging.join(&validated.executable))
        .with_context(|| format!("mark {} executable", validated.executable.display()))?;
    sync_tree(staging)?;

    // Validate the copy the host will actually run, then hash those same bytes.
    let staged = validate_package(staging).context("the copied package does not validate")?;
    ensure!(
        staged.kind == validated.kind
            && staged.extension.id == validated.extension.id
            && staged.extension.version == validated.extension.version
            && staged.display_name == validated.display_name,
        "the package changed while it was being copied"
    );
    ensure!(
        staged.files == validated.files && staged.executable == validated.executable,
        "the package file list changed while it was being copied"
    );
    let mut files = Vec::with_capacity(copied.len());
    for relative in &validated.files {
        let expected = copied
            .get(relative)
            .with_context(|| format!("{} was not copied", relative.display()))?;
        let (len, sha256) = hash_file(&staging.join(relative))?;
        ensure!(
            &sha256 == expected,
            "the copy of {} changed before it was published",
            relative.display()
        );
        files.push(StoredFile {
            path: slash_path(relative)?,
            len,
            sha256,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let digest = package_digest(
        validated.kind,
        &validated.extension.id,
        &validated.extension.version,
        &files,
    );
    let published = layout.store().join(&digest);
    if published.exists() {
        ensure_existing_copy(&published, &digest, &files)?;
        remove_tree(staging)?;
    } else {
        freeze_tree(staging)?;
        fs::rename(staging, &published)
            .with_context(|| format!("publish the package copy at {}", published.display()))?;
        // Published: an error from here on leaves this copy in place.
        sync_dir(&layout.store(), SyncMode::Full).with_context(|| format!("flush {}", layout.store().display()))?;
    }
    Ok(PublishedCopy {
        digest,
        files,
        executable,
    })
}

fn ensure_existing_copy(published: &Path, digest: &str, files: &[StoredFile]) -> Result<()> {
    let existing = hash_tree(published)?;
    ensure!(
        existing.len() == files.len(),
        "stored copy {digest} already exists with a different file list; remove it before installing"
    );
    for file in files {
        let stored = existing
            .get(&file.path)
            .with_context(|| format!("stored copy {digest} already exists without {}", file.path))?;
        ensure!(
            stored == file,
            "stored copy {digest} already exists with different bytes at {}",
            file.path
        );
    }
    Ok(())
}

fn package_digest(kind: PackageKind, id: &str, version: &str, files: &[StoredFile]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"horon-package-copy-v1\n");
    hasher.update(match kind {
        PackageKind::Extension => b"extension".as_slice(),
        PackageKind::Plugin => b"plugin".as_slice(),
    });
    hasher.update(b"\n");
    for value in [id, version] {
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    }
    for file in files {
        hasher.update(file.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(file.len.to_be_bytes());
        hasher.update(b"\0");
        hasher.update(file.sha256.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

/// Whether `digest` is a lowercase hexadecimal SHA-256 name.
pub(super) fn is_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The directory name of a published copy, rejecting anything that is not a
/// digest before it is joined onto a path.
pub(super) fn digest_dir(layout: &Layout, digest: &str) -> Result<PathBuf> {
    ensure!(is_digest(digest), "invalid stored copy digest: {digest}");
    Ok(layout.store().join(digest))
}

/// Hash every regular file under `root`, keyed by its slash-separated path.
///
/// A link anywhere in the tree is refused instead of followed.
pub(super) fn hash_tree(root: &Path) -> Result<BTreeMap<String, StoredFile>> {
    let mut files = BTreeMap::new();
    walk_tree(root, root, &mut files)?;
    Ok(files)
}

fn walk_tree(root: &Path, directory: &Path, files: &mut BTreeMap<String, StoredFile>) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry.with_context(|| format!("read {}", directory.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
        ensure!(
            !is_path_link(&metadata),
            "links are not allowed in stored packages: {}",
            path.display()
        );
        if metadata.is_dir() {
            walk_tree(root, &path, files)?;
        } else {
            ensure!(
                metadata.is_file(),
                "unexpected entry in a stored package: {}",
                path.display()
            );
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("{} is outside the package copy {}", path.display(), root.display()))?;
            let relative = slash_path(relative)?;
            let (len, sha256) = hash_file(&path)?;
            ensure!(
                files
                    .insert(
                        relative.clone(),
                        StoredFile {
                            path: relative,
                            len,
                            sha256,
                        }
                    )
                    .is_none(),
                "duplicate stored package path"
            );
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut file = package_open_options()
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut len = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        len += read as u64;
    }
    Ok((len, hex::encode(hasher.finalize())))
}

fn slash_path(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            bail!("package path is not a plain relative path: {}", path.display());
        };
        parts.push(
            part.to_str()
                .with_context(|| format!("package path is not valid UTF-8: {}", path.display()))?,
        );
    }
    Ok(parts.join("/"))
}

fn sync_tree(root: &Path) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        let directory = directories[index].clone();
        index += 1;
        for entry in fs::read_dir(&directory).with_context(|| format!("read {}", directory.display()))? {
            let entry = entry.with_context(|| format!("read {}", directory.display()))?;
            if entry
                .file_type()
                .with_context(|| format!("inspect {}", entry.path().display()))?
                .is_dir()
            {
                directories.push(entry.path());
            }
        }
    }
    for directory in directories.iter().rev() {
        sync_dir(directory, SyncMode::Full).with_context(|| format!("flush {}", directory.display()))?;
    }
    Ok(())
}

/// Make the files of a published copy read-only.
///
/// Directories stay writable so the collector can unlink inside them; the files
/// themselves carry no write bit, so no process can edit the bytes a digest
/// describes without leaving the tree read-only first.
pub(super) fn freeze_tree(root: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(root).with_context(|| format!("inspect {}", root.display()))?;
    ensure!(
        metadata.is_dir() && !is_path_link(&metadata),
        "published package copy is not a directory: {}",
        root.display()
    );
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        let directory = directories[index].clone();
        index += 1;
        for entry in fs::read_dir(&directory).with_context(|| format!("read {}", directory.display()))? {
            let entry = entry.with_context(|| format!("read {}", directory.display()))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
            ensure!(
                !is_path_link(&metadata),
                "links are not allowed in stored packages: {}",
                path.display()
            );
            if metadata.is_dir() {
                directories.push(path);
            } else {
                set_read_only(&path)?;
            }
        }
    }
    Ok(())
}

fn set_read_only(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)
        .with_context(|| format!("inspect {}", path.display()))?
        .permissions();
    if !permissions.readonly() {
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).with_context(|| format!("restrict {}", path.display()))?;
    }
    Ok(())
}

/// Remove a stored tree, clearing read-only files first.
pub(super) fn remove_tree(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if metadata.is_dir() && !is_path_link(&metadata) {
        for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
            let entry = entry.with_context(|| format!("read {}", path.display()))?;
            remove_tree(&entry.path())?;
        }
        fs::remove_dir(path).with_context(|| format!("remove {}", path.display()))
    } else {
        if !is_path_link(&metadata) {
            make_owner_writable(path, metadata.permissions())
                .with_context(|| format!("restore write access to {}", path.display()))?;
        }
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
    }
}

/// A pinned set of copy digests, written next to the lock that proves a reader
/// is alive.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinFile {
    version: u32,
    /// Random token identifying the lease that created the directory.
    token: String,
    digests: Vec<String>,
}

/// The outcome of scanning reader references.
pub(super) struct ReferenceScan {
    pub pinned: BTreeSet<String>,
    pub removed: usize,
}

/// Create the lease directory for `owner` and take its lock.
///
/// The caller holds the catalog lock, so reference creation, pinning and
/// removal are serialized against each other and against collection. The lock
/// taken here is held for the lease's lifetime; a directory whose lock can be
/// taken belongs to a reader that is gone.
pub(super) fn create_reference(layout: &Layout, owner: &str) -> Result<InstanceLock> {
    validate_owner(owner)?;
    horon_agent_perf_counters::record_package_area_write();
    fs::create_dir_all(layout.refs())
        .with_context(|| format!("create the package references directory {}", layout.refs().display()))?;
    set_dir_permissions_owner_only(&layout.refs())
        .with_context(|| format!("restrict {} to its owner", layout.refs().display()))?;
    let directory = layout.refs().join(owner);
    for _ in 0..3 {
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                match lock_instance_file(&directory.join(LEASE_LOCK))? {
                    // The previous reader is gone; its directory is garbage.
                    Some(lock) => {
                        drop(lock);
                        remove_tree(&directory)?;
                        continue;
                    }
                    None => bail!("package reference '{owner}' is held by another live process"),
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("create {}", directory.display()));
            }
        }
        set_dir_permissions_owner_only(&directory)
            .with_context(|| format!("restrict {} to its owner", directory.display()))?;
        match lock_instance_file(&directory.join(LEASE_LOCK))? {
            Some(lock) => return Ok(lock),
            // The fresh directory disappeared between creation and locking.
            None => {
                remove_tree(&directory)?;
            }
        }
    }
    bail!("could not take the package reference for '{owner}'")
}

/// Replace the pinned digest set of a held reference.
pub(super) fn write_pin(layout: &Layout, owner: &str, token: &str, digests: &BTreeSet<String>) -> Result<()> {
    validate_owner(owner)?;
    horon_agent_perf_counters::record_package_area_write();
    let directory = layout.refs().join(owner);
    let document = PinFile {
        version: PIN_VERSION,
        token: token.to_owned(),
        digests: digests.iter().cloned().collect(),
    };
    let bytes = serde_json::to_vec(&document).context("serialize the package reference")?;
    let mut temp = NamedTempFile::with_prefix_in(".pinned-", &directory)
        .with_context(|| format!("create a temporary reference in {}", directory.display()))?;
    temp.write_all(&bytes)
        .with_context(|| format!("write {}", temp.path().display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("flush {}", temp.path().display()))?;
    horon_agent_platform::fs::atomic_replace(temp.path(), &directory.join(PIN_FILE))
        .with_context(|| format!("replace {}", directory.join(PIN_FILE).display()))?;
    drop(temp);
    Ok(())
}

/// Read a reference directory's pin file, treating an absent file as a holder
/// that has not pinned anything yet.
fn read_pin(directory: &Path) -> Result<Option<PinFile>> {
    let bytes = match fs::read(directory.join(PIN_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read the package reference {}", directory.display()));
        }
    };
    let document: PinFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse the package reference {}", directory.display()))?;
    ensure!(
        document.version == PIN_VERSION,
        "unsupported package reference version {} in {}",
        document.version,
        directory.display()
    );
    Ok(Some(document))
}

/// The token of the lease that currently owns `owner`'s reference directory.
pub(super) fn reference_token(layout: &Layout, owner: &str) -> Result<String> {
    validate_owner(owner)?;
    let directory = layout.refs().join(owner);
    let pin = read_pin(&directory)?.with_context(|| format!("package reference '{owner}' no longer exists"))?;
    Ok(pin.token)
}

/// Collect the pinned digests of every live reader and remove the directories
/// of readers that are gone.
pub(super) fn scan_references(layout: &Layout) -> Result<ReferenceScan> {
    let mut pinned = BTreeSet::new();
    let mut removed = 0;
    let refs = layout.refs();
    if !refs.exists() {
        return Ok(ReferenceScan { pinned, removed });
    }
    horon_agent_perf_counters::record_package_dir_scan();
    for entry in fs::read_dir(&refs).with_context(|| format!("read {}", refs.display()))? {
        let entry = entry.with_context(|| format!("read {}", refs.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
        ensure!(
            metadata.is_dir() && !is_path_link(&metadata),
            "unexpected entry in the package references: {}",
            path.display()
        );
        match lock_instance_file(&path.join(LEASE_LOCK))? {
            Some(lock) => {
                drop(lock);
                remove_tree(&path)?;
                removed += 1;
            }
            None => {
                // A holder that has taken its lock but not yet written a pin
                // set protects nothing; reading the catalog pins before use.
                if let Some(document) = read_pin(&path)? {
                    pinned.extend(document.digests);
                }
            }
        }
    }
    Ok(ReferenceScan { pinned, removed })
}

/// Reject anything that is not a plain lowercase file name.
///
/// Upper-case letters, dots and Windows device names are refused because they
/// alias another path on at least one supported host; only `a-z`, `0-9`, `-` and
/// `_` survive, with an alphanumeric first byte.
pub(super) fn validate_owner(owner: &str) -> Result<()> {
    let bytes = owner.as_bytes();
    ensure!(
        (1..=64).contains(&bytes.len())
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(byte)),
        "invalid package reference owner: {owner}"
    );
    ensure!(
        !is_windows_device_name(owner),
        "package reference owner is a reserved Windows device name: {owner}"
    );
    Ok(())
}

fn is_windows_device_name(owner: &str) -> bool {
    let upper = owner.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    ["COM", "LPT"].into_iter().any(|prefix| {
        upper
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
    })
}

/// Remove this lease's directory, provided it still carries the lease's token.
///
/// The caller holds the catalog lock, which serializes reference creation and
/// removal, so a directory carrying another token belongs to a newer holder and
/// is left alone. Without a readable pin file the directory is left for the
/// next collection to reclaim.
pub(super) fn discard_lease(layout: &Layout, owner: &str, token: &str) -> Result<()> {
    if validate_owner(owner).is_err() {
        return Ok(());
    }
    let directory = layout.refs().join(owner);
    let Some(pin) = read_pin(&directory)? else {
        return Ok(());
    };
    if pin.token != token {
        return Ok(());
    }
    remove_tree(&directory)
}

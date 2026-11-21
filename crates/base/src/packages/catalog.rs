//! The installed-package catalog: one immutable copy per install plus one
//! `installed.json` that names the current generation.
//!
//! Every mutation takes the store's exclusive lock, validates a staged copy of
//! the package, then replaces `installed.json` in one rename. A failure before
//! that rename leaves the previous catalog and every copy it names untouched;
//! after it, the new generation may be the one on disk, so nothing is rolled
//! back. A reader that pins a generation keeps its copy alive until the last
//! reference is gone.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use horon_agent_extension_types::PackageKind;
use horon_agent_platform::SyncMode;
use horon_agent_platform::ipc::InstanceLock;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use super::manifest::{ValidatedPackage, validate_package};
use super::store::{self, CATALOG_TEMP_PREFIX, Layout, PublishedCopy};

/// The on-disk catalog format the host understands.
pub const CATALOG_VERSION: u32 = 1;

/// Where a package may be enabled.
///
/// `Project` stores one canonicalized root: two spellings of the same directory
/// resolve to the same scope, and the catalog never keeps two entries for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PackageScope {
    Global,
    Project { root: String },
}

impl PackageScope {
    /// Canonicalize `root` into a project scope.
    pub fn project(root: impl AsRef<Path>) -> Result<Self> {
        let root = root
            .as_ref()
            .canonicalize()
            .with_context(|| format!("resolve the project root {}", root.as_ref().display()))?;
        ensure!(root.is_dir(), "project root is not a directory: {}", root.display());
        let root = root
            .to_str()
            .with_context(|| format!("project root is not valid UTF-8: {}", root.display()))?;
        Ok(Self::Project { root: root.to_owned() })
    }

    /// The canonical project root, when this scope is project-local.
    pub fn project_root(&self) -> Option<&Path> {
        match self {
            Self::Project { root } => Some(Path::new(root)),
            Self::Global => None,
        }
    }

    pub fn is_global(&self) -> bool {
        matches!(self, Self::Global)
    }
}

/// What the host may do with a recorded package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageState {
    /// The package loads and its executable may be started.
    Installed,
    /// The package stays on disk but does not load.
    Disabled,
    /// The package was removed. The record stays so its ID cannot be claimed
    /// without an explicit replace, and it names no copy.
    Revoked,
}

/// One published file of a copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredFile {
    /// Slash-separated path relative to the copy root.
    pub path: String,
    pub len: u64,
    pub sha256: String,
}

/// One package as the catalog records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRecord {
    pub kind: PackageKind,
    pub id: String,
    pub version: String,
    pub name: String,
    /// Canonical directory the package was installed from.
    pub source: String,
    pub scope: PackageScope,
    pub state: PackageState,
    /// Catalog generation this record was last written at.
    pub generation: u64,
    /// Directory name under `store/`, absent once the package is removed.
    pub digest: Option<String>,
    /// Slash-separated path of the host executable inside the copy.
    pub executable: Option<String>,
    /// Files of the copy, sorted by path.
    pub files: Vec<StoredFile>,
}

/// The catalog as a reader sees it at one generation.
#[derive(Debug, Clone)]
pub struct CatalogView {
    generation: u64,
    packages: BTreeMap<String, PackageRecord>,
}

impl CatalogView {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn packages(&self) -> impl Iterator<Item = &PackageRecord> + '_ {
        self.packages.values()
    }

    pub fn get(&self, id: &str) -> Option<&PackageRecord> {
        self.packages.get(id)
    }

    /// Recorded packages that are not removed.
    pub fn active(&self) -> impl Iterator<Item = &PackageRecord> + '_ {
        self.packages
            .values()
            .filter(|record| record.state != PackageState::Revoked)
    }

    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }
}

/// What to install, and whether an existing record with the same ID may be
/// replaced.
#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub source: PathBuf,
    pub scope: PackageScope,
    pub replace: bool,
    /// ID the package must declare, checked on the validated source before any
    /// copy is published. The inbox uses it so a directory named after one ID
    /// cannot install a manifest that declares another.
    expected_id: Option<String>,
}

impl InstallRequest {
    pub fn new(source: impl Into<PathBuf>, scope: PackageScope) -> Self {
        Self {
            source: source.into(),
            scope,
            replace: false,
            expected_id: None,
        }
    }

    /// Permit replacing the record that already holds this package's ID.
    pub fn replacing(mut self) -> Self {
        self.replace = true;
        self
    }

    /// Require the package to declare `id`.
    pub fn expecting(mut self, id: impl Into<String>) -> Self {
        self.expected_id = Some(id.into());
        self
    }
}

/// The result of an integrity check of every published copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub generation: u64,
    pub packages: usize,
}

/// What a collection pass removed, kept or could not reclaim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionReport {
    /// Copies no record names and no live reader pinned.
    pub collected: Vec<String>,
    /// Unreferenced copies kept because a live reader pinned them.
    pub retained: Vec<String>,
    /// Reference directories whose holder had exited.
    pub removed_stale_references: usize,
    /// Staging directories left by an interrupted install.
    pub removed_staging_directories: usize,
    /// Catalog temporaries left by an interrupted publication.
    pub removed_catalog_temporaries: usize,
}

/// A live reader of published copies.
///
/// The lease holds an operating-system lock for as long as it exists, so a
/// collector in another process can tell a session that is still using a copy
/// from one that crashed. Pinning records the digests the reader may open.
pub struct PackageLease {
    root: PathBuf,
    owner: String,
    token: String,
    lock: Option<InstanceLock>,
    pinned: BTreeSet<String>,
}

impl PackageLease {
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The digests this lease protects, sorted.
    pub fn pinned(&self) -> Vec<String> {
        self.pinned.iter().cloned().collect()
    }

    /// Replace the protected digest set.
    ///
    /// The catalog lock is held while the reference is checked and the pin is
    /// written, so a collection in another process can never observe the
    /// reference directory between the two steps. A caller pins before it opens
    /// a copy; [`PackageStore::snapshot`] does both under one lock.
    pub fn pin(&mut self, digests: impl IntoIterator<Item = String>) -> Result<()> {
        let layout = Layout::new(&self.root);
        let _lock = store::lock(&layout)?;
        self.pin_locked(&layout, digests.into_iter().collect())
    }

    /// Write a pin while the caller already holds the catalog lock.
    ///
    /// Every digest must name a copy that is on disk right now: installs,
    /// removals and collection are serialized by the same lock, so a digest
    /// that passes this check stays readable until the pin is released.
    pub(super) fn pin_locked(&mut self, layout: &Layout, next: BTreeSet<String>) -> Result<()> {
        for digest in &next {
            ensure!(store::is_digest(digest), "invalid stored copy digest: {digest}");
            ensure!(
                store::digest_dir(layout, digest)?.is_dir(),
                "stored copy {digest} is missing"
            );
        }
        let current = store::reference_token(layout, &self.owner)?;
        ensure!(
            current == self.token,
            "package reference '{}' was replaced by another lease",
            self.owner
        );
        store::write_pin(layout, &self.owner, &self.token, &next)?;
        self.pinned = next;
        Ok(())
    }
}

impl Drop for PackageLease {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            drop(lock);
        }
        if !self.root.exists() {
            return;
        }
        let layout = Layout::new(&self.root);
        // The token check below only means anything while no other process can
        // create or remove a reference, so the catalog lock covers it.
        match store::lock(&layout) {
            Ok(_lock) => {
                if let Err(error) = store::discard_lease(&layout, &self.owner, &self.token) {
                    eprintln!(
                        "horon-agent: could not release the package reference '{}': {error}",
                        self.owner
                    );
                }
            }
            Err(error) => eprintln!(
                "horon-agent: could not lock the package catalog to release reference '{}': {error}",
                self.owner
            ),
        }
    }
}

/// The package area of one horon home.
#[derive(Debug, Clone)]
pub struct PackageStore {
    root: PathBuf,
}

impl PackageStore {
    /// A store rooted at the directory that holds `installed.json`, `store/`,
    /// `refs/` and `inbox/`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store inside a horon home: `$HORON_HOME/packages`.
    pub fn for_horon_home(home: impl AsRef<Path>) -> Self {
        Self::new(home.as_ref().join("packages"))
    }

    /// The store of the current process's horon home.
    pub fn for_current_user() -> Result<Self> {
        Ok(Self::for_horon_home(crate::storage::horon_agent_dir()?))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn layout(&self) -> Layout {
        Layout::new(&self.root)
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.layout().catalog()
    }

    pub fn store_dir(&self) -> PathBuf {
        self.layout().store()
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.layout().inbox()
    }

    /// Install `request.source` and publish its copy.
    ///
    /// The source is validated first, the staged copy is validated again from
    /// the copied bytes, and the catalog is replaced only after the copy is
    /// complete. A failure before the catalog rename leaves the previous
    /// catalog and the copies it names exactly as they were; a failure after it
    /// is reported and both generations' copies are kept.
    pub fn install(&self, request: InstallRequest) -> Result<PackageRecord> {
        let source = request
            .source
            .canonicalize()
            .with_context(|| format!("resolve the package source {}", request.source.display()))?;
        ensure!(
            source.is_dir(),
            "package source is not a directory: {}",
            source.display()
        );
        let validated = validate_package(&source)?;
        if let Some(expected) = request.expected_id.as_deref() {
            ensure!(
                validated.extension.id == expected,
                "package declares ID {}, expected {expected}",
                validated.extension.id
            );
        }
        let layout = self.layout();
        let _lock = store::lock(&layout)?;
        let mut catalog = read_catalog(&layout)?;
        let id = validated.extension.id.clone();
        if let Some(previous) = catalog.packages.get(&id) {
            ensure!(
                request.replace,
                "package {id} {}; pass replace to install over it",
                match previous.state {
                    PackageState::Installed => "is already installed",
                    PackageState::Disabled => "is already installed and disabled",
                    PackageState::Revoked => "was removed",
                }
            );
        }
        let published = store::publish_copy(&layout, &source, &validated)?;
        let generation = catalog
            .generation
            .checked_add(1)
            .context("the package catalog generation overflowed")?;
        let record = record_for(&source, request.scope, generation, &validated, &published)?;
        catalog.packages.insert(id, record.clone());
        catalog.generation = generation;
        write_catalog(&layout, &catalog)?;
        Ok(record)
    }

    /// Stop loading a package without touching its copy.
    pub fn disable(&self, id: &str) -> Result<u64> {
        self.set_state(id, PackageState::Disabled)
    }

    /// Load a disabled package again.
    pub fn enable(&self, id: &str) -> Result<u64> {
        self.set_state(id, PackageState::Installed)
    }

    fn set_state(&self, id: &str, target: PackageState) -> Result<u64> {
        let layout = self.layout();
        let _lock = store::lock(&layout)?;
        let mut catalog = read_catalog(&layout)?;
        let generation = catalog
            .generation
            .checked_add(1)
            .context("the package catalog generation overflowed")?;
        let current = catalog
            .packages
            .get_mut(id)
            .with_context(|| format!("package {id} is not in the catalog"))?;
        ensure!(
            current.state != PackageState::Revoked,
            "package {id} was removed; install it again"
        );
        if current.state == target {
            return Ok(catalog.generation);
        }
        ensure!(
            current.digest.is_some() && current.executable.is_some(),
            "package {id} has no published copy to switch"
        );
        current.state = target;
        current.generation = generation;
        catalog.generation = generation;
        write_catalog(&layout, &catalog)?;
        Ok(generation)
    }

    /// Remove a package. Its copy becomes unclaimed and the next collection
    /// reclaims it once no live reader pins it.
    pub fn remove(&self, id: &str) -> Result<u64> {
        let layout = self.layout();
        let _lock = store::lock(&layout)?;
        let mut catalog = read_catalog(&layout)?;
        let generation = catalog
            .generation
            .checked_add(1)
            .context("the package catalog generation overflowed")?;
        let current = catalog
            .packages
            .get_mut(id)
            .with_context(|| format!("package {id} is not in the catalog"))?;
        ensure!(
            current.state != PackageState::Revoked,
            "package {id} is already removed"
        );
        current.state = PackageState::Revoked;
        current.generation = generation;
        current.digest = None;
        current.executable = None;
        current.files.clear();
        catalog.generation = generation;
        write_catalog(&layout, &catalog)?;
        Ok(generation)
    }

    /// Read the catalog without taking the lock or pinning anything.
    pub fn catalog(&self) -> Result<CatalogView> {
        let layout = self.layout();
        Ok(read_catalog(&layout)?.view())
    }

    /// Take a live reader reference.
    ///
    /// Two live leases with the same owner cannot coexist; a lease left by a
    /// process that exited is reclaimed here.
    pub fn lease(&self, owner: &str) -> Result<PackageLease> {
        let layout = self.layout();
        let _lock = store::lock(&layout)?;
        let live = store::create_reference(&layout, owner)?;
        let token = format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>());
        store::write_pin(&layout, owner, &token, &BTreeSet::new())?;
        Ok(PackageLease {
            root: self.root.clone(),
            owner: owner.to_owned(),
            token,
            lock: Some(live),
            pinned: BTreeSet::new(),
        })
    }

    /// Read the catalog and pin every copy it names, so a concurrent
    /// replacement cannot reclaim them before the caller finishes.
    pub fn snapshot(&self, lease: &mut PackageLease) -> Result<CatalogView> {
        ensure!(
            lease.root == self.root,
            "package reference '{}' belongs to a different package store",
            lease.owner
        );
        let layout = self.layout();
        let _lock = store::lock(&layout)?;
        let view = read_catalog(&layout)?.view();
        let digests: BTreeSet<String> = view
            .packages
            .values()
            .filter_map(|record| record.digest.clone())
            .collect();
        lease.pin_locked(&layout, digests)?;
        Ok(view)
    }

    /// Read the catalog and pin only the copy `id` names.
    ///
    /// A launch pins the one copy it is about to run instead of every installed
    /// copy, so starting a package does not keep the whole catalog alive. The
    /// lease is checked against this store exactly as
    /// [`PackageStore::snapshot`] checks it.
    pub(super) fn pin_package(&self, lease: &mut PackageLease, id: &str) -> Result<CatalogView> {
        ensure!(
            lease.root == self.root,
            "package reference '{}' belongs to a different package store",
            lease.owner
        );
        let layout = self.layout();
        let _lock = store::lock(&layout)?;
        let view = read_catalog(&layout)?.view();
        let pinned: BTreeSet<String> = view
            .get(id)
            .and_then(|record| record.digest.clone())
            .into_iter()
            .collect();
        lease.pin_locked(&layout, pinned)?;
        Ok(view)
    }

    /// Check every published copy against the hashes recorded with it.
    ///
    /// This writes nothing: a mismatch or a missing file is reported with the
    /// record and path that disagree, and the damaged bytes are left in place
    /// for an operator to inspect.
    pub fn verify(&self) -> Result<VerificationReport> {
        let layout = self.layout();
        let catalog = read_catalog(&layout)?;
        let mut verified = 0;
        for (id, record) in &catalog.packages {
            ensure!(
                &record.id == id,
                "the catalog key {id} does not match its record ID {}",
                record.id
            );
            if record.state == PackageState::Revoked {
                ensure!(
                    record.digest.is_none() && record.files.is_empty() && record.executable.is_none(),
                    "removed package {id} still names a copy"
                );
                continue;
            }
            let digest = record
                .digest
                .as_ref()
                .with_context(|| format!("package {id} has no published copy"))?;
            let directory = store::digest_dir(&layout, digest)?;
            ensure!(
                directory.is_dir(),
                "stored copy {digest} for package {id} is missing: {}",
                directory.display()
            );
            ensure!(
                record.files.windows(2).all(|pair| pair[0].path < pair[1].path),
                "stored package {id} does not list its files in sorted order"
            );
            let published = store::hash_tree(&directory)?;
            for file in &record.files {
                ensure!(
                    published.contains_key(&file.path),
                    "stored copy {digest} for package {id} is missing {}",
                    file.path
                );
            }
            ensure!(
                published.len() == record.files.len(),
                "stored copy {digest} for package {id} holds {} files, recorded {}",
                published.len(),
                record.files.len()
            );
            for file in &record.files {
                let stored = published
                    .get(&file.path)
                    .with_context(|| format!("stored copy {digest} for package {id} is missing {}", file.path))?;
                ensure!(
                    stored.len == file.len && stored.sha256 == file.sha256,
                    "stored package {id}: {} is {} bytes with sha256 {}, recorded {} bytes with {}",
                    file.path,
                    stored.len,
                    stored.sha256,
                    file.len,
                    file.sha256
                );
            }
            let executable = record
                .executable
                .as_ref()
                .with_context(|| format!("package {id} has no recorded executable"))?;
            ensure!(
                published.contains_key(executable),
                "stored copy {digest} for package {id} does not contain its executable {executable}"
            );
            ensure!(
                horon_agent_platform::fs::is_executable(&directory.join(executable)),
                "stored executable for package {id} is not runnable: {executable}"
            );
            verified += 1;
        }
        Ok(VerificationReport {
            generation: catalog.generation,
            packages: verified,
        })
    }

    /// Reclaim copies that no record names and no live reader pins.
    ///
    /// Staging directories and catalog temporaries from an interrupted install
    /// are removed under the same lock that proves their writer is gone.
    pub fn collect(&self) -> Result<CollectionReport> {
        if !self.root.exists() {
            return Ok(CollectionReport::default());
        }
        let layout = self.layout();
        let _lock = store::lock(&layout)?;
        let catalog = read_catalog(&layout)?;
        let referenced: BTreeSet<String> = catalog
            .packages
            .values()
            .filter_map(|record| record.digest.clone())
            .collect();
        let references = store::scan_references(&layout)?;
        let entries = store_entries(&layout)?;
        for entry in &entries {
            ensure!(
                entry.starts_with(store::STAGING_PREFIX) || store::is_digest(entry),
                "unexpected entry in the package store: {entry}"
            );
        }
        let mut report = CollectionReport {
            removed_stale_references: references.removed,
            ..CollectionReport::default()
        };
        report.removed_staging_directories = remove_staging(&layout, &entries)?;
        report.removed_catalog_temporaries = remove_catalog_temporaries(&layout)?;
        for digest in entries {
            if digest.starts_with(store::STAGING_PREFIX) || referenced.contains(&digest) {
                continue;
            }
            if references.pinned.contains(&digest) {
                report.retained.push(digest);
            } else {
                store::remove_tree(&layout.store().join(&digest))?;
                report.collected.push(digest);
            }
        }
        report.collected.sort();
        report.retained.sort();
        Ok(report)
    }
}

fn store_entries(layout: &Layout) -> Result<Vec<String>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(layout.store()).with_context(|| format!("read {}", layout.store().display()))? {
        let entry = entry.with_context(|| format!("read {}", layout.store().display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
        ensure!(
            metadata.is_dir() && !horon_agent_platform::fs::is_path_link(&metadata),
            "unexpected file in the package store: {}",
            path.display()
        );
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("package store entry is not valid UTF-8: {}", path.display()))?;
        entries.push(name);
    }
    Ok(entries)
}

fn remove_staging(layout: &Layout, entries: &[String]) -> Result<usize> {
    let mut removed = 0;
    for name in entries.iter().filter(|name| name.starts_with(store::STAGING_PREFIX)) {
        store::remove_tree(&layout.store().join(name))?;
        removed += 1;
    }
    Ok(removed)
}

fn remove_catalog_temporaries(layout: &Layout) -> Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(layout.root()).with_context(|| format!("read {}", layout.root().display()))? {
        let entry = entry.with_context(|| format!("read {}", layout.root().display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(CATALOG_TEMP_PREFIX) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
        ensure!(
            metadata.is_file() && !horon_agent_platform::fs::is_path_link(&metadata),
            "unexpected catalog temporary: {}",
            path.display()
        );
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

fn record_for(
    source: &Path,
    scope: PackageScope,
    generation: u64,
    validated: &ValidatedPackage,
    published: &PublishedCopy,
) -> Result<PackageRecord> {
    Ok(PackageRecord {
        kind: validated.kind,
        id: validated.extension.id.clone(),
        version: validated.extension.version.clone(),
        name: validated.display_name.clone(),
        source: source
            .to_str()
            .with_context(|| format!("package source is not valid UTF-8: {}", source.display()))?
            .to_owned(),
        scope,
        state: PackageState::Installed,
        generation,
        digest: Some(published.digest.clone()),
        executable: Some(published.executable.clone()),
        files: published.files.clone(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    version: u32,
    generation: u64,
    #[serde(default)]
    packages: BTreeMap<String, PackageRecord>,
}

impl Default for CatalogFile {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            generation: 0,
            packages: BTreeMap::new(),
        }
    }
}

impl CatalogFile {
    fn view(self) -> CatalogView {
        CatalogView {
            generation: self.generation,
            packages: self.packages,
        }
    }
}

fn read_catalog(layout: &Layout) -> Result<CatalogFile> {
    let path = layout.catalog();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CatalogFile::default());
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let catalog: CatalogFile = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    ensure!(
        catalog.version == CATALOG_VERSION,
        "unsupported package catalog version {} in {} (expected {CATALOG_VERSION})",
        catalog.version,
        path.display()
    );
    Ok(catalog)
}

/// Replace `installed.json` with `catalog` in one step.
///
/// The bytes are written and flushed to a same-directory temporary first; only
/// then is the name switched, and the directory entry is flushed after.
///
/// The rename is the exact boundary between a recoverable and an unrecoverable
/// publication. Before it, every error leaves the previous catalog authoritative
/// and the new copy unreferenced. After it, the new catalog may already be the
/// one on disk, so an error from the directory flush is reported but nothing is
/// rolled back: every copy either catalog revision names is kept, and only an
/// explicit collection reclaims the ones no record and no live pin names any
/// more.
fn write_catalog(layout: &Layout, catalog: &CatalogFile) -> Result<()> {
    let bytes = serde_json::to_vec(catalog).context("serialize the package catalog")?;
    let mut temp = NamedTempFile::with_prefix_in(CATALOG_TEMP_PREFIX, layout.root())
        .with_context(|| format!("create a temporary catalog in {}", layout.root().display()))?;
    let mut buffer = bytes.as_slice();
    while !buffer.is_empty() {
        let written = temp
            .write(buffer)
            .with_context(|| format!("write {}", temp.path().display()))?;
        ensure!(written > 0, "the catalog temporary accepted no bytes");
        buffer = &buffer[written..];
    }
    temp.as_file()
        .sync_all()
        .with_context(|| format!("flush {}", temp.path().display()))?;
    horon_agent_platform::fs::atomic_replace(temp.path(), &layout.catalog())
        .with_context(|| format!("replace {}", layout.catalog().display()))?;
    drop(temp);
    horon_agent_platform::sync_dir(layout.root(), SyncMode::Full)
        .with_context(|| format!("flush {}", layout.root().display()))?;
    Ok(())
}

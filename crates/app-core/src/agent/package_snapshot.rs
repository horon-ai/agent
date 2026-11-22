//! The package generation a session's turn observes.
//!
//! A session captures one [`PackageSnapshot`] at its turn boundary and works
//! from that value for the whole turn: the packages it may list, load and start
//! are the ones the snapshot pinned, so a catalog replacement that lands mid-turn
//! cannot change what the running turn sees. [`PackageSession::enter_turn`]
//! returns the same `Arc` when the catalog generation and the normalized
//! workspace did not move, so an unchanged turn runs one generation check and
//! re-pins nothing; the arc identity is the proof (no counter, no test-only
//! accessor).
//!
//! Two catalog sources feed the generation check:
//!
//! - A daemon session reads the generation the daemon publishes on its watch
//!   channel, so a turn reads no package file at all.
//! - A standalone `horon run` reads `installed.json` under the store's own file
//!   lock (`PackageStore::snapshot`), the same path the daemon uses.
//!
//! The snapshot owns a [`PackageLease`] and pins every copy it captured, so the
//! store keeps those copies until the last reference is released: a replacement
//! install and a removal both reclaim managed copies only after the running turns
//! that captured them have finished.
//!
//! Revocation is not replacement. [`PackageHandle::ensure_live`] re-reads the
//! catalog for a pending request and refuses a package that is no longer
//! installed, that was revoked after the handle's own generation (even if it was
//! installed again since), or that is scoped to another project workspace. A
//! replaced package stays `Installed`, so a running turn keeps working from the
//! copy it pinned.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use anyhow::{Context, Result, ensure};
use tokio::sync::watch;

use crate::extensions::lifecycle;
use crate::packages::{CatalogView, PackageLease, PackageRecord, PackageScope, PackageState, PackageStore};
use crate::server::package_control::PublishedCatalog;

/// One immutable view of the catalog, taken at one generation for one workspace.
///
/// Only packages the session may load are listed: globally scoped packages and
/// project packages whose canonical root is the session's normalized workspace.
/// Disabled and removed packages are absent, which is what refuses a new load
/// from any session after `horon package disable` or `remove`.
pub struct PackageSnapshot {
    generation: u64,
    workspace: Option<PathBuf>,
    packages: BTreeMap<String, PackageRecord>,
    /// Pins every copy this snapshot captured. `None` when there is nothing to
    /// pin: the package area does not exist yet, or the catalog is empty. A
    /// package-free home therefore never grows a reference directory.
    lease: Option<PackageLease>,
}

impl PackageSnapshot {
    fn build(view: CatalogView, lease: Option<PackageLease>, workspace: Option<PathBuf>) -> Self {
        let generation = view.generation();
        let packages = view
            .packages()
            .filter(|record| record.state == PackageState::Installed)
            .filter(|record| in_workspace(&record.scope, workspace.as_deref()))
            .map(|record| (record.id.clone(), record.clone()))
            .collect();
        Self {
            generation,
            workspace,
            packages,
            lease,
        }
    }

    fn empty(generation: u64, workspace: Option<PathBuf>) -> Self {
        Self {
            generation,
            workspace,
            packages: BTreeMap::new(),
            lease: None,
        }
    }

    /// The catalog generation this snapshot was taken at.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The canonical workspace this snapshot was filtered for, when it has one.
    pub fn workspace(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }

    /// Every package this session may load, sorted by ID.
    pub fn packages(&self) -> impl Iterator<Item = &PackageRecord> + '_ {
        self.packages.values()
    }

    pub fn get(&self, package_id: &str) -> Option<&PackageRecord> {
        self.packages.get(package_id)
    }

    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// The copy digests the snapshot keeps alive, sorted.
    pub fn pinned_copies(&self) -> Vec<String> {
        match self.lease.as_ref() {
            Some(lease) => lease.pinned(),
            None => Vec::new(),
        }
    }
}

impl std::fmt::Debug for PackageSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageSnapshot")
            .field("generation", &self.generation)
            .field("workspace", &self.workspace)
            .field("packages", &self.packages.keys())
            .finish()
    }
}

/// A package as the catalog records it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePackage {
    pub id: String,
    pub generation: u64,
    pub state: PackageState,
    /// The canonical root when the package is project-scoped.
    pub scope_root: Option<String>,
}

/// One session's per-turn package state.
///
/// The session keeps the snapshot its last turn captured, so a request that
/// resolves a package after the tool surface locked reaches the same generation
/// once more. The catalog source is chosen per call: a daemon session uses the
/// published generation, and a standalone session reads the catalog file.
pub struct PackageSession {
    store: PackageStore,
    owner: String,
    turn: Mutex<Option<Arc<PackageSnapshot>>>,
}

impl PackageSession {
    /// A session over one package store. `owner` names the lease holder; it is
    /// prefixed onto each generation's reference so two sessions never share one.
    pub fn open(store: PackageStore, owner: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            store,
            owner: owner.into(),
            turn: Mutex::new(None),
        })
    }

    pub fn store(&self) -> &PackageStore {
        &self.store
    }

    /// The snapshot this session's current turn captured, if a turn began.
    pub fn current(&self) -> Option<Arc<PackageSnapshot>> {
        self.lock_turn().as_ref().map(Arc::clone)
    }

    /// Capture this turn's snapshot, or return the one an unchanged catalog
    /// already produced.
    ///
    /// `workspace` is normalized here (canonicalized); two spellings of one
    /// directory therefore select the same project-scoped packages, and a
    /// workspace the host cannot resolve selects only global packages.
    pub async fn enter_turn(&self, workspace: Option<&Path>) -> Result<Arc<PackageSnapshot>> {
        let workspace = normalize_workspace(workspace);
        if !self.store.root().exists() {
            if let Some(cached) = self.cached(&workspace, 0) {
                return Ok(cached);
            }
            return Ok(self.store_turn(Arc::new(PackageSnapshot::empty(0, workspace))));
        }
        if let Some(generation) = published_generation(&self.store) {
            if let Some(cached) = self.cached(&workspace, generation) {
                return Ok(cached);
            }
            return self.acquire(workspace).await;
        }
        let store = self.store.clone();
        let view = tokio::task::spawn_blocking(move || store.catalog())
            .await
            .context("the package catalog read task failed")??;
        if let Some(cached) = self.cached(&workspace, view.generation()) {
            return Ok(cached);
        }
        self.acquire(workspace).await
    }

    /// A handle for one package this turn lists, for a request that resolves it.
    pub fn handle(&self, package_id: &str) -> Option<PackageHandle> {
        let snapshot = self.current()?;
        snapshot.packages.get(package_id)?;
        Some(PackageHandle {
            snapshot,
            package_id: package_id.to_owned(),
        })
    }

    /// Read one package as the catalog records it now.
    ///
    /// This is the check a launch makes before it starts anything: it reads the
    /// catalog file itself instead of the turn's snapshot, so a package that was
    /// disabled a moment ago is refused even inside a turn that captured the
    /// previous generation.
    pub async fn live_package(&self, package_id: &str) -> Result<LivePackage> {
        let store = self.store.clone();
        let id = package_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let view = store.catalog()?;
            let record = view
                .get(&id)
                .with_context(|| format!("package {id} is not in the catalog"))?;
            Ok::<LivePackage, anyhow::Error>(LivePackage {
                id: record.id.clone(),
                generation: view.generation(),
                state: record.state,
                scope_root: scope_root(&record.scope),
            })
        })
        .await
        .context("the package catalog read task failed")?
    }

    async fn acquire(&self, workspace: Option<PathBuf>) -> Result<Arc<PackageSnapshot>> {
        let store = self.store.clone();
        let owner = format!("{}:{}:{}", self.owner, std::process::id(), next_lease_token());
        let acquired = tokio::task::spawn_blocking(move || read_turn(&store, &owner))
            .await
            .context("the package snapshot task failed")??;
        let (generation, acquired) = acquired;
        let snapshot = match acquired {
            Some((view, lease)) => PackageSnapshot::build(view, Some(lease), workspace),
            None => PackageSnapshot::empty(generation, workspace),
        };
        Ok(self.store_turn(Arc::new(snapshot)))
    }

    fn cached(&self, workspace: &Option<PathBuf>, generation: u64) -> Option<Arc<PackageSnapshot>> {
        let turn = self.lock_turn();
        let current = turn.as_ref()?;
        if current.generation() == generation && current.workspace() == workspace.as_deref() {
            return Some(Arc::clone(current));
        }
        None
    }

    fn store_turn(&self, snapshot: Arc<PackageSnapshot>) -> Arc<PackageSnapshot> {
        *self.lock_turn() = Some(Arc::clone(&snapshot));
        snapshot
    }

    fn lock_turn(&self) -> MutexGuard<'_, Option<Arc<PackageSnapshot>>> {
        self.turn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A package this turn listed, held by a request that may outlive the catalog
/// generation it was resolved under.
#[derive(Debug)]
pub struct PackageHandle {
    snapshot: Arc<PackageSnapshot>,
    package_id: String,
}

impl PackageHandle {
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// The generation the handle was resolved at.
    pub fn generation(&self) -> u64 {
        self.snapshot.generation()
    }

    /// The record the handle was resolved from.
    pub fn record(&self) -> Option<&PackageRecord> {
        self.snapshot.get(&self.package_id)
    }

    /// Refuse the handle when the package may no longer load.
    ///
    /// A replacement install leaves the state `Installed`, so a running turn
    /// keeps its copy. A disable or a removal after the handle's generation
    /// refuses it, even when the package was installed again since, and a package
    /// scoped to another project workspace is refused here as well.
    pub async fn ensure_live(&self, session: &PackageSession) -> Result<()> {
        let package_id = self.package_id.as_str();
        let generation = self.snapshot.generation();
        if let Some(event) = lifecycle::PackageLifecycleRegistry::shared().revocation(package_id) {
            ensure!(
                event.generation <= generation,
                "package {package_id} was {} at catalog generation {}; a request from generation {generation} is stale",
                event.cause.as_str(),
                event.generation
            );
        }
        let live = session.live_package(package_id).await?;
        ensure!(
            live.state == PackageState::Installed,
            "package {package_id} is {}; only an installed package loads",
            state_name(live.state)
        );
        if let Some(root) = live.scope_root.as_deref() {
            ensure!(
                self.snapshot.workspace() == Some(Path::new(root)),
                "package {package_id} is scoped to {root}, outside this session's workspace"
            );
        }
        Ok(())
    }
}

/// A daemon's published catalog, tied to the package area it describes.
struct DaemonFeed {
    root: PathBuf,
    receiver: watch::Receiver<Arc<PublishedCatalog>>,
}

static DAEMON_FEEDS: LazyLock<Mutex<Option<DaemonFeed>>> = LazyLock::new(|| Mutex::new(None));

/// Offer the daemon's published generations to the sessions in this process.
///
/// The daemon calls this once, before it starts watching the catalog, so a turn
/// reads one in-memory generation instead of the catalog file. A process that
/// never runs the package control (a standalone `horon run`) keeps reading the
/// store; a session over a different package area ignores the feed.
pub(crate) fn attach_daemon_feed(root: PathBuf, receiver: watch::Receiver<Arc<PublishedCatalog>>) {
    let mut feeds = lock_feeds();
    if feeds.is_none() {
        *feeds = Some(DaemonFeed { root, receiver });
    }
}

fn published_generation(store: &PackageStore) -> Option<u64> {
    let feeds = lock_feeds();
    let feed = feeds.as_ref()?;
    if feed.root != store.root() {
        return None;
    }
    Some(feed.receiver.borrow().generation())
}

fn lock_feeds() -> MutexGuard<'static, Option<DaemonFeed>> {
    DAEMON_FEEDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A lease owner suffix that is unique inside this process.
///
/// The package store keeps one live reference per owner name, so every
/// acquisition needs its own: two sessions that happen to share a `owner`
/// string must not collide on their first reference.
fn next_lease_token() -> u64 {
    static NEXT_LEASE: AtomicU64 = AtomicU64::new(1);
    NEXT_LEASE.fetch_add(1, Ordering::Relaxed)
}

/// Read one turn's catalog under the store lock and pin the copies it names.
///
/// An empty catalog is reported without taking a lease: a session on a home that
/// has no packages yet must not write a reference directory. A non-empty catalog
/// is read with [`PackageStore::snapshot`], which pins every copy in the same
/// critical section that proved the copies are on disk.
fn read_turn(store: &PackageStore, owner: &str) -> Result<(u64, Option<(CatalogView, PackageLease)>)> {
    let probe = store.catalog()?;
    let generation = probe.generation();
    if probe.is_empty() {
        return Ok((generation, None));
    }
    let mut lease = store.lease(owner)?;
    let view = store.snapshot(&mut lease)?;
    let generation = view.generation();
    Ok((generation, Some((view, lease))))
}

fn normalize_workspace(workspace: Option<&Path>) -> Option<PathBuf> {
    let workspace = workspace?;
    match workspace.canonicalize() {
        Ok(canonical) => Some(canonical),
        Err(error) => {
            crate::logging::debug(&format!(
                "package scope cannot resolve the workspace {}: {error}",
                workspace.display()
            ));
            None
        }
    }
}

fn in_workspace(scope: &PackageScope, workspace: Option<&Path>) -> bool {
    match scope {
        PackageScope::Global => true,
        PackageScope::Project { root } => workspace.is_some_and(|workspace| workspace == Path::new(root)),
    }
}

fn scope_root(scope: &PackageScope) -> Option<String> {
    match scope {
        PackageScope::Global => None,
        PackageScope::Project { root } => Some(root.clone()),
    }
}

fn state_name(state: PackageState) -> &'static str {
    match state {
        PackageState::Installed => "installed",
        PackageState::Disabled => "disabled",
        PackageState::Revoked => "removed",
    }
}

static SESSION_SLOTS: LazyLock<Mutex<BTreeMap<String, Arc<PackageSession>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn lock_slots() -> MutexGuard<'static, BTreeMap<String, Arc<PackageSession>>> {
    SESSION_SLOTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Drop one session's package state, called when its agent leaves memory.
///
/// The snapshot the session still holds keeps its lease alive until the last
/// request using it finishes, so a running turn is never cut by this.
pub(crate) fn forget_session(session_id: &str) {
    lock_slots().remove(session_id);
}

fn package_session(session_id: &str) -> Result<Arc<PackageSession>> {
    let mut slots = lock_slots();
    if let Some(existing) = slots.get(session_id) {
        return Ok(Arc::clone(existing));
    }
    let store = PackageStore::for_current_user()?;
    let session = PackageSession::open(store, format!("session:{session_id}"));
    slots.insert(session_id.to_owned(), Arc::clone(&session));
    Ok(session)
}

impl crate::agent::Agent {
    /// Capture this turn's package generation at the start of the turn.
    ///
    /// Every turn entry point calls this once, before the tool list and its
    /// index freeze, so each request and tool round of one turn reads the same
    /// generation. A turn whose catalog did not move returns the snapshot the
    /// session already holds.
    pub(crate) async fn enter_package_generation(&self) {
        let session = match package_session(self.session_id()) {
            Ok(session) => session,
            Err(error) => {
                crate::logging::warn(&format!(
                    "package area for session {} is unavailable: {error:#}",
                    self.session_id()
                ));
                return;
            }
        };
        let workspace = self.working_dir().map(PathBuf::from);
        if let Err(error) = session.enter_turn(workspace.as_deref()).await {
            crate::logging::warn(&format!(
                "package snapshot for session {} could not be captured: {error:#}",
                self.session_id()
            ));
        }
    }

    /// The package snapshot this turn captured, when the session has one.
    pub fn package_snapshot(&self) -> Option<Arc<PackageSnapshot>> {
        let slots = lock_slots();
        let session = slots.get(self.session_id())?;
        session.current()
    }
}

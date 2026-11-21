//! Daemon ownership of the user's package catalog.
//!
//! A running daemon is the single writer that management clients talk to, and
//! the single publisher every session reads a generation from. Three things keep
//! those two roles on the same generation:
//!
//! - Every mutation, including the inbox pass, runs under one async serial lock
//!   and through [`PackageStore`], whose own file lock orders it against another
//!   process. The reply is built only after the resulting catalog is read back
//!   and published, so a client that receives it has seen the same generation
//!   subscribers see.
//! - A bounded watcher re-reads the catalog file and the inbox directory's
//!   modification time on a fixed slice. A catalog replaced atomically by
//!   another process, and a complete folder dropped into `inbox/`, both become a
//!   published generation without any client asking. Nothing walks the store
//!   tree and no turn does filesystem work.
//! - The published value is one complete catalog. A subscriber that misses
//!   intermediate notifications still observes the latest generation, including
//!   revoked records and their scopes, because `watch` hands out the newest value
//!   rather than a queue of deltas.
//!
//! Nothing here starts a package executable, attaches a session or extends a
//! transcript: those are later turn-boundary concerns that consume
//! [`PublishedCatalog`].

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use horon_agent_extension_types::control::{
    PackageControlOperation, PackageControlReceipt, PackageControlRequest, PackageControlScope, PackageControlState,
    PackageInboxSkip, PackageSummary,
};
use tokio::sync::{Mutex, OnceCell, watch};

use crate::packages::{
    InboxOutcome, InboxReport, InstallRequest, PackageRecord, PackageScope, PackageState, PackageStore,
};
use crate::protocol::{Request, ServerEvent};

/// How long one wait blocks before the watcher re-reads the two paths it
/// watches. Bounded rather than event-only: a target without a filesystem event
/// source waits out the slice and then rechecks, so a missed notification costs
/// one slice at most.
const WATCH_SLICE: Duration = Duration::from_millis(250);

/// How long one wait blocks while the package area has no catalog and no inbox.
/// Nothing can change until one of them is created, so the watcher idles instead
/// of waking four times a second on a machine that never installed a package.
const IDLE_SLICE: Duration = Duration::from_secs(2);

/// Ticks between full catalog re-reads that are not triggered by a metadata
/// change. The regular path stays metadata-only; this covers a replacement that
/// metadata cannot tell apart from the previous file.
const FULL_RECHECK_TICKS: u32 = 40;

/// Operations whose result is remembered, so a retry after a lost reply is
/// answered instead of applied twice. A client that retries later than this
/// many distinct operations gets a fresh decision, which is why the reply also
/// carries the authoritative catalog.
const REMEMBERED_OPERATIONS: usize = 64;

/// One published catalog generation.
///
/// The summaries are the wire projection of the complete catalog, sorted by ID.
/// Removed records stay with [`PackageControlState::Revoked`], so a reader can
/// tell "never installed" from "removed", and every record keeps its scope.
#[derive(Debug, Clone)]
pub struct PublishedCatalog {
    generation: u64,
    summaries: Vec<PackageSummary>,
}

impl PublishedCatalog {
    fn empty() -> Self {
        Self {
            generation: 0,
            summaries: Vec::new(),
        }
    }

    /// The catalog generation this value describes.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Every record at this generation, tombstones included.
    pub fn packages(&self) -> &[PackageSummary] {
        &self.summaries
    }
}

/// The daemon's package catalog and its published generations.
pub(crate) struct PackageControl {
    store: PackageStore,
    published: watch::Sender<Arc<PublishedCatalog>>,
    /// Serializes every mutation and inbox pass with its publication, so two
    /// callers can never interleave a catalog write with a read-back.
    serial: Mutex<()>,
    remembered: StdMutex<Remembered>,
    /// Completed once the startup pass published the first generation and the
    /// watcher was spawned. Every later caller awaits the same cell, so a
    /// request that arrives mid-startup cannot mutate ahead of the inbox pass.
    startup: OnceCell<()>,
}

#[derive(Default)]
struct Remembered {
    order: VecDeque<String>,
    entries: BTreeMap<String, RememberedOperation>,
}

struct RememberedOperation {
    /// The request this id was first used for. A different request under the
    /// same id is refused rather than silently answered with another outcome.
    fingerprint: String,
    changed: Vec<String>,
    skipped: Vec<PackageInboxSkip>,
}

/// What one operation wrote, before it is published.
#[derive(Default)]
struct MutationOutcome {
    changed: Vec<String>,
    skipped: Vec<PackageInboxSkip>,
}

impl PackageControl {
    /// A manager over the current user's package area.
    fn open() -> Result<Self> {
        Ok(Self {
            store: PackageStore::for_current_user()?,
            published: watch::channel(Arc::new(PublishedCatalog::empty())).0,
            serial: Mutex::new(()),
            remembered: StdMutex::new(Remembered::default()),
            startup: OnceCell::new(),
        })
    }

    /// The latest published generation.
    pub(crate) fn current(&self) -> Arc<PublishedCatalog> {
        self.published.borrow().clone()
    }

    /// Validate the catalog, install complete inbox entries, and start watching.
    ///
    /// Idempotent and complete for every caller: the first one runs the startup
    /// pass and spawns the watcher, and a caller that arrives while that pass is
    /// still running waits for it instead of mutating ahead of the inbox scan.
    pub(crate) async fn start(self: &Arc<Self>) {
        self.startup
            .get_or_init(|| async {
                // Observe the two watched paths *before* the startup pass. A
                // change that lands while the pass runs then differs from this
                // baseline, so the watcher reconciles it instead of adopting the
                // post-pass state as if nothing had moved.
                let baseline = self.observe().await;
                if let Err(error) = self.startup_pass().await {
                    crate::logging::warn(&format!(
                        "package catalog startup pass failed; serving the last published generation: {error:#}"
                    ));
                }
                if let Err(error) = self.publish().await {
                    crate::logging::warn(&format!("package catalog could not be published at startup: {error:#}"));
                }
                tokio::spawn(Arc::clone(self).watch(baseline));
            })
            .await;
    }

    /// The blocking part of startup: read the catalog and take in the inbox.
    async fn startup_pass(&self) -> Result<()> {
        let store = self.store.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            // Reading the catalog validates its format and version. A corrupt
            // file is reported and left in place; it is never rewritten here.
            store.catalog()?;
            store.sync_inbox().map_err(anyhow::Error::from)
        })
        .await
        .context("the package startup task failed")?;
        let report = outcome?;
        log_inbox_report(&report);
        Ok(())
    }

    /// Apply one operation and answer with the generation it produced.
    ///
    /// A repeated operation id replays the recorded outcome against the latest
    /// catalog instead of applying the operation again. The reply is the
    /// authoritative one either way: the caller learns the current generation
    /// and the complete catalog at it.
    pub(crate) async fn apply(&self, request: PackageControlRequest) -> Result<PackageControlReceipt> {
        let _serial = self.serial.lock().await;
        let fingerprint = serde_json::to_string(&request).context("serialize the package control request")?;
        if let Some(receipt) = self.replayed(&request, &fingerprint) {
            return receipt;
        }
        let (outcome, published) = match request.operation {
            PackageControlOperation::Status => (MutationOutcome::default(), self.current()),
            operation => {
                let outcome = self.mutate(operation).await?;
                let published = self.publish().await.context(
                    "could not confirm the catalog generation after the operation; inspect the catalog before retrying",
                )?;
                (outcome, published)
            }
        };
        let receipt = PackageControlReceipt {
            operation_id: request.operation_id.clone(),
            generation: published.generation(),
            replayed: false,
            changed: outcome.changed,
            skipped: outcome.skipped,
            catalog: published.packages().to_vec(),
        };
        self.remember(&request.operation_id, fingerprint, &receipt);
        Ok(receipt)
    }

    /// Run one catalog mutation on the blocking pool.
    async fn mutate(&self, operation: PackageControlOperation) -> Result<MutationOutcome> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || mutate(&store, operation))
            .await
            .context("the package mutation task failed")?
    }

    /// Run one inbox pass and publish what it installed.
    ///
    /// `Err` means the pass or its publication failed; the caller then keeps the
    /// previous observation so the next tick tries again rather than treating the
    /// inbox as reconciled.
    async fn reconcile_inbox(&self) -> Result<()> {
        let store = self.store.clone();
        let report = tokio::task::spawn_blocking(move || store.sync_inbox())
            .await
            .context("the package inbox task failed")??;
        log_inbox_report(&report);
        self.publish().await.map(drop)
    }

    /// Read the catalog back and announce it when it differs from the current
    /// value.
    ///
    /// A read failure is returned, never papered over with the previous
    /// generation: a caller that just changed the catalog has to learn that its
    /// new generation is unknown rather than receive an older one as success.
    async fn publish(&self) -> Result<Arc<PublishedCatalog>> {
        let store = self.store.clone();
        let read = tokio::task::spawn_blocking(move || read_published(&store)).await;
        let next = match read {
            Ok(Ok(catalog)) => Arc::new(catalog),
            Ok(Err(error)) => return Err(error.context("read the package catalog back")),
            Err(error) => return Err(anyhow::Error::new(error).context("read the package catalog back")),
        };
        let previous = self.current();
        if previous.generation() == next.generation() && previous.packages() == next.packages() {
            return Ok(previous);
        }
        crate::logging::info(&format!(
            "package catalog generation {} published ({} records)",
            next.generation(),
            next.packages().len()
        ));
        self.published.send_replace(Arc::clone(&next));
        Ok(next)
    }

    /// What the two watched paths look like right now.
    async fn observe(&self) -> Observations {
        Observations {
            catalog: catalog_stamp(&self.store.catalog_path()).await,
            inbox: inbox_modified(&self.store.inbox_dir()).await,
        }
    }

    /// The recorded outcome for this operation id, when one exists.
    fn replayed(&self, request: &PackageControlRequest, fingerprint: &str) -> Option<Result<PackageControlReceipt>> {
        let recorded = {
            let remembered = self.remembered.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            remembered
                .entries
                .get(&request.operation_id)
                .map(|entry| RememberedOperation {
                    fingerprint: entry.fingerprint.clone(),
                    changed: entry.changed.clone(),
                    skipped: entry.skipped.clone(),
                })
        }?;
        if recorded.fingerprint != fingerprint {
            return Some(Err(anyhow::anyhow!(
                "operation id `{}` was already used for a different request",
                request.operation_id
            )));
        }
        let published = self.current();
        Some(Ok(PackageControlReceipt {
            operation_id: request.operation_id.clone(),
            generation: published.generation(),
            replayed: true,
            changed: recorded.changed,
            skipped: recorded.skipped,
            catalog: published.packages().to_vec(),
        }))
    }

    /// Record one applied operation, evicting the oldest beyond the bound.
    fn remember(&self, operation_id: &str, fingerprint: String, receipt: &PackageControlReceipt) {
        let mut remembered = self.remembered.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if remembered.entries.contains_key(operation_id) {
            return;
        }
        remembered.order.push_back(operation_id.to_owned());
        remembered.entries.insert(
            operation_id.to_owned(),
            RememberedOperation {
                fingerprint,
                changed: receipt.changed.clone(),
                skipped: receipt.skipped.clone(),
            },
        );
        while remembered.order.len() > REMEMBERED_OPERATIONS {
            if let Some(oldest) = remembered.order.pop_front() {
                remembered.entries.remove(&oldest);
            }
        }
    }

    /// Follow the catalog file and the inbox on a bounded slice.
    ///
    /// One tick costs two metadata reads: the catalog file's size and
    /// modification time, and the inbox directory's modification time. Nothing
    /// is read or parsed until one of them differs, and no pass walks the store
    /// tree. A complete folder appears in `inbox/` by rename, which changes the
    /// directory's time; an install or removal in another process replaces
    /// `installed.json`, which changes the file's.
    ///
    /// A stamp is only adopted once its reconciliation succeeded, so a failed
    /// publish or inbox pass is retried on the next tick instead of being
    /// recorded as seen. Every [`FULL_RECHECK_TICKS`] ticks the same check runs
    /// even without a metadata change, which recovers a notification the OS
    /// event source lost, for both the catalog and an existing inbox.
    async fn watch(self: Arc<Self>, mut last: Observations) {
        let catalog_path = self.store.catalog_path();
        let inbox = self.store.inbox_dir();
        let mut ticks = 0u32;
        let mut failures = 0u32;
        loop {
            // An untouched package area has nothing to watch: waiting on
            // nonexistent paths every quarter second would only add wakeups.
            let slice = match last.catalog.is_some() || last.inbox.is_some() {
                true => WATCH_SLICE,
                false => IDLE_SLICE,
            };
            let paths = vec![catalog_path.clone(), inbox.clone()];
            let waited =
                tokio::task::spawn_blocking(move || horon_agent_platform::watch::wait_for_any_change(&paths, slice))
                    .await;
            match waited {
                Ok(Ok(())) => {}
                Ok(Err(error)) => crate::logging::debug(&format!("package catalog watch could not wait: {error}")),
                Err(error) => crate::logging::warn(&format!("the package catalog watch task failed: {error}")),
            }
            ticks = ticks.wrapping_add(1);
            let current = self.observe().await;
            let recheck = ticks % FULL_RECHECK_TICKS == 0;
            let catalog_due = current.catalog != last.catalog || recheck;
            let inbox_due = current.inbox != last.inbox || (recheck && current.inbox.is_some());
            if catalog_due || inbox_due {
                // One lock for the whole tick: a read this watcher took before an
                // operation published must never be published after it, which
                // would move subscribers back a generation.
                let _serial = self.serial.lock().await;
                if catalog_due {
                    match self.publish().await {
                        Ok(_) => last.catalog = current.catalog,
                        Err(error) => {
                            failures = failures.saturating_add(1);
                            log_watch_failure(
                                failures,
                                &format!("package catalog could not be republished: {error:#}"),
                            );
                        }
                    }
                }
                if inbox_due {
                    match self.reconcile_inbox().await {
                        Ok(()) => last.inbox = current.inbox,
                        Err(error) => {
                            failures = failures.saturating_add(1);
                            log_watch_failure(failures, &format!("package inbox pass failed: {error:#}"));
                        }
                    }
                }
            }
        }
    }
}

/// What the two watched paths looked like when they were last reconciled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Observations {
    catalog: Option<CatalogStamp>,
    inbox: Option<std::time::SystemTime>,
}

/// The catalog file's identity as the OS reports it.
///
/// An install writes a temporary file and renames it over this one, so size and
/// modification time both change. Comparing these is one `stat`, where reading
/// the file would parse the whole catalog on every tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CatalogStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

async fn catalog_stamp(path: &Path) -> Option<CatalogStamp> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => {
            let modified = match metadata.modified() {
                Ok(modified) => Some(modified),
                Err(error) => {
                    crate::logging::debug(&format!(
                        "could not read the modification time of {}: {error}",
                        path.display()
                    ));
                    None
                }
            };
            Some(CatalogStamp {
                len: metadata.len(),
                modified,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            crate::logging::debug(&format!("could not inspect {}: {error}", path.display()));
            None
        }
    }
}

/// When the inbox directory last changed, or `None` while it does not exist.
///
/// A complete folder arrives by rename, which changes the directory's
/// modification time; a pass is only worth starting when that changed.
async fn inbox_modified(path: &Path) -> Option<std::time::SystemTime> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::logging::debug(&format!("could not inspect {}: {error}", path.display()));
            return None;
        }
    };
    match metadata.modified() {
        Ok(modified) => Some(modified),
        Err(error) => {
            crate::logging::debug(&format!(
                "could not read the modification time of {}: {error}",
                path.display()
            ));
            None
        }
    }
}

/// Report a watcher failure once, then keep the retry quiet.
///
/// A package area that stays unreadable (a broken mount, a permission change)
/// fails on every tick; one warning names the cause and the rest are debug, so
/// the retry loop cannot flood the log.
fn log_watch_failure(failures: u32, message: &str) {
    if failures == 1 {
        crate::logging::warn(message);
    } else {
        crate::logging::debug(message);
    }
}

/// The process-wide manager, started on first use.
///
/// One daemon owns one package area, so one instance is authoritative for this
/// process. Tests that build a manager directly use [`PackageControl::open`]
/// through a local arc instead.
pub(super) async fn shared() -> Result<Arc<PackageControl>> {
    static SHARED: StdMutex<Option<Arc<PackageControl>>> = StdMutex::new(None);
    let control = {
        let mut shared = SHARED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match shared.as_ref() {
            Some(control) => Arc::clone(control),
            None => {
                let control = Arc::new(PackageControl::open()?);
                *shared = Some(Arc::clone(&control));
                control
            }
        }
    };
    control.start().await;
    Ok(control)
}

/// Subscribe to the daemon's published catalog. The receiver starts with the
/// current generation and holds the latest complete catalog across coalesced
/// updates, including scoped packages and revocations.
pub async fn subscribe() -> Result<watch::Receiver<Arc<PublishedCatalog>>> {
    Ok(shared().await?.published.subscribe())
}

/// Answer package control without binding the connection to a session. A
/// bridge may send more requests over the same stream after this reply.
pub(super) async fn reply(request: &mut Request) -> Option<ServerEvent> {
    let Request::PackageControl {
        id,
        operation_id,
        operation,
    } = request
    else {
        return None;
    };
    let request = PackageControlRequest {
        operation_id: std::mem::take(operation_id),
        operation: std::mem::replace(operation, PackageControlOperation::Status),
    };
    Some(handle_request(*id, request).await)
}

async fn handle_request(id: u64, request: PackageControlRequest) -> ServerEvent {
    let control = match shared().await {
        Ok(control) => control,
        Err(error) => return error_event(id, &error),
    };
    match control.apply(request).await {
        Ok(receipt) => ServerEvent::PackageControlResult { id, receipt },
        Err(error) => error_event(id, &error),
    }
}

fn error_event(id: u64, error: &anyhow::Error) -> ServerEvent {
    ServerEvent::Error {
        id,
        message: format!("{error:#}"),
        retry_after_secs: None,
    }
}

/// Apply one checked operation to the store.
///
/// Paths, IDs, scopes and the replace option go through the same installation
/// path a daemon-less CLI uses, so the two can never disagree about what a
/// valid package is.
fn mutate(store: &PackageStore, operation: PackageControlOperation) -> Result<MutationOutcome> {
    match operation {
        PackageControlOperation::Install {
            directory,
            scope,
            replace,
        } => {
            let scope = scope_of(&scope)?;
            let request = match replace {
                true => InstallRequest::new(directory, scope).replacing(),
                false => InstallRequest::new(directory, scope),
            };
            let record = store.install(request)?;
            Ok(MutationOutcome {
                changed: vec![record.id],
                skipped: Vec::new(),
            })
        }
        PackageControlOperation::Disable { id } => {
            store.disable(&id)?;
            Ok(changed(id))
        }
        PackageControlOperation::Enable { id } => {
            store.enable(&id)?;
            Ok(changed(id))
        }
        PackageControlOperation::Remove { id } => {
            store.remove(&id)?;
            Ok(changed(id))
        }
        PackageControlOperation::SyncInbox => {
            let report = store.sync_inbox()?;
            Ok(MutationOutcome {
                changed: report.installed().map(|record| record.id.clone()).collect(),
                skipped: skipped_entries(&report),
            })
        }
        PackageControlOperation::Status => Ok(MutationOutcome::default()),
    }
}

fn changed(id: String) -> MutationOutcome {
    MutationOutcome {
        changed: vec![id],
        skipped: Vec::new(),
    }
}

/// Resolve a wire scope into the catalog's scope.
///
/// A project root is canonicalized here, so two spellings of one directory name
/// one scope and a root the host cannot resolve is refused before any copy is
/// published.
fn scope_of(scope: &PackageControlScope) -> Result<PackageScope> {
    match scope {
        PackageControlScope::Global => Ok(PackageScope::Global),
        PackageControlScope::Project { root } => {
            if root.is_empty() {
                bail!("a project scope needs a root directory");
            }
            PackageScope::project(root)
        }
    }
}

fn skipped_entries(report: &InboxReport) -> Vec<PackageInboxSkip> {
    report
        .entries
        .iter()
        .filter_map(|entry| match &entry.outcome {
            InboxOutcome::Skipped(reason) => Some(PackageInboxSkip {
                name: entry.name.clone(),
                reason: reason.clone(),
            }),
            InboxOutcome::Installed(_) => None,
        })
        .collect()
}

fn log_inbox_report(report: &InboxReport) {
    if report.entries.is_empty() {
        return;
    }
    let installed: Vec<&str> = report.installed().map(|record| record.id.as_str()).collect();
    crate::logging::info(&format!(
        "package inbox: {} entries, installed {:?}",
        report.entries.len(),
        installed
    ));
    for skip in skipped_entries(report) {
        crate::logging::warn(&format!("package inbox skipped {}: {}", skip.name, skip.reason));
    }
}

fn read_published(store: &PackageStore) -> Result<PublishedCatalog> {
    let view = store.catalog()?;
    Ok(PublishedCatalog {
        generation: view.generation(),
        summaries: view.packages().map(summary_of).collect(),
    })
}

fn summary_of(record: &PackageRecord) -> PackageSummary {
    PackageSummary {
        kind: record.kind,
        id: record.id.clone(),
        version: record.version.clone(),
        name: record.name.clone(),
        scope: match &record.scope {
            PackageScope::Global => PackageControlScope::Global,
            PackageScope::Project { root } => PackageControlScope::Project { root: root.clone() },
        },
        state: match record.state {
            PackageState::Installed => PackageControlState::Installed,
            PackageState::Disabled => PackageControlState::Disabled,
            PackageState::Revoked => PackageControlState::Revoked,
        },
        generation: record.generation,
    }
}

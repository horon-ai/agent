//! Revocation dispatch for live package holders.
//!
//! A session that starts a package executable or opens a package resource
//! registers a callback here for the catalog generation it captured. When the
//! daemon publishes a generation that disables or removes that package, it calls
//! [`PackageLifecycleRegistry::apply_states`] before it answers the management
//! client, so the reply is only built after every registered cancel/reap
//! callback for the revoked package ran.
//!
//! Two rules keep a replacement and a revocation apart:
//!
//! - A replacement install leaves the state `Installed`, so no callback runs and
//!   a running turn keeps the generation it captured. Its copies stay pinned by
//!   the package lease the snapshot holds.
//! - A disable or a removal records one [`RevocationEvent`] per package ID under
//!   the published generation. A holder that captured an older generation is
//!   refused even after the package is enabled or installed again, because the
//!   record proves the request predates the revocation.
//!
//! The registry is per process and keyed by package ID: a daemon is the only
//! publisher of a package area, so one registry covers every session it hosts. A
//! process restart starts from an empty registry, which is safe because every
//! session's holders are gone with it.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use anyhow::{Result, bail};

use crate::packages::PackageState;

/// Why the catalog stopped offering a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationCause {
    /// The package stays installed but must not load.
    Disabled,
    /// The package was removed and names no copy any more.
    Removed,
}

impl RevocationCause {
    /// The catalog state this cause describes, as an error message spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Removed => "removed",
        }
    }
}

/// One revocation, as an observer and a refused holder see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationEvent {
    pub package_id: String,
    /// Catalog generation the revocation was published under.
    pub generation: u64,
    pub cause: RevocationCause,
}

/// What a holder runs when its package is revoked.
///
/// The callback cancels an in-flight request or stops a supervised child. It runs
/// while the daemon publishes the new generation, so it must not block: a long
/// operation belongs on the caller's own task.
pub type RevocationCallback = Arc<dyn Fn(&RevocationEvent) + Send + Sync>;

/// How many registered callbacks ran and how many panicked.
///
/// A panic is contained so one holder cannot stop the daemon from publishing the
/// generation every other session needs; the count reaches the caller's log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RevocationReport {
    pub fired: usize,
    pub failed: usize,
}

#[derive(Default)]
struct RegistryInner {
    holders: BTreeMap<u64, Holder>,
    revocations: BTreeMap<String, RevocationEvent>,
}

struct Holder {
    package_id: String,
    generation: u64,
    callback: RevocationCallback,
}

/// The live holders of one process and the revocations they must observe.
pub struct PackageLifecycleRegistry {
    inner: Mutex<RegistryInner>,
    next_holder: AtomicU64,
}

/// One registered holder's registration.
///
/// Dropping it unregisters the callback, so a finished request or an exited
/// child leaves nothing behind for a later revocation to call.
pub struct PackageLifecycleHolder {
    registry: Arc<PackageLifecycleRegistry>,
    token: u64,
}

impl Drop for PackageLifecycleHolder {
    fn drop(&mut self) {
        self.registry.remove(self.token);
    }
}

impl PackageLifecycleRegistry {
    /// A registry with no holders and no revocations.
    pub fn open() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RegistryInner::default()),
            next_holder: AtomicU64::new(1),
        })
    }

    /// The registry the daemon and its sessions share.
    pub fn shared() -> Arc<Self> {
        static SHARED: LazyLock<Arc<PackageLifecycleRegistry>> = LazyLock::new(PackageLifecycleRegistry::open);
        Arc::clone(&SHARED)
    }

    /// Register a callback for a package a request or a child is using.
    ///
    /// `generation` is the catalog generation the caller captured. Registering
    /// for a package that was already revoked after that generation fails, so a
    /// request that arrives late is refused instead of starting a package the
    /// user just disabled.
    pub fn register(
        self: &Arc<Self>,
        package_id: impl Into<String>,
        generation: u64,
        callback: RevocationCallback,
    ) -> Result<PackageLifecycleHolder> {
        let package_id = package_id.into();
        let mut inner = self.lock();
        if let Some(event) = inner.revocations.get(&package_id)
            && event.generation > generation
        {
            bail!(
                "package {package_id} was {} at catalog generation {}; a holder from generation {generation} is stale",
                event.cause.as_str(),
                event.generation
            );
        }
        let token = self.next_holder.fetch_add(1, Ordering::Relaxed);
        inner.holders.insert(
            token,
            Holder {
                package_id,
                generation,
                callback,
            },
        );
        Ok(PackageLifecycleHolder {
            registry: Arc::clone(self),
            token,
        })
    }

    /// Record one revocation and run every registered callback for it.
    ///
    /// The event is recorded whether or not a holder exists: the record is what
    /// refuses a request that captured an older generation, including one that
    /// arrives after the package was enabled or installed again.
    pub fn revoke(&self, package_id: &str, generation: u64, cause: RevocationCause) -> RevocationReport {
        let event = RevocationEvent {
            package_id: package_id.to_owned(),
            generation,
            cause,
        };
        let firing = {
            let mut inner = self.lock();
            inner.revocations.insert(event.package_id.clone(), event.clone());
            let tokens: Vec<u64> = inner
                .holders
                .iter()
                .filter(|(_, holder)| holder.package_id == event.package_id && holder.generation < event.generation)
                .map(|(token, _)| *token)
                .collect();
            tokens
                .into_iter()
                .filter_map(|token| inner.holders.remove(&token))
                .map(|holder| holder.callback)
                .collect::<Vec<_>>()
        };
        let mut report = RevocationReport::default();
        for callback in firing {
            match catch_unwind(AssertUnwindSafe(|| callback(&event))) {
                Ok(()) => report.fired += 1,
                Err(_) => report.failed += 1,
            }
        }
        report
    }

    /// Revoke every package that moved out of `Installed` between two published
    /// generation maps.
    ///
    /// This is the daemon's whole call into the registry: `previous` and `next`
    /// are the record states of the generation it last published and the one it
    /// is publishing now.
    pub fn apply_states(
        &self,
        previous: &BTreeMap<String, PackageState>,
        next: &BTreeMap<String, PackageState>,
        generation: u64,
    ) -> RevocationReport {
        let mut report = RevocationReport::default();
        for (package_id, state) in next {
            let cause = match state {
                PackageState::Installed => continue,
                PackageState::Disabled => RevocationCause::Disabled,
                PackageState::Revoked => RevocationCause::Removed,
            };
            if previous.get(package_id) == Some(state) {
                continue;
            }
            let fired = self.revoke(package_id, generation, cause);
            report.fired += fired.fired;
            report.failed += fired.failed;
        }
        report
    }

    /// The latest revocation recorded for a package ID, if any.
    pub fn revocation(&self, package_id: &str) -> Option<RevocationEvent> {
        self.lock().revocations.get(package_id).cloned()
    }

    /// How many callbacks are currently registered for a package ID.
    pub fn holders(&self, package_id: &str) -> usize {
        self.lock()
            .holders
            .values()
            .filter(|holder| holder.package_id == package_id)
            .count()
    }

    fn remove(&self, token: u64) {
        self.lock().holders.remove(&token);
    }

    fn lock(&self) -> MutexGuard<'_, RegistryInner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

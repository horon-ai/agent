//! Process-wide cache of memory graphs, keyed by store revision.
//!
//! Every operation that reads a store would otherwise rebuild the graph from
//! the markdown pages, and the turn path reads the global and project graphs
//! once per turn. The cache holds the graph a store revision produced: the
//! revision changes inside every write transaction (see the store index), so a
//! write by any handle, thread or process makes the next read miss instead of
//! serving a stale graph. That is the whole invalidate rule: no file watching,
//! no modification times, no explicit flushes.
//!
//! Both caches are bounded. A daemon that touches many projects must not keep a
//! graph (and an open connection) per project for its lifetime, so an entry
//! that is pushed out is dropped; reopening it costs one scan.

use crate::memory_graph::MemoryGraph;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Most graphs kept at once; the least recently used one is dropped first.
const MAX_GRAPHS: usize = 32;

/// Borrowed scope selector for zero-allocation cache lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheScopeRef<'a> {
    /// The user-level store.
    Global,
    /// One project store, by stable project key.
    Project(&'a str),
}

enum CacheScope {
    Global,
    Project(String),
}

impl CacheScope {
    fn matches(&self, other: CacheScopeRef<'_>) -> bool {
        match (self, other) {
            (Self::Global, CacheScopeRef::Global) => true,
            (Self::Project(left), CacheScopeRef::Project(right)) => left == right,
            _ => false,
        }
    }

    fn from_ref(scope: CacheScopeRef<'_>) -> Self {
        match scope {
            CacheScopeRef::Global => Self::Global,
            CacheScopeRef::Project(project) => Self::Project(project.to_string()),
        }
    }
}

struct GraphEntry {
    root: PathBuf,
    scope: CacheScope,
    revision: u64,
    graph: Arc<MemoryGraph>,
    used: u64,
}

static GRAPH_CACHE: OnceLock<Mutex<Vec<GraphEntry>>> = OnceLock::new();
static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn graph_cache() -> &'static Mutex<Vec<GraphEntry>> {
    GRAPH_CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn next_tick() -> u64 {
    TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The graph `revision` produced for this store and scope, if it is still
/// cached. A different revision is a miss: the store moved.
pub(super) fn cached_graph(root: &Path, scope: CacheScopeRef<'_>, revision: u64) -> Option<Arc<MemoryGraph>> {
    let mut cache = graph_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = cache
        .iter_mut()
        .find(|entry| entry.root == root && entry.scope.matches(scope))?;
    if entry.revision != revision {
        return None;
    }
    entry.used = next_tick();
    Some(Arc::clone(&entry.graph))
}

/// Remember `graph` as the content of this store and scope at `revision`.
pub(super) fn cache_graph(root: &Path, scope: CacheScopeRef<'_>, revision: u64, graph: Arc<MemoryGraph>) {
    let mut cache = graph_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = cache
        .iter_mut()
        .find(|entry| entry.root == root && entry.scope.matches(scope))
    {
        entry.revision = revision;
        entry.graph = graph;
        entry.used = next_tick();
        return;
    }
    cache.push(GraphEntry {
        root: root.to_path_buf(),
        scope: CacheScope::from_ref(scope),
        revision,
        graph,
        used: next_tick(),
    });
    while cache.len() > MAX_GRAPHS {
        let Some((oldest_idx, _)) = cache.iter().enumerate().min_by_key(|(_, entry)| entry.used) else {
            break;
        };
        cache.swap_remove(oldest_idx);
    }
}

/// Forget every cached graph, for test-storage cleanup.
pub(super) fn forget_all() {
    let mut cache = graph_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.clear();
}

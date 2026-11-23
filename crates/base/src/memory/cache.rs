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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Most graphs kept at once; the least recently used one is dropped first.
const MAX_GRAPHS: usize = 32;

/// Which store a cached graph belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum CacheScope {
    /// The user-level store.
    Global,
    /// One project store, by stable project key.
    Project(String),
}

struct GraphEntry {
    revision: u64,
    graph: MemoryGraph,
    used: u64,
}

static GRAPH_CACHE: OnceLock<Mutex<HashMap<(PathBuf, CacheScope), GraphEntry>>> = OnceLock::new();
static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn graph_cache() -> &'static Mutex<HashMap<(PathBuf, CacheScope), GraphEntry>> {
    GRAPH_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_tick() -> u64 {
    TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The graph `revision` produced for this store and scope, if it is still
/// cached. A different revision is a miss: the store moved.
pub(super) fn cached_graph(root: &Path, scope: &CacheScope, revision: u64) -> Option<MemoryGraph> {
    let mut cache = graph_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = (root.to_path_buf(), scope.clone());
    let entry = cache.get_mut(&key)?;
    if entry.revision != revision {
        return None;
    }
    entry.used = next_tick();
    Some(entry.graph.clone())
}

/// Remember `graph` as the content of this store and scope at `revision`.
pub(super) fn cache_graph(root: &Path, scope: CacheScope, revision: u64, graph: &MemoryGraph) {
    let mut cache = graph_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(
        (root.to_path_buf(), scope),
        GraphEntry {
            revision,
            graph: graph.clone(),
            used: next_tick(),
        },
    );
    while cache.len() > MAX_GRAPHS {
        let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.used)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        cache.remove(&oldest);
    }
}

/// Forget every cached graph, for test-storage cleanup.
pub(super) fn forget_all() {
    let mut cache = graph_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.clear();
}

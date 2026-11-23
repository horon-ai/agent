//! The store behind [`crate::memory::MemoryManager`].
//!
//! The manager speaks one vocabulary to its callers: a [`MemoryGraph`] per
//! scope. This module is where that graph is backed by the markdown wiki
//! (`crate::memory::store`): a read builds the graph from the pages of that
//! scope, and a write diffs the changed graph against the pages and writes only
//! what actually changed. The old JSON files are a migration source
//! ([`super::migrate`]) and are never written again.
//!
//! Two properties matter more than speed here.
//!
//! * **One writer per store.** Every mutation takes an exclusive lock on
//!   `memory/.write.lock` for the whole read-change-write step, so two handles
//!   in one process, or two processes, cannot both read revision *n* and
//!   overwrite each other. The store's own SQLite transactions serialise the
//!   index writes underneath that lock.
//! * **No stale reads.** The store keeps a revision counter that changes inside
//!   every write transaction, and the process-wide graph cache is keyed by it.
//!   A write through any handle therefore makes the next read recompute instead
//!   of serving the graph from before that write.

use super::cache::{CacheScope, cache_graph, cached_graph};
use super::graph_pages::{GraphSnapshot, snapshot_from_pages};
use super::migrate::{self, JsonSource};
use super::project_key::{pre_wiki_namespace, stable_project_key};
use super::store::MemoryWiki;
use crate::memory_graph::MemoryGraph;
use crate::storage;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Directory under the horon home that holds every memory file.
const STORE_DIRNAME: &str = "memory";

/// How long a writer waits for another writer before reporting the store busy.
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// Delay between two lock attempts.
const LOCK_RETRY: Duration = Duration::from_millis(25);

/// Most wiki handles kept open at once. An evicted root costs one reconcile
/// scan the next time it is used.
const MAX_HANDLES: usize = 32;

/// The project key every test-mode project shares, exactly as the single
/// `test_project.json` did.
const TEST_PROJECT_KEY: &str = "test";

/// Which store a read or write addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StoreScope {
    /// The user-level store, visible from every project.
    Global,
    /// One project's store, by stable project key.
    Project,
}

impl StoreScope {
    /// `"global"` or `"project"`, the labels the consolidation state and logs
    /// use.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod backend_tests;

/// A resolved store location plus the project of one caller.
#[derive(Debug, Clone)]
pub(crate) struct Store {
    root: PathBuf,
    /// `None` when the caller has no project directory, which makes the project
    /// scope unavailable (the refusal `remember` has always given).
    project: Option<String>,
    /// The pre-0.2 project hash for this caller's directory. It names the pre-wiki
    /// JSON file to import and the namespace those pages landed in; `None` when
    /// there is no project directory.
    pre_wiki_hash: Option<String>,
    test_mode: bool,
}

impl Store {
    /// Resolve the store for one manager call.
    pub(crate) fn open(test_mode: bool, project_dir: Option<&Path>) -> Result<Self> {
        let base = storage::horon_agent_dir()?.join(STORE_DIRNAME);
        if test_mode {
            // Test storage is one private store under `memory/test`: every
            // test-mode project shares the `test` key, exactly as the single
            // `test_project.json` did.
            return Ok(Self {
                root: base.join(TEST_PROJECT_KEY),
                project: Some(TEST_PROJECT_KEY.to_string()),
                pre_wiki_hash: Some(TEST_PROJECT_KEY.to_string()),
                test_mode,
            });
        }
        let (project, pre_wiki_hash) = match project_dir {
            Some(dir) => (
                Some(stable_project_key(dir)),
                Some(super::project_key::pre_wiki_project_hash(dir)),
            ),
            None => (None, None),
        };
        Ok(Self {
            root: base,
            project,
            pre_wiki_hash,
            test_mode,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The project key, or the refusal `remember` has always produced.
    pub(crate) fn project_key(&self) -> Result<&str> {
        self.project
            .as_deref()
            .ok_or_else(|| anyhow!("Project memory requires a working directory; use global scope explicitly"))
    }

    /// The project key of one scope, or the refusal when the caller has no
    /// working directory.
    fn project_of(&self, scope: StoreScope) -> Result<Option<&str>> {
        match scope {
            StoreScope::Global => Ok(None),
            StoreScope::Project => self.project_key().map(Some),
        }
    }

    /// The consolidation state file of one scope, or `None` when the scope is
    /// unavailable to this caller.
    pub(crate) fn state_path_opt(&self, scope: StoreScope) -> Result<Option<PathBuf>> {
        let name = match scope {
            StoreScope::Global => "consolidation-global.json".to_string(),
            StoreScope::Project => {
                let Some(project) = self.project.as_deref() else {
                    return Ok(None);
                };
                if self.test_mode {
                    "consolidation-test.json".to_string()
                } else {
                    format!("consolidation-{project}.json")
                }
            }
        };
        Ok(Some(self.root.join("state").join(name)))
    }

    /// Read one scope as a graph.
    ///
    /// A project scope this caller cannot address (no working directory) reads
    /// as empty rather than as an error, which is what every reader has always
    /// done: `remember` is the call that refuses, not a read.
    pub(crate) fn read(&self, scope: StoreScope) -> Result<MemoryGraph> {
        if scope == StoreScope::Project && self.project.is_none() {
            return Ok(MemoryGraph::new());
        }
        let project = self.project_of(scope)?;
        if reconciled(self, scope) {
            return self.read_current(project);
        }
        // The first use of a store prepares it (creates the directory, opens and
        // reconciles the index, imports a JSON file). That work writes, so it
        // happens under the store lock before any handle is opened: two first
        // readers that both opened and reconciled would race inside SQLite.
        let _lock = self.write_lock()?;
        let handle = wiki_handle(&self.root)?;
        let mut wiki = lock_handle(&handle);
        self.reconcile(&mut wiki, scope)?;
        drop(wiki);
        self.read_current(project)
    }

    /// Run `change` on one scope's graph under the write lock, then save it.
    ///
    /// The graph handed to `change` is read after the lock is taken, so a
    /// duplicate check, a removal or a tag sees every write that already
    /// committed instead of a copy from before it.
    pub(crate) fn update<T>(&self, scope: StoreScope, change: impl FnOnce(&mut MemoryGraph) -> Result<T>) -> Result<T> {
        let project = self.project_of(scope)?;
        let _lock = self.write_lock()?;
        let handle = wiki_handle(&self.root)?;
        let mut wiki = lock_handle(&handle);
        self.reconcile(&mut wiki, scope)?;
        let snapshot = self.snapshot(&wiki, scope, project)?;
        let mut graph = snapshot.graph;
        let outcome = change(&mut graph)?;
        self.write_graph(&mut wiki, project, &graph, snapshot.pages)?;
        let revision = wiki.revision()?;
        cache_graph(&self.root, cache_scope(scope, project), revision, &graph);
        Ok(outcome)
    }

    /// Read the current graph, assuming this scope was already reconciled.
    fn read_current(&self, project: Option<&str>) -> Result<MemoryGraph> {
        let handle = wiki_handle(&self.root)?;
        let wiki = lock_handle(&handle);
        let revision = wiki.revision()?;
        let scope = if project.is_some() {
            StoreScope::Project
        } else {
            StoreScope::Global
        };
        if let Some(graph) = cached_graph(&self.root, &cache_scope(scope, project), revision) {
            return Ok(graph);
        }
        let snapshot = snapshot_from_pages(wiki.scope_pages(project)?);
        cache_graph(&self.root, cache_scope(scope, project), revision, &snapshot.graph);
        Ok(snapshot.graph)
    }

    /// The cached or freshly built graph of one scope, with its pages.
    fn snapshot(&self, wiki: &MemoryWiki, scope: StoreScope, project: Option<&str>) -> Result<GraphSnapshot> {
        let revision = wiki.revision()?;
        if let Some(graph) = cached_graph(&self.root, &cache_scope(scope, project), revision) {
            return Ok(GraphSnapshot {
                graph,
                pages: HashMap::new(),
            });
        }
        Ok(snapshot_from_pages(wiki.scope_pages(project)?))
    }

    /// A counter that changes on every write to this store.
    pub(crate) fn revision(&self) -> Result<u64> {
        let handle = wiki_handle(&self.root)?;
        let wiki = lock_handle(&handle);
        wiki.revision()
    }

    /// Write `graph` as the complete content of one scope: changed pages are
    /// rewritten and pages that left the graph are deleted. `known` is the
    /// snapshot the graph was loaded from, when it was loaded under the same
    /// lock, so an unchanged page is not rewritten.
    fn write_graph(
        &self,
        wiki: &mut MemoryWiki,
        project: Option<&str>,
        graph: &MemoryGraph,
        mut known: HashMap<String, horon_agent_memory_types::MemoryPage>,
    ) -> Result<()> {
        if known.is_empty() {
            for page in wiki.scope_pages(project)? {
                known.insert(page.id.as_str().to_string(), page);
            }
        }
        for entry in graph.memories.values() {
            let page = super::graph_pages::page_for_entry(entry, project, graph)
                .with_context(|| format!("cannot store memory {}", entry.id))?;
            let unchanged = matches!(known.remove(page.id.as_str()), Some(existing) if existing == page);
            if !unchanged {
                wiki.write_page(&page)?;
            }
        }
        let mut removed: Vec<String> = known.into_keys().collect();
        removed.sort();
        for id in removed {
            if let Ok(page_id) = horon_agent_memory_types::page::PageId::parse(&id) {
                wiki.delete_page(&page_id)?;
            }
        }
        Ok(())
    }

    /// Take the store's exclusive write lock.
    fn write_lock(&self) -> Result<WriteLock> {
        ensure_private_dir(&self.root)?;
        WriteLock::acquire(&self.root.join(".write.lock"))
    }

    /// A wiki handle for the whole migration command, with the lock held.
    pub(crate) fn migration_session(&self) -> Result<(WriteLock, MemoryWiki)> {
        ensure_private_dir(&self.root)?;
        let lock = WriteLock::acquire(&self.root.join(".write.lock"))?;
        let wiki = MemoryWiki::open(&self.root)
            .with_context(|| format!("cannot open the memory store {}", self.root.display()))?;
        Ok((lock, wiki))
    }

    /// Bring one scope up to date with its pre-wiki JSON files, once per process.
    ///
    /// Runs with the write lock held. Every step is idempotent: the migration
    /// record names what was already imported, adopting a namespace moves pages
    /// that are still in it, and the notes import adds only ids the store lacks.
    fn reconcile(&self, wiki: &mut MemoryWiki, scope: StoreScope) -> Result<()> {
        if reconciled(self, scope) {
            return Ok(());
        }
        let project = self.project_of(scope)?;
        if let Some(source) = self.json_source(scope) {
            let (report, record) = migrate::import_file(wiki, &self.root, &source.path, project)?;
            if let Some(record) = record {
                migrate::record_import(&self.root, record)?;
            }
            if !report.refused.is_empty() {
                crate::logging::warn(&format!(
                    "memory migration left {} entries of {} unmigrated: {}",
                    report.refused.len(),
                    source.path.display(),
                    report.refused.join("; ")
                ));
            }
        }
        if scope == StoreScope::Project {
            // Pages imported earlier by `memory migrate` sit in the placeholder
            // namespace of this project's hash; move them to the stable key now
            // that the directory is known.
            if let (Some(hash), Some(to)) = (self.pre_wiki_hash(), project) {
                let moved = migrate::adopt_namespace(wiki, &pre_wiki_namespace(hash), to)?;
                if moved > 0 {
                    crate::logging::info(&format!("Moved {moved} migrated memories to the project key {to}"));
                }
            }
            if let Some(hash) = self.pre_wiki_hash() {
                self.import_pre_wiki_notes(wiki, hash)?;
            }
        }
        mark_reconciled(self, scope);
        Ok(())
    }

    /// The pre-wiki JSON file one scope migrates from, when it exists.
    fn json_source(&self, scope: StoreScope) -> Option<JsonSource> {
        let (path, project) = match scope {
            StoreScope::Global if self.test_mode => (self.root.join("test_global.json"), None),
            StoreScope::Global => (self.root.join("global.json"), None),
            StoreScope::Project if self.test_mode => (self.root.join("test_project.json"), None),
            StoreScope::Project => {
                let hash = self.pre_wiki_hash()?;
                (
                    self.root.join("projects").join(format!("{hash}.json")),
                    Some(pre_wiki_namespace(hash)),
                )
            }
        };
        path.is_file().then_some(JsonSource {
            path,
            project: project.or_else(|| self.project.clone()),
        })
    }

    /// Import the flat `notes/<hash>.json` list once, as the old loader did.
    fn import_pre_wiki_notes(&self, wiki: &mut MemoryWiki, hash: &str) -> Result<()> {
        let path = super::project_key::pre_wiki_notes_path(&self.root, hash);
        if !path.is_file() {
            return Ok(());
        }
        let notes: super::PreWikiNotesFile = crate::storage::read_json(&path)?;
        if notes.entries.is_empty() {
            return Ok(());
        }
        let graph = snapshot_from_pages(wiki.scope_pages(Some(self.project_key()?))?).graph;
        let mut added = 0;
        for note in notes.entries {
            if graph.memories.contains_key(&note.id) {
                continue;
            }
            let mut entry = crate::memory_types::MemoryEntry::new(
                crate::memory_types::MemoryCategory::Custom(super::PRE_WIKI_NOTE_CATEGORY.to_string()),
                note.content,
            );
            entry.id = note.id;
            entry.created_at = note.created_at;
            entry.updated_at = note.created_at;
            entry.source = Some(super::PRE_WIKI_NOTES_SOURCE.to_string());
            if let Some(tag) = note.tag {
                entry.tags.push(tag);
            }
            let page = super::graph_pages::page_for_entry(&entry, Some(self.project_key()?), &graph)?;
            wiki.write_page(&page)?;
            added += 1;
        }
        if added > 0 {
            crate::logging::info(&format!("Imported {added} pre-wiki notes into project memory"));
        }
        Ok(())
    }

    pub(crate) fn pre_wiki_hash(&self) -> Option<&str> {
        self.pre_wiki_hash.as_deref()
    }
}

fn cache_scope(scope: StoreScope, project: Option<&str>) -> CacheScope {
    match scope {
        StoreScope::Global => CacheScope::Global,
        StoreScope::Project => CacheScope::Project(match project {
            Some(project) => project.to_string(),
            None => String::new(),
        }),
    }
}

/// Process-wide memory of which scopes were already reconciled.
fn reconciled_key(store: &Store, scope: StoreScope) -> String {
    format!(
        "{}|{}|{}",
        store.root.display(),
        scope.as_str(),
        store.pre_wiki_hash.as_deref().unwrap_or("")
    )
}

fn reconciled(store: &Store, scope: StoreScope) -> bool {
    reconciled_keys()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&reconciled_key(store, scope))
}

fn mark_reconciled(store: &Store, scope: StoreScope) {
    reconciled_keys()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(reconciled_key(store, scope));
}

fn reconciled_keys() -> &'static Mutex<std::collections::HashSet<String>> {
    static KEYS: LazyLock<Mutex<std::collections::HashSet<String>>> =
        LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
    &KEYS
}

/// Forget every scope of every store root, for test-storage cleanup.
pub(crate) fn forget_all() {
    if let Ok(mut handles) = handles().lock() {
        handles.clear();
    }
    super::cache::forget_all();
    reconciled_keys()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

struct HandleEntry {
    handle: Arc<Mutex<MemoryWiki>>,
    used: u64,
}

fn handles() -> &'static Mutex<HashMap<PathBuf, HandleEntry>> {
    static HANDLES: LazyLock<Mutex<HashMap<PathBuf, HandleEntry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    &HANDLES
}

static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_tick() -> u64 {
    TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The process's open handle for one store root, opened and reconciled once.
fn wiki_handle(root: &Path) -> Result<Arc<Mutex<MemoryWiki>>> {
    let existing = {
        let mut map = handles().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        map.get_mut(root).map(|entry| {
            entry.used = next_tick();
            entry.handle.clone()
        })
    };
    if let Some(handle) = existing {
        return Ok(handle);
    }
    // Open outside the registry lock: the reconcile scan reads every page file,
    // and holding the registry while it runs would block every other store.
    let wiki = MemoryWiki::open(root).with_context(|| format!("cannot open the memory store {}", root.display()))?;
    let handle = Arc::new(Mutex::new(wiki));
    let mut map = handles().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = map.get_mut(root) {
        existing.used = next_tick();
        return Ok(existing.handle.clone());
    }
    map.insert(
        root.to_path_buf(),
        HandleEntry {
            handle: handle.clone(),
            used: next_tick(),
        },
    );
    evict_handles(&mut map);
    Ok(handle)
}

/// Drop least recently used handles nobody is using right now.
fn evict_handles(map: &mut HashMap<PathBuf, HandleEntry>) {
    while map.len() > MAX_HANDLES {
        let Some(victim) = map
            .iter()
            .filter(|(_, entry)| Arc::strong_count(&entry.handle) == 1)
            .min_by_key(|(_, entry)| entry.used)
            .map(|(root, _)| root.clone())
        else {
            return;
        };
        map.remove(&victim);
    }
}

fn lock_handle(handle: &Arc<Mutex<MemoryWiki>>) -> MutexGuard<'_, MemoryWiki> {
    handle.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An exclusive lock on the store, released on drop.
pub(crate) struct WriteLock {
    file: std::fs::File,
}

impl WriteLock {
    /// Take the lock, waiting a bounded time for another writer.
    fn acquire(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("cannot open the memory store lock {}", path.display()))?;
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(LOCK_RETRY),
                Err(std::fs::TryLockError::WouldBlock) => bail!(
                    "another writer holds the memory store at {}; try again in a moment",
                    path.display()
                ),
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(anyhow!(error).context(format!("cannot lock the memory store {}", path.display())));
                }
            }
        }
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            crate::logging::debug(&format!("memory store lock released with an error: {error}"));
        }
    }
}

/// Create the store directory owner-only.
pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("cannot create the memory directory {}", path.display()))?;
    horon_agent_platform::fs::set_dir_permissions_owner_only(path)
        .with_context(|| format!("cannot restrict the memory directory {}", path.display()))?;
    Ok(())
}

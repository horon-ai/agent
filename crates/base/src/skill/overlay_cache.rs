//! Per-workspace cache of the project skill overlay, revalidated with `stat`.
//!
//! The project rows of the source table are read for every request that asks
//! for the session's skills. Reading them again when nothing changed costs one
//! directory scan and one frontmatter read per skill, every turn. This cache
//! keeps the composed overlay per workspace and proves it is still current with
//! metadata calls only:
//!
//! - every project source directory is stamped, so an added, removed, renamed
//!   or re-pointed entry changes the directory's stamp and forces a rescan;
//! - every child's `SKILL.md` is stamped (absent is a stamp too), so an edit, a
//!   same-second rewrite with a restored mtime (ctime and inode change), a file
//!   created in an existing directory, or a re-pointed symlink is noticed.
//!
//! When anything changed the overlay is rebuilt, and every `SKILL.md` whose
//! stamp still matches is taken from the previous build instead of being read
//! again. Nothing is read while the lock is held: two requests that revalidate
//! the same workspace at once may both rebuild, and both get the same result.
//!
//! Windows reports no ctime, so a deliberate same-size rewrite that restores
//! the mtime is not noticed there; every other platform compares nanosecond
//! ctime and the inode.

use super::body::FileStamp;
use super::{Skill, SkillRegistry, sources};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

/// Workspaces kept at once. A daemon serves a handful of repositories; the
/// bound keeps one that wanders through many directories from growing forever.
const MAX_WORKSPACES: usize = 64;

/// Parsed skills of the previous build, keyed by their `SKILL.md`, with the
/// stamp the file had before that build read it.
pub(super) type Reused = HashMap<PathBuf, (FileStamp, Skill)>;

/// One child directory of a source and the stamp of its `SKILL.md`.
struct ChildPrint {
    skill_md: PathBuf,
    stamp: Option<FileStamp>,
}

/// One project source directory as it was when the overlay was built.
struct SourcePrint {
    dir: PathBuf,
    stamp: Option<FileStamp>,
    children: Vec<ChildPrint>,
}

struct CachedOverlay {
    sources: Vec<SourcePrint>,
    registry: Arc<SkillRegistry>,
    reused: Reused,
}

struct Slot {
    overlay: Arc<CachedOverlay>,
    last_used: u64,
}

static SLOTS: LazyLock<Mutex<HashMap<PathBuf, Slot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn slots() -> MutexGuard<'static, HashMap<PathBuf, Slot>> {
    SLOTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn tick() -> u64 {
    static TICK: AtomicU64 = AtomicU64::new(0);
    TICK.fetch_add(1, Ordering::Relaxed)
}

/// The stamp of `path`, or `None` when nothing is there to stamp. "Absent" is
/// a state the next check compares against, so a directory or file created
/// later is noticed. A path that exists but cannot be stamped is also `None`,
/// which keeps comparing unequal to a real stamp once it becomes readable.
fn stamp(path: &Path) -> Option<FileStamp> {
    match FileStamp::capture(path) {
        Ok(stamp) => Some(stamp),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            crate::logging::debug(&format!("Skills: cannot stat {}: {error}", path.display()));
            None
        }
    }
}

/// The cache key for a workspace: its real path, so two spellings of one
/// directory share an entry. `None` when the process cwd cannot be named, in
/// which case the overlay is built without caching.
fn key_for(working_dir: Option<&Path>) -> Option<PathBuf> {
    let dir = match working_dir {
        Some(dir) => dir.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(_) => return None,
        },
    };
    Some(match dir.canonicalize() {
        Ok(real) => real,
        Err(_) => dir,
    })
}

/// The project overlay for `working_dir`, read from disk only when a stamp
/// says something changed.
pub(super) fn overlay(working_dir: Option<&Path>) -> Arc<SkillRegistry> {
    let dirs = sources::project_source_dirs(working_dir);
    let Some(key) = key_for(working_dir) else {
        return Arc::clone(&build(working_dir, dirs, &Reused::new()).registry);
    };

    let cached = slots().get_mut(&key).map(|slot| {
        slot.last_used = tick();
        Arc::clone(&slot.overlay)
    });
    if let Some(cached) = &cached
        && is_current(cached, &dirs)
    {
        return Arc::clone(&cached.registry);
    }

    let empty = Reused::new();
    let reuse = cached.as_ref().map_or(&empty, |cached| &cached.reused);
    let rebuilt = Arc::new(build(working_dir, dirs, reuse));
    let registry = Arc::clone(&rebuilt.registry);
    insert(key, rebuilt);
    registry
}

/// Drop the cached overlay of `working_dir`, so the next lookup reads it again.
pub(super) fn forget(working_dir: Option<&Path>) {
    if let Some(key) = key_for(working_dir) {
        slots().remove(&key);
    }
}

/// Empty the cache, so a test starts from a cold process.
#[cfg(test)]
pub(super) fn clear_for_tests() {
    slots().clear();
}

#[cfg(test)]
pub(super) fn contains_for_tests(working_dir: &Path) -> bool {
    key_for(Some(working_dir)).is_some_and(|key| slots().contains_key(&key))
}

#[cfg(test)]
pub(super) fn len_for_tests() -> usize {
    slots().len()
}

fn is_current(cached: &CachedOverlay, dirs: &[PathBuf]) -> bool {
    cached.sources.iter().map(|source| &source.dir).eq(dirs.iter())
        && cached.sources.iter().all(|source| {
            stamp(&source.dir) == source.stamp
                && source
                    .children
                    .iter()
                    .all(|child| stamp(&child.skill_md) == child.stamp)
        })
}

/// Stamp the sources first, then read them: a file that changes in between is
/// newer than its recorded stamp, so the next check rebuilds rather than
/// keeping a stale entry.
fn build(working_dir: Option<&Path>, dirs: Vec<PathBuf>, reuse: &Reused) -> CachedOverlay {
    let sources: Vec<SourcePrint> = dirs.into_iter().map(capture_source).collect();
    let mut registry = SkillRegistry::default();
    let parsed = sources::load_project_reusing(&mut registry, working_dir, reuse);

    let stamps: HashMap<&Path, &FileStamp> = sources
        .iter()
        .flat_map(|source| source.children.iter())
        .filter_map(|child| child.stamp.as_ref().map(|stamp| (child.skill_md.as_path(), stamp)))
        .collect();
    let reused = parsed
        .into_iter()
        .filter_map(|skill| {
            let stamp = (*stamps.get(skill.path.as_path())?).clone();
            Some((skill.path.clone(), (stamp, skill)))
        })
        .collect();
    CachedOverlay {
        sources,
        registry: Arc::new(registry),
        reused,
    }
}

fn capture_source(dir: PathBuf) -> SourcePrint {
    let dir_stamp = stamp(&dir);
    let mut children = Vec::new();
    if dir_stamp.is_some() && dir.is_dir() {
        horon_agent_perf_counters::record_skill_dir_read();
        #[cfg(test)]
        super::test_support::record_dir_scan();
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let skill_md = path.join("SKILL.md");
                        let stamp = stamp(&skill_md);
                        children.push(ChildPrint { skill_md, stamp });
                    }
                }
            }
            Err(error) => {
                // The scan that follows reports the same failure as a warning;
                // the stamp still notices when the directory becomes readable.
                crate::logging::debug(&format!("Skills: cannot list {}: {error}", dir.display()));
            }
        }
    }
    children.sort_by(|a, b| a.skill_md.cmp(&b.skill_md));
    SourcePrint {
        dir,
        stamp: dir_stamp,
        children,
    }
}

fn insert(key: PathBuf, overlay: Arc<CachedOverlay>) {
    let mut slots = slots();
    slots.insert(
        key,
        Slot {
            overlay,
            last_used: tick(),
        },
    );
    while slots.len() > MAX_WORKSPACES {
        let Some(oldest) = slots
            .iter()
            .min_by_key(|(_, slot)| slot.last_used)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        slots.remove(&oldest);
    }
}

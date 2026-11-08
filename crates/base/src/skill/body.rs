//! Lazy skill bodies and the file identity key that keeps them fresh.
//!
//! Discovery reads only the frontmatter, so a body is loaded on first use and
//! reused while the file keeps the same [`FileStamp`]. The same stamp is the
//! shared identity key for the workspace overlay cache, which compares it for
//! directories and files alike.

use anyhow::Result;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// Identity of one file or directory: enough to notice an edit, a replacement,
/// a same-second rewrite, or a re-pointed symlink without reading contents.
///
/// Unix compares the nanosecond timestamps plus the inode, so restoring the
/// mtime after an edit is still detected through the ctime, and a symlink that
/// now points at another same-sized file is detected through the inode. Windows
/// has no ctime, so it compares length, modification time, and creation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileStamp {
    len: u64,
    #[cfg(unix)]
    mtime: (i64, i64),
    #[cfg(unix)]
    ctime: (i64, i64),
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    dev: u64,
    #[cfg(windows)]
    modified: Option<std::time::SystemTime>,
    #[cfg(windows)]
    created: Option<std::time::SystemTime>,
    #[cfg(not(any(unix, windows)))]
    modified: Option<std::time::SystemTime>,
}

impl FileStamp {
    /// Stamp the file or directory `path` currently resolves to.
    pub(crate) fn capture(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self::from_metadata(&metadata))
    }

    #[cfg(unix)]
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
            ino: metadata.ino(),
            dev: metadata.dev(),
        }
    }

    #[cfg(windows)]
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: optional_time(metadata.modified(), "modification"),
            created: optional_time(metadata.created(), "creation"),
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: optional_time(metadata.modified(), "modification"),
        }
    }
}

/// One timestamp a platform may decline to report.
///
/// The length is always known, so a missing timestamp weakens this stamp rather
/// than invalidating it: two files of one length then compare equal, which is
/// the same trust level a filesystem without sub-second times already has. The
/// reason is recorded so a stale body can be traced to the platform.
#[cfg(any(windows, not(any(unix, windows))))]
fn optional_time(result: std::io::Result<std::time::SystemTime>, what: &str) -> Option<std::time::SystemTime> {
    match result {
        Ok(time) => Some(time),
        Err(error) => {
            crate::logging::debug(&format!("Skills: no {what} time on this platform ({error})"));
            None
        }
    }
}

/// One loaded body, tied to the stamp it was read under. The search text is
/// built once per loaded body, so it is rebuilt exactly when the file changes.
pub(crate) struct CachedBody {
    stamp: FileStamp,
    body: Arc<str>,
    search_text: OnceLock<String>,
}

impl CachedBody {
    fn new(stamp: FileStamp, body: Arc<str>) -> Self {
        Self {
            stamp,
            body,
            search_text: OnceLock::new(),
        }
    }

    pub(crate) fn body(&self) -> &Arc<str> {
        &self.body
    }

    pub(crate) fn search_text(&self, build: impl FnOnce() -> String) -> &str {
        self.search_text.get_or_init(build).as_str()
    }
}

impl std::fmt::Debug for CachedBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedBody")
            .field("stamp", &self.stamp)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Body cache shared by every clone of a skill. A plain `OnceLock` cannot be
/// replaced when the file changes, so the cached value sits behind a mutex that
/// `&self` loads can replace.
pub(crate) type BodyCache = Arc<Mutex<Option<CachedBody>>>;

pub(crate) fn new_cache() -> BodyCache {
    Arc::new(Mutex::new(None))
}

fn cache_guard(cache: &BodyCache) -> MutexGuard<'_, Option<CachedBody>> {
    cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Serve the cached body, reading the file first when its identity changed.
///
/// `read` receives the loaded body and whether this call had to read the file,
/// so a caller can report or assert on its own measurement instead of a shared
/// process-wide counter. The unchanged path costs one `metadata` call and no
/// file read. A stat failure is reported instead of holding a stale body, so a
/// deleted skill errors rather than silently serving old text.
pub(crate) fn with_cached<T>(path: &Path, cache: &BodyCache, read: impl FnOnce(&CachedBody, bool) -> T) -> Result<T> {
    let stamp = FileStamp::capture(path)?;
    let mut guard = cache_guard(cache);
    let mut read_from_disk = false;
    if guard.as_ref().is_none_or(|cached| cached.stamp != stamp) {
        let content = std::fs::read_to_string(path)?;
        horon_agent_perf_counters::record_skill_file_read(content.len() as u64);
        let body = Arc::from(super::frontmatter::parse_text(&content)?.body);
        *guard = Some(CachedBody::new(stamp, body));
        read_from_disk = true;
    }
    let Some(cached) = guard.as_ref() else {
        // The branch above always fills the cache before this point; reporting
        // the impossible state costs one comparison and keeps a panic out of a
        // path that a future edit could reach.
        anyhow::bail!("skill body cache for {} is empty after caching", path.display());
    };
    Ok(read(cached, read_from_disk))
}

/// The markdown body, reading the file only when its identity changed.
pub(crate) fn load(path: &Path, cache: &BodyCache) -> Result<Arc<str>> {
    with_cached(path, cache, |cached, _read| Arc::clone(cached.body()))
}

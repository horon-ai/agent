use super::{
    ChangeKind, MAX_WATCHED_FILE_BYTES, MonitorChange, MonitorError, MonitorResult, PauseReason, SAMPLE_CHUNK_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// One self-consistent view of a watched file.
///
/// `digest` is the whole-file SHA-256, which is what makes a rewrite that keeps
/// both the size and the mtime a detected change instead of an invisible one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
    pub digest: String,
    /// Where the recorded path resolved to at sample time, so a symlink retarget
    /// is visible even when the inode is reused.
    pub target: PathBuf,
}

impl FileSnapshot {
    /// Whether two samples describe the same file object.
    pub fn same_file(&self, other: &Self) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
}

/// What one sampling attempt produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SampleOutcome {
    /// The path does not exist.
    Missing,
    /// A complete read that held still.
    Stable(FileSnapshot),
    /// The file changed between the opening and closing `fstat`, so this sample
    /// proves nothing and must be discarded.
    Unstable,
    /// Larger than [`MAX_WATCHED_FILE_BYTES`]; a partial digest would be wrong.
    TooLarge { len: u64 },
    /// A directory, socket, fifo, or device.
    NotRegular { kind: &'static str },
    /// The filesystem refused the read.
    Denied { message: String },
}

/// Validate a path for a file monitor and return the absolute canonical form.
///
/// A path that does not exist yet is allowed, because a monitor may be created
/// before the file it waits for. Its parent directory must exist, so the
/// canonical form is still absolute and free of `..`.
pub fn canonical_watch_path(raw: &Path) -> MonitorResult<PathBuf> {
    if !raw.is_absolute() {
        return Err(MonitorError::invalid(format!(
            "watch path {} must be absolute",
            raw.display()
        )));
    }
    match std::fs::metadata(raw) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(MonitorError::invalid(format!(
                    "watch path {} is a {}, not a regular file",
                    raw.display(),
                    describe_kind(&metadata)
                )));
            }
            let canonical = std::fs::canonicalize(raw)
                .map_err(|error| MonitorError::io(format!("cannot canonicalize {}: {error}", raw.display())))?;
            Ok(canonical)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = raw
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or_else(|| MonitorError::invalid(format!("watch path {} has no parent", raw.display())))?;
            let name = raw
                .file_name()
                .ok_or_else(|| MonitorError::invalid(format!("watch path {} has no file name", raw.display())))?;
            let parent = std::fs::canonicalize(parent).map_err(|error| {
                MonitorError::invalid(format!(
                    "watch path {} cannot be watched because its directory is unreadable: {error}",
                    raw.display()
                ))
            })?;
            Ok(parent.join(name))
        }
        Err(error) => Err(MonitorError::io(format!(
            "cannot inspect watch path {}: {error}",
            raw.display()
        ))),
    }
}

fn describe_kind(metadata: &std::fs::Metadata) -> &'static str {
    if metadata.is_dir() {
        return "directory";
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let kind = metadata.file_type();
        if kind.is_socket() {
            return "socket";
        }
        if kind.is_fifo() {
            return "fifo";
        }
        if kind.is_block_device() {
            return "block device";
        }
        if kind.is_char_device() {
            return "character device";
        }
    }
    "non-regular file"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_secs: i64,
    mtime_nanos: u32,
}

fn stamp_of(metadata: &std::fs::Metadata) -> FileStamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileStamp {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mtime_secs: metadata.mtime(),
            mtime_nanos: metadata.mtime_nsec() as u32,
        }
    }
    #[cfg(not(unix))]
    {
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .unwrap_or_default();
        FileStamp {
            dev: 0,
            ino: 0,
            len: metadata.len(),
            mtime_secs: mtime.as_secs() as i64,
            mtime_nanos: mtime.subsec_nanos(),
        }
    }
}

/// Sample one path: `open` → `fstat` → streaming digest → `fstat`.
pub fn sample_file(path: &Path) -> SampleOutcome {
    sample_file_with(path, &mut || {})
}

/// Sample one path, running `hook` between the opening `fstat` and the read.
///
/// The hook exists so a test can mutate the file mid-read deterministically
/// instead of racing a writer; production calls [`sample_file`].
pub fn sample_file_with(path: &Path, hook: &mut dyn FnMut()) -> SampleOutcome {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SampleOutcome::Missing,
        Err(error) => {
            return SampleOutcome::Denied {
                message: error.to_string(),
            };
        }
    };
    if !metadata.is_file() {
        return SampleOutcome::NotRegular {
            kind: describe_kind(&metadata),
        };
    }

    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return SampleOutcome::Denied {
                message: error.to_string(),
            };
        }
        Err(error) => {
            return SampleOutcome::Denied {
                message: error.to_string(),
            };
        }
    };

    let opened = match file.metadata() {
        Ok(metadata) => stamp_of(&metadata),
        Err(error) => {
            return SampleOutcome::Denied {
                message: error.to_string(),
            };
        }
    };
    if opened.len > MAX_WATCHED_FILE_BYTES {
        return SampleOutcome::TooLarge { len: opened.len };
    }

    hook();

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; SAMPLE_CHUNK_BYTES];
    let mut read_total: u64 = 0;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                read_total = read_total.saturating_add(count as u64);
                if read_total > MAX_WATCHED_FILE_BYTES {
                    return SampleOutcome::TooLarge { len: read_total };
                }
                hasher.update(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return SampleOutcome::Denied {
                    message: error.to_string(),
                };
            }
        }
    }

    let closed = match file.metadata() {
        Ok(metadata) => stamp_of(&metadata),
        Err(error) => {
            return SampleOutcome::Denied {
                message: error.to_string(),
            };
        }
    };

    if closed != opened || read_total != opened.len {
        return SampleOutcome::Unstable;
    }

    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    SampleOutcome::Stable(FileSnapshot {
        dev: closed.dev,
        ino: closed.ino,
        len: closed.len,
        mtime_secs: closed.mtime_secs,
        mtime_nanos: closed.mtime_nanos,
        digest: hex::encode(hasher.finalize()),
        target,
    })
}

/// The file half of a monitor: a baseline plus the last change it proved.
///
/// The first sample establishes the baseline silently, because creating a
/// monitor is not itself a change. A monitor created before its file exists
/// reports the file's arrival as [`ChangeKind::Created`].
#[derive(Clone, Debug)]
pub struct FileWatcher {
    path: PathBuf,
    last: Option<FileSnapshot>,
    started_missing: bool,
    baselined: bool,
    pause: Option<PauseReason>,
}

impl FileWatcher {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            last: None,
            started_missing: false,
            baselined: false,
            pause: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn last(&self) -> Option<&FileSnapshot> {
        self.last.as_ref()
    }

    pub fn pause(&self) -> Option<PauseReason> {
        self.pause
    }

    /// Adopt a snapshot a previous process journaled, so a change that happened
    /// while nothing was running is still detected on the next sample.
    pub fn resume(&mut self, snapshot: Option<FileSnapshot>) {
        if let Some(snapshot) = snapshot {
            self.last = Some(snapshot);
            self.baselined = true;
        }
    }

    pub fn clear_pause(&mut self) {
        self.pause = None;
    }

    /// Fold one sample in and return the change it proves, if any.
    pub fn observe(&mut self, outcome: SampleOutcome) -> Option<MonitorChange> {
        match outcome {
            SampleOutcome::Unstable => None,
            SampleOutcome::TooLarge { len } => self.pause_with(
                PauseReason::TooLarge,
                format!(
                    "{} is {len} bytes, over the {} byte cap; no comparison is possible",
                    self.path.display(),
                    MAX_WATCHED_FILE_BYTES
                ),
                serde_json::json!({"path": self.path.to_string_lossy(), "bytes": len, "cap": MAX_WATCHED_FILE_BYTES}),
            ),
            SampleOutcome::Denied { message } => self.pause_with(
                PauseReason::Denied,
                format!("{} cannot be read: {message}", self.path.display()),
                serde_json::json!({"path": self.path.to_string_lossy(), "error": message}),
            ),
            SampleOutcome::NotRegular { kind } => self.pause_with(
                PauseReason::NotRegular,
                format!("{} is now a {kind}, not a regular file", self.path.display()),
                serde_json::json!({"path": self.path.to_string_lossy(), "kind": kind}),
            ),
            SampleOutcome::Missing => {
                if !self.baselined {
                    if self.started_missing {
                        return None;
                    }
                    self.started_missing = true;
                    return None;
                }
                let previous = self.last.take()?;
                Some(
                    MonitorChange::new(ChangeKind::Deleted, format!("{} was deleted", self.path.display()))
                        .with_detail(serde_json::json!({
                            "path": self.path.to_string_lossy(),
                            "previous_bytes": previous.len,
                        })),
                )
            }
            SampleOutcome::Stable(snapshot) => {
                self.pause = None;
                if !self.baselined {
                    self.baselined = true;
                    let arrived = self.started_missing;
                    self.last = Some(snapshot.clone());
                    if !arrived {
                        return None;
                    }
                    return Some(
                        MonitorChange::new(
                            ChangeKind::Created,
                            format!("{} appeared with {} bytes", self.path.display(), snapshot.len),
                        )
                        .with_detail(serde_json::json!({
                            "path": self.path.to_string_lossy(),
                            "bytes": snapshot.len,
                            "digest": snapshot.digest,
                        })),
                    );
                }
                let Some(previous) = self.last.replace(snapshot.clone()) else {
                    return Some(
                        MonitorChange::new(
                            ChangeKind::Created,
                            format!("{} reappeared with {} bytes", self.path.display(), snapshot.len),
                        )
                        .with_detail(serde_json::json!({
                            "path": self.path.to_string_lossy(),
                            "bytes": snapshot.len,
                        })),
                    );
                };
                if !previous.same_file(&snapshot) || previous.target != snapshot.target {
                    return Some(
                        MonitorChange::new(
                            ChangeKind::Replaced,
                            format!(
                                "{} was replaced by a different file of {} bytes",
                                self.path.display(),
                                snapshot.len
                            ),
                        )
                        .with_detail(serde_json::json!({
                            "path": self.path.to_string_lossy(),
                            "target": snapshot.target.to_string_lossy(),
                            "previous_target": previous.target.to_string_lossy(),
                            "bytes": snapshot.len,
                            "previous_bytes": previous.len,
                        })),
                    );
                }
                if previous.digest != snapshot.digest {
                    return Some(
                        MonitorChange::new(
                            ChangeKind::Modified,
                            format!(
                                "{} changed, {} bytes (was {})",
                                self.path.display(),
                                snapshot.len,
                                previous.len
                            ),
                        )
                        .with_detail(serde_json::json!({
                            "path": self.path.to_string_lossy(),
                            "bytes": snapshot.len,
                            "previous_bytes": previous.len,
                            "digest": snapshot.digest,
                            "previous_digest": previous.digest,
                        })),
                    );
                }
                None
            }
        }
    }

    fn pause_with(&mut self, reason: PauseReason, summary: String, detail: serde_json::Value) -> Option<MonitorChange> {
        if self.pause == Some(reason) {
            return None;
        }
        self.pause = Some(reason);
        Some(
            MonitorChange::new(ChangeKind::Paused, summary)
                .with_detail(detail)
                .with_pause(reason),
        )
    }
}

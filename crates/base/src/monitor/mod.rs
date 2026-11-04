//! Persistent file and command monitors with bounded durable wake delivery.
//!
//! A monitor watches one thing so the model does not have to poll it: a single
//! absolute regular file, or one command's stdout/stderr and exit. What a monitor
//! observes becomes a durable wake, and a wake is the only way it reaches the
//! conversation.
//!
//! * A file is sampled once per second. A stable sample is `open` → `fstat` →
//!   64 KiB streaming digest → `fstat`; a sample that changed in between is
//!   discarded and retried on the next tick. Over the 16 MiB cap the monitor
//!   pauses as `too_large` rather than compare a partial digest. A missing file is
//!   allowed and its creation is detected; a replacement or symlink retarget is
//!   reported as `replaced` and the new regular file is followed. Directories,
//!   sockets, fifos, and devices are refused.
//! * A command runs once through the same shell, working directory, permission
//!   gate, and process-group cancellation the `bash` tool uses. Its only restart
//!   policy is `never`: a restore records `needs_rearm` and starts no process, and
//!   only an explicit `rearm` starts a new one.
//! * Every bound is a hard ceiling: 16 monitors per session and 256 per process,
//!   8 KiB of inline output cut on a UTF-8 boundary, a spool of two 1 MiB files
//!   that reports how many bytes rotated away, and a 64-line output queue before a
//!   separate spool task takes over, past which an overflow state records the
//!   dropped count instead of growing memory.
//! * Wakes are paced to one per monitor per 30 s and six tokens per session per
//!   minute. The first change spends a token immediately; the rest coalesce into
//!   the latest checkpoint plus a count. Delivery is `wake.pending` (fsynced) →
//!   enqueue on the durable input path with the wake id as its source id →
//!   `wake.acked`, and a redelivery after a restart dedups by that id. A busy
//!   queue keeps the pending wake for the next attempt.

mod command;
mod delivery;
mod file;

pub use command::*;
pub use delivery::*;
pub use file::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Monitors one session may hold.
pub const MAX_MONITORS_PER_SESSION: usize = 16;

/// Monitors one process may hold across every session.
pub const MAX_MONITORS_PER_PROCESS: usize = 256;

/// Output bytes kept inline before the spool takes over.
pub const INLINE_OUTPUT_BYTES: usize = 8 * 1024;

/// Bytes in one spool file.
pub const SPOOL_FILE_BYTES: u64 = 1024 * 1024;

/// Spool files per monitor; the older one rotates away and its byte count is
/// reported instead of forgotten.
pub const SPOOL_FILE_COUNT: usize = 2;

/// Output lines buffered before the separate spool task takes over.
pub const OUTPUT_QUEUE_LINES: usize = 64;

/// File sampling cadence.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Digest chunk size; a sample never allocates the whole file.
pub const SAMPLE_CHUNK_BYTES: usize = 64 * 1024;

/// Largest file a monitor digests.
pub const MAX_WATCHED_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Shortest gap between two wakes from one monitor.
pub const MIN_WAKE_INTERVAL: Duration = Duration::from_secs(30);

/// Wake tokens one session may spend per rolling window.
pub const WAKE_TOKENS_PER_MINUTE: usize = 6;

/// The window the wake tokens refill over.
pub const WAKE_TOKEN_WINDOW: Duration = Duration::from_secs(60);

/// What a monitor watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorKind {
    File,
    Command,
}

impl MonitorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Command => "command",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "file" => Some(Self::File),
            "command" => Some(Self::Command),
            _ => None,
        }
    }
}

/// When a command monitor may start a process again after a restart.
///
/// One value, deliberately: a monitor never restarts a command by itself, so a
/// crash loop cannot turn one shell command into an unbounded one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    #[default]
    Never,
}

impl RestartPolicy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
        }
    }
}

/// Where a monitor stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorStatus {
    /// Observing a file, or running a command.
    Running,
    /// Restored after a restart with a command that must not be restarted. No
    /// process runs until an explicit `rearm`.
    NeedsRearm,
    /// Not comparing, because the target cannot be compared safely.
    Paused,
    /// Finished or cancelled.
    Stopped,
}

impl MonitorStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::NeedsRearm => "needs_rearm",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
        }
    }
}

/// Why a monitor paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// Over [`MAX_WATCHED_FILE_BYTES`]; a partial digest would be a wrong answer.
    TooLarge,
    /// The path could not be read.
    Denied,
    /// The path is no longer a regular file.
    NotRegular,
}

impl PauseReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::Denied => "denied",
            Self::NotRegular => "not_regular",
        }
    }
}

/// What one monitor watches, as it is journaled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorSpec {
    pub kind: MonitorKind,
    /// Absolute canonical path of a file monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Shell command of a command monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Working directory; `None` uses the session's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Why the monitor exists; echoed in every wake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default)]
    pub restart_policy: RestartPolicy,
}

impl MonitorSpec {
    pub fn file(path: impl Into<String>) -> Self {
        Self {
            kind: MonitorKind::File,
            path: Some(path.into()),
            command: None,
            cwd: None,
            intent: None,
            restart_policy: RestartPolicy::Never,
        }
    }

    pub fn command(command: impl Into<String>) -> Self {
        Self {
            kind: MonitorKind::Command,
            path: None,
            command: Some(command.into()),
            cwd: None,
            intent: None,
            restart_policy: RestartPolicy::Never,
        }
    }

    pub fn target(&self) -> &str {
        self.path
            .as_deref()
            .or(self.command.as_deref())
            .unwrap_or("<unspecified>")
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn from_value(value: &Value) -> MonitorResult<Self> {
        serde_json::from_value(value.clone())
            .map_err(|error| MonitorError::invalid(format!("monitor spec is unreadable: {error}")))
    }
}

/// A change a monitor observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A missing path appeared.
    Created,
    /// Content changed. The digest decides this, so a same-size same-mtime
    /// rewrite is still a change.
    Modified,
    /// A different file now occupies the path, or a symlink was retargeted.
    Replaced,
    /// The path stopped existing.
    Deleted,
    /// The command wrote new output lines.
    Output,
    /// The command exited.
    Exited,
    /// The monitor paused; the change carries the reason.
    Paused,
}

impl ChangeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Replaced => "replaced",
            Self::Deleted => "deleted",
            Self::Output => "output",
            Self::Exited => "exited",
            Self::Paused => "paused",
        }
    }
}

/// One observed change, ready to fold into a checkpoint and a wake.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorChange {
    pub kind: ChangeKind,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<PauseReason>,
}

impl MonitorChange {
    pub fn new(kind: ChangeKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: summary.into(),
            detail: Value::Null,
            pause: None,
        }
    }

    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }

    pub fn with_pause(mut self, reason: PauseReason) -> Self {
        self.pause = Some(reason);
        self
    }
}

/// A monitor as the tool and the journal see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorRecord {
    pub id: String,
    pub session_id: String,
    pub spec: MonitorSpec,
    pub status: MonitorStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<PauseReason>,
    /// Journal sequence of the row that created or last re-armed this monitor.
    pub created_seq: u64,
    /// Latest observation counter; a wake names the sequence it covers.
    pub monitor_seq: u64,
    /// Changes observed since creation.
    pub changes: u64,
    /// Wakes delivered for this monitor.
    pub wakes: u64,
    /// Changes folded into a wake that has not been delivered yet.
    #[serde(default)]
    pub coalesced: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_summary: Option<String>,
}

impl MonitorRecord {
    pub fn new(session_id: &str, id: String, spec: MonitorSpec, created_seq: u64) -> Self {
        Self {
            id,
            session_id: session_id.to_string(),
            spec,
            status: MonitorStatus::Running,
            pause: None,
            created_seq,
            monitor_seq: 0,
            changes: 0,
            wakes: 0,
            coalesced: 0,
            last_summary: None,
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(
            self.status,
            MonitorStatus::Running | MonitorStatus::Paused | MonitorStatus::NeedsRearm
        )
    }
}

/// Why a monitor operation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorErrorKind {
    /// A relative path, a directory, a missing target, an unknown action, or a
    /// restart policy other than `never`.
    Invalid,
    /// No monitor with that id exists in this session.
    NotFound,
    /// A bound refused the monitor.
    Limit,
    /// The filesystem or the child process refused an operation.
    Io,
    /// The durable journal refused a transition; nothing was published.
    Journal,
}

impl MonitorErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotFound => "not_found",
            Self::Limit => "limit",
            Self::Io => "io",
            Self::Journal => "journal",
        }
    }
}

/// A monitor failure with the reason callers branch on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorError {
    kind: MonitorErrorKind,
    message: String,
}

impl MonitorError {
    pub fn new(kind: MonitorErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(MonitorErrorKind::Invalid, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(MonitorErrorKind::NotFound, message)
    }

    pub fn limit(message: impl Into<String>) -> Self {
        Self::new(MonitorErrorKind::Limit, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(MonitorErrorKind::Io, message)
    }

    pub fn journal(message: impl Into<String>) -> Self {
        Self::new(MonitorErrorKind::Journal, message)
    }

    pub fn kind(&self) -> MonitorErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MonitorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for MonitorError {}

impl From<crate::durable_runtime::JournalError> for MonitorError {
    fn from(error: crate::durable_runtime::JournalError) -> Self {
        Self::journal(error.to_string())
    }
}

impl From<std::io::Error> for MonitorError {
    fn from(error: std::io::Error) -> Self {
        Self::io(error.to_string())
    }
}

pub type MonitorResult<T> = Result<T, MonitorError>;

/// Monitors this process holds, across every session.
static PROCESS_LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Monitors this process currently holds; the process ceiling reads it.
pub fn process_monitor_count() -> usize {
    PROCESS_LIVE.load(std::sync::atomic::Ordering::Relaxed)
}

fn process_acquire(count: usize) -> MonitorResult<()> {
    use std::sync::atomic::Ordering;
    let mut current = PROCESS_LIVE.load(Ordering::Relaxed);
    loop {
        if current + count > MAX_MONITORS_PER_PROCESS {
            return Err(MonitorError::limit(format!(
                "this process already holds {current} monitors; the ceiling is {MAX_MONITORS_PER_PROCESS}"
            )));
        }
        match PROCESS_LIVE.compare_exchange_weak(current, current + count, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return Ok(()),
            Err(actual) => current = actual,
        }
    }
}

fn process_release(count: usize) {
    use std::sync::atomic::Ordering;
    let _ = PROCESS_LIVE.try_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(count))
    });
}

/// What a monitor is observing in memory.
struct MonitorBody {
    file: Option<FileWatcher>,
    command: Option<CommandWatcher>,
}

struct ActiveMonitor {
    record: MonitorRecord,
    body: MonitorBody,
    pending: Option<CoalescedWake>,
    /// Whether the journal already holds this monitor's starting point. Until it
    /// does, the first sample is a baseline and not a change.
    baselined: bool,
}

/// One session's monitors, their durable journal, and their wake delivery.
///
/// Every method that can touch the disk is synchronous and must run off a Tokio
/// worker. `start_command` is the one entry point that spawns a child, and it
/// stays synchronous too, so no caller ever holds this runtime's lock across an
/// await.
pub struct MonitorRuntime {
    session_id: String,
    store: crate::durable_runtime::DurableRuntimeStore,
    clock: std::sync::Arc<dyn Clock>,
    sink: std::sync::Arc<dyn WakeSink>,
    policy: std::sync::Arc<dyn CommandPolicy>,
    pacer: WakePacer,
    monitors: std::collections::BTreeMap<String, ActiveMonitor>,
    acquired: usize,
}

impl Drop for MonitorRuntime {
    fn drop(&mut self) {
        for monitor in self.monitors.values_mut() {
            if let Some(command) = monitor.body.command.as_mut() {
                command.cancel();
            }
        }
        process_release(self.acquired);
        self.acquired = 0;
    }
}

impl MonitorRuntime {
    /// Open a session's monitors over an explicit journal root.
    pub fn open_in(
        root: &std::path::Path,
        session_id: &str,
        clock: std::sync::Arc<dyn Clock>,
        sink: std::sync::Arc<dyn WakeSink>,
        policy: std::sync::Arc<dyn CommandPolicy>,
    ) -> MonitorResult<Self> {
        let store = crate::durable_runtime::DurableRuntimeStore::open_in(root, session_id)?;
        let mut runtime = Self {
            session_id: session_id.to_string(),
            store,
            clock,
            sink,
            policy,
            pacer: WakePacer::default(),
            monitors: std::collections::BTreeMap::new(),
            acquired: 0,
        };
        runtime.restore()?;
        runtime.redeliver_pending()?;
        Ok(runtime)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn store(&self) -> &crate::durable_runtime::DurableRuntimeStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut crate::durable_runtime::DurableRuntimeStore {
        &mut self.store
    }

    pub fn pacer(&self) -> &WakePacer {
        &self.pacer
    }

    /// Rebuild the in-memory monitors from the journal.
    ///
    /// A command monitor never restarts here: it comes back as
    /// [`MonitorStatus::NeedsRearm`] with no process. A file monitor resumes from
    /// the snapshot its last checkpoint recorded, so a change that happened while
    /// the process was down is reported on the next tick.
    pub fn restore(&mut self) -> MonitorResult<()> {
        let state = self.store.snapshot()?;
        let mut wanted: Vec<(String, String, Value, Option<Value>, bool)> = Vec::new();
        for (id, entry) in &state.monitors {
            if entry.stopped.is_some() {
                continue;
            }
            let checkpoint = entry.last_checkpoint.as_ref();
            let detail = checkpoint
                .map(|checkpoint| checkpoint.detail.clone())
                .filter(|detail| !detail.is_null());
            let baselined = checkpoint.is_some();
            wanted.push((
                id.clone(),
                entry.monitor_kind.clone(),
                entry.spec.clone(),
                detail,
                baselined,
            ));
        }

        let fresh = wanted
            .iter()
            .filter(|(id, _, _, _, _)| !self.monitors.contains_key(id))
            .count();
        process_acquire(fresh)?;
        self.acquired += fresh;

        for (id, kind, spec, detail, baselined) in wanted {
            if self.monitors.contains_key(&id) {
                continue;
            }
            let spec = MonitorSpec::from_value(&spec)?;
            let mut record = MonitorRecord::new(&self.session_id, id.clone(), spec.clone(), 0);
            let body = match MonitorKind::parse(&kind) {
                Some(MonitorKind::File) => {
                    let path = spec
                        .path
                        .as_ref()
                        .map(PathBuf::from)
                        .ok_or_else(|| MonitorError::invalid(format!("monitor {id} has no path")))?;
                    let mut watcher = FileWatcher::new(path);
                    if let Some(snapshot) = detail.as_ref().and_then(|detail| detail.get("snapshot")) {
                        watcher.resume(serde_json::from_value(snapshot.clone()).ok());
                    }
                    MonitorBody {
                        file: Some(watcher),
                        command: None,
                    }
                }
                Some(MonitorKind::Command) => {
                    record.status = MonitorStatus::NeedsRearm;
                    let spool = Box::new(FileSpool::new(self.spool_dir(&id))?);
                    MonitorBody {
                        file: None,
                        command: Some(CommandWatcher::prepare(&spec, spool)?),
                    }
                }
                None => continue,
            };
            self.monitors.insert(
                id,
                ActiveMonitor {
                    record,
                    body,
                    pending: None,
                    baselined,
                },
            );
        }
        Ok(())
    }

    fn spool_dir(&self, id: &str) -> PathBuf {
        self.store.directory().join("spool").join(id)
    }

    /// Create a monitor and journal it.
    pub fn create(&mut self, spec: MonitorSpec, intent: Option<String>) -> MonitorResult<MonitorRecord> {
        // Stopped entries are kept so `inspect` can still answer for them, but
        // they are pruned here: without this, a session that creates and cancels
        // monitors forever would grow its map (and its journal) without bound.
        let live = self
            .monitors
            .values()
            .filter(|monitor| monitor.record.is_live())
            .count();
        if live < MAX_MONITORS_PER_SESSION && self.monitors.len() > MAX_MONITORS_PER_SESSION {
            let stopped: Vec<String> = self
                .monitors
                .iter()
                .filter(|(_, monitor)| !monitor.record.is_live())
                .map(|(id, _)| id.clone())
                .collect();
            for id in stopped {
                self.monitors.remove(&id);
            }
        }
        if live >= MAX_MONITORS_PER_SESSION {
            return Err(MonitorError::limit(format!(
                "this session already holds {live} monitors; the ceiling is {MAX_MONITORS_PER_SESSION}"
            )));
        }
        if spec.restart_policy != RestartPolicy::Never {
            return Err(MonitorError::invalid(
                "a monitor's restart policy is always `never`; use rearm to start a command again",
            ));
        }
        let mut spec = spec;
        if spec.intent.is_none() {
            spec.intent = intent;
        }
        let id = crate::durable_runtime::new_id();
        let body = match spec.kind {
            MonitorKind::File => {
                let raw = spec
                    .path
                    .as_ref()
                    .ok_or_else(|| MonitorError::invalid("a file monitor needs a path"))?;
                let path = canonical_watch_path(std::path::Path::new(raw))?;
                spec.path = Some(path.to_string_lossy().to_string());
                MonitorBody {
                    file: Some(FileWatcher::new(path)),
                    command: None,
                }
            }
            MonitorKind::Command => {
                let spool = Box::new(FileSpool::new(self.spool_dir(&id))?);
                MonitorBody {
                    file: None,
                    command: Some(CommandWatcher::prepare(&spec, spool)?),
                }
            }
        };

        process_acquire(1)?;
        self.acquired += 1;

        let payload = crate::durable_runtime::Payload::MonitorUpsert(crate::durable_runtime::MonitorUpsert {
            monitor_kind: spec.kind.as_str().to_string(),
            spec: spec.to_value(),
        });
        let row = match self.store.append(&id, payload) {
            Ok(row) => row,
            Err(error) => {
                process_release(1);
                self.acquired = self.acquired.saturating_sub(1);
                return Err(error.into());
            }
        };
        let record = MonitorRecord::new(&self.session_id, id.clone(), spec, row.seq);
        self.monitors.insert(
            id,
            ActiveMonitor {
                record: record.clone(),
                body,
                pending: None,
                baselined: false,
            },
        );
        Ok(record)
    }

    pub fn list(&self) -> Vec<MonitorRecord> {
        self.monitors.values().map(|monitor| monitor.record.clone()).collect()
    }

    pub fn inspect(&self, id: &str) -> MonitorResult<MonitorRecord> {
        self.monitors
            .get(id)
            .map(|monitor| monitor.record.clone())
            .ok_or_else(|| MonitorError::not_found(format!("no monitor {id} in this session")))
    }

    /// Stop a monitor and journal the stop. The stop row is written before the
    /// in-memory state changes, so a journal failure leaves the monitor live.
    pub fn cancel(&mut self, id: &str, reason: &str) -> MonitorResult<MonitorRecord> {
        let monitor = self
            .monitors
            .get_mut(id)
            .ok_or_else(|| MonitorError::not_found(format!("no monitor {id} in this session")))?;
        if monitor.record.status == MonitorStatus::Stopped {
            return Ok(monitor.record.clone());
        }
        let payload = crate::durable_runtime::Payload::MonitorStopped(crate::durable_runtime::MonitorStopped {
            reason: reason.to_string(),
        });
        self.store.append(id, payload)?;
        let monitor = self.monitors.get_mut(id).expect("monitor still present");
        if let Some(command) = monitor.body.command.as_mut() {
            command.cancel();
        }
        monitor.record.status = MonitorStatus::Stopped;
        monitor.pending = None;
        // A stopped monitor keeps no process and no spool bytes. The watcher
        // handle itself stays: `rearm` restarts the same command through it, and
        // it holds no live process once `cancel` has run.
        let record = monitor.record.clone();
        process_release(1);
        self.acquired = self.acquired.saturating_sub(1);
        let spool = self.spool_dir(id);
        if spool.exists()
            && let Err(error) = std::fs::remove_dir_all(&spool)
        {
            crate::logging::warn(&format!("could not remove the spool for monitor {id}: {error}"));
        }
        Ok(record)
    }

    /// Bring a stopped, paused, or restored monitor back into service.
    ///
    /// This is the only path that can start a command process again, and it only
    /// does so because a caller asked explicitly.
    pub fn rearm(&mut self, id: &str) -> MonitorResult<MonitorRecord> {
        let monitor = self
            .monitors
            .get(id)
            .ok_or_else(|| MonitorError::not_found(format!("no monitor {id} in this session")))?;
        let spec = monitor.record.spec.clone();
        let payload = crate::durable_runtime::Payload::MonitorUpsert(crate::durable_runtime::MonitorUpsert {
            monitor_kind: spec.kind.as_str().to_string(),
            spec: spec.to_value(),
        });
        let row = self.store.append(id, payload)?;
        let monitor = self.monitors.get_mut(id).expect("monitor still present");
        monitor.record.created_seq = row.seq;
        monitor.record.status = MonitorStatus::Running;
        monitor.record.pause = None;
        if let Some(watcher) = monitor.body.file.as_mut() {
            watcher.clear_pause();
        }
        Ok(monitor.record.clone())
    }

    /// Start a command monitor's process. Refuses unless the monitor is a command
    /// monitor that is not already running.
    pub fn start_command(&mut self, id: &str) -> MonitorResult<u32> {
        let session_id = self.session_id.clone();
        let policy = std::sync::Arc::clone(&self.policy);
        let monitor = self
            .monitors
            .get_mut(id)
            .ok_or_else(|| MonitorError::not_found(format!("no monitor {id} in this session")))?;
        let command = monitor
            .body
            .command
            .as_mut()
            .ok_or_else(|| MonitorError::invalid(format!("monitor {id} watches a file, not a command")))?;
        let pid = command.start(&session_id, policy.as_ref())?;
        monitor.record.status = MonitorStatus::Running;
        Ok(pid)
    }

    /// Sample every live monitor, journal what changed, and deliver any wake the
    /// pacer allows.
    pub fn tick(&mut self) -> MonitorResult<Vec<String>> {
        let ids: Vec<String> = self.monitors.keys().cloned().collect();
        let mut changed = false;
        for id in ids {
            changed |= self.observe(&id)?;
        }
        if !changed {
            return Ok(Vec::new());
        }
        self.deliver()
    }

    /// Block until a command monitor's child exits, then journal and deliver what
    /// it produced. This is the barrier a caller uses when it wants the terminal
    /// state now instead of sampling ticks.
    pub fn wait_command(&mut self, id: &str) -> MonitorResult<Vec<String>> {
        let changes = {
            let monitor = self
                .monitors
                .get_mut(id)
                .ok_or_else(|| MonitorError::not_found(format!("no monitor {id} in this session")))?;
            match monitor.body.command.as_mut() {
                Some(command) => command.wait(),
                None => Vec::new(),
            }
        };
        if changes.is_empty() {
            return Ok(Vec::new());
        }
        self.record_changes(id, changes)?;
        self.deliver()
    }

    fn observe(&mut self, id: &str) -> MonitorResult<bool> {
        let Some(monitor) = self.monitors.get(id) else {
            return Ok(false);
        };
        if !monitor.record.is_live() {
            return Ok(false);
        }
        let baselined = monitor.baselined;
        let changes = {
            let monitor = self.monitors.get_mut(id).expect("monitor still present");
            if let Some(watcher) = monitor.body.file.as_mut() {
                let outcome = sample_file(watcher.path());
                watcher.observe(outcome).into_iter().collect::<Vec<_>>()
            } else if let Some(command) = monitor.body.command.as_mut() {
                command.poll()
            } else {
                Vec::new()
            }
        };
        if changes.is_empty() {
            if !baselined {
                self.checkpoint_baseline(id)?;
            }
            return Ok(false);
        }
        self.record_changes(id, changes)?;
        Ok(true)
    }

    fn checkpoint_baseline(&mut self, id: &str) -> MonitorResult<()> {
        let detail = self.checkpoint_detail(id, &[]);
        let payload = crate::durable_runtime::Payload::MonitorCheckpoint(crate::durable_runtime::MonitorCheckpoint {
            monitor_seq: 0,
            summary: String::new(),
            detail,
        });
        self.store.append(id, payload)?;
        if let Some(monitor) = self.monitors.get_mut(id) {
            monitor.baselined = true;
        }
        Ok(())
    }

    fn checkpoint_detail(&self, id: &str, changes: &[MonitorChange]) -> Value {
        let snapshot = self
            .monitors
            .get(id)
            .and_then(|monitor| monitor.body.file.as_ref())
            .and_then(|watcher| watcher.last())
            .and_then(|snapshot| serde_json::to_value(snapshot).ok())
            .unwrap_or(Value::Null);
        serde_json::json!({
            "changes": changes.iter().map(change_value).collect::<Vec<_>>(),
            "snapshot": snapshot,
        })
    }

    fn record_changes(&mut self, id: &str, changes: Vec<MonitorChange>) -> MonitorResult<()> {
        let latest = changes.last().map(|change| change.summary.clone()).unwrap_or_default();
        let pause = changes.last().and_then(|change| change.pause);
        let detail = self.checkpoint_detail(id, &changes);
        let Some(monitor) = self.monitors.get_mut(id) else {
            return Ok(());
        };
        monitor.record.monitor_seq = monitor.record.monitor_seq.saturating_add(1);
        monitor.record.changes = monitor.record.changes.saturating_add(changes.len() as u64);
        monitor.record.last_summary = Some(latest.clone());
        match pause {
            Some(reason) => {
                monitor.record.pause = Some(reason);
                monitor.record.status = MonitorStatus::Paused;
            }
            None if monitor.record.status == MonitorStatus::Paused => {
                monitor.record.pause = None;
                monitor.record.status = MonitorStatus::Running;
            }
            None => {}
        }
        monitor.baselined = true;
        let seq = monitor.record.monitor_seq;
        let payload = crate::durable_runtime::Payload::MonitorCheckpoint(crate::durable_runtime::MonitorCheckpoint {
            monitor_seq: seq,
            summary: latest.clone(),
            detail,
        });
        self.store.append(id, payload)?;
        let monitor = self.monitors.get_mut(id).expect("monitor still present");
        match monitor.pending.as_mut() {
            Some(pending) => pending.fold(seq, latest),
            None => monitor.pending = Some(CoalescedWake::new(seq, latest)),
        }
        monitor.record.coalesced = monitor.pending.as_ref().map(|pending| pending.changes).unwrap_or(0);
        Ok(())
    }

    /// Hand every ready pending wake to the input queue.
    ///
    /// The order is the contract: `wake.pending` is durable before the enqueue,
    /// and `wake.acked` is written only after the enqueue succeeded. A busy queue
    /// keeps the pending wake under the same wake id for the next attempt, so it
    /// never writes a second `wake.pending` row for the same work.
    pub fn deliver(&mut self) -> MonitorResult<Vec<String>> {
        let ids: Vec<String> = self.monitors.keys().cloned().collect();
        let mut delivered = Vec::new();
        for id in ids {
            let Some(pending) = self.monitors.get(&id).and_then(|monitor| monitor.pending.clone()) else {
                continue;
            };
            if !self.monitors.get(&id).is_some_and(|monitor| monitor.record.is_live()) {
                continue;
            }
            let now = self.clock.now();
            if self.pacer.decide(&id, now) == WakeDecision::Coalesce {
                continue;
            }
            let label = self.monitors.get(&id).map(monitor_label).unwrap_or_else(|| id.clone());
            let text = pending.text(&label);
            let wake_id = match pending.wake_id.clone() {
                Some(wake_id) => wake_id,
                None => {
                    let wake_id = crate::durable_runtime::new_id();
                    let payload = crate::durable_runtime::Payload::WakePending(crate::durable_runtime::WakePending {
                        monitor_id: id.clone(),
                        monitor_seq: pending.monitor_seq,
                        summary: text.clone(),
                    });
                    self.store.append(&wake_id, payload)?;
                    if let Some(monitor) = self.monitors.get_mut(&id)
                        && let Some(current) = monitor.pending.as_mut()
                    {
                        current.wake_id = Some(wake_id.clone());
                    }
                    wake_id
                }
            };
            // The sink returns the id the wake actually landed under, which is
            // the input id in the journal. Acking the pending record's own id
            // instead would name a row that no consumer can resolve.
            let delivered_input_id = match self.sink.enqueue(&wake_id, &text) {
                Ok(input_id) => input_id,
                Err(WakeError::Busy) => continue,
                Err(WakeError::Failed(message)) => {
                    return Err(MonitorError::io(format!("wake delivery failed: {message}")));
                }
            };
            let acked = crate::durable_runtime::Payload::WakeAcked(crate::durable_runtime::WakeAcked {
                delivered_input_id: Some(delivered_input_id.clone()),
            });
            self.store.append(&wake_id, acked)?;
            self.pacer.record(&id, now);
            if let Some(monitor) = self.monitors.get_mut(&id) {
                monitor.pending = None;
                monitor.record.wakes = monitor.record.wakes.saturating_add(1);
                monitor.record.coalesced = 0;
            }
            delivered.push(delivered_input_id);
        }
        Ok(delivered)
    }

    /// Re-deliver wakes the journal still holds as pending.
    ///
    /// A redelivery reuses the recorded wake id, so the sink recognizes an
    /// already-queued wake and this becomes an ack rather than a second message.
    pub fn redeliver_pending(&mut self) -> MonitorResult<Vec<String>> {
        let state = self.store.snapshot()?;
        let mut pending: Vec<(String, String, u64, String)> = state
            .pending_wakes()
            .map(|(id, wake)| {
                (
                    id.clone(),
                    wake.monitor_id.clone(),
                    wake.monitor_seq,
                    wake.summary.clone(),
                )
            })
            .collect();
        pending.sort_by(|left, right| left.0.cmp(&right.0));
        let mut delivered = Vec::new();
        for (wake_id, monitor_id, monitor_seq, summary) in pending {
            match self.sink.enqueue(&wake_id, &summary) {
                Ok(_) => {}
                Err(WakeError::Busy) => continue,
                Err(WakeError::Failed(message)) => {
                    return Err(MonitorError::io(format!("wake redelivery failed: {message}")));
                }
            }
            let acked = crate::durable_runtime::Payload::WakeAcked(crate::durable_runtime::WakeAcked {
                delivered_input_id: Some(wake_id.clone()),
            });
            self.store.append(&wake_id, acked)?;
            if let Some(monitor) = self.monitors.get_mut(&monitor_id) {
                monitor.record.wakes = monitor.record.wakes.saturating_add(1);
                monitor.record.monitor_seq = monitor.record.monitor_seq.max(monitor_seq);
            }
            delivered.push(wake_id);
        }
        Ok(delivered)
    }
}

fn monitor_label(monitor: &ActiveMonitor) -> String {
    let intent = monitor
        .record
        .spec
        .intent
        .as_deref()
        .map(|intent| format!(" — {intent}"))
        .unwrap_or_default();
    format!(
        "monitor {} ({} {}){intent}",
        monitor.record.id,
        monitor.record.spec.kind.as_str(),
        monitor.record.spec.target()
    )
}

fn change_value(change: &MonitorChange) -> Value {
    serde_json::json!({
        "kind": change.kind.as_str(),
        "summary": change.summary,
        "detail": change.detail,
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

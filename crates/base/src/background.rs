//! Background task execution manager
//!
//! Allows tools to run in the background and notify the agent when complete.
//! Uses file-based storage for crash resilience + event channel for real-time notifications.

use crate::bus::{
    BackgroundTaskCompleted, BackgroundTaskProgress, BackgroundTaskProgressEvent, BackgroundTaskStatus, Bus, BusEvent,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant as TokioInstant, MissedTickBehavior};

mod lifecycle;
mod model;

pub use lifecycle::FinalizedTask;
pub use model::{
    ARTIFACT_BYTE_BUDGET, ARTIFACT_FILE_BUDGET, ARTIFACT_RETENTION_ENV, ArtifactRetention, ArtifactRetentionPolicy,
    BackgroundCleanupResult, BackgroundTaskEventKind, BackgroundTaskEventRecord, BackgroundTaskInfo,
    BackgroundTaskWaitReason, BackgroundTaskWaitResult, RETAIN_RECENT_TERMINAL, RunningBackgroundProgress, TaskResult,
    TaskStatusFile, format_progress_display, format_progress_summary, render_progress_bar,
};
use model::{
    EXIT_MARKER_PREFIX, RunningTask, normalize_delivery, progress_equivalent, progress_event_record,
    progress_wait_reason, push_task_event, task_dir, terminal_event_record,
};

/// How long a completed budget sweep is reused by the spawn path.
///
/// A sweep walks the whole shared task directory (`read_dir` plus a `metadata`
/// and a JSON parse per status file), so its cost grows with the number of
/// tasks on the machine. Running it inline on every spawn made spawn latency
/// proportional to that directory: measured at 2037 ms for 4097 files against
/// 68 ms for an empty directory. Reusing a sweep younger than this window
/// means a session that spawns every few seconds pays for one walk per window
/// instead of one per spawn; the next spawn past the window sweeps again, so
/// `rotate` reclamation resumes within the window.
///
/// The window is measured on the sweep, not on the spawn, and it is
/// deliberately longer than any gap between the spawns of a single tool call:
/// a burst must not re-walk the directory between its first and second spawn,
/// which is exactly the case the self-dev build-queue test exercises.
const ARTIFACT_SWEEP_THROTTLE: Duration = Duration::from_secs(30);

/// The most recent budget sweep, reused by the spawn path while it is younger
/// than [`ARTIFACT_SWEEP_THROTTLE`].
struct SweepCache {
    at: TokioInstant,
    report: BackgroundCleanupResult,
}

/// Artifacts one background task owns on disk, as the budget sweep sees them.
struct ArtifactTask {
    task_id: String,
    output: Option<(PathBuf, u64)>,
    status: Option<(PathBuf, u64)>,
    status_file: Option<TaskStatusFile>,
    /// Last-modified millis, used when `started_at` cannot be parsed.
    fallback_order: u64,
}

fn artifact_order_key(task: &ArtifactTask) -> u64 {
    task.status_file
        .as_ref()
        .and_then(|status| DateTime::parse_from_rfc3339(&status.started_at).ok())
        .and_then(|started| u64::try_from(started.timestamp_millis()).ok())
        .unwrap_or(task.fallback_order)
}

/// Counts a task's active `wait` calls so the sweep leaves its artifacts alone
/// while a waiter can still read them.
struct WaitLease {
    waits: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    task_id: String,
}

impl WaitLease {
    fn acquire(waits: &Arc<std::sync::Mutex<HashMap<String, usize>>>, task_id: &str) -> Self {
        if let Ok(mut map) = waits.lock() {
            *map.entry(task_id.to_string()).or_insert(0) += 1;
        }
        Self {
            waits: Arc::clone(waits),
            task_id: task_id.to_string(),
        }
    }
}

impl Drop for WaitLease {
    fn drop(&mut self) {
        if let Ok(mut map) = self.waits.lock()
            && let Some(count) = map.get_mut(&self.task_id)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&self.task_id);
            }
        }
    }
}

/// Gives every in-flight status write its own temporary path, so two writers
/// racing on one task never share a scratch file.
static STATUS_TEMP_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Sibling temporary path for a status file.
///
/// It sits in the same directory because `rename` is only atomic within one
/// file system, and its `.tmp` suffix keeps the artifact sweep and `cleanup`,
/// which match `.status.json` and `.output`, from treating it as a task.
fn status_temp_path(path: &std::path::Path) -> PathBuf {
    let serial = STATUS_TEMP_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "status.json".to_string());
    path.with_file_name(format!(".{name}.{}.{serial}.tmp", std::process::id()))
}

/// Publish a status document so a concurrent reader sees the previous
/// document or the new one, never a partial one.
///
/// Status files are rewritten while other code polls them (progress updates,
/// the terminal write, `bg status`, `bg wait`). `fs::write` truncates the
/// target in place, so a reader that opened it between the truncate and the
/// last byte parsed an empty or half-written document, and `read_status_file`
/// reported an existing task as missing. Writing a sibling temporary file and
/// renaming it over the target closes that window. Every status writer goes
/// through this function or [`write_status_json_blocking`].
async fn write_status_json(path: &std::path::Path, json: &str) {
    let temp = status_temp_path(path);
    if fs::write(&temp, json).await.is_err() || fs::rename(&temp, path).await.is_err() {
        let _ = fs::remove_file(&temp).await;
    }
}

/// Blocking form of [`write_status_json`] for the spawn paths, which persist
/// the initial status before they return the task id.
fn write_status_json_blocking(path: &std::path::Path, json: &str) {
    let temp = status_temp_path(path);
    if std::fs::write(&temp, json).is_err() || std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

/// Manages background task execution
pub struct BackgroundTaskManager {
    tasks: Arc<RwLock<HashMap<String, RunningTask>>>,
    /// Serializes progress status read-modify-write cycles so concurrent output
    /// readers cannot overwrite a newer high-water mark with a stale update.
    progress_updates: Arc<Mutex<()>>,
    /// Live stall watchdogs by task id. Each entry is a spawned monitor that
    /// fires a [`BusEvent::BackgroundTaskStalled`] after its task produces no
    /// output bytes and no progress events for the configured window.
    stall_watchdogs: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
    output_dir: PathBuf,
    /// Artifact budget enforced by [`Self::enforce_artifact_budget`].
    retention: ArtifactRetention,
    /// What to do when the budget is exhausted.
    retention_policy: ArtifactRetentionPolicy,
    /// Task ids with a live `wait` call, counted per task. The sweep never
    /// removes artifacts a blocked waiter is about to read.
    in_flight_waits: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    /// Last budget sweep and when it ran, so a burst of spawns walks the task
    /// directory once instead of once per spawn.
    sweep_cache: Mutex<Option<SweepCache>>,
    /// Terminal-transition broadcast shared with spawned task bodies, which
    /// own no reference to this manager.
    finalizer: Arc<lifecycle::Finalizer>,
}

impl BackgroundTaskManager {
    /// Create a manager rooted at a specific output directory.
    ///
    /// Primarily for tests; production code should use [`global`].
    pub fn with_output_dir(output_dir: PathBuf) -> Self {
        Self::with_retention(
            output_dir,
            ArtifactRetention::default(),
            ArtifactRetentionPolicy::from_env(),
        )
    }

    /// Create a manager with explicit budgets and policy.
    pub fn with_retention(
        output_dir: PathBuf,
        retention: ArtifactRetention,
        retention_policy: ArtifactRetentionPolicy,
    ) -> Self {
        std::fs::create_dir_all(&output_dir).ok();
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            progress_updates: Arc::new(Mutex::new(())),
            stall_watchdogs: Arc::new(RwLock::new(HashMap::new())),
            output_dir,
            retention,
            retention_policy,
            in_flight_waits: Arc::new(std::sync::Mutex::new(HashMap::new())),
            sweep_cache: Mutex::new(None),
            finalizer: Arc::new(lifecycle::Finalizer::new()),
        }
    }

    pub fn retention(&self) -> ArtifactRetention {
        self.retention
    }

    pub fn retention_policy(&self) -> ArtifactRetentionPolicy {
        self.retention_policy
    }

    /// Create a new background task manager
    pub fn new() -> Self {
        let output_dir = task_dir();
        Self::with_output_dir(output_dir)
    }

    /// Generate a short, unique task ID
    fn generate_task_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        const TASK_ID_ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        // Use last 6 digits of timestamp + 4 random chars
        let rand_part: String = (0..4)
            .map(|_| {
                let idx = (rand::random::<u8>() as usize) % TASK_ID_ALPHABET.len();
                TASK_ID_ALPHABET[idx] as char
            })
            .collect();
        format!(
            "{}{}",
            &timestamp.to_string()[timestamp.to_string().len().saturating_sub(6)..],
            rand_part
        )
    }

    pub fn output_path_for(&self, task_id: &str) -> PathBuf {
        self.output_dir.join(format!("{}.output", task_id))
    }

    pub fn status_path_for(&self, task_id: &str) -> PathBuf {
        self.output_dir.join(format!("{}.status.json", task_id))
    }

    fn publish_task_started_activity(
        task_id: &str,
        tool_name: &str,
        display_name: Option<&str>,
        session_id: &str,
        notify: bool,
    ) {
        if !notify {
            return;
        }
        let label = crate::message::background_task_display_label(tool_name, display_name);
        let safe_label = label.replace('`', "'");
        Bus::global().publish(BusEvent::UiActivity(crate::bus::UiActivity::background(
            Some(session_id.to_string()),
            format!(
                "**Background task started** `{}` · `{}`\n\nHoronAgent is running this in the background. Progress, checkpoints, and completion will appear here.",
                task_id, safe_label
            ),
            Some(format!("Background task started · {}", label)),
        )));
    }

    fn status_duration_secs(started_at: &str, completed_at: DateTime<Utc>) -> Option<f64> {
        DateTime::parse_from_rfc3339(started_at)
            .ok()
            .and_then(|started| (completed_at - started.with_timezone(&Utc)).to_std().ok())
            .map(|duration| duration.as_secs_f64())
    }

    fn parse_exit_code_from_output(output: &str) -> Option<i32> {
        output.lines().rev().find_map(|line| {
            let trimmed = line.trim();
            let suffix = trimmed.strip_prefix(EXIT_MARKER_PREFIX)?;
            let suffix = suffix.strip_suffix(" ---")?;
            suffix.trim().parse::<i32>().ok()
        })
    }

    async fn read_status_file(&self, path: &std::path::Path) -> Option<TaskStatusFile> {
        let content = fs::read_to_string(path).await.ok()?;
        serde_json::from_str(&content).ok()
    }

    async fn write_status_file(&self, path: &std::path::Path, status: &TaskStatusFile) {
        if let Ok(json) = serde_json::to_string_pretty(status) {
            write_status_json(path, &json).await;
        }
    }

    /// Startup/reload sweep: mark orphaned non-detached `Running` status
    /// files as `Failed` with a "server reloaded" note.
    ///
    /// Only owner-tagged files are considered, using the liveness rules of
    /// [`Self::status_is_reconcilable_orphan`]. Files without owner metadata
    /// (written by older builds, or by processes that legitimately still run
    /// them) are left untouched: the task dir is shared machine-wide, so
    /// without owner metadata there is no safe way to distinguish a phantom
    /// from another live process's task. Returns how many files were
    /// reconciled.
    pub async fn reconcile_orphaned_tasks(&self) -> usize {
        let mut reconciled = 0;
        let Ok(mut entries) = fs::read_dir(&self.output_dir).await else {
            return reconciled;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(status) = self.read_status_file(&path).await else {
                continue;
            };
            if !Self::status_is_reconcilable_orphan(&status) {
                continue;
            }
            if self.tasks.read().await.contains_key(&status.task_id) {
                continue;
            }
            self.finalize_orphaned_status_if_needed(status, &path).await;
            reconciled += 1;
        }
        reconciled
    }

    pub fn reserve_task_info(&self) -> BackgroundTaskInfo {
        let task_id = Self::generate_task_id();
        let output_file = self.output_path_for(&task_id);
        let status_file = self.status_path_for(&task_id);
        BackgroundTaskInfo {
            task_id,
            output_file,
            status_file,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Detached task registration mirrors persisted status fields and existing call sites"
    )]
    pub async fn register_detached_task(
        &self,
        info: &BackgroundTaskInfo,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        pid: u32,
        started_at: &str,
        notify: bool,
        wake: bool,
    ) {
        let (notify, wake) = normalize_delivery(notify, wake);
        let status = TaskStatusFile {
            task_id: info.task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name,
            session_id: session_id.to_string(),
            status: BackgroundTaskStatus::Running,
            exit_code: None,
            error: None,
            started_at: started_at.to_string(),
            completed_at: None,
            duration_secs: None,
            pid: Some(pid),
            // Detached processes outlive this server, so no in-process owner:
            // reconciliation must never clobber them.
            owner_pid: None,
            owner_instance: None,
            detached: true,
            notify,
            wake,
            progress: None,
            event_history: Vec::new(),
            stall_wake_seconds: None,
            // Detached registration has no tool call of its own: the caller
            // that detached the process keeps its own call record.
            tool_call_id: None,
            command: None,
            cwd: None,
        };
        self.write_status_file(&info.status_file, &status).await;
        Self::publish_task_started_activity(
            &info.task_id,
            tool_name,
            status.display_name.as_deref(),
            session_id,
            notify,
        );
    }

    /// Spawn a background task
    ///
    /// The `execute_fn` receives the output file path and should write output there.
    /// It returns a TaskResult with exit code and optional error.
    pub async fn spawn<F, Fut>(&self, tool_name: &str, session_id: &str, execute_fn: F) -> Result<BackgroundTaskInfo>
    where
        F: FnOnce(PathBuf) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<TaskResult>> + Send,
    {
        self.spawn_with_notify(tool_name, None, session_id, true, false, execute_fn)
            .await
    }

    /// Spawn a background task with explicit notify flag
    pub async fn spawn_with_notify<F, Fut>(
        &self,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        notify: bool,
        wake: bool,
        execute_fn: F,
    ) -> Result<BackgroundTaskInfo>
    where
        F: FnOnce(PathBuf) -> Fut + Send + 'static,
        Fut: Future<Output = Result<TaskResult>> + Send,
    {
        self.spawn_with_notify_impl(
            tool_name,
            display_name,
            session_id,
            notify,
            wake,
            None,
            None,
            None,
            execute_fn,
        )
        .await
    }

    /// Adopt an already-spawned task as a background task.
    /// Used when the user moves a currently-executing tool to background via Alt+B.
    /// The `handle` is an already-running tokio task; we just register it for tracking
    /// and wire up completion notifications.
    pub async fn adopt(
        &self,
        tool_name: &str,
        session_id: &str,
        handle: JoinHandle<Result<horon_agent_tool_types::ToolOutput>>,
    ) -> BackgroundTaskInfo {
        self.adopt_with_options(tool_name, None, session_id, true, false, handle)
            .await
    }

    /// Adopt an already-spawned task as a background task, with explicit display
    /// name and delivery flags. Used both for user-initiated handoff (Alt+B) and
    /// for promoting a foreground command that exceeded its timeout but is still
    /// running, so it keeps running and surfaces as a background-task card.
    pub async fn adopt_with_options(
        &self,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        notify: bool,
        wake: bool,
        handle: JoinHandle<Result<horon_agent_tool_types::ToolOutput>>,
    ) -> BackgroundTaskInfo {
        self.adopt_with_options_impl(
            tool_name,
            display_name,
            session_id,
            notify,
            wake,
            None,
            None,
            None,
            handle,
        )
        .await
    }

    /// List all tasks (both running and completed from disk)
    pub async fn list(&self) -> Vec<TaskStatusFile> {
        let mut results = Vec::new();

        // Read all status files from disk
        if let Ok(mut entries) = fs::read_dir(&self.output_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false)
                    && let Some(status) = self.read_status_file(&path).await
                {
                    let reconciled = self.finalize_detached_status_if_needed(status, &path).await;
                    let reconciled = self.finalize_orphaned_status_if_needed(reconciled, &path).await;
                    results.push(reconciled);
                }
            }
        }

        // Sort by task_id (which includes timestamp)
        results.sort_by(|a, b| b.task_id.cmp(&a.task_id));
        results
    }

    /// Get status of a specific task
    pub async fn status(&self, task_id: &str) -> Option<TaskStatusFile> {
        let status_path = self.status_path_for(task_id);
        let status = self.read_status_file(&status_path).await?;
        let status = self.finalize_detached_status_if_needed(status, &status_path).await;
        Some(self.finalize_orphaned_status_if_needed(status, &status_path).await)
    }

    /// Best-effort synchronous check for whether a task is still live in this process.
    pub fn is_live_task(&self, task_id: &str) -> bool {
        let Ok(tasks) = self.tasks.try_read() else {
            return false;
        };
        tasks.contains_key(task_id)
    }

    /// Whether the process that owns this task is still alive.
    ///
    /// A persisted `Running` status outlives its task: a cancelled task and a
    /// task whose process died both leave the file behind, and an in-process
    /// task that recorded a child pid is deliberately skipped by the orphan
    /// reconciler. The live map answers for this process; a process check
    /// answers for another one.
    pub fn task_owner_is_live(&self, task: &TaskStatusFile) -> bool {
        match task.owner_pid {
            Some(owner) if owner == std::process::id() => self.is_live_task(&task.task_id),
            Some(owner) => horon_agent_platform::is_process_running(owner),
            None => false,
        }
    }

    /// Get full output of a task
    pub async fn output(&self, task_id: &str) -> Option<String> {
        let output_path = self.output_path_for(task_id);
        fs::read_to_string(&output_path).await.ok()
    }

    /// Wait for a task to finish, emit progress, or reach the caller's maximum wait.
    ///
    /// This combines bus-driven wakeups with a light periodic status reconciliation so
    /// detached tasks, missed broadcast messages, or crash/reload edges still return no
    /// later than `max_wait` and can notice completion without active polling by the agent.
    pub async fn wait(
        &self,
        task_id: &str,
        max_wait: Duration,
        return_on_progress: bool,
    ) -> Option<BackgroundTaskWaitResult> {
        let _lease = WaitLease::acquire(&self.in_flight_waits, task_id);
        let mut bus_rx = Bus::global().subscribe();
        let initial = self.status(task_id).await?;
        if initial.status != BackgroundTaskStatus::Running {
            return Some(BackgroundTaskWaitResult {
                reason: BackgroundTaskWaitReason::AlreadyFinished,
                task: initial,
                progress_event: None,
                event_record: None,
            });
        }
        if max_wait.is_zero() {
            return Some(BackgroundTaskWaitResult {
                reason: BackgroundTaskWaitReason::Timeout,
                task: initial,
                progress_event: None,
                event_record: None,
            });
        }

        let mut last_progress = initial.progress.clone();
        let deadline = TokioInstant::now() + max_wait;
        let timeout = tokio::time::sleep_until(deadline);
        tokio::pin!(timeout);
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = &mut timeout => {
                    let task = self.status(task_id).await?;
                    let reason = if task.status == BackgroundTaskStatus::Running {
                        BackgroundTaskWaitReason::Timeout
                    } else {
                        BackgroundTaskWaitReason::Finished
                    };
                    return Some(BackgroundTaskWaitResult {
                        reason,
                        task,
                        progress_event: None,
                        event_record: None,
                    });
                }
                _ = poll.tick() => {
                    let task = self.status(task_id).await?;
                    if task.status != BackgroundTaskStatus::Running {
                        return Some(BackgroundTaskWaitResult {
                            reason: BackgroundTaskWaitReason::Finished,
                            task,
                            progress_event: None,
                            event_record: None,
                        });
                    }
                    if return_on_progress && task.progress != last_progress {
                        let event_record = task.event_history.last().cloned();
                        return Some(BackgroundTaskWaitResult {
                            reason: progress_wait_reason(event_record.as_ref()),
                            progress_event: None,
                            task,
                            event_record,
                        });
                    }
                    last_progress = task.progress.clone();
                }
                event = bus_rx.recv() => {
                    match event {
                        Ok(BusEvent::BackgroundTaskCompleted(event)) if event.task_id == task_id => {
                            let task = self.status(task_id).await?;
                            return Some(BackgroundTaskWaitResult {
                                reason: BackgroundTaskWaitReason::Finished,
                                task,
                                progress_event: None,
                                event_record: None,
                            });
                        }
                        Ok(BusEvent::BackgroundTaskProgress(event)) if event.task_id == task_id => {
                            if return_on_progress {
                                let task = self.status(task_id).await?;
                                let event_record = task.event_history.last().cloned();
                                return Some(BackgroundTaskWaitResult {
                                    reason: progress_wait_reason(event_record.as_ref()),
                                    task,
                                    progress_event: Some(event),
                                    event_record,
                                });
                            }
                        }
                        Ok(BusEvent::BackgroundTaskStalled(event)) if event.task_id == task_id => {
                            // A stall is always worth returning: the caller
                            // should decide whether to keep waiting or act.
                            let task = self.status(task_id).await?;
                            return Some(BackgroundTaskWaitResult {
                                reason: BackgroundTaskWaitReason::Stalled,
                                task,
                                progress_event: None,
                                event_record: None,
                            });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let task = self.status(task_id).await?;
                            if task.status != BackgroundTaskStatus::Running {
                                return Some(BackgroundTaskWaitResult {
                                    reason: BackgroundTaskWaitReason::Finished,
                                    task,
                                    progress_event: None,
                                    event_record: None,
                                });
                            }
                            if return_on_progress && task.progress != last_progress {
                                let event_record = task.event_history.last().cloned();
                                return Some(BackgroundTaskWaitResult {
                                    reason: progress_wait_reason(event_record.as_ref()),
                                    progress_event: None,
                                    task,
                                    event_record,
                                });
                            }
                            last_progress = task.progress.clone();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            let task = self.status(task_id).await?;
                            let reason = if task.status == BackgroundTaskStatus::Running {
                                BackgroundTaskWaitReason::Timeout
                            } else {
                                BackgroundTaskWaitReason::Finished
                            };
                            return Some(BackgroundTaskWaitResult {
                                reason,
                                task,
                                progress_event: None,
                                event_record: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Update progress for an existing background task.
    pub async fn update_progress(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
    ) -> Result<Option<TaskStatusFile>> {
        self.update_progress_with_event_kind(task_id, progress, BackgroundTaskEventKind::Progress)
            .await
    }

    /// Record the child process a live task is running.
    ///
    /// In-process tasks spawn their child inside their own future, so the
    /// manager cannot know the pid at registration. Recording it here lets
    /// cancellation signal the process group before the future is dropped.
    pub async fn record_task_pid(&self, task_id: &str, pid: u32) -> Result<()> {
        // Share the progress lock so this read-modify-write cannot lose a
        // progress update written concurrently.
        let _progress_update_guard = self.progress_updates.lock().await;
        let status_path = self.status_path_for(task_id);
        let Some(mut status) = self.read_status_file(&status_path).await else {
            return Ok(());
        };
        if status.pid == Some(pid) {
            return Ok(());
        }
        status.pid = Some(pid);
        self.write_status_file(&status_path, &status).await;
        Ok(())
    }

    /// Record an explicit checkpoint for an existing background task.
    pub async fn update_checkpoint(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
    ) -> Result<Option<TaskStatusFile>> {
        self.update_progress_with_event_kind(task_id, progress, BackgroundTaskEventKind::Checkpoint)
            .await
    }

    async fn update_progress_with_event_kind(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
        event_kind: BackgroundTaskEventKind,
    ) -> Result<Option<TaskStatusFile>> {
        let _progress_update_guard = self.progress_updates.lock().await;
        let status_path = self.status_path_for(task_id);
        let Some(mut status) = self.read_status_file(&status_path).await else {
            return Ok(None);
        };

        let mut progress = progress.normalize();
        if let Some(existing) = status.progress.as_ref() {
            if progress_equivalent(existing, &progress) {
                return Ok(Some(status));
            }

            // A determinate bar represents progress through the whole task. A
            // lower later value would make that representation knowingly
            // inconsistent, regardless of whether it was explicitly reported
            // or inferred from output. Keep the last trustworthy high-water
            // mark instead of allowing the UI to move backwards.
            if let (Some(existing_percent), Some(new_percent)) = (existing.percent, progress.percent)
                && new_percent < existing_percent
            {
                return Ok(Some(status));
            }

            let existing_is_more_determinate = existing.percent.is_some()
                || matches!((existing.current, existing.total), (_, Some(total)) if total > 0);
            let new_is_less_determinate = progress.percent.is_none()
                && !matches!((progress.current, progress.total), (_, Some(total)) if total > 0);
            if existing_is_more_determinate && new_is_less_determinate {
                // Preserve the measurable high-water mark while still allowing
                // a new checkpoint/phase message to reach status and clients.
                progress.kind = existing.kind.clone();
                progress.percent = existing.percent;
                progress.current = existing.current;
                progress.total = existing.total;
                if progress.unit.is_none() {
                    progress.unit.clone_from(&existing.unit);
                }
            }
        }

        status.progress = Some(progress.clone());
        push_task_event(&mut status, progress_event_record(event_kind, progress.clone()));
        self.write_status_file(&status_path, &status).await;

        Bus::global().publish(BusEvent::BackgroundTaskProgress(BackgroundTaskProgressEvent {
            task_id: status.task_id.clone(),
            tool_name: status.tool_name.clone(),
            display_name: status.display_name.clone(),
            session_id: status.session_id.clone(),
            progress,
        }));

        Ok(Some(status))
    }

    /// How often a stall watchdog samples output size and progress state.
    const STALL_POLL_INTERVAL: Duration = Duration::from_secs(5);
    /// Minimum accepted stall window, to keep watchdogs from spamming wakes.
    pub const MIN_STALL_WAKE_SECONDS: u64 = 30;

    /// Arm (or re-arm) a stall watchdog for a running task.
    ///
    /// The watchdog fires a [`BusEvent::BackgroundTaskStalled`] after the task
    /// produces no new output bytes and no new progress/checkpoint events for
    /// `stall_wake_seconds`. Any activity resets the countdown. It fires at
    /// most once per silence episode and re-arms itself when activity resumes,
    /// so a task that stalls, recovers, and stalls again produces one wake per
    /// stall. The watchdog exits when the task leaves `Running`.
    ///
    /// Returns the effective (clamped) window, or `None` when the task does
    /// not exist or is no longer running.
    pub async fn arm_stall_watchdog(&self, task_id: &str, stall_wake_seconds: u64) -> Option<u64> {
        let stall_wake_seconds = stall_wake_seconds.max(Self::MIN_STALL_WAKE_SECONDS);
        let status_path = self.status_path_for(task_id);
        let mut status = self.read_status_file(&status_path).await?;
        if status.status != BackgroundTaskStatus::Running {
            return None;
        }
        status.stall_wake_seconds = Some(stall_wake_seconds);
        self.write_status_file(&status_path, &status).await;

        let task_id_owned = task_id.to_string();
        let output_path = self.output_path_for(task_id);
        let status_path_clone = status_path.clone();
        let started_at = status.started_at.clone();
        let watchdogs = Arc::clone(&self.stall_watchdogs);

        let handle = tokio::spawn(async move {
            let mut last_len = fs::metadata(&output_path).await.map(|meta| meta.len()).unwrap_or(0);
            let mut last_event_count: Option<usize> = None;
            let mut quiet_since = TokioInstant::now();
            let mut fired_this_episode = false;
            let window = Duration::from_secs(stall_wake_seconds);
            let mut poll = tokio::time::interval(Self::STALL_POLL_INTERVAL);
            poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
            // interval fires immediately on first tick; consume it.
            poll.tick().await;

            loop {
                poll.tick().await;

                let Ok(content) = fs::read_to_string(&status_path_clone).await else {
                    break;
                };
                let Ok(status) = serde_json::from_str::<TaskStatusFile>(&content) else {
                    break;
                };
                if status.status != BackgroundTaskStatus::Running {
                    break;
                }
                // Disarmed externally (e.g. re-armed with a new window by a
                // newer watchdog that overwrote this one's registry entry and
                // cleared the field first): keep it simple, exit if cleared.
                if status.stall_wake_seconds != Some(stall_wake_seconds) {
                    break;
                }

                let output_len = fs::metadata(&output_path).await.map(|meta| meta.len()).unwrap_or(0);
                let event_count = status.event_history.len();
                let activity = output_len != last_len || last_event_count.is_some_and(|prior| prior != event_count);
                last_len = output_len;
                last_event_count = Some(event_count);

                if activity {
                    quiet_since = TokioInstant::now();
                    fired_this_episode = false;
                    continue;
                }

                if fired_this_episode || quiet_since.elapsed() < window {
                    continue;
                }
                fired_this_episode = true;

                let running_secs = DateTime::parse_from_rfc3339(&started_at)
                    .ok()
                    .and_then(|started| (Utc::now() - started.with_timezone(&Utc)).to_std().ok())
                    .map(|duration| duration.as_secs_f64())
                    .unwrap_or_default();
                let output_tail = fs::read_to_string(&output_path)
                    .await
                    .map(|output| {
                        let tail: Vec<&str> = output.lines().rev().take(20).collect();
                        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                    })
                    .unwrap_or_default();
                let output_tail = if output_tail.len() > 2000 {
                    crate::util::truncate_str(&output_tail, 2000).to_string()
                } else {
                    output_tail
                };

                Bus::global().publish(BusEvent::BackgroundTaskStalled(crate::bus::BackgroundTaskStalled {
                    task_id: status.task_id.clone(),
                    tool_name: status.tool_name.clone(),
                    display_name: status.display_name.clone(),
                    session_id: status.session_id.clone(),
                    stall_wake_seconds,
                    running_secs,
                    output_tail,
                    output_file: output_path.clone(),
                    // Arming a stall watchdog is an explicit "wake me if
                    // this goes quiet" request, independent of the task's
                    // completion-delivery flags.
                    notify: true,
                    wake: true,
                }));
            }

            watchdogs.write().await.remove(&task_id_owned);
        });

        // Replace any existing watchdog for this task.
        if let Some(prior) = self.stall_watchdogs.write().await.insert(task_id.to_string(), handle) {
            prior.abort();
        }

        Some(stall_wake_seconds)
    }

    /// Disarm the stall watchdog for a task, if one is armed.
    pub async fn disarm_stall_watchdog(&self, task_id: &str) -> bool {
        let removed = self.stall_watchdogs.write().await.remove(task_id);
        let disarmed = removed.is_some();
        if let Some(handle) = removed {
            handle.abort();
        }
        let status_path = self.status_path_for(task_id);
        if let Some(mut status) = self.read_status_file(&status_path).await
            && status.stall_wake_seconds.is_some()
        {
            status.stall_wake_seconds = None;
            self.write_status_file(&status_path, &status).await;
            return true;
        }
        disarmed
    }

    /// Update delivery behavior for an existing background task.
    ///
    /// This supports retroactively enabling notify/wake after the task was already started.
    pub async fn update_delivery(&self, task_id: &str, notify: bool, wake: bool) -> Result<Option<TaskStatusFile>> {
        let (notify, wake) = normalize_delivery(notify, wake);
        let status_path = self.status_path_for(task_id);
        let Some(mut status) = self.read_status_file(&status_path).await else {
            return Ok(None);
        };
        status.notify = notify;
        status.wake = wake;
        let event_status = status.status.clone();
        let event_exit_code = status.exit_code;
        let event_progress = status.progress.clone();
        push_task_event(
            &mut status,
            BackgroundTaskEventRecord {
                kind: BackgroundTaskEventKind::DeliveryUpdated,
                timestamp: Utc::now().to_rfc3339(),
                message: Some(format!("notify={}, wake={}", notify, wake)),
                status: Some(event_status),
                exit_code: event_exit_code,
                progress: event_progress,
            },
        );
        self.write_status_file(&status_path, &status).await;

        if let Some(task) = self.tasks.read().await.get(task_id) {
            let _ = task.delivery_flags.send((notify, wake));
        }

        Ok(Some(status))
    }

    /// Cancel a running task
    pub async fn cancel(&self, task_id: &str) -> Result<bool> {
        self.cancel_with_grace(task_id, std::time::Duration::from_millis(400))
            .await
    }

    /// Cancel a running task, allowing detached processes a configurable grace period
    /// between TERM and KILL on Unix.
    pub async fn cancel_with_grace(&self, task_id: &str, graceful_timeout: std::time::Duration) -> Result<bool> {
        // Take the task out of the live map, then drop the lock: the graceful
        // stop below can block for the whole grace period.
        let task = self.tasks.write().await.remove(task_id);
        if let Some(task) = task {
            // Read the persisted record once: it carries both the child pid
            // and the tool-call link the final record must keep.
            let prior_status = self.read_status_file(&task.status_path).await;
            // A task that finished on its own keeps its own record. Its map
            // entry outlives the terminal write by the instant the wrapper
            // takes to prune it, so being in the map is not proof that it is
            // still running; the status file is.
            if prior_status
                .as_ref()
                .is_some_and(|status| status.status != BackgroundTaskStatus::Running)
            {
                return Ok(false);
            }
            // Marked only now that the task is known to be running: stopping
            // the process group below lets its own future publish `Failed`
            // first, and the mark is what makes that earlier record report the
            // cancellation that caused it.
            self.finalizer.mark_cancelled(task_id);
            // Stop the process group before dropping the future. Aborting the
            // handle kills the shell (kill_on_drop), but its children — a
            // compiler, a test runner — would keep running orphaned, and the
            // shell never gets the chance to run a TERM trap.
            if let Some(pid) = prior_status.as_ref().and_then(|status| status.pid) {
                let _ = tokio::task::spawn_blocking(move || {
                    horon_agent_platform::terminate_process_group(pid, graceful_timeout)
                })
                .await;
                // Let the task's output reader drain what the stopped group
                // wrote before the future is dropped.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            task.handle.abort();

            // Update status file
            let (notify_flag, wake_flag) = *task.delivery_flags.borrow();
            let mut final_status = TaskStatusFile {
                task_id: task.task_id,
                tool_name: task.tool_name,
                display_name: task.display_name,
                session_id: task.session_id,
                status: BackgroundTaskStatus::Failed,
                exit_code: None,
                error: Some("Cancelled by user".to_string()),
                started_at: task.started_at_rfc3339,
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
                duration_secs: Some(task.started_at.elapsed().as_secs_f64()),
                pid: None,
                owner_pid: Some(std::process::id()),
                owner_instance: Some(model::process_instance_token().to_string()),
                detached: false,
                notify: notify_flag,
                wake: wake_flag,
                progress: None,
                event_history: Vec::new(),
                stall_wake_seconds: None,
                tool_call_id: prior_status.as_ref().and_then(|status| status.tool_call_id.clone()),
                command: prior_status.as_ref().and_then(|status| status.command.clone()),
                cwd: prior_status.as_ref().and_then(|status| status.cwd.clone()),
            };
            let event_status = final_status.status.clone();
            let event_exit_code = final_status.exit_code;
            let event_error = final_status.error.clone();
            push_task_event(
                &mut final_status,
                terminal_event_record(event_status, event_exit_code, event_error.as_deref()),
            );
            if let Ok(json) = serde_json::to_string_pretty(&final_status) {
                write_status_json(&task.status_path, &json).await;
            }
            // The gate decides what the client saw. Winning it means this
            // cancellation is the task's transition; losing it means the task
            // had already published its own, so this cancel did not happen and
            // the reply must not claim it did. The mark tells the two losing
            // cases apart: still present means the winner reported a plain
            // completion, gone means the winner carried our cancellation.
            if self.emit_finalized(&final_status, true) {
                return Ok(true);
            }
            Ok(!self.finalizer.take_cancelled(task_id))
        } else {
            let status_path = self.status_path_for(task_id);
            let Some(mut status) = self.read_status_file(&status_path).await else {
                return Ok(false);
            };
            status = self.finalize_detached_status_if_needed(status, &status_path).await;
            if status.status != BackgroundTaskStatus::Running || !status.detached {
                return Ok(false);
            }

            let Some(pid) = status.pid else {
                return Ok(false);
            };

            // Confirmed running and detached: this call is what ends it, so the
            // transition below is a cancellation.
            self.finalizer.mark_cancelled(task_id);

            // The daemon's cancel path and the platform adapter agree on one
            // meaning: ask the group to stop, wait up to the grace period, then
            // force it. Windows has no catchable signal and terminates the tree
            // immediately. The wait polls, so it runs off the async worker.
            let termination = tokio::task::spawn_blocking(move || {
                horon_agent_platform::terminate_process_group(pid, graceful_timeout)
            });
            drop(termination.await);

            let completed_at = Utc::now();
            status.status = BackgroundTaskStatus::Failed;
            status.exit_code = None;
            status.error = Some("Cancelled by user".to_string());
            status.completed_at = Some(completed_at.to_rfc3339());
            status.duration_secs = Self::status_duration_secs(&status.started_at, completed_at);
            let event_status = status.status.clone();
            let event_exit_code = status.exit_code;
            let event_error = status.error.clone();
            push_task_event(
                &mut status,
                terminal_event_record(event_status, event_exit_code, event_error.as_deref()),
            );
            self.write_status_file(&status_path, &status).await;
            if self.emit_finalized(&status, true) {
                return Ok(true);
            }
            Ok(!self.finalizer.take_cancelled(task_id))
        }
    }

    /// Abort every live in-process task before an exec-based server reload.
    ///
    /// `exec` replaces the process image without running destructors, so
    /// without this the spawned task futures simply vanish: their
    /// `kill_on_drop` children (e.g. cargo builds) are never killed and keep
    /// running orphaned, and their status files stay `Running` until the next
    /// process's reconcile sweep happens to notice. Aborting the handles here
    /// drops the futures (killing children) and persisting a terminal
    /// `Failed` status makes the interruption deterministic and immediately
    /// visible to `bg wait`/`bg status` and self-dev queue reconciliation.
    ///
    /// Returns the number of tasks finalized.
    pub async fn abort_live_tasks_for_reload(&self) -> usize {
        let tasks: Vec<RunningTask> = {
            let mut map = self.tasks.write().await;
            map.drain().map(|(_, task)| task).collect()
        };
        let mut finalized = 0;

        for task in tasks {
            task.handle.abort();
            // Wait (bounded) for the aborted future to actually drop, so
            // kill_on_drop children are killed before the upcoming exec.
            let _ = tokio::time::timeout(Duration::from_secs(2), task.handle).await;

            let (notify_flag, wake_flag) = *task.delivery_flags.borrow();
            let prior_status = self.read_status_file(&task.status_path).await;
            // If the task won the race and finished naturally, keep its real
            // terminal status instead of stamping it as interrupted.
            if prior_status
                .as_ref()
                .is_some_and(|status| status.status != BackgroundTaskStatus::Running)
            {
                continue;
            }
            let error = "Interrupted by server reload: the owning server process was replaced before the task finished"
                .to_string();
            let mut final_status = TaskStatusFile {
                task_id: task.task_id,
                tool_name: task.tool_name,
                display_name: prior_status
                    .as_ref()
                    .and_then(|status| status.display_name.clone())
                    .or(task.display_name),
                session_id: task.session_id,
                status: BackgroundTaskStatus::Failed,
                exit_code: None,
                error: Some(error.clone()),
                started_at: task.started_at_rfc3339,
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
                duration_secs: Some(task.started_at.elapsed().as_secs_f64()),
                pid: None,
                owner_pid: Some(std::process::id()),
                owner_instance: Some(model::process_instance_token().to_string()),
                detached: false,
                notify: notify_flag,
                wake: wake_flag,
                progress: prior_status.as_ref().and_then(|status| status.progress.clone()),
                tool_call_id: prior_status.as_ref().and_then(|status| status.tool_call_id.clone()),
                command: prior_status.as_ref().and_then(|status| status.command.clone()),
                cwd: prior_status.as_ref().and_then(|status| status.cwd.clone()),
                event_history: prior_status.map(|status| status.event_history).unwrap_or_default(),
                stall_wake_seconds: None,
            };
            push_task_event(
                &mut final_status,
                terminal_event_record(BackgroundTaskStatus::Failed, None, Some(&error)),
            );
            self.write_status_file(&task.status_path, &final_status).await;
            self.emit_finalized(&final_status, false);
            finalized += 1;
        }

        finalized
    }

    /// Clean up old task files (older than specified hours)
    pub async fn cleanup(&self, max_age_hours: u64) -> Result<usize> {
        Ok(self
            .cleanup_filtered(max_age_hours, &std::collections::HashSet::new(), false)
            .await?
            .removed_files)
    }

    /// Clean up old task files, skipping running tasks and optionally filtering by status.
    pub async fn cleanup_filtered(
        &self,
        max_age_hours: u64,
        status_filter: &std::collections::HashSet<&str>,
        dry_run: bool,
    ) -> Result<BackgroundCleanupResult> {
        let mut result = BackgroundCleanupResult {
            matched_files: 0,
            removed_files: 0,
            skipped_running_files: 0,
            policy: self.retention_policy,
            reclaimed_bytes: 0,
            over_budget: false,
            remaining_bytes: 0,
            remaining_files: 0,
            removed_task_ids: Vec::new(),
        };
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(max_age_hours * 3600);

        if let Ok(mut entries) = fs::read_dir(&self.output_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let Ok(metadata) = fs::metadata(&path).await else {
                    continue;
                };
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                if modified >= cutoff {
                    continue;
                }

                let mut associated_status = None;
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    associated_status = self.read_status_file(&path).await;
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("output")
                    && let Some(task_id) = path.file_stem().and_then(|stem| stem.to_str())
                {
                    associated_status = self.status(task_id).await;
                }

                if let Some(status) = associated_status.as_ref() {
                    if status.status == BackgroundTaskStatus::Running {
                        result.skipped_running_files += 1;
                        continue;
                    }
                    let status_label = match status.status {
                        BackgroundTaskStatus::Running => "running",
                        BackgroundTaskStatus::Completed => "completed",
                        BackgroundTaskStatus::Superseded => "superseded",
                        BackgroundTaskStatus::Failed => "failed",
                    };
                    if !status_filter.is_empty() && !status_filter.contains(status_label) {
                        continue;
                    }
                } else if !status_filter.is_empty() {
                    continue;
                }

                result.matched_files += 1;
                if !dry_run {
                    let _ = fs::remove_file(&path).await;
                    result.removed_files += 1;
                }
            }
        }

        if dry_run {
            result.removed_files = result.matched_files;
        }

        // The sweep cache describes the directory as it was before this cleanup;
        // drop it so the next spawn measures what is actually left instead of
        // refusing on a reading this cleanup just invalidated.
        self.sweep_cache.lock().await.take();
        Ok(result)
    }

    /// Enforce the artifact budget over `output_dir`.
    ///
    /// Terminal, non-detached tasks are candidates, oldest first; each one
    /// loses `.output` before `.status.json`. A task is protected when it is
    /// running, detached, live in this process, blocked on by a `wait` call, or
    /// among the `retain_recent_terminal` newest terminal tasks. When the
    /// protected set alone exceeds the budget nothing further is removed and
    /// the report carries `over_budget: true` with the measured size. Under
    /// [`ArtifactRetentionPolicy::Preserve`] the sweep only measures.
    pub async fn enforce_artifact_budget(&self) -> BackgroundCleanupResult {
        let (tasks, total_bytes, total_files) = self.collect_artifact_tasks().await;
        let mut result = BackgroundCleanupResult {
            matched_files: 0,
            removed_files: 0,
            skipped_running_files: 0,
            policy: self.retention_policy,
            reclaimed_bytes: 0,
            over_budget: false,
            remaining_bytes: total_bytes,
            remaining_files: total_files,
            removed_task_ids: Vec::new(),
        };

        let (always_kept, terminal): (Vec<ArtifactTask>, Vec<ArtifactTask>) = tasks
            .into_iter()
            .partition(|task| self.artifact_task_is_protected(task));
        result.skipped_running_files = always_kept
            .iter()
            .filter(|task| {
                task.status_file
                    .as_ref()
                    .is_some_and(|status| status.status == BackgroundTaskStatus::Running)
            })
            .map(|task| task.output.iter().chain(task.status.iter()).count())
            .sum();

        let mut terminal = terminal;
        terminal.sort_by(|a, b| {
            artifact_order_key(b)
                .cmp(&artifact_order_key(a))
                .then_with(|| b.task_id.cmp(&a.task_id))
        });
        let floor = self.retention.retain_recent_terminal.min(terminal.len());
        let (retained_recent, candidates) = terminal.split_at(floor);
        result.skipped_running_files += retained_recent
            .iter()
            .map(|task| task.output.iter().chain(task.status.iter()).count())
            .sum::<usize>();

        result.matched_files = candidates
            .iter()
            .map(|task| task.output.iter().chain(task.status.iter()).count())
            .sum();

        let mut candidates: Vec<&ArtifactTask> = candidates.iter().collect();
        candidates.reverse();

        if self.retention_policy == ArtifactRetentionPolicy::Preserve {
            result.over_budget = self.retention.is_full(total_bytes, total_files);
            self.publish_artifact_reclaim(&result);
            self.record_sweep(&result).await;
            return result;
        }

        let mut bytes = total_bytes;
        let mut files = total_files;
        for candidate in candidates {
            if !self.retention.is_full(bytes, files) {
                break;
            }
            let mut removed_any = false;
            for (path, len) in candidate.output.iter().chain(candidate.status.iter()) {
                if fs::remove_file(path).await.is_ok() {
                    result.removed_files += 1;
                    result.reclaimed_bytes += *len;
                    bytes = bytes.saturating_sub(*len);
                    files = files.saturating_sub(1);
                    removed_any = true;
                }
            }
            if removed_any {
                result.removed_task_ids.push(candidate.task_id.clone());
            }
        }

        result.over_budget = self.retention.is_full(bytes, files);
        result.remaining_bytes = bytes;
        result.remaining_files = files;
        self.publish_artifact_reclaim(&result);
        self.record_sweep(&result).await;
        result
    }

    /// Remember a sweep so the spawn path can reuse it inside the throttle
    /// window instead of walking the directory again.
    async fn record_sweep(&self, report: &BackgroundCleanupResult) {
        let mut cache = self.sweep_cache.lock().await;
        *cache = Some(SweepCache {
            at: TokioInstant::now(),
            report: report.clone(),
        });
    }

    /// A sweep young enough for a spawn to act on without walking the
    /// directory again.
    async fn recent_sweep(&self) -> Option<BackgroundCleanupResult> {
        let cache = self.sweep_cache.lock().await;
        cache
            .as_ref()
            .filter(|cached| cached.at.elapsed() < ARTIFACT_SWEEP_THROTTLE)
            .map(|cached| cached.report.clone())
    }

    /// Run the retention sweep before creating new artifacts.
    ///
    /// `rotate` reclaims and always proceeds; `preserve` refuses once the
    /// budget is full so the operator decides what to drop.
    ///
    /// The sweep is throttled: a spawn reuses the result of a sweep younger
    /// than [`ARTIFACT_SWEEP_THROTTLE`], so a burst of spawns pays for one
    /// directory walk instead of one per spawn. An explicit
    /// [`Self::enforce_artifact_budget`] call always sweeps and refreshes the
    /// cached result.
    ///
    /// `rotate` reuses any reading inside the window: it already reclaimed
    /// what it could, and a moment-old measurement only defers the next
    /// reclamation. `preserve` reuses only a refusal, because refusing is all
    /// it can do and an operator cleanup invalidates the cache; an under-budget
    /// reading is what admits work, so `preserve` re-measures before admitting.
    /// A spawn that pushes the directory over budget is therefore refused by
    /// the very next spawn, not one spawn later.
    pub async fn prepare_for_new_task(&self) -> Result<()> {
        let report = match self.recent_sweep().await {
            Some(report) if self.retention_policy == ArtifactRetentionPolicy::Rotate || report.over_budget => report,
            _ => self.enforce_artifact_budget().await,
        };
        if self.retention_policy == ArtifactRetentionPolicy::Preserve && report.over_budget {
            anyhow::bail!(
                "Background artifact retention is `preserve` and the artifact budget is full: {} bytes and {} files on disk (budgets {} bytes and {} files). Reclaim with `bg action=\"cleanup\"` or set {}=rotate.",
                report.remaining_bytes,
                report.remaining_files,
                self.retention.byte_budget,
                self.retention.file_budget,
                ARTIFACT_RETENTION_ENV
            );
        }
        Ok(())
    }

    fn publish_artifact_reclaim(&self, result: &BackgroundCleanupResult) {
        if result.removed_files == 0 && !result.over_budget {
            return;
        }
        Bus::global().publish(BusEvent::BackgroundArtifactsReclaimed(result.clone()));
    }

    fn artifact_task_is_protected(&self, task: &ArtifactTask) -> bool {
        let Some(status) = task.status_file.as_ref() else {
            return true;
        };
        status.status == BackgroundTaskStatus::Running
            || status.detached
            || self.is_live_task(&task.task_id)
            || self.has_in_flight_wait(&task.task_id)
    }

    fn has_in_flight_wait(&self, task_id: &str) -> bool {
        self.in_flight_waits
            .lock()
            .map(|waits| waits.contains_key(task_id))
            .unwrap_or(false)
    }

    async fn collect_artifact_tasks(&self) -> (Vec<ArtifactTask>, u64, usize) {
        let mut by_id: HashMap<String, ArtifactTask> = HashMap::new();
        let mut total_bytes = 0u64;
        let mut total_files = 0usize;

        let Ok(mut entries) = fs::read_dir(&self.output_dir).await else {
            return (Vec::new(), 0, 0);
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            total_bytes += metadata.len();
            total_files += 1;

            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let (task_id, is_status) = if let Some(id) = name.strip_suffix(".status.json") {
                (id.to_string(), true)
            } else if let Some(id) = name.strip_suffix(".output") {
                (id.to_string(), false)
            } else {
                continue;
            };
            let modified_order = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            let task = by_id.entry(task_id.clone()).or_insert_with(|| ArtifactTask {
                task_id,
                output: None,
                status: None,
                status_file: None,
                fallback_order: modified_order,
            });
            task.fallback_order = task.fallback_order.max(modified_order);
            if is_status {
                task.status = Some((path, metadata.len()));
            } else {
                task.output = Some((path, metadata.len()));
            }
        }

        let mut tasks: Vec<ArtifactTask> = by_id.into_values().collect();
        for task in &mut tasks {
            if let Some((path, _)) = &task.status {
                task.status_file = self.read_status_file(path).await;
            }
        }
        (tasks, total_bytes, total_files)
    }

    /// Best-effort synchronous snapshot of currently running tasks.
    /// This avoids async calls in render paths.
    pub fn running_snapshot(&self) -> (usize, Vec<String>, Option<RunningBackgroundProgress>) {
        let Ok(tasks) = self.tasks.try_read() else {
            return (0, Vec::new(), None);
        };

        let mut rows: Vec<RunningBackgroundProgress> = Vec::new();
        for task in tasks.values() {
            let status = std::fs::read_to_string(&task.status_path)
                .ok()
                .and_then(|content| serde_json::from_str::<TaskStatusFile>(&content).ok());
            let progress = status.as_ref().and_then(|status| status.progress.clone());
            let label = status
                .as_ref()
                .and_then(|status| status.display_name.clone())
                .or_else(|| task.display_name.clone())
                .unwrap_or_else(|| task.tool_name.clone());

            rows.push(RunningBackgroundProgress {
                task_id: task.task_id.clone(),
                tool_name: task.tool_name.clone(),
                label,
                detail: progress.map(|progress| format_progress_display(&progress, 10)),
            });
        }

        rows.sort_by(|a, b| b.task_id.cmp(&a.task_id));
        let latest = rows.iter().find(|row| row.detail.is_some()).cloned();

        (tasks.len(), rows.iter().map(|row| row.label.clone()).collect(), latest)
    }

    /// Best-effort synchronous lookup of detached tasks that are still running
    /// for a specific session.
    ///
    /// This is primarily used during self-dev reload recovery, where the new
    /// process needs to remind the agent that a previous `bash` command was
    /// persisted into the background instead of being interrupted.
    pub fn persisted_detached_running_tasks_for_session(&self, session_id: &str) -> Vec<TaskStatusFile> {
        let mut matches = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.output_dir) else {
            return matches;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(status) = serde_json::from_str::<TaskStatusFile>(&content) else {
                continue;
            };

            if status.session_id != session_id || status.status != BackgroundTaskStatus::Running || !status.detached {
                continue;
            }

            let Some(pid) = status.pid else {
                continue;
            };

            if horon_agent_platform::is_process_running(pid) {
                matches.push(status);
            }
        }

        matches.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        matches
    }
}

impl Default for BackgroundTaskManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Global singleton for background task manager
static BACKGROUND_MANAGER: std::sync::OnceLock<BackgroundTaskManager> = std::sync::OnceLock::new();

/// Get the global background task manager
pub fn global() -> &'static BackgroundTaskManager {
    BACKGROUND_MANAGER.get_or_init(BackgroundTaskManager::new)
}

#[cfg(test)]
mod tests;

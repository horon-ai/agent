//! Task lifecycle: task-to-tool-call links, live lookups, and terminal news.
//!
//! These members of [`BackgroundTaskManager`] live in their own module because
//! `background.rs` is at its code-size ratchet. Being a child of the
//! `background` module gives them access to its private state, so moving them
//! changes no visibility for callers.

use super::*;

/// Capacity of the terminal-transition broadcast. A subscriber that falls
/// further behind than this sees `Lagged`; the status file on disk stays the
/// source of truth.
const FINALIZED_CHANNEL_CAPACITY: usize = 256;

/// Upper bound on the once-only task ids remembered in memory, so a
/// long-lived server does not accumulate one entry per task forever.
const FINALIZED_SEEN_LIMIT: usize = 8192;

/// One terminal transition of a background task.
///
/// Sent exactly once per task by whichever path finalized it: natural
/// completion, failure, `cancel_with_grace`, detached or orphan
/// reconciliation, or the abort sweep before an exec reload. `cancelled`
/// separates a user cancellation from a failure because the persisted status
/// has no dedicated cancelled variant. `signal` is `None`: the status file
/// records an exit code but not the signal that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct FinalizedTask {
    pub task_id: String,
    pub session_id: String,
    pub tool_call_id: Option<String>,
    pub status: BackgroundTaskStatus,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub cancelled: bool,
}

/// The terminal-transition broadcast and the once-only gate that feeds it.
pub(super) struct Finalizer {
    tx: tokio::sync::broadcast::Sender<FinalizedTask>,
    sent: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl Finalizer {
    pub(super) fn new() -> Self {
        Self {
            tx: tokio::sync::broadcast::channel(FINALIZED_CHANNEL_CAPACITY).0,
            sent: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<FinalizedTask> {
        self.tx.subscribe()
    }

    /// Broadcast a terminal transition unless this task already produced one.
    ///
    /// Finalization paths can observe the same task twice (a reconciliation
    /// racing a cancel, a status read after a natural finish), so the gate is
    /// keyed by task id rather than by path.
    pub(super) fn send_once(&self, status: &TaskStatusFile, cancelled: bool) {
        {
            let Ok(mut sent) = self.sent.lock() else {
                return;
            };
            if !sent.insert(status.task_id.clone()) {
                return;
            }
            if sent.len() > FINALIZED_SEEN_LIMIT {
                sent.clear();
                sent.insert(status.task_id.clone());
            }
        }
        let _ = self.tx.send(FinalizedTask {
            task_id: status.task_id.clone(),
            session_id: status.session_id.clone(),
            tool_call_id: status.tool_call_id.clone(),
            status: status.status.clone(),
            exit_code: status.exit_code,
            signal: None,
            cancelled,
        });
    }
}

impl BackgroundTaskManager {
    /// Subscribe to terminal task transitions.
    ///
    /// Every task appears once, whether it finished on its own, failed, was
    /// cancelled, was reconciled after its owner died, or was aborted for a
    /// server reload. Receivers see events published after subscribing only.
    pub fn subscribe_finalized(&self) -> tokio::sync::broadcast::Receiver<FinalizedTask> {
        self.finalizer.subscribe()
    }

    pub(super) fn emit_finalized(&self, status: &TaskStatusFile, cancelled: bool) {
        self.finalizer.send_once(status, cancelled);
    }

    /// Tasks running in this process for one session.
    ///
    /// Detached tasks belong to a child process rather than to this process's
    /// task map, so they are not listed here; a task whose terminal status is
    /// already on disk is excluded even if the map removal is still pending.
    ///
    /// The task map is the authority for "live in this process", so the
    /// snapshot waits for the lock instead of probing it: the map entry is
    /// removed only after the terminal status is written, which makes a
    /// `Running` file for a mapped task mean the task future is still running.
    /// A best-effort probe that gives up while another writer holds the lock
    /// would drop exactly those running tasks.
    pub async fn live_tasks_for_session(&self, session_id: &str) -> Vec<TaskStatusFile> {
        let candidates: Vec<PathBuf> = {
            let tasks = self.tasks.read().await;
            tasks
                .values()
                .filter(|task| task.session_id == session_id)
                .map(|task| task.status_path.clone())
                .collect()
        };

        let mut live = Vec::with_capacity(candidates.len());
        for status_path in candidates {
            let Some(status) = self.read_status_file(&status_path).await else {
                continue;
            };
            if status.status != BackgroundTaskStatus::Running {
                continue;
            }
            live.push(status);
        }
        live.sort_by(|a, b| b.task_id.cmp(&a.task_id));
        live
    }

    /// Find the background task a tool call started, live or already terminal.
    ///
    /// Live tasks win over disk state so a running task is reported with its
    /// current progress; when nothing is live the newest matching status file
    /// answers, which lets a caller distinguish "finished" from "unknown".
    pub async fn task_for_tool_call(&self, session_id: &str, call_id: &str) -> Option<TaskStatusFile> {
        if let Some(status) = self
            .live_tasks_for_session(session_id)
            .await
            .into_iter()
            .find(|status| status.tool_call_id.as_deref() == Some(call_id))
        {
            return Some(status);
        }

        let Ok(mut entries) = fs::read_dir(&self.output_dir).await else {
            return None;
        };
        let mut matches = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(status) = self.read_status_file(&path).await else {
                continue;
            };
            if status.session_id == session_id && status.tool_call_id.as_deref() == Some(call_id) {
                matches.push(status);
            }
        }
        matches.sort_by(|a, b| b.task_id.cmp(&a.task_id));
        matches.into_iter().next()
    }

    /// Spawn a background task and record which tool call asked for it.
    ///
    /// Identical to [`Self::spawn_with_notify`] except that `tool_call_id`,
    /// `command`, and `cwd` are persisted with the task so the daemon can
    /// report and cancel it against the originating tool call.
    #[expect(
        clippy::too_many_arguments,
        reason = "The entry point mirrors the persisted task metadata plus the executor"
    )]
    pub async fn spawn_with_notify_for_tool_call<F, Fut>(
        &self,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        notify: bool,
        wake: bool,
        tool_call_id: &str,
        command: Option<&str>,
        cwd: Option<&std::path::Path>,
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
            Some(tool_call_id),
            command,
            cwd,
            execute_fn,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Shared body for the plain and tool-call-linked spawn entry points"
    )]
    pub(super) async fn spawn_with_notify_impl<F, Fut>(
        &self,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        notify: bool,
        wake: bool,
        tool_call_id: Option<&str>,
        command: Option<&str>,
        cwd: Option<&std::path::Path>,
        execute_fn: F,
    ) -> Result<BackgroundTaskInfo>
    where
        F: FnOnce(PathBuf) -> Fut + Send + 'static,
        Fut: Future<Output = Result<TaskResult>> + Send,
    {
        self.prepare_for_new_task().await?;
        let (notify, wake) = normalize_delivery(notify, wake);
        let task_id = Self::generate_task_id();
        let output_path = self.output_dir.join(format!("{}.output", task_id));
        let status_path = self.output_dir.join(format!("{}.status.json", task_id));
        let started_at_rfc3339 = chrono::Utc::now().to_rfc3339();
        let tool_call_id_owned = tool_call_id.map(str::to_string);
        let command_owned = command.map(str::to_string);
        let cwd_owned = cwd.map(|path| path.to_string_lossy().into_owned());

        // Write initial status file
        let initial_status = TaskStatusFile {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name: display_name.clone(),
            session_id: session_id.to_string(),
            status: BackgroundTaskStatus::Running,
            exit_code: None,
            error: None,
            started_at: started_at_rfc3339.clone(),
            completed_at: None,
            duration_secs: None,
            pid: None,
            owner_pid: Some(std::process::id()),
            owner_instance: Some(model::process_instance_token().to_string()),
            detached: false,
            notify,
            wake,
            progress: None,
            event_history: Vec::new(),
            stall_wake_seconds: None,
            tool_call_id: tool_call_id_owned.clone(),
            command: command_owned.clone(),
            cwd: cwd_owned.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&initial_status) {
            write_status_json_blocking(&status_path, &json);
        }
        Self::publish_task_started_activity(&task_id, tool_name, display_name.as_deref(), session_id, notify);

        let output_path_clone = output_path.clone();
        let status_path_clone = status_path.clone();
        let task_id_clone = task_id.clone();
        let tool_name_owned = tool_name.to_string();
        let display_name_owned = display_name.clone();
        let session_id_owned = session_id.to_string();
        let started_at = Instant::now();
        let started_at_rfc3339_for_task = started_at_rfc3339.clone();
        let (delivery_flags_tx, delivery_flags_rx) = watch::channel((notify, wake));
        let tasks_for_prune = Arc::clone(&self.tasks);
        let finalizer = Arc::clone(&self.finalizer);
        let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();

        // Spawn the background task
        let handle = tokio::spawn(async move {
            let result = execute_fn(output_path_clone.clone()).await;

            let duration_secs = started_at.elapsed().as_secs_f64();
            let (status, exit_code, error) = match &result {
                Ok(task_result) => {
                    let status = task_result.status.clone().unwrap_or_else(|| {
                        if task_result.error.is_some() {
                            BackgroundTaskStatus::Failed
                        } else {
                            BackgroundTaskStatus::Completed
                        }
                    });
                    (status, task_result.exit_code, task_result.error.clone())
                }
                Err(e) => (BackgroundTaskStatus::Failed, None, Some(e.to_string())),
            };

            let (notify_flag, wake_flag) = *delivery_flags_rx.borrow();
            let prior_status = tokio::fs::read_to_string(&status_path_clone)
                .await
                .ok()
                .and_then(|content| serde_json::from_str::<TaskStatusFile>(&content).ok());
            let prior_progress = prior_status.as_ref().and_then(|status| status.progress.clone());
            let prior_event_history = prior_status.map(|status| status.event_history).unwrap_or_default();

            // Update status file
            let mut final_status = TaskStatusFile {
                task_id: task_id_clone.clone(),
                tool_name: tool_name_owned.clone(),
                display_name: display_name_owned.clone(),
                session_id: session_id_owned.clone(),
                status: status.clone(),
                exit_code,
                error: error.clone(),
                started_at: started_at_rfc3339_for_task,
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
                duration_secs: Some(duration_secs),
                pid: None,
                owner_pid: Some(std::process::id()),
                owner_instance: Some(model::process_instance_token().to_string()),
                detached: false,
                notify: notify_flag,
                wake: wake_flag,
                progress: prior_progress,
                event_history: prior_event_history,
                stall_wake_seconds: None,
                tool_call_id: tool_call_id_owned,
                command: command_owned,
                cwd: cwd_owned,
            };
            push_task_event(
                &mut final_status,
                terminal_event_record(status.clone(), exit_code, error.as_deref()),
            );
            if let Ok(json) = serde_json::to_string_pretty(&final_status) {
                write_status_json(&status_path_clone, &json).await;
            }
            finalizer.send_once(&final_status, false);

            // Drop this task from the live map now that its terminal status is
            // persisted. Order matters: pruning only after the status-file
            // write keeps "in the live map while the status file says Running"
            // equivalent to "a task future is actually executing", which the
            // run_plan duplicate-driver guard and self-dev build reconciliation
            // rely on. Awaiting registration first means a task that finishes
            // instantly cannot race the insert below and leave a permanent
            // phantom entry in the map.
            let _ = registered_rx.await;
            tasks_for_prune.write().await.remove(&task_id_clone);

            // Read output preview for notification
            let output_preview = tokio::fs::read_to_string(&output_path_clone)
                .await
                .map(|s| {
                    if s.len() > 500 {
                        format!("{}...", crate::util::truncate_str(&s, 500))
                    } else {
                        s
                    }
                })
                .unwrap_or_default();

            // Publish completion event to the bus
            Bus::global().publish(BusEvent::BackgroundTaskCompleted(BackgroundTaskCompleted {
                task_id: task_id_clone,
                tool_name: tool_name_owned,
                display_name: display_name_owned,
                session_id: session_id_owned,
                status,
                exit_code,
                output_preview,
                output_file: output_path_clone,
                duration_secs,
                notify: notify_flag,
                wake: wake_flag,
            }));

            result
        });

        // Track the running task
        let running_task = RunningTask {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name,
            session_id: session_id.to_string(),
            status_path: status_path.clone(),
            started_at,
            started_at_rfc3339,
            delivery_flags: delivery_flags_tx,
            handle,
        };

        self.tasks.write().await.insert(task_id.clone(), running_task);
        let _ = registered_tx.send(());

        Ok(BackgroundTaskInfo {
            task_id,
            output_file: output_path,
            status_file: status_path,
        })
    }

    /// Adopt a running task and record which tool call asked for it.
    ///
    /// Identical to [`Self::adopt_with_options`] except that `tool_call_id`,
    /// `command`, and `cwd` are persisted with the task.
    #[expect(
        clippy::too_many_arguments,
        reason = "The entry point mirrors the persisted task metadata plus the running handle"
    )]
    pub async fn adopt_with_options_for_tool_call(
        &self,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        notify: bool,
        wake: bool,
        tool_call_id: &str,
        command: Option<&str>,
        cwd: Option<&std::path::Path>,
        handle: JoinHandle<Result<horon_agent_tool_types::ToolOutput>>,
    ) -> BackgroundTaskInfo {
        self.adopt_with_options_impl(
            tool_name,
            display_name,
            session_id,
            notify,
            wake,
            Some(tool_call_id),
            command,
            cwd,
            handle,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Shared body for the plain and tool-call-linked adopt entry points"
    )]
    pub(super) async fn adopt_with_options_impl(
        &self,
        tool_name: &str,
        display_name: Option<String>,
        session_id: &str,
        notify: bool,
        wake: bool,
        tool_call_id: Option<&str>,
        command: Option<&str>,
        cwd: Option<&std::path::Path>,
        handle: JoinHandle<Result<horon_agent_tool_types::ToolOutput>>,
    ) -> BackgroundTaskInfo {
        let (notify, wake) = normalize_delivery(notify, wake);
        let task_id = Self::generate_task_id();
        let output_path = self.output_dir.join(format!("{}.output", task_id));
        let status_path = self.output_dir.join(format!("{}.status.json", task_id));
        let tool_call_id_owned = tool_call_id.map(str::to_string);
        let command_owned = command.map(str::to_string);
        let cwd_owned = cwd.map(|path| path.to_string_lossy().into_owned());

        // Retain an output artifact before publishing the running task or
        // returning its ID. Adopted work buffers its output until completion,
        // when the wrapper below replaces this initially empty file. Create it
        // before spawning that wrapper so a fast completion cannot be truncated.
        let _ = std::fs::write(&output_path, "");

        let initial_status = TaskStatusFile {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name: display_name.clone(),
            session_id: session_id.to_string(),
            status: BackgroundTaskStatus::Running,
            exit_code: None,
            error: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            duration_secs: None,
            pid: None,
            owner_pid: Some(std::process::id()),
            owner_instance: Some(model::process_instance_token().to_string()),
            detached: false,
            notify,
            wake,
            progress: None,
            event_history: Vec::new(),
            stall_wake_seconds: None,
            tool_call_id: tool_call_id_owned.clone(),
            command: command_owned.clone(),
            cwd: cwd_owned.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&initial_status) {
            write_status_json_blocking(&status_path, &json);
        }
        Self::publish_task_started_activity(&task_id, tool_name, display_name.as_deref(), session_id, notify);

        let output_path_clone = output_path.clone();
        let status_path_clone = status_path.clone();
        let task_id_clone = task_id.clone();
        let tool_name_owned = tool_name.to_string();
        let session_id_owned = session_id.to_string();
        let started_at = Instant::now();
        let started_at_rfc3339 = initial_status.started_at.clone();
        let display_name_owned = initial_status.display_name.clone();
        let (delivery_flags_tx, delivery_flags_rx) = watch::channel((notify, wake));
        let tasks_for_prune = Arc::clone(&self.tasks);
        let finalizer = Arc::clone(&self.finalizer);
        let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();

        let wrapper_handle = tokio::spawn(async move {
            let tool_result = handle.await;
            let duration_secs = started_at.elapsed().as_secs_f64();

            let (status, exit_code, error, output_text) = match tool_result {
                Ok(Ok(output)) => (BackgroundTaskStatus::Completed, Some(0), None, output.output),
                Ok(Err(e)) => (BackgroundTaskStatus::Failed, None, Some(e.to_string()), e.to_string()),
                Err(e) => (
                    BackgroundTaskStatus::Failed,
                    None,
                    Some(e.to_string()),
                    format!("Task panicked: {}", e),
                ),
            };

            if let Ok(mut file) = File::create(&output_path_clone).await {
                let _ = file.write_all(output_text.as_bytes()).await;
            }

            let (notify_flag, wake_flag) = *delivery_flags_rx.borrow();
            let prior_status = tokio::fs::read_to_string(&status_path_clone)
                .await
                .ok()
                .and_then(|content| serde_json::from_str::<TaskStatusFile>(&content).ok());
            let prior_progress = prior_status.as_ref().and_then(|status| status.progress.clone());
            let prior_event_history = prior_status.map(|status| status.event_history).unwrap_or_default();

            let mut final_status = TaskStatusFile {
                task_id: task_id_clone.clone(),
                tool_name: tool_name_owned.clone(),
                display_name: display_name_owned.clone(),
                session_id: session_id_owned.clone(),
                status: status.clone(),
                exit_code,
                error: error.clone(),
                started_at: started_at_rfc3339,
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
                duration_secs: Some(duration_secs),
                pid: None,
                owner_pid: Some(std::process::id()),
                owner_instance: Some(model::process_instance_token().to_string()),
                detached: false,
                notify: notify_flag,
                wake: wake_flag,
                progress: prior_progress,
                event_history: prior_event_history,
                stall_wake_seconds: None,
                tool_call_id: tool_call_id_owned,
                command: command_owned,
                cwd: cwd_owned,
            };
            push_task_event(
                &mut final_status,
                terminal_event_record(status.clone(), exit_code, error.as_deref()),
            );
            if let Ok(json) = serde_json::to_string_pretty(&final_status) {
                write_status_json(&status_path_clone, &json).await;
            }
            finalizer.send_once(&final_status, false);

            // Prune the live-map entry only after the terminal status file is
            // persisted (and after registration below, so instant completions
            // cannot race the insert and leave a phantom entry).
            let _ = registered_rx.await;
            tasks_for_prune.write().await.remove(&task_id_clone);

            let output_preview = if output_text.len() > 500 {
                format!("{}...", crate::util::truncate_str(&output_text, 500))
            } else {
                output_text
            };

            Bus::global().publish(BusEvent::BackgroundTaskCompleted(BackgroundTaskCompleted {
                task_id: task_id_clone,
                tool_name: tool_name_owned,
                display_name: display_name_owned,
                session_id: session_id_owned,
                status: status.clone(),
                exit_code,
                output_preview,
                output_file: output_path_clone,
                duration_secs,
                notify: notify_flag,
                wake: wake_flag,
            }));

            Ok(TaskResult {
                exit_code,
                error,
                status: Some(status),
            })
        });

        let running_task = RunningTask {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            display_name: None,
            session_id: session_id.to_string(),
            status_path: status_path.clone(),
            started_at,
            started_at_rfc3339: initial_status.started_at.clone(),
            delivery_flags: delivery_flags_tx,
            handle: wrapper_handle,
        };

        self.tasks.write().await.insert(task_id.clone(), running_task);
        let _ = registered_tx.send(());

        BackgroundTaskInfo {
            task_id,
            output_file: output_path,
            status_file: status_path,
        }
    }

    pub(super) async fn finalize_detached_status_if_needed(
        &self,
        mut status: TaskStatusFile,
        status_path: &std::path::Path,
    ) -> TaskStatusFile {
        if status.status != BackgroundTaskStatus::Running || !status.detached {
            return status;
        }

        let Some(pid) = status.pid else {
            return status;
        };

        let reaped_exit = horon_agent_platform::try_reap_child_process(pid).ok().flatten();

        if reaped_exit.is_none() && horon_agent_platform::is_process_running(pid) {
            return status;
        }

        let output_path = self.output_path_for(&status.task_id);
        let output = fs::read_to_string(&output_path).await.unwrap_or_default();
        let exit_code = reaped_exit.or_else(|| Self::parse_exit_code_from_output(&output));
        let completed_at = Utc::now();
        let duration_secs = Self::status_duration_secs(&status.started_at, completed_at);
        let final_status = if matches!(exit_code, Some(0)) {
            BackgroundTaskStatus::Completed
        } else {
            BackgroundTaskStatus::Failed
        };
        let final_error = if matches!(final_status, BackgroundTaskStatus::Failed) {
            Some(match exit_code {
                Some(code) => format!("Command exited with code {}", code),
                None => "Detached command exited without a readable exit code".to_string(),
            })
        } else {
            None
        };

        status.status = final_status.clone();
        status.exit_code = exit_code;
        status.error = final_error.clone();
        status.completed_at = Some(completed_at.to_rfc3339());
        status.duration_secs = duration_secs;
        status.pid = Some(pid);
        push_task_event(
            &mut status,
            terminal_event_record(final_status.clone(), exit_code, final_error.as_deref()),
        );

        self.write_status_file(status_path, &status).await;
        self.emit_finalized(&status, false);

        let output_preview = if output.len() > 500 {
            format!("{}...", crate::util::truncate_str(&output, 500))
        } else {
            output
        };
        Bus::global().publish(BusEvent::BackgroundTaskCompleted(BackgroundTaskCompleted {
            task_id: status.task_id.clone(),
            tool_name: status.tool_name.clone(),
            display_name: status.display_name.clone(),
            session_id: status.session_id.clone(),
            status: final_status,
            exit_code,
            output_preview,
            output_file: output_path,
            duration_secs: duration_secs.unwrap_or_default(),
            notify: status.notify,
            wake: status.wake,
        }));

        status
    }

    /// True when a non-detached `Running` status file provably belongs to a
    /// process image that no longer exists, so no future can ever finalize it.
    ///
    /// Rules, deliberately conservative because the task dir is shared by
    /// every horon-agent process on the machine:
    /// - Terminal, detached, or pid-bearing files are never orphans here
    ///   (detached reconciliation is `finalize_detached_status_if_needed`).
    /// - Files owned by this exact process image are never orphans: the
    ///   initial status file is written before the task lands in the live
    ///   map, so "Running + not in map + my instance" can simply mean the
    ///   task is still bootstrapping.
    /// - Files owned by this PID but a different instance token are orphans:
    ///   an exec-based reload replaced the process image, so the owning
    ///   future is gone even though the PID matches.
    /// - Files owned by another PID are orphans only once that process is
    ///   dead.
    /// - Files without owner metadata (written by older builds) are left
    ///   alone; only the explicit startup sweep in
    ///   [`Self::reconcile_orphaned_tasks`] handles those.
    pub(super) fn status_is_reconcilable_orphan(status: &TaskStatusFile) -> bool {
        if status.status != BackgroundTaskStatus::Running || status.detached || status.pid.is_some() {
            return false;
        }
        let Some(owner_pid) = status.owner_pid else {
            return false;
        };
        if status.owner_instance.as_deref() == Some(model::process_instance_token()) {
            return false;
        }
        if owner_pid == std::process::id() {
            return true;
        }
        !horon_agent_platform::is_process_running(owner_pid)
    }

    /// Finalize an orphaned non-detached `Running` status file as `Failed`.
    ///
    /// The owning process's task future died with the process (crash or
    /// exec-based server reload), so without this the file reads `Running`
    /// forever: `bg list`/`bg status` show a phantom task and `bg wait`
    /// blocks until its timeout.
    pub(super) async fn finalize_orphaned_status_if_needed(
        &self,
        mut status: TaskStatusFile,
        status_path: &std::path::Path,
    ) -> TaskStatusFile {
        if !Self::status_is_reconcilable_orphan(&status) {
            return status;
        }
        // Belt and braces: never rewrite a task this process is executing.
        if self.is_live_task(&status.task_id) {
            return status;
        }

        let completed_at = Utc::now();
        let duration_secs = Self::status_duration_secs(&status.started_at, completed_at);
        let error = "Task orphaned: the owning server process exited (reloaded or crashed) before the task finished"
            .to_string();
        status.status = BackgroundTaskStatus::Failed;
        status.exit_code = None;
        status.error = Some(error.clone());
        status.completed_at = Some(completed_at.to_rfc3339());
        status.duration_secs = duration_secs;
        push_task_event(
            &mut status,
            terminal_event_record(BackgroundTaskStatus::Failed, None, Some(&error)),
        );
        self.write_status_file(status_path, &status).await;
        self.emit_finalized(&status, false);

        let output_path = self.output_path_for(&status.task_id);
        let output = fs::read_to_string(&output_path).await.unwrap_or_default();
        let output_preview = if output.len() > 500 {
            format!("{}...", crate::util::truncate_str(&output, 500))
        } else {
            output
        };
        Bus::global().publish(BusEvent::BackgroundTaskCompleted(BackgroundTaskCompleted {
            task_id: status.task_id.clone(),
            tool_name: status.tool_name.clone(),
            display_name: status.display_name.clone(),
            session_id: status.session_id.clone(),
            status: BackgroundTaskStatus::Failed,
            exit_code: None,
            output_preview,
            output_file: output_path,
            duration_secs: duration_secs.unwrap_or_default(),
            notify: status.notify,
            wake: status.wake,
        }));

        status
    }
}

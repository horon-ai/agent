//! Tests for task-to-tool-call links, the live task snapshot, and the
//! terminal-transition broadcast.
//!
//! They live in their own module because `tests.rs` is at its code-size
//! ratchet; being a child of the `tests` module keeps every helper and import
//! of the parent available unchanged.

use super::*;

pub(super) fn running_status_fixture(task_id: &str, session_id: &str) -> TaskStatusFile {
    TaskStatusFile {
        task_id: task_id.to_string(),
        tool_name: "swarm".to_string(),
        display_name: None,
        session_id: session_id.to_string(),
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        error: None,
        started_at: Utc::now().to_rfc3339(),
        completed_at: None,
        duration_secs: None,
        pid: None,
        owner_pid: None,
        owner_instance: None,
        detached: false,
        notify: false,
        wake: false,
        progress: None,
        event_history: Vec::new(),
        stall_wake_seconds: None,
        tool_call_id: None,
        command: None,
        cwd: None,
    }
}

pub(super) async fn write_status_fixture(manager: &BackgroundTaskManager, status: &TaskStatusFile) {
    let path = manager.status_path_for(&status.task_id);
    let json = serde_json::to_string_pretty(status).expect("serialize status fixture");
    tokio::fs::write(&path, json).await.expect("write fixture");
}

/// A tool-call-linked task is discoverable by that link while it runs, and a
/// cancellation reports exactly one `FinalizedTask` marked cancelled.
#[tokio::test]
async fn tool_call_link_survives_cancel_and_reports_one_finalization() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mut finalized = manager.subscribe_finalized();

    let info = manager
        .spawn_with_notify_for_tool_call(
            "bash",
            Some("sleep".to_string()),
            "session-link",
            false,
            false,
            "call-1",
            Some("sleep 30"),
            Some(std::path::Path::new("/tmp")),
            |_output_path| async move {
                sleep(Duration::from_secs(60)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let live = manager.live_tasks_for_session("session-link").await;
    assert_eq!(live.len(), 1, "the spawned task must be listed as live");
    assert_eq!(live[0].task_id, info.task_id);
    assert_eq!(live[0].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(live[0].command.as_deref(), Some("sleep 30"));
    assert_eq!(live[0].cwd.as_deref(), Some("/tmp"));

    let found = manager
        .task_for_tool_call("session-link", "call-1")
        .await
        .ok_or_else(|| anyhow!("tool call lookup should find the live task"))?;
    assert_eq!(found.task_id, info.task_id);
    assert!(
        manager
            .task_for_tool_call("session-link", "call-unknown")
            .await
            .is_none()
    );

    assert!(
        manager
            .cancel_with_grace(&info.task_id, Duration::from_millis(10))
            .await?
    );

    let event = tokio::time::timeout(Duration::from_secs(5), finalized.recv())
        .await
        .map_err(|_| anyhow!("no finalization broadcast within 5s"))?
        .map_err(|err| anyhow!("finalization broadcast closed: {err}"))?;
    assert_eq!(event.task_id, info.task_id);
    assert_eq!(event.session_id, "session-link");
    assert_eq!(event.tool_call_id.as_deref(), Some("call-1"));
    assert!(event.cancelled, "a user cancellation must say so");
    assert_eq!(event.status, BackgroundTaskStatus::Failed);
    assert_eq!(event.exit_code, None);
    assert_eq!(event.signal, None);

    // Exactly one event: a second observation must not produce another.
    assert!(matches!(
        finalized.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    let final_status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("cancelled task status should exist"))?;
    assert_eq!(final_status.error.as_deref(), Some("Cancelled by user"));
    assert_eq!(final_status.tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(final_status.command.as_deref(), Some("sleep 30"));
    assert!(manager.live_tasks_for_session("session-link").await.is_empty());
    Ok(())
}

/// A status file written before the link fields existed still reads, and a
/// session's live list never includes another session's tasks.
#[tokio::test]
async fn legacy_status_reads_without_links_and_sessions_stay_separate() -> Result<()> {
    let legacy = r#"{
        "task_id": "000001legacy",
        "tool_name": "bash",
        "session_id": "session-legacy",
        "status": "completed",
        "exit_code": 0,
        "error": null,
        "started_at": "2026-01-01T00:00:00+00:00",
        "completed_at": "2026-01-01T00:00:01+00:00",
        "duration_secs": 1.0
    }"#;
    let parsed: TaskStatusFile = serde_json::from_str(legacy).expect("legacy status must parse");
    assert_eq!(parsed.tool_call_id, None);
    assert_eq!(parsed.command, None);
    assert_eq!(parsed.cwd, None);

    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let info_a = manager
        .spawn_with_notify_for_tool_call(
            "bash",
            None,
            "session-a",
            false,
            false,
            "call-a",
            None,
            None,
            |_output_path| async move {
                sleep(Duration::from_secs(60)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;
    let info_b = manager
        .spawn_with_notify_for_tool_call(
            "bash",
            None,
            "session-b",
            false,
            false,
            "call-b",
            None,
            None,
            |_output_path| async move {
                sleep(Duration::from_secs(60)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let live_a = manager.live_tasks_for_session("session-a").await;
    assert_eq!(live_a.len(), 1);
    assert_eq!(live_a[0].task_id, info_a.task_id);
    assert!(live_a.iter().all(|task| task.session_id == "session-a"));
    assert!(live_a.iter().all(|task| task.task_id != info_b.task_id));
    assert_eq!(manager.live_tasks_for_session("session-b").await.len(), 1);
    assert!(manager.live_tasks_for_session("session-none").await.is_empty());
    assert!(manager.task_for_tool_call("session-b", "call-a").await.is_none());

    manager.cancel(&info_a.task_id).await?;
    manager.cancel(&info_b.task_id).await?;
    Ok(())
}

/// Natural completion reports one finalization, with the task's own status and
/// exit code and no cancellation flag.
#[tokio::test]
async fn natural_completion_reports_one_finalized_event() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mut finalized = manager.subscribe_finalized();

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-natural",
            false,
            false,
            |_output_path| async move { Ok(TaskResult::completed(Some(3))) },
        )
        .await?;

    let event = tokio::time::timeout(Duration::from_secs(5), finalized.recv())
        .await
        .map_err(|_| anyhow!("no finalization broadcast within 5s"))?
        .map_err(|err| anyhow!("finalization broadcast closed: {err}"))?;
    assert_eq!(event.task_id, info.task_id);
    assert_eq!(event.session_id, "session-natural");
    assert_eq!(event.tool_call_id, None);
    assert_eq!(event.status, BackgroundTaskStatus::Completed);
    assert_eq!(event.exit_code, Some(3));
    assert!(!event.cancelled);
    assert!(matches!(
        finalized.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    Ok(())
}

/// The reload abort sweep reports one finalization for each task it stamps.
#[tokio::test]
async fn reload_abort_reports_one_finalized_event() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let info = manager
        .spawn_with_notify_for_tool_call(
            "bash",
            None,
            "session-reload",
            false,
            false,
            "call-reload",
            Some("sleep 60"),
            None,
            |_output_path| async move {
                sleep(Duration::from_secs(60)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;
    let mut finalized = manager.subscribe_finalized();

    assert_eq!(manager.abort_live_tasks_for_reload().await, 1);

    let event = tokio::time::timeout(Duration::from_secs(5), finalized.recv())
        .await
        .map_err(|_| anyhow!("no finalization broadcast within 5s"))?
        .map_err(|err| anyhow!("finalization broadcast closed: {err}"))?;
    assert_eq!(event.task_id, info.task_id);
    assert_eq!(event.tool_call_id.as_deref(), Some("call-reload"));
    assert_eq!(event.status, BackgroundTaskStatus::Failed);
    assert!(!event.cancelled, "a reload interruption is not a user cancellation");
    assert!(matches!(
        finalized.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    Ok(())
}

/// An orphan reconciled after its owner died reports one finalization, and a
/// later read does not repeat it.
#[tokio::test]
async fn orphan_reconciliation_reports_one_finalized_event() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mut orphan = running_status_fixture("orphan9zzzz", "session-orphan-event");
    orphan.owner_pid = Some(std::process::id());
    orphan.owner_instance = Some("previous-process-image".to_string());
    orphan.tool_call_id = Some("call-orphan".to_string());
    write_status_fixture(&manager, &orphan).await;
    let mut finalized = manager.subscribe_finalized();

    assert_eq!(manager.reconcile_orphaned_tasks().await, 1);
    // Both paths observe the same terminal status; only the first reports it.
    assert!(manager.status("orphan9zzzz").await.is_some());

    let event = tokio::time::timeout(Duration::from_secs(5), finalized.recv())
        .await
        .map_err(|_| anyhow!("no finalization broadcast within 5s"))?
        .map_err(|err| anyhow!("finalization broadcast closed: {err}"))?;
    assert_eq!(event.task_id, "orphan9zzzz");
    assert_eq!(event.tool_call_id.as_deref(), Some("call-orphan"));
    assert_eq!(event.status, BackgroundTaskStatus::Failed);
    assert!(!event.cancelled);
    assert!(matches!(
        finalized.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    Ok(())
}

use std::sync::atomic::{AtomicBool, Ordering};

/// The live snapshot must never lose a running task to a lock it only probed.
///
/// `is_live_task` answers `false` when the task map is write-locked or has a
/// writer queued, so re-checking it after the candidate snapshot dropped
/// every running task of the session whenever a spawn or a prune held the
/// map. Holding the map open has to make the call wait, not answer early,
/// and churning writers keep one queued exactly while status files are read.
#[tokio::test]
async fn live_snapshot_never_drops_a_running_task_behind_a_writer() -> Result<()> {
    let tmp = tempdir()?;
    let manager = Arc::new(BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf()));
    let info = manager
        .spawn_with_notify_for_tool_call(
            "bash",
            None,
            "session-locked",
            false,
            false,
            "call-locked",
            Some("sleep 60"),
            None,
            |_output_path| async move {
                sleep(Duration::from_secs(60)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    // A writer holds the map: the snapshot has to wait for it, not bail out.
    let tasks = Arc::clone(&manager.tasks);
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let holder = tokio::spawn(async move {
        let _guard = tasks.write().await;
        let _ = held_tx.send(());
        let _ = release_rx.await;
    });
    held_rx.await.map_err(|_| anyhow!("task map writer never started"))?;

    let mut query = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move { manager.live_tasks_for_session("session-locked").await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut query)
            .await
            .is_err(),
        "the snapshot must wait for the task map instead of answering early"
    );
    release_tx.send(()).map_err(|_| anyhow!("task map writer went away"))?;
    let waited = tokio::time::timeout(Duration::from_secs(5), query)
        .await
        .map_err(|_| anyhow!("snapshot did not finish after the map was released"))??;
    holder.await.map_err(|err| anyhow!("task map writer failed: {err}"))?;
    assert_eq!(waited.len(), 1, "the running task must survive the waited lock");
    assert_eq!(waited[0].task_id, info.task_id);
    assert_eq!(waited[0].status, BackgroundTaskStatus::Running);

    // One queued writer is enough to make a best-effort probe report the
    // task as gone, so keep a writer churning during the snapshot reads.
    let stop = Arc::new(AtomicBool::new(false));
    let churn = tokio::spawn({
        let tasks = Arc::clone(&manager.tasks);
        let stop = Arc::clone(&stop);
        async move {
            while !stop.load(Ordering::Relaxed) {
                tokio::task::yield_now().await;
                let guard = tasks.write().await;
                tokio::task::yield_now().await;
                drop(guard);
            }
        }
    });
    for round in 0..50 {
        let live = manager.live_tasks_for_session("session-locked").await;
        assert_eq!(
            live.len(),
            1,
            "a queued or held map write must not hide the running task (round {round})"
        );
        assert_eq!(live[0].task_id, info.task_id);
    }
    stop.store(true, Ordering::Relaxed);
    churn.await.map_err(|err| anyhow!("task map churn failed: {err}"))?;

    manager.cancel(&info.task_id).await?;
    Ok(())
}

/// A finished task leaves the live snapshot and another session's running
/// task never enters it.
#[tokio::test]
async fn live_snapshot_excludes_finished_and_other_session_tasks() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let finished = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-finished",
            false,
            false,
            |_output_path| async move { Ok(TaskResult::completed(Some(0))) },
        )
        .await?;
    manager
        .wait(&finished.task_id, Duration::from_secs(10), false)
        .await
        .ok_or_else(|| anyhow!("the short task should reach a terminal state"))?;
    assert_ne!(
        manager
            .status(&finished.task_id)
            .await
            .ok_or_else(|| anyhow!("finished task status should exist"))?
            .status,
        BackgroundTaskStatus::Running
    );

    let mine = manager
        .spawn_with_notify("bash", None, "session-mine", false, false, |_output_path| async move {
            sleep(Duration::from_secs(60)).await;
            Ok(TaskResult::completed(Some(0)))
        })
        .await?;
    let other = manager
        .spawn_with_notify("bash", None, "session-other", false, false, |_output_path| async move {
            sleep(Duration::from_secs(60)).await;
            Ok(TaskResult::completed(Some(0)))
        })
        .await?;

    let live = manager.live_tasks_for_session("session-mine").await;
    assert_eq!(live.len(), 1, "only this session's running task belongs here");
    assert_eq!(live[0].task_id, mine.task_id);
    assert_eq!(live[0].status, BackgroundTaskStatus::Running);
    assert!(
        live.iter().all(|task| task.task_id != other.task_id),
        "another session's task must stay out"
    );
    assert!(
        manager.live_tasks_for_session("session-finished").await.is_empty(),
        "a finished task must leave the live snapshot"
    );
    let other_live = manager.live_tasks_for_session("session-other").await;
    assert_eq!(other_live.len(), 1);
    assert_eq!(other_live[0].task_id, other.task_id);

    manager.cancel(&mine.task_id).await?;
    manager.cancel(&other.task_id).await?;
    Ok(())
}

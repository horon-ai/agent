use super::*;
use crate::bus::{BackgroundTaskProgressKind, BackgroundTaskProgressSource, BusEvent};
use anyhow::anyhow;
use tempfile::tempdir;
use tokio::time::{Duration, sleep};

#[tokio::test]
async fn adopted_task_output_exists_while_running_and_is_replaced_on_completion() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        finish_rx.await?;
        Ok(horon_agent_tool_types::ToolOutput::new("complete output\n"))
    });

    let info = manager
        .adopt_with_options("bash", None, "session-adopt-output", false, false, handle)
        .await;

    // Keep the adopted work blocked so this checks Running, not a lucky completion.
    assert_eq!(
        manager.status(&info.task_id).await.unwrap().status,
        BackgroundTaskStatus::Running
    );
    assert!(info.output_file.is_file());
    assert_eq!(manager.output(&info.task_id).await.as_deref(), Some(""));

    finish_tx.send(()).unwrap();
    let finished = manager
        .wait(&info.task_id, Duration::from_secs(5), false)
        .await
        .expect("adopted task should exist");
    assert_eq!(finished.task.status, BackgroundTaskStatus::Completed);
    assert_eq!(
        manager.output(&info.task_id).await.as_deref(),
        Some("complete output\n")
    );
    Ok(())
}

#[tokio::test]
async fn spawn_with_notify_emits_started_ui_activity() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mut bus_rx = Bus::global().subscribe();

    let info = manager
        .spawn_with_notify(
            "bash",
            Some("checks".to_string()),
            "session-started",
            true,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(10)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    for _ in 0..20 {
        let event = tokio::time::timeout(Duration::from_millis(200), bus_rx.recv())
            .await
            .map_err(|err| anyhow!("timed out waiting for UI activity event: {err}"))?
            .map_err(|err| anyhow!("bus should stay open: {err}"))?;
        if let BusEvent::UiActivity(activity) = event
            && activity.session_id.as_deref() == Some("session-started")
            && activity.message.contains(&info.task_id)
        {
            assert_eq!(activity.kind, crate::bus::UiActivityKind::Background);
            assert!(activity.message.contains("Background task started"));
            assert!(activity.message.contains("checks"));
            assert_eq!(
                activity.status_notice.as_deref(),
                Some("Background task started · checks")
            );
            return Ok(());
        }
    }

    Err(anyhow!(
        "started UI activity event for task {} not received",
        info.task_id
    ))
}

#[tokio::test]
async fn update_delivery_applies_to_running_task_completion() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify("bash", None, "session-test", false, false, |output_path| async move {
            sleep(Duration::from_millis(25)).await;
            tokio::fs::write(&output_path, "hello").await?;
            Ok(TaskResult::completed(Some(0)))
        })
        .await?;

    let updated = manager
        .update_delivery(&info.task_id, true, true)
        .await
        .map_err(|err| anyhow!("update delivery should succeed: {err}"))?
        .ok_or_else(|| anyhow!("task should exist"))?;
    assert!(updated.notify);
    assert!(updated.wake);

    for _ in 0..40 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running {
            assert!(status.notify);
            assert!(status.wake);
            assert_eq!(status.status, BackgroundTaskStatus::Completed);
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }

    Err(anyhow!("background task did not complete in time"))
}

#[tokio::test]
async fn update_progress_persists_status_and_emits_bus_event() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(50)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let progress = BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(42.0),
        message: Some("Running checks".to_string()),
        current: Some(21),
        total: Some(50),
        unit: Some("tests".to_string()),
        eta_seconds: Some(8),
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };

    let mut bus_rx = Bus::global().subscribe();
    let updated = manager
        .update_progress(&info.task_id, progress.clone())
        .await
        .map_err(|err| anyhow!("update progress should succeed: {err}"))?
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(updated.progress, Some(progress.clone().normalize()));

    for _ in 0..20 {
        let event = tokio::time::timeout(Duration::from_millis(200), bus_rx.recv())
            .await
            .map_err(|err| anyhow!("timed out waiting for progress event: {err}"))?
            .map_err(|err| anyhow!("bus should stay open: {err}"))?;
        if let BusEvent::BackgroundTaskProgress(event) = event
            && event.task_id == info.task_id
        {
            assert_eq!(event.session_id, "session-progress");
            assert_eq!(event.progress, progress.normalize());
            return Ok(());
        }
    }

    Err(anyhow!("progress event for task {} not received", info.task_id))
}

#[tokio::test]
async fn update_progress_keeps_the_determinate_high_water_mark() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-monotonic-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(2)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let progress = |percent| BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(percent),
        message: None,
        current: None,
        total: None,
        unit: None,
        eta_seconds: None,
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };

    manager.update_progress(&info.task_id, progress(60.0)).await?;
    let status = manager
        .update_progress(&info.task_id, progress(25.0))
        .await?
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(status.progress.and_then(|value| value.percent), Some(60.0));
    Ok(())
}

#[tokio::test]
async fn update_progress_preserves_high_water_mark_through_reported_checkpoint() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-checkpoint-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(2)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let mut progress = BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(60.0),
        message: Some("running".into()),
        current: Some(6),
        total: Some(10),
        unit: Some("tests".into()),
        eta_seconds: None,
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };
    manager.update_progress(&info.task_id, progress.clone()).await?;

    progress.kind = BackgroundTaskProgressKind::Indeterminate;
    progress.percent = None;
    progress.current = None;
    progress.total = None;
    progress.unit = None;
    progress.message = Some("linking".into());
    let status = manager
        .update_checkpoint(&info.task_id, progress)
        .await?
        .ok_or_else(|| anyhow!("task should exist"))?;
    let stored = status.progress.ok_or_else(|| anyhow!("progress missing"))?;

    assert_eq!(stored.percent, Some(60.0));
    assert_eq!(stored.current, Some(6));
    assert_eq!(stored.total, Some(10));
    assert_eq!(stored.message.as_deref(), Some("linking"));
    Ok(())
}

#[tokio::test]
async fn concurrent_progress_updates_cannot_overwrite_the_high_water_mark() -> Result<()> {
    let tmp = tempdir()?;
    let manager = Arc::new(BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf()));
    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-concurrent-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(2)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let update = |percent| BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(percent),
        message: None,
        current: None,
        total: None,
        unit: None,
        eta_seconds: None,
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };
    let first = manager.update_progress(&info.task_id, update(80.0));
    let second = manager.update_progress(&info.task_id, update(20.0));
    tokio::try_join!(first, second)?;

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;
    assert_eq!(status.progress.and_then(|value| value.percent), Some(80.0));
    Ok(())
}

#[tokio::test]
async fn wait_returns_when_task_finishes() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-wait-finish",
            false,
            false,
            |output_path| async move {
                sleep(Duration::from_millis(25)).await;
                tokio::fs::write(&output_path, "done").await?;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let wait_result = manager
        .wait(&info.task_id, Duration::from_secs(2), true)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Finished);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Completed);
    assert_eq!(wait_result.task.exit_code, Some(0));
    Ok(())
}

#[tokio::test]
async fn wait_returns_on_progress_checkpoint() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-wait-progress",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(2)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let progress = BackgroundTaskProgress {
        kind: BackgroundTaskProgressKind::Determinate,
        percent: Some(25.0),
        message: Some("checkpoint".to_string()),
        current: Some(1),
        total: Some(4),
        unit: Some("steps".to_string()),
        eta_seconds: Some(3),
        updated_at: Utc::now().to_rfc3339(),
        source: BackgroundTaskProgressSource::Reported,
    };

    let waiter = manager.wait(&info.task_id, Duration::from_secs(2), true);
    let updater = async {
        sleep(Duration::from_millis(25)).await;
        manager
            .update_progress(&info.task_id, progress.clone())
            .await
            .map_err(|err| anyhow!("progress update should succeed: {err}"))?
            .ok_or_else(|| anyhow!("task should exist"))?;
        Result::<()>::Ok(())
    };
    let (wait_result, updater_result) = tokio::join!(waiter, updater);
    updater_result?;
    let wait_result = wait_result.ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Progress);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Running);
    assert_eq!(wait_result.task.progress, Some(progress.normalize()));
    assert!(wait_result.progress_event.is_some());
    Ok(())
}

#[tokio::test]
async fn wait_returns_on_timeout() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-wait-timeout",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_millis(250)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    let wait_result = manager
        .wait(&info.task_id, Duration::from_millis(25), true)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;

    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Timeout);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Running);
    Ok(())
}

fn running_status_fixture(task_id: &str, session_id: &str) -> TaskStatusFile {
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
    }
}

async fn write_status_fixture(manager: &BackgroundTaskManager, status: &TaskStatusFile) {
    let path = manager.status_path_for(&status.task_id);
    let json = serde_json::to_string_pretty(status).expect("serialize status fixture");
    tokio::fs::write(&path, json).await.expect("write fixture");
}

#[tokio::test]
async fn tasks_map_prunes_entry_after_natural_completion() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify("bash", None, "session-prune", false, false, |_output_path| async move {
            sleep(Duration::from_millis(10)).await;
            Ok(TaskResult::completed(Some(0)))
        })
        .await?;
    assert!(
        manager.is_live_task(&info.task_id),
        "task should be live right after spawn"
    );

    for _ in 0..200 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running && !manager.is_live_task(&info.task_id) {
            // Pruned only after the status file was finalized, so the live
            // map never claims a task whose status file is already terminal.
            let (running_count, labels, _) = manager.running_snapshot();
            assert_eq!(running_count, 0, "snapshot should not count finished tasks");
            assert!(labels.is_empty());
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }

    Err(anyhow!(
        "task {} was not pruned from the live map after completion",
        info.task_id
    ))
}

#[tokio::test]
async fn reconcile_marks_orphan_from_reloaded_process_failed() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // Same PID, different instance token: exactly what an exec-based server
    // reload leaves behind.
    let mut orphan = running_status_fixture("orphan1aaaa", "session-orphan");
    orphan.owner_pid = Some(std::process::id());
    orphan.owner_instance = Some("previous-process-image".to_string());
    write_status_fixture(&manager, &orphan).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 1);

    let status = manager
        .status("orphan1aaaa")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    let error = status.error.unwrap_or_default();
    assert!(
        error.contains("orphaned") && error.contains("reloaded"),
        "error should explain the reload orphaning, got: {error}"
    );
    assert!(status.completed_at.is_some());
    Ok(())
}

#[tokio::test]
async fn reconcile_marks_orphan_from_dead_process_failed() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // A child process that has already exited and been reaped gives us a PID
    // that is provably not running.
    let mut child = std::process::Command::new("true")
        .spawn()
        .map_err(|err| anyhow!("spawn child: {err}"))?;
    let dead_pid = child.id();
    child.wait().map_err(|err| anyhow!("wait child: {err}"))?;

    let mut orphan = running_status_fixture("orphan2bbbb", "session-orphan-dead");
    orphan.owner_pid = Some(dead_pid);
    orphan.owner_instance = Some("some-dead-instance".to_string());
    write_status_fixture(&manager, &orphan).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 1);
    let status = manager
        .status("orphan2bbbb")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    Ok(())
}

#[tokio::test]
async fn reconcile_leaves_non_orphans_alone() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // Owned by this exact process image: could still be bootstrapping.
    let mut own = running_status_fixture("keep1aaaa", "session-keep");
    own.owner_pid = Some(std::process::id());
    own.owner_instance = Some(model::process_instance_token().to_string());
    write_status_fixture(&manager, &own).await;

    // Legacy file without owner metadata: no safe liveness signal, leave it.
    let legacy = running_status_fixture("keep2bbbb", "session-keep");
    write_status_fixture(&manager, &legacy).await;

    // Owned by a live foreign process (PID 1 is always alive on Unix).
    let mut foreign = running_status_fixture("keep3cccc", "session-keep");
    foreign.owner_pid = Some(1);
    foreign.owner_instance = Some("init-instance".to_string());
    write_status_fixture(&manager, &foreign).await;

    // Detached with a live pid: reconciled by the detached path, not this one.
    let mut detached = running_status_fixture("keep4dddd", "session-keep");
    detached.detached = true;
    detached.pid = Some(std::process::id());
    write_status_fixture(&manager, &detached).await;

    let reconciled = manager.reconcile_orphaned_tasks().await;
    assert_eq!(reconciled, 0);

    for task_id in ["keep1aaaa", "keep2bbbb", "keep3cccc", "keep4dddd"] {
        let status = manager
            .status(task_id)
            .await
            .ok_or_else(|| anyhow!("status for {task_id} should exist"))?;
        assert_eq!(
            status.status,
            BackgroundTaskStatus::Running,
            "{task_id} should not be reconciled"
        );
    }
    Ok(())
}

#[tokio::test]
async fn status_read_self_heals_orphaned_task() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let mut orphan = running_status_fixture("orphan3cccc", "session-orphan-read");
    orphan.owner_pid = Some(std::process::id());
    orphan.owner_instance = Some("previous-process-image".to_string());
    write_status_fixture(&manager, &orphan).await;

    // A plain status read (used by bg status / bg wait) heals the phantom
    // without waiting for the startup sweep.
    let status = manager
        .status("orphan3cccc")
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);

    // And wait() returns immediately instead of blocking to timeout.
    let wait_result = manager
        .wait("orphan3cccc", Duration::from_secs(5), false)
        .await
        .ok_or_else(|| anyhow!("wait should find the task"))?;
    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::AlreadyFinished);
    Ok(())
}

#[tokio::test]
async fn abort_live_tasks_for_reload_finalizes_running_tasks() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    // A long-running task that would otherwise vanish across an exec reload.
    let info = manager
        .spawn_with_notify(
            "selfdev-build",
            Some("selfdev build".to_string()),
            "session-reload-abort",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(60)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;
    assert!(manager.is_live_task(&info.task_id));

    let aborted = manager.abort_live_tasks_for_reload().await;
    assert_eq!(aborted, 1);
    assert!(!manager.is_live_task(&info.task_id), "live map should be drained");

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.status, BackgroundTaskStatus::Failed);
    let error = status.error.unwrap_or_default();
    assert!(
        error.contains("Interrupted by server reload"),
        "error should explain the reload interruption, got: {error}"
    );
    assert!(status.completed_at.is_some());

    // Idempotent: a second sweep finds nothing.
    assert_eq!(manager.abort_live_tasks_for_reload().await, 0);
    Ok(())
}

#[tokio::test]
async fn abort_live_tasks_for_reload_keeps_naturally_finished_status() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-reload-finished",
            false,
            false,
            |_output_path| async move { Ok(TaskResult::completed(Some(0))) },
        )
        .await?;

    // Let the task finish and write its terminal status.
    for _ in 0..200 {
        let status = manager
            .status(&info.task_id)
            .await
            .ok_or_else(|| anyhow!("status should exist"))?;
        if status.status != BackgroundTaskStatus::Running {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        manager.abort_live_tasks_for_reload().await,
        0,
        "a naturally completed task should not be counted as finalized"
    );

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(
        status.status,
        BackgroundTaskStatus::Completed,
        "a task that finished before the sweep must keep its real status"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn stall_watchdog_fires_and_wait_returns_stalled() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            Some("quiet task".to_string()),
            "session-stall-fire",
            false,
            false,
            |_output_path| async move {
                // Hang well past the stall window without producing output.
                sleep(Duration::from_secs(3600)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    // Requested window below the minimum is clamped up.
    let effective = manager
        .arm_stall_watchdog(&info.task_id, 1)
        .await
        .ok_or_else(|| anyhow!("watchdog should arm on a running task"))?;
    assert_eq!(effective, BackgroundTaskManager::MIN_STALL_WAKE_SECONDS);

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.stall_wake_seconds, Some(effective));

    let wait_result = manager
        .wait(&info.task_id, Duration::from_secs(600), false)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;
    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Stalled);
    assert_eq!(wait_result.task.status, BackgroundTaskStatus::Running);

    manager.cancel(&info.task_id).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn stall_watchdog_does_not_fire_for_task_that_finishes() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mut bus_rx = Bus::global().subscribe();

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-stall-no-fire",
            false,
            false,
            |output_path| async move {
                sleep(Duration::from_secs(10)).await;
                tokio::fs::write(&output_path, "done").await?;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    manager
        .arm_stall_watchdog(&info.task_id, 30)
        .await
        .ok_or_else(|| anyhow!("watchdog should arm on a running task"))?;

    let wait_result = manager
        .wait(&info.task_id, Duration::from_secs(600), false)
        .await
        .ok_or_else(|| anyhow!("task should exist"))?;
    assert_eq!(wait_result.reason, BackgroundTaskWaitReason::Finished);

    // Give the watchdog time to notice the terminal status and exit.
    sleep(Duration::from_secs(30)).await;
    while let Ok(event) = bus_rx.try_recv() {
        if let BusEvent::BackgroundTaskStalled(event) = event {
            assert_ne!(
                event.task_id, info.task_id,
                "watchdog must not fire for a task that finished within its window"
            );
        }
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn disarm_stall_watchdog_clears_state() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());

    let info = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-stall-disarm",
            false,
            false,
            |_output_path| async move {
                sleep(Duration::from_secs(3600)).await;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;

    manager
        .arm_stall_watchdog(&info.task_id, 45)
        .await
        .ok_or_else(|| anyhow!("watchdog should arm"))?;
    assert!(manager.disarm_stall_watchdog(&info.task_id).await);

    let status = manager
        .status(&info.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist"))?;
    assert_eq!(status.stall_wake_seconds, None);

    manager.cancel(&info.task_id).await?;
    Ok(())
}

fn retention_manager(
    tmp: &std::path::Path,
    retention: ArtifactRetention,
    policy: ArtifactRetentionPolicy,
) -> BackgroundTaskManager {
    BackgroundTaskManager::with_retention(tmp.to_path_buf(), retention, policy)
}

fn terminal_status_fixture(task_id: &str, started_at: &str) -> TaskStatusFile {
    TaskStatusFile {
        task_id: task_id.to_string(),
        tool_name: "bash".to_string(),
        display_name: None,
        session_id: "session-retention".to_string(),
        status: BackgroundTaskStatus::Completed,
        exit_code: Some(0),
        error: None,
        started_at: started_at.to_string(),
        completed_at: Some(started_at.to_string()),
        duration_secs: Some(1.0),
        pid: None,
        owner_pid: None,
        owner_instance: None,
        detached: false,
        notify: false,
        wake: false,
        progress: None,
        event_history: Vec::new(),
        stall_wake_seconds: None,
    }
}

async fn write_terminal_task(
    dir: &std::path::Path,
    task_id: &str,
    started_at: &str,
    output_bytes: usize,
    status_pad: usize,
) -> (PathBuf, u64) {
    let status_path = dir.join(format!("{task_id}.status.json"));
    let output_path = dir.join(format!("{task_id}.output"));
    let mut status = terminal_status_fixture(task_id, started_at);
    status.display_name = Some("x".repeat(status_pad));
    let json = serde_json::to_string(&status).expect("serialize terminal fixture");
    tokio::fs::write(&status_path, json)
        .await
        .expect("write status fixture");
    tokio::fs::write(&output_path, "y".repeat(output_bytes))
        .await
        .expect("write output fixture");
    let freed = tokio::fs::metadata(&status_path).await.expect("status meta").len()
        + tokio::fs::metadata(&output_path).await.expect("output meta").len();
    (status_path, freed)
}

fn artifact_bytes(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len())
                .sum()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn retention_removes_oldest_terminal() -> Result<()> {
    let tmp = tempdir()?;

    let mut ids = Vec::new();
    let mut sizes = Vec::new();
    for index in 0..5 {
        let id = format!("task-{index}");
        let (_, bytes) =
            write_terminal_task(tmp.path(), &id, &format!("2026-10-19T00:0{index}:00+00:00"), 1_000, 0).await;
        ids.push(id);
        sizes.push(bytes);
    }
    // Budget sized so freeing exactly the two oldest tasks is enough.
    let byte_budget = sizes.iter().sum::<u64>() - sizes[0] - sizes[1];
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget,
            file_budget: 10_000,
            retain_recent_terminal: RETAIN_RECENT_TERMINAL,
        },
        ArtifactRetentionPolicy::Rotate,
    );

    let report = manager.enforce_artifact_budget().await;

    assert_eq!(report.policy, ArtifactRetentionPolicy::Rotate);
    assert_eq!(
        report.removed_task_ids,
        vec!["task-0".to_string(), "task-1".to_string()]
    );
    assert!(!tmp.path().join("task-0.output").exists());
    assert!(!tmp.path().join("task-0.status.json").exists());
    assert!(!tmp.path().join("task-1.output").exists());
    for kept in ["task-2", "task-3", "task-4"] {
        assert!(
            tmp.path().join(format!("{kept}.output")).exists(),
            "{kept} output must survive"
        );
        assert!(
            tmp.path().join(format!("{kept}.status.json")).exists(),
            "{kept} status must survive"
        );
    }
    assert!(!report.over_budget);
    assert_eq!(report.remaining_bytes, artifact_bytes(tmp.path()));
    Ok(())
}

#[tokio::test]
async fn retention_reclaims_exactly_the_freed_bytes() -> Result<()> {
    let tmp = tempdir()?;
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1,
            file_budget: 10_000,
            retain_recent_terminal: 1,
        },
        ArtifactRetentionPolicy::Rotate,
    );

    let (_, first_freed) = write_terminal_task(tmp.path(), "a1", "2026-10-19T00:01:00+00:00", 700, 0).await;
    write_terminal_task(tmp.path(), "a2", "2026-10-19T00:02:00+00:00", 700, 0).await;
    let before = artifact_bytes(tmp.path());

    let report = manager.enforce_artifact_budget().await;

    let after = artifact_bytes(tmp.path());
    assert_eq!(report.reclaimed_bytes, first_freed);
    assert_eq!(before - after, report.reclaimed_bytes);
    assert_eq!(report.remaining_bytes, after);
    Ok(())
}

#[tokio::test]
async fn retention_never_removes_the_protected_floor() -> Result<()> {
    let tmp = tempdir()?;
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1,
            file_budget: 10_000,
            retain_recent_terminal: RETAIN_RECENT_TERMINAL,
        },
        ArtifactRetentionPolicy::Rotate,
    );

    for index in 0..11 {
        write_terminal_task(
            tmp.path(),
            &format!("floor-{index:02}"),
            &format!("2026-10-19T00:{index:02}:00+00:00"),
            100,
            0,
        )
        .await;
    }

    let report = manager.enforce_artifact_budget().await;

    assert_eq!(
        report.removed_task_ids.len(),
        9,
        "only tasks older than the floor are eligible"
    );
    assert_eq!(report.removed_task_ids.first().map(String::as_str), Some("floor-00"));
    assert_eq!(report.removed_task_ids.last().map(String::as_str), Some("floor-08"));
    for kept in ["floor-09", "floor-10"] {
        assert!(tmp.path().join(format!("{kept}.status.json")).exists());
    }
    assert!(report.over_budget, "the protected floor alone exceeds a 1-byte budget");
    Ok(())
}

#[tokio::test]
async fn retention_never_removes_a_running_task() -> Result<()> {
    let tmp = tempdir()?;
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1,
            file_budget: 10_000,
            retain_recent_terminal: RETAIN_RECENT_TERMINAL,
        },
        ArtifactRetentionPolicy::Rotate,
    );

    let running = running_status_fixture("running-1", "session-retention");
    write_status_fixture(&manager, &running).await;
    tokio::fs::write(manager.output_path_for("running-1"), "z".repeat(4_000)).await?;

    let report = manager.enforce_artifact_budget().await;

    assert_eq!(report.removed_files, 0);
    assert!(tmp.path().join("running-1.output").exists());
    assert!(tmp.path().join("running-1.status.json").exists());
    assert!(report.over_budget);
    Ok(())
}

#[tokio::test]
async fn retention_reports_over_budget_without_touching_protected_files() -> Result<()> {
    let tmp = tempdir()?;
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1_000,
            file_budget: 10_000,
            retain_recent_terminal: RETAIN_RECENT_TERMINAL,
        },
        ArtifactRetentionPolicy::Rotate,
    );

    write_terminal_task(tmp.path(), "solo-1", "2026-10-19T00:01:00+00:00", 5_000, 0).await;
    let before = artifact_bytes(tmp.path());

    let report = manager.enforce_artifact_budget().await;

    assert!(report.over_budget);
    assert_eq!(report.removed_files, 0);
    assert!(report.reclaimed_bytes == 0);
    assert_eq!(report.remaining_bytes, before);
    assert!(tmp.path().join("solo-1.status.json").exists());
    Ok(())
}

#[tokio::test]
async fn retention_preserve_refuses_new() -> Result<()> {
    let tmp = tempdir()?;
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1,
            file_budget: 1,
            retain_recent_terminal: RETAIN_RECENT_TERMINAL,
        },
        ArtifactRetentionPolicy::Preserve,
    );

    write_terminal_task(tmp.path(), "keep-1", "2026-10-19T00:01:00+00:00", 500, 0).await;

    let report = manager.enforce_artifact_budget().await;
    assert_eq!(report.policy, ArtifactRetentionPolicy::Preserve);
    assert!(report.over_budget);
    assert_eq!(report.removed_files, 0, "preserve must not reclaim");
    assert!(tmp.path().join("keep-1.output").exists());

    let err = manager
        .spawn_with_notify("bash", None, "session-retention", false, false, |_| async {
            Ok(TaskResult::completed(Some(0)))
        })
        .await
        .expect_err("preserve must refuse a new task while over budget");
    let message = err.to_string();
    assert!(message.contains("preserve"), "err={message}");
    assert!(message.contains("HORON_AGENT_JOBS_ARTIFACT_RETENTION"), "err={message}");
    Ok(())
}

#[tokio::test]
async fn retention_leaves_unrelated_files_alone() -> Result<()> {
    let tmp = tempdir()?;
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1,
            file_budget: 1,
            retain_recent_terminal: 0,
        },
        ArtifactRetentionPolicy::Rotate,
    );

    write_terminal_task(tmp.path(), "own-1", "2026-10-19T00:01:00+00:00", 500, 0).await;
    tokio::fs::write(tmp.path().join("notes.txt"), "user file").await?;

    let report = manager.enforce_artifact_budget().await;

    assert!(
        tmp.path().join("notes.txt").exists(),
        "only task artifacts are reclaimable"
    );
    assert_eq!(report.removed_task_ids, vec!["own-1".to_string()]);
    Ok(())
}

#[tokio::test]
async fn retention_skips_artifacts_with_a_live_wait() -> Result<()> {
    let tmp = tempdir()?;
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1,
            file_budget: 1,
            retain_recent_terminal: 0,
        },
        ArtifactRetentionPolicy::Rotate,
    );

    write_terminal_task(tmp.path(), "waited-1", "2026-10-19T00:01:00+00:00", 500, 0).await;
    let _lease = WaitLease::acquire(&manager.in_flight_waits, "waited-1");

    let report = manager.enforce_artifact_budget().await;

    assert_eq!(report.removed_files, 0);
    assert!(tmp.path().join("waited-1.output").exists());
    assert!(report.over_budget);
    Ok(())
}

#[tokio::test]
async fn retention_preserve_bails_on_the_first_spawn_after_the_budget_fills() -> Result<()> {
    let tmp = tempdir()?;
    // The first task's own output pushes the directory over the byte budget;
    // the sweep before it sees an empty directory.
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1_000,
            file_budget: 10_000,
            retain_recent_terminal: 0,
        },
        ArtifactRetentionPolicy::Preserve,
    );

    let first = manager
        .spawn_with_notify(
            "bash",
            None,
            "session-preserve-throttle",
            false,
            false,
            |output_path| async move {
                tokio::fs::write(&output_path, "x".repeat(5_000)).await?;
                Ok(TaskResult::completed(Some(0)))
            },
        )
        .await?;
    manager
        .wait(&first.task_id, Duration::from_secs(5), false)
        .await
        .expect("first task should finish");

    // The first spawn left a fresh, under-budget sweep in the cache and then
    // wrote artifacts that push the directory over the budget. The very next
    // spawn must not reuse that stale reading to admit more work.
    let err = manager
        .spawn_with_notify("bash", None, "session-preserve-throttle", false, false, |_| async {
            Ok(TaskResult::completed(Some(0)))
        })
        .await
        .expect_err("the first spawn after the budget fills must refuse");
    let message = err.to_string();
    assert!(message.contains("preserve"), "err={message}");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn retention_throttle_reuses_a_recent_sweep_and_resumes_after_the_window() -> Result<()> {
    let tmp = tempdir()?;
    let manager = retention_manager(
        tmp.path(),
        ArtifactRetention {
            byte_budget: 1_000_000,
            file_budget: 1,
            retain_recent_terminal: 0,
        },
        ArtifactRetentionPolicy::Rotate,
    );

    // Warm the cache on an empty directory: the sweep is under budget, so a
    // burst of spawns has nothing to reclaim and reuses it.
    let warm = manager.enforce_artifact_budget().await;
    assert!(!warm.over_budget);

    write_terminal_task(tmp.path(), "fixture-0", "2026-10-19T00:01:00+00:00", 500, 0).await;
    write_terminal_task(tmp.path(), "fixture-1", "2026-10-19T00:02:00+00:00", 500, 0).await;

    let burst = manager
        .spawn_with_notify("bash", None, "session-throttle", false, false, |_| async {
            Ok(TaskResult::completed(Some(0)))
        })
        .await?;
    assert!(
        tmp.path().join("fixture-0.status.json").exists(),
        "a spawn inside the throttle window must not walk the directory"
    );
    manager
        .wait(&burst.task_id, Duration::from_secs(5), false)
        .await
        .expect("burst task should finish");

    tokio::time::advance(ARTIFACT_SWEEP_THROTTLE + Duration::from_secs(1)).await;

    manager
        .spawn_with_notify("bash", None, "session-throttle", false, false, |_| async {
            Ok(TaskResult::completed(Some(0)))
        })
        .await?;
    assert!(
        !tmp.path().join("fixture-0.status.json").exists(),
        "the first spawn after the window must sweep and reclaim the oldest task"
    );
    Ok(())
}

#[test]
fn retention_policy_parses_known_values_only() {
    assert_eq!(
        ArtifactRetentionPolicy::parse("rotate"),
        Some(ArtifactRetentionPolicy::Rotate)
    );
    assert_eq!(
        ArtifactRetentionPolicy::parse(" Preserve "),
        Some(ArtifactRetentionPolicy::Preserve)
    );
    assert_eq!(ArtifactRetentionPolicy::parse("archive"), None);
    assert_eq!(ArtifactRetentionPolicy::default(), ArtifactRetentionPolicy::Rotate);
}

/// An existing task must never read as missing while its status file is being
/// rewritten. Before status writes went through `write_status_json`, the file
/// was truncated in place and a concurrent `status()` could parse an empty or
/// half-written document and return `None` (the app-core
/// `test_background_command_*` "status should exist" failures).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_reads_never_observe_a_torn_write() -> Result<()> {
    let tmp = tempdir()?;
    let manager = BackgroundTaskManager::with_output_dir(tmp.path().to_path_buf());
    let mut status = running_status_fixture("atomic1aaaa", "session-atomic");
    // A full event history makes the document as large as a busy real task's,
    // so a truncate-then-write window is wide enough to be observed.
    status.event_history = (0..50)
        .map(|step| BackgroundTaskEventRecord {
            kind: BackgroundTaskEventKind::Progress,
            timestamp: Utc::now().to_rfc3339(),
            message: Some(format!("step {step} of a long running build")),
            status: None,
            exit_code: None,
            progress: None,
        })
        .collect();
    let path = manager.status_path_for(&status.task_id);
    write_status_json(&path, &serde_json::to_string_pretty(&status)?).await;
    assert!(
        manager.status(&status.task_id).await.is_some(),
        "fixture must be readable before the race starts"
    );

    let writer = {
        let path = path.clone();
        let status = status.clone();
        tokio::spawn(async move {
            for step in 0..400u32 {
                let mut snapshot = status.clone();
                snapshot.exit_code = Some(i32::try_from(step).unwrap_or(i32::MAX));
                let json = serde_json::to_string_pretty(&snapshot).expect("serialize status");
                write_status_json(&path, &json).await;
            }
        })
    };

    let mut reads = 0usize;
    while !writer.is_finished() {
        let read = manager
            .status(&status.task_id)
            .await
            .ok_or_else(|| anyhow!("existing task read as missing after {reads} clean reads"))?;
        assert_eq!(read.task_id, status.task_id);
        assert_eq!(read.event_history.len(), 50, "a partial document must never parse");
        reads += 1;
    }
    writer.await?;
    assert!(reads > 0, "the reader must overlap the writer");
    let final_read = manager
        .status(&status.task_id)
        .await
        .ok_or_else(|| anyhow!("status should exist after the last write"))?;
    assert_eq!(final_read.exit_code, Some(399));

    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
    Ok(())
}

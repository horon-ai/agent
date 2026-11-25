use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{
    RunDeadlineExceeded, run_deadline, run_jobs, run_report_provider_name,
    run_single_message_command_capture_with_auto_poke, run_single_message_command_ndjson,
    run_single_message_command_plain_with_auto_poke,
};
use anyhow::Result;

use serde::Serialize;

use crate::background::{BackgroundTaskManager, TaskStatusFile};
use crate::bus::BackgroundTaskStatus;

#[derive(Debug, Serialize)]
pub(super) struct JobReport {
    task_id: String,
    status: BackgroundTaskStatus,
    exit_code: Option<i32>,
}

pub(super) struct OwnedJobs<'a> {
    manager: &'a BackgroundTaskManager,
    statuses: Vec<JobReport>,
}

impl OwnedJobs<'static> {
    pub(super) fn new() -> Self {
        Self {
            manager: crate::background::global(),
            statuses: Vec::new(),
        }
    }
}

impl OwnedJobs<'_> {
    pub(super) async fn settle(&mut self, session_id: &str) -> Vec<JobReport> {
        let mut finalized = self.manager.subscribe_finalized();
        loop {
            self.refresh(session_id).await;
            if self
                .statuses
                .iter()
                .all(|job| job.status != BackgroundTaskStatus::Running)
            {
                return std::mem::take(&mut self.statuses);
            }
            // Detached jobs can finish without an in-process event.
            let _wake = tokio::time::timeout(Duration::from_secs(1), finalized.recv()).await;
        }
    }

    pub(super) async fn refresh(&mut self, session_id: &str) {
        self.statuses = self
            .manager
            .list_local_for_session(session_id)
            .await
            .into_iter()
            .map(
                |TaskStatusFile {
                     task_id,
                     status,
                     exit_code,
                     ..
                 }| JobReport {
                    task_id,
                    status,
                    exit_code,
                },
            )
            .collect();
    }

    pub(super) fn statuses(&self) -> &[JobReport] {
        &self.statuses
    }
}

pub(super) fn print_statuses(jobs: &[JobReport]) {
    for job in jobs {
        eprintln!("background job {}: {:?}", job.task_id, job.status);
    }
}
#[derive(Debug, Serialize)]
pub(super) struct RunCommandReport {
    pub(super) session_id: String,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) text: String,
    pub(super) usage: crate::agent::TokenUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) background_jobs: Option<Vec<run_jobs::JobReport>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) a2ui: Vec<serde_json::Value>,
}

#[derive(Default)]
pub(in crate::cli) struct RunLimits {
    pub max_time: Option<Duration>,
    pub wait_async: bool,
    pub print_thoughts: bool,
    pub a2ui: bool,
}

pub(in crate::cli) async fn run_single_message_with_agent(
    agent: &mut crate::agent::Agent,
    provider: std::sync::Arc<dyn crate::provider::Provider>,
    message: &str,
    emit_json: bool,
    emit_ndjson: bool,
    limits: RunLimits,
) -> Result<()> {
    let elapsed = AtomicBool::new(false);
    let deadline = limits
        .max_time
        .map(|duration| (duration, agent.graceful_shutdown_signal()));
    let mut jobs = limits.wait_async.then(run_jobs::OwnedJobs::new);
    let run_provider = provider.clone();
    let run = async {
        if emit_json {
            let msg_start = agent.messages().len();
            let text = run_single_message_command_capture_with_auto_poke(agent, message).await?;
            if elapsed.load(Ordering::Relaxed) {
                return Ok(());
            }
            if let Some(reason) = agent.one_shot_incomplete_failure() {
                anyhow::bail!(reason);
            }
            let a2ui = super::a2ui_run::collect_a2ui_json_blocks(&agent.messages()[msg_start..]);
            let report = RunCommandReport {
                session_id: agent.session_id().to_string(),
                provider: run_report_provider_name(provider.as_ref(), agent.session_provider_key().as_deref()),
                model: provider.model(),
                text,
                usage: agent.last_usage().clone(),
                background_jobs: if let Some(jobs) = jobs.as_mut() {
                    Some(jobs.settle(agent.session_id()).await)
                } else {
                    None
                },
                a2ui,
            };
            if elapsed.load(Ordering::Relaxed) {
                return Ok(());
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else if emit_ndjson {
            run_single_message_command_ndjson(agent, provider, message, &elapsed, jobs.as_mut()).await?;
        } else {
            let msg_start = agent.messages().len();
            run_single_message_command_plain_with_auto_poke(agent, message, limits.print_thoughts, &elapsed).await?;
            if !elapsed.load(Ordering::Relaxed)
                && let Some(reason) = agent.one_shot_incomplete_failure()
            {
                anyhow::bail!(reason);
            }
            if !elapsed.load(Ordering::Relaxed) && limits.a2ui {
                super::a2ui_run::print_a2ui_text_cards(&agent.messages()[msg_start..])?;
            }
            if let Some(jobs) = jobs.as_mut() {
                let statuses = jobs.settle(agent.session_id()).await;
                if !elapsed.load(Ordering::Relaxed) {
                    run_jobs::print_statuses(&statuses);
                }
            }
        }
        Ok(())
    };
    let result = match deadline {
        Some((duration, cancel)) => run_deadline::run_with_deadline(duration, &elapsed, || cancel.fire(), run).await,
        None => run.await,
    };

    // `Agent::new` and session restore both register this process as the active
    // owner. Unlike the interactive lifecycle, `horon-agent run` has no later quit
    // path to close the session. Finalize after output has been emitted, while
    // returning the original command result unchanged. This prevents a normal
    // one-shot exit from looking like a stale-PID crash on the next startup
    // (issue #988).
    agent.mark_closed();
    if let Some(error) = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<RunDeadlineExceeded>())
    {
        if let Some(jobs) = jobs.as_mut() {
            if jobs.statuses().is_empty() {
                let _finished =
                    tokio::time::timeout(Duration::from_millis(100), jobs.refresh(agent.session_id())).await;
            }
            run_jobs::print_statuses(jobs.statuses());
        }
        error.report(agent.session_id(), emit_json, emit_ndjson)?;
    } else if emit_json && let Err(ref error) = result {
        let report = serde_json::json!({
            "type": "error",
            "session_id": agent.session_id(),
            "provider": run_report_provider_name(run_provider.as_ref(), agent.session_provider_key().as_deref()),
            "model": run_provider.model(),
            "message": format!("{error:#}"),
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{BackgroundTaskManager, BackgroundTaskStatus, Duration, OwnedJobs};
    use crate::background::TaskResult;
    use std::sync::Arc;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn waits_for_two_owned_jobs_without_waiting_for_another_session() {
        let home = tempfile::tempdir().unwrap();
        let manager = BackgroundTaskManager::with_output_dir(home.path().to_path_buf());
        let release_first = Arc::new(Notify::new());
        let release_second = Arc::new(Notify::new());
        let release_other = Arc::new(Notify::new());
        let spawn = |session: &'static str, release: Arc<Notify>, exit| {
            manager.spawn("test", session, move |_| async move {
                release.notified().await;
                Ok(TaskResult::completed(Some(exit)))
            })
        };
        let first = spawn("owned", release_first.clone(), 0).await.unwrap();
        let second = spawn("owned", release_second.clone(), 7).await.unwrap();
        let other = spawn("other", release_other.clone(), 0).await.unwrap();
        let mut jobs = OwnedJobs {
            manager: &manager,
            statuses: Vec::new(),
        };
        jobs.refresh("owned").await;
        assert_eq!(jobs.statuses().len(), 2);
        assert!(
            jobs.statuses()
                .iter()
                .all(|job| job.status == BackgroundTaskStatus::Running)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), jobs.settle("owned"))
                .await
                .is_err()
        );

        release_first.notify_one();
        release_second.notify_one();
        let output = tokio::time::timeout(Duration::from_secs(5), jobs.settle("owned"))
            .await
            .unwrap();
        let mut delivered: Vec<_> = output.into_iter().map(|job| (job.task_id, job.exit_code)).collect();
        delivered.sort_unstable();
        let mut expected = [(first.task_id, Some(0)), (second.task_id, Some(7))];
        expected.sort_unstable();
        assert_eq!(delivered, expected);
        assert_eq!(
            manager.status(&other.task_id).await.unwrap().status,
            BackgroundTaskStatus::Running
        );
        release_other.notify_one();
        manager
            .wait(&other.task_id, Duration::from_secs(5), false)
            .await
            .unwrap();
    }
}

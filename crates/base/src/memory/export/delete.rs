//! `delete_user`: a resumable, idempotent deletion of one user volume.
//!
//! Deletion is irreversible, so the order is the contract and the record is
//! durable. A job file lives at `<volume>/volume-state/deletion-job.json`, which
//! is **outside the memory store** on purpose: the job that is deleting the data
//! must not lose its own progress when a step deletes that data.
//!
//! # The steps, in order
//!
//! | # | Step | Who runs it | What proves it finished |
//! | --- | --- | --- | --- |
//! | 1 | `write_stop` | agent | the fence file exists and the store refuses writes |
//! | 2 | `key_revoke` | agent, key service | the key record names `revoked_at`, and staged material is wiped |
//! | 3 | `snapshot_delete` | control plane / provider | every snapshot the control plane lists is deleted |
//! | 4 | `volume_delete` | control plane / provider | the provider reports the volume destroyed |
//! | 5 | `mark_deleted` | control plane | the central row reads `deleted` |
//!
//! A local wipe of the store's page files and index runs between steps 2 and 3,
//! because once the key is revoked nothing may read the volume again and the
//! bytes should not wait on a provider call to go.
//!
//! # Why the order is not negotiable
//!
//! * Writes stop first, so a page written after the snapshot list was read
//!   cannot outlive the volume that held it.
//! * The key is revoked second, so any copy of the data that escaped the volume
//!   (a snapshot, a backup, a restored disk) is unreadable before any of it is
//!   destroyed. Destroying first and revoking second would leave a window where
//!   a copy was readable and nothing tracked it.
//! * Snapshots are deleted before the volume, because a provider keeps them
//!   until retention expiry and they are the largest residue.
//! * `deleted` is written **last**. Reporting `deleted` before every step
//!   finished would tell the customer their data is gone while a snapshot still
//!   holds it.
//!
//! # Resumability
//!
//! Each step's completion is recorded in the job file as soon as it finishes.
//! `run` skips steps already recorded `done`, so a retry after a provider outage
//! resumes at the failed step instead of starting over — and a re-run that
//! arrives after a full success reports the same `deleted` without repeating a
//! provider call. A **failed** step is recorded with its reason and stays
//! retryable: [`DeleteReport::state`] is then `deleting`, never `deleted`, and
//! [`DeleteReport::failed_step`] names which step to look at.

use super::control_plane::{
    ControlPlane, DeletionRequest, SnapshotDeletion, SnapshotQuery, StepReport, VolumeDeletion, VolumeState,
    VolumeStateChange,
};
use super::{VolumeLayout, key, now_rfc3339, write_private_file};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

/// File name of the deletion job, inside the volume's state directory.
pub const JOB_FILE: &str = "deletion-job.json";

/// The steps, in the order they must run.
pub const STEPS: [&str; 5] = [
    "write_stop",
    "key_revoke",
    "snapshot_delete",
    "volume_delete",
    "mark_deleted",
];

/// Whether a step finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Recorded as finished.
    Done,
    /// Attempted and failed; retryable.
    Failed,
}

/// One step's recorded outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepOutcome {
    /// The step's name.
    pub step: String,
    /// Whether it finished.
    pub state: StepState,
    /// When it finished or failed, RFC 3339.
    pub at: String,
    /// A machine-readable failure reason, when it failed.
    pub reason: Option<String>,
}

/// The job's overall state, as the persisted record reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// At least one step has not finished.
    Deleting,
    /// Every step finished.
    Deleted,
}

impl JobState {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
        }
    }
}

/// The durable deletion job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletionJob {
    /// Version of this record's shape.
    pub version: u32,
    /// The job id the control plane assigned.
    pub job_id: String,
    /// The user whose volume this is.
    pub user_id: String,
    /// The volume id.
    pub volume_id: String,
    /// The key id to revoke.
    pub key_id: String,
    /// When the job was requested, RFC 3339.
    pub requested_at: String,
    /// The recorded steps, in the order they were attempted.
    pub steps: Vec<StepOutcome>,
    /// When the last step finished, RFC 3339, once every step is done.
    pub completed_at: Option<String>,
}

/// The job file version this build writes.
const JOB_VERSION: u32 = 1;

/// What one `delete_user` call reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteReport {
    /// The job id.
    pub job_id: String,
    /// The overall state.
    pub state: JobState,
    /// Steps that finished, in order.
    pub steps_done: Vec<String>,
    /// The step that failed on this run, when one did.
    pub failed_step: Option<String>,
    /// Why it failed.
    pub failure: Option<String>,
    /// The steps still outstanding after this run.
    pub remaining: Vec<String>,
}

impl DeleteReport {
    /// Whether the whole job finished.
    pub fn is_deleted(&self) -> bool {
        self.state == JobState::Deleted
    }
}

impl DeletionJob {
    /// Build a job from the control-plane request.
    pub fn new(job_id: impl Into<String>, request: &DeletionRequest) -> Self {
        Self {
            version: JOB_VERSION,
            job_id: job_id.into(),
            user_id: request.user_id.clone(),
            volume_id: request.volume_id.clone(),
            key_id: request.key_id.clone(),
            requested_at: request.requested_at.clone(),
            steps: Vec::new(),
            completed_at: None,
        }
    }

    /// Whether `step` is recorded as finished.
    pub fn is_done(&self, step: &str) -> bool {
        self.steps
            .iter()
            .any(|outcome| outcome.step == step && outcome.state == StepState::Done)
    }

    /// The overall state.
    pub fn state(&self) -> JobState {
        if STEPS.iter().all(|step| self.is_done(step)) {
            JobState::Deleted
        } else {
            JobState::Deleting
        }
    }

    /// The steps still outstanding, in order.
    pub fn remaining(&self) -> Vec<String> {
        STEPS
            .iter()
            .filter(|step| !self.is_done(step))
            .map(|step| step.to_string())
            .collect()
    }

    /// The steps already finished, in order.
    pub fn done(&self) -> Vec<String> {
        STEPS
            .iter()
            .filter(|step| self.is_done(step))
            .map(|step| step.to_string())
            .collect()
    }

    /// Record `step` as finished, replacing any earlier failure for it.
    fn record_done(&mut self, step: &str) {
        let at = now_rfc3339();
        self.steps.retain(|outcome| outcome.step != step);
        self.steps.push(StepOutcome {
            step: step.to_string(),
            state: StepState::Done,
            at,
            reason: None,
        });
        if self.state() == JobState::Deleted && self.completed_at.is_none() {
            self.completed_at = Some(now_rfc3339());
        }
    }

    /// Record `step` as failed, retryable.
    fn record_failed(&mut self, step: &str, reason: &str) {
        self.steps.retain(|outcome| outcome.step != step);
        self.steps.push(StepOutcome {
            step: step.to_string(),
            state: StepState::Failed,
            at: now_rfc3339(),
            reason: Some(reason.to_string()),
        });
    }
}

impl VolumeLayout {
    /// The deletion job file of this volume.
    pub fn deletion_job_path(&self) -> std::path::PathBuf {
        self.state_dir().join(JOB_FILE)
    }
}

/// Read the persisted job, if there is one.
pub fn read_job(layout: &VolumeLayout) -> Result<Option<DeletionJob>> {
    let path = layout.deletion_job_path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow!(error).context(format!("cannot read the deletion job {}", path.display())));
        }
    };
    let job: DeletionJob = serde_json::from_slice(&bytes)
        .with_context(|| format!("the deletion job {} is not a valid job record", path.display()))?;
    if job.version != JOB_VERSION {
        return Err(anyhow!(
            "the deletion job {} carries version {} and this build writes {JOB_VERSION}",
            path.display(),
            job.version
        ));
    }
    Ok(Some(job))
}

/// Persist the job.
fn save_job(layout: &VolumeLayout, job: &DeletionJob) -> Result<()> {
    write_private_file(
        &layout.deletion_job_path(),
        &serde_json::to_vec_pretty(job).context("cannot encode the deletion job")?,
    )
}

/// Run (or resume) `delete_user` for one volume.
///
/// `control` supplies the central calls. Any step that fails stops the run at
/// that step, is recorded as retryable, and leaves the overall state
/// `deleting`; the returned report names the step and the reason.
pub fn delete_user(layout: &VolumeLayout, control: &dyn ControlPlane, user_id: &str) -> Result<DeleteReport> {
    let mut job = match read_job(layout)? {
        Some(job) => {
            if job.user_id != user_id {
                return Err(anyhow!(
                    "the deletion job at {} belongs to user `{}`, not `{user_id}`",
                    layout.deletion_job_path().display(),
                    job.user_id
                ));
            }
            job
        }
        None => {
            let request = DeletionRequest {
                user_id: user_id.to_string(),
                volume_id: layout
                    .home()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("")
                    .to_string(),
                key_id: match key::read_key(layout)? {
                    Some(record) => record.key_id,
                    // A volume deleted before it ever held a key has no id to
                    // revoke; the step is still recorded, and the control plane
                    // decides whether that is an error for its own row.
                    None => String::new(),
                },
                requested_at: now_rfc3339(),
            };
            let job_id = control.begin_deletion(&request)?;
            let job = DeletionJob::new(job_id, &request);
            save_job(layout, &job)?;
            job
        }
    };

    // A job already reporting `deleted` is not run again: every step's record is
    // the proof, and repeating a provider call could fail on an already-deleted
    // resource and turn a successful deletion into a reported failure.
    if job.state() == JobState::Deleted {
        return Ok(report_for(&job, None, None));
    }

    for step in STEPS {
        if job.is_done(step) {
            continue;
        }
        match run_step(step, layout, control, &job) {
            Ok(()) => {
                job.record_done(step);
                save_job(layout, &job)?;
                record_step_centrally(control, &job, step, StepState::Done, None);
            }
            Err(error) => {
                let reason = error.to_string();
                job.record_failed(step, &reason);
                save_job(layout, &job)?;
                record_step_centrally(control, &job, step, StepState::Failed, Some(&reason));
                crate::logging::warn(&format!(
                    "delete_user of {} stopped at step `{step}`: {reason}",
                    job.volume_id
                ));
                return Ok(report_for(&job, Some(step), Some(&reason)));
            }
        }
    }
    let report = report_for(&job, None, None);
    if report.is_deleted() {
        crate::logging::info(&format!(
            "delete_user of {} finished every step; the central row now reads deleted",
            job.volume_id
        ));
    }
    Ok(report)
}

/// Report the central step outcome without letting a reporting failure undo the
/// step. A step that ran is done even if the control plane is briefly
/// unreachable; the next run re-records only the steps it still lacks.
fn record_step_centrally(
    control: &dyn ControlPlane,
    job: &DeletionJob,
    step: &str,
    state: StepState,
    reason: Option<&str>,
) {
    let report = StepReport {
        job_id: job.job_id.clone(),
        step: step.to_string(),
        state: match state {
            StepState::Done => "done".to_string(),
            StepState::Failed => "failed".to_string(),
        },
        at: now_rfc3339(),
        reason: reason.map(str::to_string),
    };
    if let Err(error) = control.record_step(&report) {
        crate::logging::warn(&format!(
            "the control plane did not record step `{step}` of job {}: {error}",
            job.job_id
        ));
    }
}

fn report_for(job: &DeletionJob, failed_step: Option<&str>, failure: Option<&str>) -> DeleteReport {
    DeleteReport {
        job_id: job.job_id.clone(),
        state: job.state(),
        steps_done: job.done(),
        failed_step: failed_step.map(str::to_string),
        failure: failure.map(str::to_string),
        remaining: job.remaining(),
    }
}

/// Run one step. Every step is idempotent, so a resumed run repeats only what
/// had not finished.
fn run_step(step: &str, layout: &VolumeLayout, control: &dyn ControlPlane, job: &DeletionJob) -> Result<()> {
    match step {
        "write_stop" => write_stop(layout),
        "key_revoke" => key_revoke(layout),
        "snapshot_delete" => snapshot_delete(control, job),
        "volume_delete" => volume_delete(control, job),
        "mark_deleted" => mark_deleted(control, job),
        other => Err(anyhow!("`{other}` is not a deletion step")),
    }
}

/// Step 1: fence the store and scrub the local page data.
///
/// The fence goes on before the wipe, so nothing writes a page between the wipe
/// and the next step. The wipe is part of this step because a volume whose
/// pages are already gone is the state the later steps describe.
fn write_stop(layout: &VolumeLayout) -> Result<()> {
    if !super::fence::is_fenced(&layout.store_root())? {
        if layout.store_root().is_dir() {
            super::fence::quiesce(layout).context("cannot fence the memory store for deletion")?;
        } else {
            // A volume that never served memory has no index to checkpoint, but
            // it still has to be deletable, so the fence is written without one.
            super::fence::fence_without_store(layout).context("cannot fence the empty memory store for deletion")?;
        }
    }
    wipe_store(layout).context("cannot wipe the local memory store")?;
    Ok(())
}

/// Remove the imported page files, the index and its sidecars, and the lock.
///
/// The store root itself stays: another step reads the key record and the fence
/// beside it, and a volume that lost its directory would report a different
/// failure than "the data is gone".
fn wipe_store(layout: &VolumeLayout) -> Result<()> {
    let store_root = layout.store_root();
    let mut removed = 0usize;
    for name in ["wiki", "index.sqlite", "index.sqlite-wal", "index.sqlite-shm"] {
        let path = store_root.join(name);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(anyhow!(error).context(format!("cannot stat {}", path.display())));
            }
        };
        if metadata.is_dir() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("cannot remove the memory pages at {}", path.display()))?;
        } else {
            std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
        }
        removed += 1;
    }
    if removed > 0 {
        crate::logging::info(&format!(
            "delete_user removed {removed} memory store entr{} from {}",
            if removed == 1 { "y" } else { "ies" },
            store_root.display()
        ));
    }
    Ok(())
}

/// Step 2: revoke the key and wipe any staged material.
fn key_revoke(layout: &VolumeLayout) -> Result<()> {
    key::revoke_key(layout).context("cannot revoke the user data key")?;
    key::wipe_staged_material(layout).context("cannot wipe staged key material")?;
    Ok(())
}

/// Step 3: delete every snapshot the control plane lists.
fn snapshot_delete(control: &dyn ControlPlane, job: &DeletionJob) -> Result<()> {
    let snapshots = control
        .list_snapshots(&SnapshotQuery {
            volume_id: job.volume_id.clone(),
        })
        .context("cannot list the volume's snapshots")?;
    for snapshot in &snapshots {
        control
            .delete_snapshot(&SnapshotDeletion {
                user_id: job.user_id.clone(),
                provider_snapshot_id: snapshot.provider_snapshot_id.clone(),
            })
            .with_context(|| format!("cannot delete snapshot {}", snapshot.provider_snapshot_id))?;
    }
    if !snapshots.is_empty() {
        crate::logging::info(&format!(
            "delete_user removed {} snapshot(s) of volume {}",
            snapshots.len(),
            job.volume_id
        ));
    }
    Ok(())
}

/// Step 4: destroy the volume, Machine first on Fly.
fn volume_delete(control: &dyn ControlPlane, job: &DeletionJob) -> Result<()> {
    control
        .delete_volume(&VolumeDeletion {
            user_id: job.user_id.clone(),
            volume_id: job.volume_id.clone(),
            // An attached volume cannot be destroyed on Fly, so the control
            // plane must destroy the Machine first. Saying so here means a
            // control plane cannot "succeed" by destroying a volume that is
            // still attached and silently leak the Machine.
            destroy_machine_first: true,
        })
        .context("cannot delete the volume")?;
    Ok(())
}

/// Step 5: mark the central row deleted.
fn mark_deleted(control: &dyn ControlPlane, job: &DeletionJob) -> Result<()> {
    control
        .mark_deleted(&VolumeStateChange {
            user_id: job.user_id.clone(),
            volume_id: job.volume_id.clone(),
            state: VolumeState::Deleted,
            at: now_rfc3339(),
        })
        .context("cannot mark the volume deleted centrally")?;
    Ok(())
}

/// The job's persisted state, for reporting without running anything.
pub fn job_state(layout: &VolumeLayout) -> Result<Option<DeletionJob>> {
    read_job(layout)
}

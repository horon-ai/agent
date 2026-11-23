//! The central metadata client, against the draft horon-ai/server#28 contract.
//!
//! The control plane keeps one row per user volume, its snapshots, its exports
//! and its deletion jobs. This module is the agent's side of that API and exists
//! to enforce one property the issue states as a requirement: **the central
//! metadata carries no memory content.** Every request type below is built out
//! of identifiers, states, timestamps, key ids, snapshot ids and object keys.
//! There is no body, text, title, tag or path field, and a test plants a marker
//! body in the store, drives every request through a loopback mock server and
//! asserts the marker never appears in a request byte.
//!
//! # The contract is a draft
//!
//! server#28 has frozen no endpoint and no schema. The shapes here follow the
//! draft tables in `.hyper/research/16.multitenant-hosting.md` (5.5) closely
//! enough to implement the deletion order, and everything that depends on the
//! final contract is behind [`ControlPlane`] so the control plane can replace
//! HTTP with whatever it finally ships:
//!
//! | Method | Request body | Draft source |
//! | --- | --- | --- |
//! | `begin_deletion` | `DeletionRequest` | `deletion_jobs` insert |
//! | `record_step` | `StepReport` | `deletion_jobs.steps` |
//! | `record_snapshot` | `SnapshotRecord` | `volume_snapshots` insert |
//! | `list_snapshots` | `SnapshotQuery` | `volume_snapshots` select |
//! | `delete_snapshot` | `SnapshotDeletion` | `volume_snapshots` delete |
//! | `delete_volume` | `VolumeDeletion` | `user_volumes` delete |
//! | `mark_deleted` | `VolumeStateChange` | `user_volumes.state = 'deleted'` |
//!
//! # Authentication
//!
//! A bearer token read from an environment variable or a file named in the
//! configuration. The token is a `Secret` wrapper: its `Debug` is redacted, it
//! is never written to a log line or an error, and it is dropped without being
//! copied into a request body (it travels in the `Authorization` header only).

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// The control-plane root a client calls.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8080";

/// How long one control-plane call may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A token that never prints itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap `token`.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Read the token from `variable`'s environment, then from `file`.
    ///
    /// The file's trailing newline is stripped, because a token file written by
    /// `echo` carries one and a header with a newline is rejected by every
    /// server. A blank result is an error: an empty `Authorization` header turns
    /// an authenticated call into an unauthenticated one.
    pub fn resolve(variable: Option<&str>, file: Option<&PathBuf>) -> Result<Self> {
        if let Some(variable) = variable
            && let Ok(value) = std::env::var(variable)
        {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(Self::new(trimmed));
            }
        }
        if let Some(file) = file {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("cannot read the control-plane token file {}", file.display()))?;
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Ok(Self::new(trimmed));
            }
            return Err(anyhow!("the control-plane token file {} is empty", file.display()));
        }
        Err(anyhow!(
            "no control-plane token: set the token variable or point the token file at one"
        ))
    }

    /// The header value, as `Bearer <token>`.
    pub fn authorization(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

/// A user volume's lifecycle state in the central row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeState {
    /// Serving.
    Active,
    /// A deletion job is running.
    Deleting,
    /// Every step finished; the volume, its key and its snapshots are gone.
    Deleted,
}

impl VolumeState {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
        }
    }
}

/// One snapshot's metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRecord {
    /// The provider's snapshot id.
    pub provider_snapshot_id: String,
    /// The volume it was taken from.
    pub volume_id: String,
    /// When it was taken, RFC 3339.
    pub created_at: String,
    /// When retention expires, RFC 3339.
    pub expires_at: String,
}

/// The request that opens a deletion job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletionRequest {
    /// The user whose volume is being deleted.
    pub user_id: String,
    /// The volume id.
    pub volume_id: String,
    /// The key id to revoke.
    pub key_id: String,
    /// When the job was requested, RFC 3339.
    pub requested_at: String,
}

/// One step's outcome inside a deletion job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepReport {
    /// The job the step belongs to.
    pub job_id: String,
    /// The step's name.
    pub step: String,
    /// `done` or `failed`.
    pub state: String,
    /// When it finished, RFC 3339.
    pub at: String,
    /// A machine-readable failure reason, when it failed.
    pub reason: Option<String>,
}

/// Ask which snapshots a volume still has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotQuery {
    /// The volume to list.
    pub volume_id: String,
}

/// Delete one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotDeletion {
    /// The user the volume belongs to.
    pub user_id: String,
    /// The provider's snapshot id.
    pub provider_snapshot_id: String,
}

/// Destroy one volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeDeletion {
    /// The user the volume belongs to.
    pub user_id: String,
    /// The volume id.
    pub volume_id: String,
    /// Whether a Machine must be destroyed first (Fly: always).
    pub destroy_machine_first: bool,
}

/// Move a volume's central row to another state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeStateChange {
    /// The user the volume belongs to.
    pub user_id: String,
    /// The volume id.
    pub volume_id: String,
    /// The state to write.
    pub state: VolumeState,
    /// When the change happened, RFC 3339.
    pub at: String,
}

/// The central operations a deletion job needs.
///
/// Steps the agent owns (the write fence, key revocation, the local wipe) never
/// cross this interface; only the calls that need the control plane's database
/// or the provider's API do. A test implementation is a plain `struct`, so the
/// job's ordering and retry semantics are testable without a server.
pub trait ControlPlane {
    /// Open a deletion job and return its id.
    fn begin_deletion(&self, request: &DeletionRequest) -> Result<String>;
    /// Record one step's outcome.
    fn record_step(&self, report: &StepReport) -> Result<()>;
    /// Record a snapshot the provider took.
    fn record_snapshot(&self, record: &SnapshotRecord) -> Result<()>;
    /// Every snapshot the volume still has.
    fn list_snapshots(&self, query: &SnapshotQuery) -> Result<Vec<SnapshotRecord>>;
    /// Delete one snapshot.
    fn delete_snapshot(&self, deletion: &SnapshotDeletion) -> Result<()>;
    /// Destroy one volume.
    fn delete_volume(&self, deletion: &VolumeDeletion) -> Result<()>;
    /// Mark the volume's central row `deleted`.
    fn mark_deleted(&self, change: &VolumeStateChange) -> Result<()>;
}

/// What the client says it is doing with one request, for the mock server's
/// dispatch and for an operator reading a request log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Open a deletion job.
    BeginDeletion,
    /// Record a step.
    RecordStep,
    /// Record a snapshot.
    RecordSnapshot,
    /// List snapshots.
    ListSnapshots,
    /// Delete a snapshot.
    DeleteSnapshot,
    /// Destroy a volume.
    DeleteVolume,
    /// Mark deleted.
    MarkDeleted,
}

impl Operation {
    /// The HTTP path this operation posts to.
    pub const fn path(self) -> &'static str {
        match self {
            Self::BeginDeletion => "/internal/v1/deletion-jobs",
            Self::RecordStep => "/internal/v1/deletion-jobs/steps",
            Self::RecordSnapshot => "/internal/v1/volume-snapshots",
            Self::ListSnapshots => "/internal/v1/volume-snapshots/query",
            Self::DeleteSnapshot => "/internal/v1/volume-snapshots/delete",
            Self::DeleteVolume => "/internal/v1/user-volumes/delete",
            Self::MarkDeleted => "/internal/v1/user-volumes/state",
        }
    }
}

/// What the HTTP client reads back from a response.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResponseBody {
    /// The job id `begin_deletion` returned.
    pub job_id: Option<String>,
    /// The snapshots `list_snapshots` returned.
    pub snapshots: Vec<SnapshotRecord>,
}

/// An HTTP implementation of [`ControlPlane`].
///
/// Only the draft paths in [`Operation`] are called, the body is one of the
/// typed requests above, and every body is JSON the type system built, so no
/// caller can smuggle a page body into a request.
pub struct HttpControlPlane {
    client: reqwest::blocking::Client,
    endpoint: String,
    token: Secret,
}

impl HttpControlPlane {
    /// Build a client for `endpoint`, authenticating with `token`.
    ///
    /// The TLS configuration is the platform's, the same one every other
    /// `reqwest` client in this tree uses, so a client here cannot silently
    /// trust nothing and cannot silently trust everything.
    pub fn new(endpoint: impl Into<String>, token: Secret) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .use_preconfigured_tls(horon_agent_platform::tls_client_config().as_ref().clone())
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("cannot build the control-plane HTTP client")?;
        Ok(Self {
            client,
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token,
        })
    }

    /// The endpoint this client calls.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// POST one typed body and parse a [`ResponseBody`].
    fn post<T: Serialize>(&self, operation: Operation, body: &T) -> Result<ResponseBody> {
        let url = format!("{}{}", self.endpoint, operation.path());
        let response = self
            .client
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, self.token.authorization())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .with_context(|| format!("cannot reach the control plane at {url}"))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .with_context(|| format!("cannot read the control-plane response from {url}"))?;
        if !status.is_success() {
            // The body of a failed call may echo a validation message; it is
            // truncated and never carries a request field this client did not
            // send, but it can carry a request id, so it goes to the operator
            // rather than to a metric label.
            return Err(anyhow!(
                "the control plane answered {} for {url}: {}",
                status.as_u16(),
                String::from_utf8_lossy(&bytes).chars().take(200).collect::<String>()
            ));
        }
        if bytes.is_empty() {
            return Ok(ResponseBody::default());
        }
        serde_json::from_slice(&bytes).with_context(|| format!("the control-plane response from {url} is not JSON"))
    }
}

impl ControlPlane for HttpControlPlane {
    fn begin_deletion(&self, request: &DeletionRequest) -> Result<String> {
        let body = self.post(Operation::BeginDeletion, request)?;
        body.job_id
            .ok_or_else(|| anyhow!("the control plane answered begin_deletion without a job id"))
    }

    fn record_step(&self, report: &StepReport) -> Result<()> {
        self.post(Operation::RecordStep, report)?;
        Ok(())
    }

    fn record_snapshot(&self, record: &SnapshotRecord) -> Result<()> {
        self.post(Operation::RecordSnapshot, record)?;
        Ok(())
    }

    fn list_snapshots(&self, query: &SnapshotQuery) -> Result<Vec<SnapshotRecord>> {
        let body = self.post(Operation::ListSnapshots, query)?;
        Ok(body.snapshots)
    }

    fn delete_snapshot(&self, deletion: &SnapshotDeletion) -> Result<()> {
        self.post(Operation::DeleteSnapshot, deletion)?;
        Ok(())
    }

    fn delete_volume(&self, deletion: &VolumeDeletion) -> Result<()> {
        self.post(Operation::DeleteVolume, deletion)?;
        Ok(())
    }

    fn mark_deleted(&self, change: &VolumeStateChange) -> Result<()> {
        self.post(Operation::MarkDeleted, change)?;
        Ok(())
    }
}

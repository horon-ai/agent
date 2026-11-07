pub use crate::background::BackgroundCleanupResult;
use crate::message::ToolCall;
use crate::todo::TodoItem;
pub use horon_agent_background_types::{
    BackgroundTaskCompleted, BackgroundTaskProgress, BackgroundTaskProgressEvent, BackgroundTaskProgressKind,
    BackgroundTaskProgressSource, BackgroundTaskStalled, BackgroundTaskStatus,
};
pub use horon_agent_batch_types::{BatchProgress, BatchSubcallProgress, BatchSubcallState};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Completed,
    Error,
}

impl ToolStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolStatus::Running => "running",
            ToolStatus::Completed => "completed",
            ToolStatus::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolEvent {
    pub session_id: String,
    pub message_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub status: ToolStatus,
    /// Agent-provided human-readable intent for this tool call.
    pub intent: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TodoEvent {
    pub session_id: String,
    pub todos: Vec<TodoItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSummaryState {
    pub status: String,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSummary {
    pub id: String,
    pub tool: String,
    pub state: ToolSummaryState,
}

/// Status update from a subagent (used by Task tool)
#[derive(Clone, Debug)]
pub struct SubagentStatus {
    pub session_id: String,
    pub status: String, // e.g., "calling API", "running grep", "streaming"
    pub model: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ManualToolCompleted {
    pub session_id: String,
    pub tool_call: ToolCall,
    pub output: String,
    pub is_error: bool,
    pub title: Option<String>,
    pub duration_ms: u64,
}

/// Type of file operation for swarm awareness
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileOp {
    Read,
    Write,
    Edit,
}

impl FileOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileOp::Read => "read",
            FileOp::Write => "wrote",
            FileOp::Edit => "edited",
        }
    }

    pub fn is_modification(&self) -> bool {
        matches!(self, FileOp::Write | FileOp::Edit)
    }
}

/// File touch event for swarm coordination
#[derive(Clone, Debug)]
pub struct FileTouch {
    pub session_id: String,
    pub path: PathBuf,
    pub op: FileOp,
    /// Agent-provided intent for the tool call that touched this file.
    pub intent: Option<String>,
    /// Human-readable summary like "edited lines 45-60" or "read 200 lines"
    pub summary: Option<String>,
    /// Optional compact preview of what changed. Keep this short and already truncated.
    pub detail: Option<String>,
}

/// Streaming output tail for a swarm worker session, used to render the inline
/// swarm gallery's live viewports. The text is a short, already-truncated tail
/// of the worker's in-progress assistant output (not the full transcript).
#[derive(Clone, Debug)]
pub struct SwarmOutputTail {
    /// Session id of the worker producing output.
    pub session_id: String,
    /// Recent output tail (capped to a handful of lines by the producer).
    pub tail: String,
}

#[derive(Clone, Debug)]
pub struct LoginCompleted {
    pub provider: String,
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiActivityKind {
    Auth,
    Catalog,
    Background,
}

impl UiActivityKind {
    pub fn scope(&self) -> &'static str {
        match self {
            Self::Auth => "auth_activity",
            Self::Catalog => "catalog_activity",
            Self::Background => "background_activity",
        }
    }
}

#[derive(Clone, Debug)]
pub struct UiActivity {
    pub session_id: Option<String>,
    pub kind: UiActivityKind,
    pub message: String,
    pub status_notice: Option<String>,
}

impl UiActivity {
    pub fn new(
        session_id: Option<String>,
        kind: UiActivityKind,
        message: impl Into<String>,
        status_notice: Option<impl Into<String>>,
    ) -> Self {
        Self {
            session_id,
            kind,
            message: message.into(),
            status_notice: status_notice.map(Into::into),
        }
    }

    pub fn auth(
        session_id: Option<String>,
        message: impl Into<String>,
        status_notice: Option<impl Into<String>>,
    ) -> Self {
        Self::new(session_id, UiActivityKind::Auth, message, status_notice)
    }

    pub fn catalog(
        session_id: Option<String>,
        message: impl Into<String>,
        status_notice: Option<impl Into<String>>,
    ) -> Self {
        Self::new(session_id, UiActivityKind::Catalog, message, status_notice)
    }

    pub fn background(
        session_id: Option<String>,
        message: impl Into<String>,
        status_notice: Option<impl Into<String>>,
    ) -> Self {
        Self::new(session_id, UiActivityKind::Background, message, status_notice)
    }

    pub fn is_visible_to_session(&self, session_id: &str) -> bool {
        match self.session_id.as_deref() {
            Some(target) => target == session_id,
            None => true,
        }
    }
}

/// A backgrounded `swarm await_members` watcher reached a terminal result
/// (members satisfied the target status, or the wait timed out).
///
/// Carries the already-rendered notification/wake payloads so the server's bus
/// monitor can deliver completion through the same notify/wake path as
/// background tasks, without re-coupling `horon-agent-base` to swarm internals.
#[derive(Clone, Debug)]
pub struct SwarmAwaitCompleted {
    /// Session that requested the await (delivery target).
    pub session_id: String,
    /// Whether the await completed successfully (false = timed out).
    pub completed: bool,
    /// Human-readable summary line (e.g. "All 2 members are done: fox, wolf").
    pub summary: String,
    /// Rich, already-formatted notification body (member statuses + reports).
    pub notification: String,
    /// Surface a notification card to attached clients.
    pub notify: bool,
    /// Wake an idle requesting agent (or soft-interrupt it when busy).
    pub wake: bool,
}

/// Result of a `/productivity` report generation run.
///
/// Carries already-rendered outputs (markdown + PNG bytes) so the TUI layer can
/// display/copy them without depending on the productivity crate's types here in
/// `horon-agent-base`.
#[derive(Clone, Debug)]
pub struct ProductivityReportReady {
    pub session_id: String,
    pub result: std::result::Result<ProductivityReportPayload, String>,
}

#[derive(Clone, Debug)]
pub struct ProductivityReportPayload {
    /// Rendered Markdown report for the transcript.
    pub markdown: String,
    /// PNG dashboard bytes.
    pub png: Vec<u8>,
    /// Filesystem path the PNG was saved to.
    pub png_path: std::path::PathBuf,
}

#[derive(Clone, Debug)]
pub enum UpdateStatus {
    Checking,
    Available {
        current: String,
        latest: String,
    },
    Downloading {
        version: String,
        /// Bytes downloaded so far (0 before the transfer starts).
        downloaded: u64,
        /// Total asset size when known, for progress-bar rendering.
        total: Option<u64>,
    },
    Installing {
        version: String,
    },
    Installed {
        version: String,
    },
    UpToDate,
    /// Automatic checks are not applicable, e.g. a local untracked checkout.
    Skipped {
        reason: String,
    },
    Error(String),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientMaintenanceAction {
    Update,
    Rebuild,
}

impl ClientMaintenanceAction {
    pub fn noun(&self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Rebuild => "rebuild",
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::Update => "Update",
            Self::Rebuild => "Rebuild",
        }
    }
}

#[derive(Clone, Debug)]
pub enum SessionUpdateStatus {
    Status {
        session_id: String,
        action: ClientMaintenanceAction,
        message: String,
    },
    NoUpdate {
        session_id: String,
        current: String,
    },
    ReadyToReload {
        session_id: String,
        action: ClientMaintenanceAction,
        version: String,
    },
    Error {
        session_id: String,
        action: ClientMaintenanceAction,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub enum BusEvent {
    ToolUpdated(ToolEvent),
    TodoUpdated(TodoEvent),
    SubagentStatus(SubagentStatus),
    ManualToolCompleted(ManualToolCompleted),
    BatchProgress(BatchProgress),
    /// File was touched by an agent (for swarm conflict detection)
    FileTouch(FileTouch),
    /// Streaming output tail from a swarm worker, for inline gallery viewports.
    SwarmOutputTail(SwarmOutputTail),
    /// Background task completed
    BackgroundTaskCompleted(BackgroundTaskCompleted),
    /// Background task reported progress
    BackgroundTaskProgress(BackgroundTaskProgressEvent),
    /// Background task stall watchdog fired: no output/progress for its window
    BackgroundTaskStalled(BackgroundTaskStalled),
    /// The background artifact budget sweep reclaimed files, or found the
    /// protected set alone over budget.
    BackgroundArtifactsReclaimed(BackgroundCleanupResult),
    /// A backgrounded `swarm await_members` watcher reached a terminal result.
    SwarmAwaitCompleted(SwarmAwaitCompleted),
    /// Usage report fetched from providers
    UsageReport(Vec<horon_agent_usage_types::ProviderUsage>),
    /// Progressive usage report update while providers are still loading
    UsageReportProgress(horon_agent_usage_types::ProviderUsageProgress),
    /// OAuth/login flow completed in the background
    LoginCompleted(LoginCompleted),
    /// UI-visible runtime activity from auth/catalog/background operations.
    UiActivity(UiActivity),
    /// Update check status from background thread
    UpdateStatus(UpdateStatus),
    /// Interactive client update status for a specific session
    SessionUpdateStatus(SessionUpdateStatus),
    /// Background compaction task finished (check_and_apply should be called)
    CompactionFinished,
    /// Provider's available models list may have changed
    ModelsUpdated,
    /// A single route's usage changed. Carries its snapshot so busy agents do
    /// not prevent clients from refreshing cached picker metadata.
    ModelUsageUpdated(crate::provider::ModelRoute),
    /// Synchronous provider activation after a login/import has completed, so
    /// the model picker can stop hiding the stale pre-auth catalog.
    AuthCatalogRefreshReady,
    /// A background provider setup task selected a model for this session.
    ProviderModelActivated {
        session_id: String,
        model: String,
        provider_key: Option<String>,
        message: String,
        open_picker: bool,
    },
    /// Deferred Mermaid rendering completed and cached content may now be visible
    MermaidRenderCompleted,
    /// The TUI's background git-status probe produced a value that differs
    /// from the cached one, so the branch/dirty/commit chrome needs a repaint
    /// now rather than at the next unrelated redraw.
    GitInfoRefreshed,
    /// Productivity report finished generating off the UI thread
    ProductivityReportReady(ProductivityReportReady),
}

pub struct Bus {
    sender: broadcast::Sender<BusEvent>,
    /// Debounce state for [`Bus::publish_models_updated`]. Per-instance (not a
    /// global static) so tests can exercise coalescing on a private bus
    /// without racing other tests that publish to the global bus.
    models_updated_state: std::sync::Arc<Mutex<ModelsUpdatedPublishState>>,
}

const MODELS_UPDATED_DEBOUNCE: Duration = Duration::from_millis(750);

fn latest_update_status() -> &'static Mutex<Option<UpdateStatus>> {
    static STATE: OnceLock<Mutex<Option<UpdateStatus>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

#[derive(Default)]
struct ModelsUpdatedPublishState {
    last_published_at: Option<Instant>,
    publish_pending: bool,
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_models_updated_publish_state_for_tests() {
    let mut state = Bus::global()
        .models_updated_state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *state = ModelsUpdatedPublishState::default();
}

impl Bus {
    pub fn global() -> &'static Bus {
        static INSTANCE: OnceLock<Bus> = OnceLock::new();
        INSTANCE.get_or_init(Bus::new)
    }

    /// A standalone bus with its own subscribers and debounce state. Used by
    /// tests; production code shares [`Bus::global`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_isolated_for_tests() -> std::sync::Arc<Bus> {
        std::sync::Arc::new(Bus::new())
    }

    fn new() -> Bus {
        let (sender, _) = broadcast::channel(256);
        Bus {
            sender,
            models_updated_state: std::sync::Arc::new(Mutex::new(ModelsUpdatedPublishState::default())),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BusEvent> {
        self.sender.subscribe()
    }

    pub fn publish(&self, event: BusEvent) {
        if let BusEvent::UpdateStatus(status) = &event {
            let mut latest = latest_update_status()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *latest = Some(status.clone());
        }
        let _ = self.sender.send(event);
    }

    pub fn latest_update_status(&self) -> Option<UpdateStatus> {
        latest_update_status()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn publish_models_updated(&self) {
        // A models-updated publish means some provider catalog changed
        // out-of-band. Invalidate memoized route catalogs so the next render
        // rebuilds from the new cache instead of serving a stale memo.
        crate::provider::catalog_scheduler::bump_catalog_generation();

        let delay = {
            let now = Instant::now();
            let mut state = self
                .models_updated_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match state.last_published_at {
                None => {
                    state.last_published_at = Some(now);
                    None
                }
                Some(last) => {
                    let elapsed = now.saturating_duration_since(last);
                    if elapsed >= MODELS_UPDATED_DEBOUNCE {
                        state.last_published_at = Some(now);
                        None
                    } else if state.publish_pending {
                        return;
                    } else {
                        state.publish_pending = true;
                        Some(MODELS_UPDATED_DEBOUNCE - elapsed)
                    }
                }
            }
        };

        if let Some(delay) = delay {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                let mut state = self
                    .models_updated_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.publish_pending = false;
                state.last_published_at = Some(Instant::now());
                drop(state);
                self.publish(BusEvent::ModelsUpdated);
                return;
            };
            // The delayed publish must flow through *this* bus instance (its
            // sender and its debounce state), not Bus::global(): an isolated
            // test bus would otherwise leak its coalesced event onto the
            // global bus and clear the wrong pending flag.
            let state = std::sync::Arc::clone(&self.models_updated_state);
            let sender = self.sender.clone();
            handle.spawn(async move {
                tokio::time::sleep(delay).await;
                let mut state = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                state.publish_pending = false;
                state.last_published_at = Some(Instant::now());
                drop(state);
                let _ = sender.send(BusEvent::ModelsUpdated);
            });
            return;
        }

        self.publish(BusEvent::ModelsUpdated);
    }
}

#[cfg(test)]
mod tests {
    use super::{Bus, BusEvent};
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn models_updated_publishes_are_coalesced() {
        // An isolated bus: subscribing to Bus::global() here used to race
        // other tests that publish ModelsUpdated (or any event) to the global
        // bus, making the "exactly two events" assertions flaky.
        let bus = Bus::new_isolated_for_tests();
        let mut rx = bus.subscribe();

        bus.publish_models_updated();
        bus.publish_models_updated();
        bus.publish_models_updated();

        match timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(BusEvent::ModelsUpdated)) => {}
            other => panic!("expected immediate ModelsUpdated event, got {other:?}"),
        }

        match timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(BusEvent::ModelsUpdated)) => {}
            other => panic!("expected coalesced delayed ModelsUpdated event, got {other:?}"),
        }

        assert!(timeout(Duration::from_millis(300), rx.recv()).await.is_err());
    }
}

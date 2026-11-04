use super::{MIN_WAKE_INTERVAL, WAKE_TOKEN_WINDOW, WAKE_TOKENS_PER_MINUTE};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The clock the wake pacer reads.
///
/// Injected so a rate-limit test advances time instead of sleeping through it.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Wall time.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A clock a test moves by hand.
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<Instant>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            now: Mutex::new(Instant::now()),
        }
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *now += by;
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Whether a change may become a wake now, or has to wait and coalesce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeDecision {
    Emit,
    Coalesce,
}

/// Two independent rate limits, both read from the injected clock.
///
/// * one wake per monitor per [`MIN_WAKE_INTERVAL`], so one busy file cannot fill
///   the conversation;
/// * [`WAKE_TOKENS_PER_MINUTE`] wakes per session per rolling
///   [`WAKE_TOKEN_WINDOW`], so many monitors cannot either.
///
/// A session's first change finds an empty window and spends a token at once;
/// everything that does not fit coalesces into one later wake.
#[derive(Debug)]
pub struct WakePacer {
    min_interval: Duration,
    tokens_per_window: usize,
    window: Duration,
    emissions: VecDeque<Instant>,
    last_per_monitor: HashMap<String, Instant>,
}

impl Default for WakePacer {
    fn default() -> Self {
        Self::new(MIN_WAKE_INTERVAL, WAKE_TOKENS_PER_MINUTE, WAKE_TOKEN_WINDOW)
    }
}

impl WakePacer {
    pub fn new(min_interval: Duration, tokens_per_window: usize, window: Duration) -> Self {
        Self {
            min_interval,
            tokens_per_window,
            window,
            emissions: VecDeque::new(),
            last_per_monitor: HashMap::new(),
        }
    }

    /// Whether a wake may go out now. Does not spend a token; a caller that
    /// delivers the wake calls [`WakePacer::record`].
    pub fn decide(&mut self, monitor_id: &str, now: Instant) -> WakeDecision {
        self.prune(now);
        if let Some(last) = self.last_per_monitor.get(monitor_id)
            && now.saturating_duration_since(*last) < self.min_interval
        {
            return WakeDecision::Coalesce;
        }
        if self.emissions.len() >= self.tokens_per_window {
            return WakeDecision::Coalesce;
        }
        WakeDecision::Emit
    }

    pub fn record(&mut self, monitor_id: &str, now: Instant) {
        self.prune(now);
        self.emissions.push_back(now);
        self.last_per_monitor.insert(monitor_id.to_string(), now);
    }

    /// When the oldest spent token frees up, if the window is full.
    pub fn next_token_at(&mut self, now: Instant) -> Option<Instant> {
        self.prune(now);
        if self.emissions.len() < self.tokens_per_window {
            return None;
        }
        self.emissions.front().map(|oldest| *oldest + self.window)
    }

    pub fn spent(&self) -> usize {
        self.emissions.len()
    }

    fn prune(&mut self, now: Instant) {
        while let Some(front) = self.emissions.front() {
            if now.saturating_duration_since(*front) >= self.window {
                self.emissions.pop_front();
            } else {
                break;
            }
        }
    }
}

/// Why a wake could not be handed to the input queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeError {
    /// The queue is full right now; the pending wake stays pending.
    Busy,
    /// The queue refused the wake for another reason.
    Failed(String),
}

/// Where a delivered wake goes.
///
/// The wake id doubles as the enqueued input's source id, which is what makes a
/// redelivery after a restart a dedup instead of a second message.
pub trait WakeSink: Send + Sync {
    fn enqueue(&self, wake_id: &str, summary: &str) -> Result<String, WakeError>;

    fn is_queued(&self, wake_id: &str) -> Result<bool, WakeError>;
}

/// The wake sink over the durable input journal.
///
/// A wake must reach the conversation, and the journal is what a running
/// session reads: `restore_persisted_soft_interrupts` recovers `pending` from
/// it on every turn. Writing only the legacy JSON file would be a dead end,
/// because that file is imported exactly once as a baseline and never read
/// again — the wake would be recorded and never delivered.
///
/// The wake id is the accepted input's `client_id`, so a redelivery after a
/// restart is a dedup rather than a second message.
pub struct SoftInterruptWakeSink {
    session_id: String,
    /// Explicit journal root. `None` resolves the process HOME, which is what a
    /// running daemon wants; a test that opens the monitor journal under its own
    /// directory passes it here so both sides agree on one journal.
    root: Option<std::path::PathBuf>,
}

impl SoftInterruptWakeSink {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            root: None,
        }
    }

    /// Open the same journal root the monitor runtime was given.
    pub fn with_root(root: impl Into<std::path::PathBuf>, session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            root: Some(root.into()),
        }
    }

    fn open_store(&self) -> Result<crate::durable_input::DurableInputStore, WakeError> {
        let opened = match &self.root {
            Some(root) => crate::durable_input::DurableInputStore::open_in(root, &self.session_id),
            None => crate::durable_input::DurableInputStore::open(&self.session_id),
        };
        opened.map_err(|error| WakeError::Failed(error.to_string()))
    }
}

impl WakeSink for SoftInterruptWakeSink {
    fn enqueue(&self, wake_id: &str, summary: &str) -> Result<String, WakeError> {
        if self.is_queued(wake_id)? {
            return Ok(wake_id.to_string());
        }
        let mut message = horon_agent_agent_runtime::SoftInterruptMessage {
            content: summary.to_string(),
            images: Vec::new(),
            urgent: false,
            source: horon_agent_agent_runtime::SoftInterruptSource::BackgroundTask,
            client_id: Some(wake_id.to_string()),
            input_id: None,
        };
        let mut store = self.open_store()?;
        let accepted = store
            .accept(&mut message)
            .map_err(|error| WakeError::Failed(error.to_string()))?;
        // The legacy file is still mirrored so an older build reading this HOME
        // sees the same pending input; a mirror failure cannot undo the accept,
        // which is already synced.
        if let Err(error) = store.mirror() {
            crate::logging::warn(&format!(
                "wake mirror to the legacy queue failed for session {}: {error}",
                self.session_id
            ));
        }
        Ok(accepted.input_id)
    }

    fn is_queued(&self, wake_id: &str) -> Result<bool, WakeError> {
        // The journal is the live queue; a wake already accepted there must not
        // be accepted a second time under the same client id.
        let mut store = self.open_store()?;
        let state = store.state().map_err(|error| WakeError::Failed(error.to_string()))?;
        let in_journal = state
            .inputs
            .values()
            .any(|entry| entry.accepted.client_id.as_deref() == Some(wake_id));
        if in_journal {
            return Ok(true);
        }
        let queued = crate::soft_interrupt_store::load(&self.session_id)
            .map_err(|error| WakeError::Failed(error.to_string()))?;
        Ok(queued
            .iter()
            .any(|message| message.client_id.as_deref() == Some(wake_id)))
    }
}

/// A wake that is waiting for a delivery slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoalescedWake {
    /// The journaled wake id, once a `wake.pending` row exists. Reusing it is
    /// what makes a retry after a busy queue an ack instead of a second wake.
    pub wake_id: Option<String>,
    pub monitor_seq: u64,
    pub summary: String,
    pub changes: u64,
}

impl CoalescedWake {
    pub fn new(monitor_seq: u64, summary: String) -> Self {
        Self {
            wake_id: None,
            monitor_seq,
            summary,
            changes: 1,
        }
    }

    pub fn fold(&mut self, monitor_seq: u64, summary: String) {
        self.monitor_seq = monitor_seq;
        self.summary = summary;
        self.changes = self.changes.saturating_add(1);
    }

    /// The text the model receives: the latest checkpoint plus how many changes
    /// it stands for, so coalescing hides nothing.
    pub fn text(&self, label: &str) -> String {
        if self.changes <= 1 {
            format!("{label}: {}", self.summary)
        } else {
            format!(
                "{label}: {} ({} changes coalesced into this wake)",
                self.summary, self.changes
            )
        }
    }
}

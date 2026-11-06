//! Process-wide "keep the machine awake while horon-agent is working".
//!
//! The shared daemon hosts every session, so one inhibitor in that process is
//! enough to keep the machine awake while any session is streaming or
//! processing. The operating-system mechanism lives in each adapter
//! ([`SleepInhibitor`]); the policy that keeps it honest lives here, so no
//! caller branches on the operating system:
//!
//! - a bounded helper is replaced before its lifetime ends, and the replacement
//!   starts before the old one is released, so a closed lid cannot suspend the
//!   machine in the gap;
//! - a helper that exits unsuccessfully is retried with exponential backoff
//!   (logind refuses new locks around suspend/resume) rather than disabled for
//!   the rest of the process lifetime.

use std::time::{Duration, Instant};

use horon_agent_platform_api::log::{PlatformLogLevel, log};
use horon_agent_platform_api::{InhibitorState, SleepInhibitor};

use crate::Current;
use crate::PlatformAdapter;

/// The inhibitor the current operating system provides.
pub type Inhibitor = <Current as PlatformAdapter>::Inhibitor;

/// Legacy/global override shared with the desktop app: when set, never inhibit.
const DISABLE_ENV: &str = "HORON_AGENT_DISABLE_POWER_INHIBIT";

/// How long a replaced helper is kept alive after its successor is started.
///
/// logind re-evaluates a closed lid on every event, so even a few milliseconds
/// without a registered lock suspend the machine. The new helper needs time to
/// register over D-Bus before the old one may go.
const HANDOVER_OVERLAP: Duration = Duration::from_secs(3);

/// First retry delay after an inhibitor helper fails (for example because
/// logind refuses new locks while a suspend/resume is still in progress).
/// Doubles on each consecutive failure up to [`MAX_RETRY_BACKOFF`].
const INITIAL_RETRY_BACKOFF: Duration = Duration::from_secs(10);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(600);

/// Best-effort inhibitor that keeps the machine awake while active work exists.
pub struct PowerInhibitor<I: SleepInhibitor = Inhibitor> {
    handle: Option<I>,
    acquired_at: Option<Instant>,
    /// Previous helper kept alive until its replacement has had time to register.
    retiring: Option<I>,
    available: bool,
    /// After a helper failure, do not start another before this instant.
    retry_at: Option<Instant>,
    retry_backoff: Duration,
}

impl<I: SleepInhibitor> Default for PowerInhibitor<I> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: SleepInhibitor> PowerInhibitor<I> {
    /// Build an inhibitor. It is available on a supported platform unless the
    /// legacy `HORON_AGENT_DISABLE_POWER_INHIBIT` escape hatch is set.
    ///
    /// The user-facing config toggle is deliberately not baked in here: the
    /// caller evaluates it per reconcile (via [`PowerInhibitor::set_active`]) so
    /// it can be flipped at runtime in either direction without a restart.
    pub fn new() -> Self {
        let supported = I::supported();
        Self {
            handle: None,
            acquired_at: None,
            retiring: None,
            available: supported && std::env::var_os(DISABLE_ENV).is_none(),
            retry_at: None,
            retry_backoff: INITIAL_RETRY_BACKOFF,
        }
    }

    /// Whether this inhibitor can actually do anything here.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Reconcile the guard against the desired active state. Safe to call
    /// frequently; it is idempotent and refreshes a bounded helper.
    pub fn set_active(&mut self, active: bool) {
        if active && self.available {
            self.acquire();
        } else {
            self.release();
        }
    }

    fn acquire(&mut self) {
        let now = Instant::now();
        let state = self.handle.as_mut().map(SleepInhibitor::state);
        if state == Some(InhibitorState::Failed) {
            // Usually transient: logind rejects new locks while a suspend or
            // resume is in progress. Back off and retry rather than giving up
            // for the rest of the process lifetime.
            warn(&format!(
                "power_inhibit: inhibitor exited unsuccessfully; retrying in {}s",
                self.retry_backoff.as_secs()
            ));
            self.release();
            self.retry_at = Some(now + self.retry_backoff);
            self.retry_backoff = (self.retry_backoff * 2).min(MAX_RETRY_BACKOFF);
            return;
        }
        if self.retry_at.is_some_and(|at| now < at) {
            return;
        }

        let healthy = self.handle.is_some() && state == Some(InhibitorState::Holding);
        if healthy
            && self
                .acquired_at
                .is_some_and(|at| now.saturating_duration_since(at) >= HANDOVER_OVERLAP)
        {
            // The replacement has had time to register: a lock that survived
            // this long is working, so reset the failure backoff too.
            self.retire_previous();
            self.retry_backoff = INITIAL_RETRY_BACKOFF;
        }
        let fresh = self.handle.as_ref().is_some_and(|handle| !handle.needs_refresh());
        if healthy && fresh {
            return;
        }

        // Either there is no helper, it exited, or its lifetime is nearly over.
        // Make before break: keep a still-running helper alive until the new one
        // has registered, otherwise a closed lid suspends in the gap.
        self.retire_previous();
        self.acquired_at = None;
        if let Some(handle) = self.handle.take() {
            if healthy {
                self.retiring = Some(handle);
            } else {
                drop(handle);
            }
        }

        match I::start() {
            Ok(handle) => {
                self.handle = Some(handle);
                self.acquired_at = Some(now);
            }
            Err(error) => {
                warn(&format!("power_inhibit: failed to acquire inhibitor: {error}"));
                self.available = false;
            }
        }
    }

    fn release(&mut self) {
        self.acquired_at = None;
        self.retire_previous();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
    }

    fn retire_previous(&mut self) {
        if let Some(handle) = self.retiring.take() {
            drop(handle);
        }
    }
}

impl<I: SleepInhibitor> Drop for PowerInhibitor<I> {
    fn drop(&mut self) {
        self.release();
    }
}

fn warn(message: &str) {
    log(PlatformLogLevel::Warn, message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

    /// The fake's start behaviour is process state, so the tests that touch it
    /// run one at a time.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Start behaviour for the fake: `0` succeeds, anything else is the raw
    /// error number to report.
    static START_ERROR: AtomicU8 = AtomicU8::new(0);
    static STARTED: AtomicBool = AtomicBool::new(false);

    struct Fake {
        state: InhibitorState,
        refresh: bool,
    }

    impl SleepInhibitor for Fake {
        fn supported() -> bool {
            true
        }

        fn start() -> std::io::Result<Self> {
            STARTED.store(true, Ordering::SeqCst);
            let code = START_ERROR.load(Ordering::SeqCst);
            if code != 0 {
                return Err(std::io::Error::from_raw_os_error(code as i32));
            }
            Ok(Self {
                state: InhibitorState::Holding,
                refresh: true,
            })
        }

        fn state(&mut self) -> InhibitorState {
            self.state
        }

        fn needs_refresh(&self) -> bool {
            self.refresh
        }

        fn stop(&mut self) {
            self.state = InhibitorState::ExitedCleanly;
        }
    }

    fn inhibitor(handle: Fake) -> PowerInhibitor<Fake> {
        PowerInhibitor {
            handle: Some(handle),
            acquired_at: Some(Instant::now()),
            retiring: None,
            available: true,
            retry_at: None,
            retry_backoff: INITIAL_RETRY_BACKOFF,
        }
    }

    #[test]
    fn a_failed_helper_backs_off_instead_of_disabling_forever() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut subject = inhibitor(Fake {
            state: InhibitorState::Failed,
            refresh: true,
        });

        subject.set_active(true);

        assert!(subject.is_available());
        assert!(subject.handle.is_none());
        assert!(subject.retry_at.is_some());
        assert_eq!(subject.retry_backoff, INITIAL_RETRY_BACKOFF * 2);

        // Still inside the backoff window: no new helper yet.
        subject.set_active(true);
        assert!(subject.handle.is_none());
    }

    #[test]
    fn a_refresh_keeps_the_previous_helper_until_the_successor_registers() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        START_ERROR.store(0, Ordering::SeqCst);
        STARTED.store(false, Ordering::SeqCst);
        let mut subject = inhibitor(Fake {
            state: InhibitorState::Holding,
            refresh: true,
        });

        subject.set_active(true);

        assert!(STARTED.load(Ordering::SeqCst), "the successor starts");
        assert!(subject.handle.is_some(), "the successor is held");
        assert!(subject.retiring.is_some(), "the old helper was not dropped first");

        subject.release();
        assert!(subject.retiring.is_none());
    }

    #[test]
    fn an_inhibitor_that_cannot_expire_is_left_alone() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        STARTED.store(false, Ordering::SeqCst);
        let mut subject = inhibitor(Fake {
            state: InhibitorState::Holding,
            refresh: false,
        });

        subject.set_active(true);

        assert!(!STARTED.load(Ordering::SeqCst), "no refresh is due");
    }

    #[test]
    fn a_failed_acquisition_marks_the_inhibitor_unavailable() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        START_ERROR.store(22, Ordering::SeqCst);
        STARTED.store(false, Ordering::SeqCst);
        let mut subject: PowerInhibitor<Fake> = PowerInhibitor::new();

        subject.set_active(true);

        assert!(STARTED.load(Ordering::SeqCst));
        assert!(!subject.is_available());
        START_ERROR.store(0, Ordering::SeqCst);
    }

    #[test]
    fn release_stops_the_held_helper() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut subject = inhibitor(Fake {
            state: InhibitorState::Holding,
            refresh: true,
        });
        subject.set_active(false);
        assert!(subject.handle.is_none());
        assert!(subject.is_available());
    }
}

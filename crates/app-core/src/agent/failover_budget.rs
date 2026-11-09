//! Durable per-chain budget for automatic provider failover.
//!
//! One record per session lives at
//! `~/.horon/failover-budget/<safe-session-id>.json`. `chain_id` is the
//! unit of the budget (C6): a new run chain starts at zero and an attempt
//! counter never carries across chains, while the same chain keeps its count
//! across a process restart (IS-4).
//!
//! Load results are deliberately three-way. `Ok(None)` means no file, so this
//! is a first run and a fresh chain may start at zero. `Ok(Some(_))` restores
//! the stored chain and its attempt count. `Err(_)` means the state exists but
//! cannot be trusted, and the caller must treat the budget as exhausted
//! instead of resetting it: an unattended run must never be handed unlimited
//! retries because its accounting file was truncated, corrupt, or unreadable.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Persisted attempt counter for one run chain of one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FailoverBudget {
    pub session_id: String,
    pub chain_id: String,
    pub attempts: u32,
    pub updated_at: DateTime<Utc>,
}

/// Why a stored budget could not be trusted.
///
/// Both variants are conservative: callers must block automatic failover
/// rather than restart the count. They are separate so logs and the surfaced
/// event can say whether the file was malformed or unreachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BudgetLoadError {
    /// The file exists but does not parse as a budget record, including a
    /// partially written file.
    Corrupt { path: PathBuf, detail: String },
    /// The file or its directory could not be read or written.
    Io { path: PathBuf, detail: String },
}

impl fmt::Display for BudgetLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt { path, detail } => {
                write!(f, "corrupt failover budget at {}: {detail}", path.display())
            }
            Self::Io { path, detail } => {
                write!(f, "failover budget is unreadable at {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for BudgetLoadError {}

/// Result of one attempt to spend an automatic-failover attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConsumeOutcome {
    /// The attempt was recorded; automatic failover may proceed.
    Allowed { attempts: u32 },
    /// The chain already spent its cap.
    Exhausted { attempts: u32 },
    /// The stored budget could not be trusted, so no attempt was recorded and
    /// automatic failover must stay blocked.
    Unavailable(BudgetLoadError),
}

/// In-memory projection of the persisted budget for the live agent.
///
/// It also remembers which session the projection belongs to. An `Agent` can
/// swap its session without being reconstructed, and a chain id from the old
/// session must never be spent against the new session's record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FailoverBudgetState {
    session_id: Option<String>,
    chain_id: Option<String>,
    attempts: u32,
    unknown: bool,
}

impl FailoverBudgetState {
    /// First run: no stored record, so begin a chain at zero.
    pub(crate) fn fresh(session_id: &str) -> Self {
        Self {
            session_id: Some(session_id.to_string()),
            chain_id: Some(new_chain_id()),
            attempts: 0,
            unknown: false,
        }
    }

    /// Normal restore: keep the stored chain and its attempt count.
    pub(crate) fn restored(budget: FailoverBudget) -> Self {
        Self {
            session_id: Some(budget.session_id),
            chain_id: Some(budget.chain_id),
            attempts: budget.attempts,
            unknown: false,
        }
    }

    /// The stored record exists but cannot be trusted.
    pub(crate) fn unreadable(session_id: &str) -> Self {
        Self {
            session_id: Some(session_id.to_string()),
            chain_id: None,
            attempts: 0,
            unknown: true,
        }
    }

    /// Whether this projection belongs to `session_id`.
    ///
    /// `false` means it was loaded for another session; the caller must restore
    /// from `session_id`'s record before reading a chain id or attempt count.
    pub(crate) fn belongs_to(&self, session_id: &str) -> bool {
        self.session_id.as_deref() == Some(session_id)
    }

    /// The chain the attempts are counted against, or `None` when no chain is
    /// known. A caller that needs one must start a new chain, never reuse
    /// another session's.
    pub(crate) fn chain_id(&self) -> Option<&str> {
        self.chain_id.as_deref()
    }

    /// Attempts recorded for the current chain, as last read or written.
    ///
    /// This is not a remaining count: compare it with the cap, and check
    /// [`Self::is_unknown`] first, because an untrusted budget reports a stale value.
    pub(crate) fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Whether the stored record could not be trusted.
    ///
    /// Callers must treat this as blocked and stop the run for a human, never as
    /// zero attempts used: the real count may already be at the cap.
    pub(crate) fn is_unknown(&self) -> bool {
        self.unknown
    }

    /// Adopt a count that was just persisted (or read back) for this chain.
    ///
    /// It changes the in-memory projection only and does not write to disk, so
    /// the caller must have persisted the value first or it is lost on restart.
    pub(crate) fn set_attempts(&mut self, attempts: u32) {
        self.attempts = attempts;
    }

    /// Keep the chain identity for reporting while marking the persisted state
    /// untrustworthy, so failover stays blocked for the rest of the run.
    pub(crate) fn set_unreadable_chain(&mut self, chain_id: String) {
        self.chain_id = Some(chain_id);
        self.unknown = true;
    }

    /// Mark the projection untrusted after a failed read or write, keeping the
    /// chain id and count for the report. Every later spend is refused, so the
    /// run stops for a human instead of retrying without accounting.
    pub(crate) fn mark_unknown(&mut self) {
        self.unknown = true;
    }
}

/// A new identity for a run chain. Each user prompt starts one.
pub(crate) fn new_chain_id() -> String {
    format!("chain-{}", uuid::Uuid::new_v4())
}

fn budget_dir() -> Result<PathBuf, BudgetLoadError> {
    crate::storage::horon_agent_dir()
        .map(|dir| dir.join("failover-budget"))
        .map_err(|error| BudgetLoadError::Io {
            path: PathBuf::from("failover-budget"),
            detail: format!("{error:#}"),
        })
}

/// Collapse anything outside `[A-Za-z0-9_-]` so a session id can never escape
/// the budget directory.
fn safe_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// The file holding `session_id`'s stored budget, inside the budget directory.
///
/// The id is folded to `[A-Za-z0-9_-]`, so it cannot escape that directory; two
/// ids that differ only in other characters share one file. `Err` means the
/// directory could not be located.
pub(crate) fn budget_path(session_id: &str) -> Result<PathBuf, BudgetLoadError> {
    Ok(budget_dir()?.join(format!("{}.json", safe_session_id(session_id))))
}

/// Read this session's stored budget.
///
/// `Ok(None)` is a first run. `Err` means the file is present but cannot be
/// trusted; callers must not overwrite it with a zeroed budget.
pub(crate) fn load(session_id: &str) -> Result<Option<FailoverBudget>, BudgetLoadError> {
    let path = budget_path(session_id)?;
    // Metadata, not `Path::exists`: `exists` folds every error into `false`,
    // which would turn an unreadable directory into a silent first run and
    // refund the whole budget.
    match std::fs::metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A missing primary is a first run only when no backup exists.
            // `write_json` leaves `<path>.bak` beside any record written more
            // than once, so a surviving backup proves the state existed and was
            // lost. Reporting a first run there restarts the chain at zero and
            // refunds every attempt already spent: the same refund as trusting
            // a stale backup, reached by deleting the primary instead.
            //
            // The backup is probed with `symlink_metadata` so the directory
            // entry itself is what counts: a dangling `<path>.bak` symlink is a
            // surviving backup, while following it to `NotFound` would report a
            // first run and refund the budget. The primary keeps `metadata`
            // because a symlink that resolves is a legitimate record.
            //
            // Known bound: a record written exactly once has no backup yet, so
            // deleting the primary right after a single consume is
            // indistinguishable from a first run and still yields `Ok(None)`.
            // Nothing on disk records that the attempt happened.
            let backup = path.with_extension("bak");
            return match std::fs::symlink_metadata(&backup) {
                Ok(_) => Err(BudgetLoadError::Corrupt {
                    path,
                    detail: format!("record is missing but a backup remains at {}", backup.display()),
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(BudgetLoadError::Io {
                    path: backup,
                    detail: error.to_string(),
                }),
            };
        }
        Err(error) => {
            return Err(BudgetLoadError::Io {
                path,
                detail: error.to_string(),
            });
        }
    }
    // Read the primary directly instead of going through
    // `crate::storage::read_json`.
    //
    // That helper falls back to `<path>.bak` when the primary is corrupt and
    // copies the backup over the primary. For a queue an older version beats a
    // lost one, but for an attempt counter it is a refund: `write_json` leaves
    // a backup holding the state from ONE WRITE EARLIER, so recovering it hands
    // back an attempt the run already spent and the cap can be bypassed by
    // corrupting the file. A stale-but-valid backup is exactly the wrong answer
    // here, so this module owns its read. Do not "fix" this back to the
    // recovering helper.
    let data = std::fs::read_to_string(&path).map_err(|error| BudgetLoadError::Io {
        path: path.clone(),
        detail: error.to_string(),
    })?;
    let budget: FailoverBudget = serde_json::from_str(&data).map_err(|error| BudgetLoadError::Corrupt {
        path: path.clone(),
        detail: error.to_string(),
    })?;
    // A body naming another session cannot be trusted as this session's
    // budget: its attempt count was spent by someone else.
    if budget.session_id != session_id {
        return Err(BudgetLoadError::Corrupt {
            path,
            detail: format!(
                "record names session {:?} but is stored for {:?}",
                budget.session_id, session_id
            ),
        });
    }
    Ok(Some(budget))
}

/// Write the record for `session_id`.
///
/// The caller's session owns the file, not the body: a body naming a different
/// session would otherwise spend attempts against a session that never made
/// them, so the mismatch is refused rather than written.
fn persist(session_id: &str, budget: &FailoverBudget) -> Result<(), BudgetLoadError> {
    let path = budget_path(session_id)?;
    if budget.session_id != session_id {
        return Err(BudgetLoadError::Corrupt {
            path,
            detail: format!(
                "refusing to write a record for session {:?} as session {:?}",
                budget.session_id, session_id
            ),
        });
    }
    crate::storage::write_json(&path, budget).map_err(|error| BudgetLoadError::Io {
        path,
        detail: format!("{error:#}"),
    })
}

/// Start `chain_id` at zero, reusing the stored record when it already names
/// this chain.
///
/// A stored record that cannot be read is propagated unchanged: beginning a
/// new chain must never clear an unknown state.
pub(crate) fn begin_chain(session_id: &str, chain_id: &str) -> Result<FailoverBudget, BudgetLoadError> {
    if let Some(current) = load(session_id)?
        && current.chain_id == chain_id
    {
        return Ok(current);
    }
    let budget = FailoverBudget {
        session_id: session_id.to_string(),
        chain_id: chain_id.to_string(),
        attempts: 0,
        updated_at: Utc::now(),
    };
    persist(session_id, &budget)?;
    Ok(budget)
}

/// Attempts to take a held budget lock before giving up.
const BUDGET_LOCK_ATTEMPTS: u32 = 100;
/// Delay between lock attempts (20ms x 100 = about 2 seconds of patience).
const BUDGET_LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

/// Exclusive lock over one session's stored budget.
///
/// The lock is a `<path>.lock` file beside the record rather than one lock for
/// the whole budget directory: only writers of the same session's record have
/// anything to serialize, and a daemon running many sessions must not queue
/// them behind each other. The lock is held by the open file itself and is
/// released in [`Drop`], so every return path frees it, errors included; no
/// part of this relies on process exit.
struct BudgetLock {
    file: std::fs::File,
}

impl BudgetLock {
    /// Take this session's lock, retrying a bounded number of times.
    ///
    /// A lock that cannot be taken is reported as an error and never as an
    /// implicit "go ahead": the caller turns it into
    /// [`ConsumeOutcome::Unavailable`], which blocks failover instead of
    /// allowing an attempt that could not be accounted for.
    fn acquire(session_id: &str, attempts: u32, delay: Duration) -> Result<Self, BudgetLoadError> {
        let path = budget_path(session_id)?.with_extension("lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| BudgetLoadError::Io {
                path: parent.to_path_buf(),
                detail: error.to_string(),
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| BudgetLoadError::Io {
                path: path.clone(),
                detail: error.to_string(),
            })?;

        let mut attempt = 0;
        loop {
            attempt += 1;
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) if attempt < attempts => std::thread::sleep(delay),
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(BudgetLoadError::Io {
                        path,
                        detail: format!(
                            "failover budget lock is still held after {attempts} attempts; another writer is updating this session's record"
                        ),
                    });
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(BudgetLoadError::Io {
                        path,
                        detail: error.to_string(),
                    });
                }
            }
        }
    }
}

impl Drop for BudgetLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Spend one attempt of `chain_id`, keeping the reason a caller must surface.
///
/// The read-modify-write runs under this session's lock so two writers cannot
/// both read the same count and persist `attempts + 1`, which would let the
/// chain spend more than `max`. A daemon and a resumed CLI writing one
/// session's record is the case this exists for.
pub(crate) fn try_consume(session_id: &str, chain_id: &str, max: u32) -> ConsumeOutcome {
    let _lock = match BudgetLock::acquire(session_id, BUDGET_LOCK_ATTEMPTS, BUDGET_LOCK_RETRY_DELAY) {
        Ok(lock) => lock,
        Err(error) => return ConsumeOutcome::Unavailable(error),
    };
    let mut budget = match load(session_id) {
        Ok(Some(budget)) => budget,
        Ok(None) => FailoverBudget {
            session_id: session_id.to_string(),
            chain_id: chain_id.to_string(),
            attempts: 0,
            updated_at: Utc::now(),
        },
        Err(error) => return ConsumeOutcome::Unavailable(error),
    };
    if budget.chain_id != chain_id {
        budget.chain_id = chain_id.to_string();
        budget.attempts = 0;
    }
    if budget.attempts >= max {
        return ConsumeOutcome::Exhausted {
            attempts: budget.attempts,
        };
    }
    budget.attempts += 1;
    budget.updated_at = Utc::now();
    if let Err(error) = persist(session_id, &budget) {
        return ConsumeOutcome::Unavailable(error);
    }
    ConsumeOutcome::Allowed {
        attempts: budget.attempts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::message::{Message, ToolDefinition};
    use crate::protocol::ServerEvent;
    use crate::provider::{EventStream, Provider};
    use crate::tool::Registry;
    use async_trait::async_trait;
    use std::ffi::OsString;
    use std::sync::Arc;

    /// Serialize this module's tests and sandbox `HORON_HOME`.
    ///
    /// `HORON_AGENT_QA_HOME` points the sandbox at a directory that is kept on
    /// disk, so a manual QA run can inspect the persisted record; otherwise a
    /// throwaway temp directory is used.
    ///
    /// Fields drop in declaration order, so `_lock` is declared last: the env
    /// var is restored before the process-wide lock is released.
    struct TestHome {
        previous: Option<OsString>,
        _temp: Option<tempfile::TempDir>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TestHome {
        fn new() -> Self {
            let lock = crate::storage::lock_test_env();
            let (home, temp) = match std::env::var_os("HORON_AGENT_QA_HOME") {
                Some(dir) => {
                    let dir = PathBuf::from(dir);
                    std::fs::create_dir_all(&dir).expect("qa home");
                    (dir, None)
                }
                None => {
                    let dir = tempfile::tempdir().expect("temp home");
                    let path = dir.path().to_path_buf();
                    (path, Some(dir))
                }
            };
            let previous = std::env::var_os("HORON_HOME");
            crate::env::set_var("HORON_HOME", &home);
            crate::config::Config::invalidate_cache();
            Self {
                previous,
                _temp: temp,
                _lock: lock,
            }
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => crate::env::set_var("HORON_HOME", value),
                None => crate::env::remove_var("HORON_HOME"),
            }
            crate::config::Config::invalidate_cache();
        }
    }

    struct IdleProvider;

    /// The production cap: the config default the failover path hands to
    /// [`try_consume`], so these tests spend against the number a user gets.
    fn default_max_attempts() -> u32 {
        crate::config::ProviderConfig::default().autonomous_failover_max_attempts
    }

    /// Start a test from a cleared record so a kept QA home stays re-runnable.
    fn fresh(session: &str) {
        let path = budget_path(session).expect("budget path");
        for candidate in [path.clone(), path.with_extension("bak")] {
            match std::fs::remove_file(&candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove {}: {error}", candidate.display()),
            }
        }
    }

    #[async_trait]
    impl Provider for IdleProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            anyhow::bail!("the idle provider never completes a request")
        }

        fn name(&self) -> &str {
            "idle"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(IdleProvider)
        }
    }

    #[test]
    fn three_attempts_are_allowed_and_the_fourth_is_denied() {
        let _home = TestHome::new();
        let session = "sess-happy";
        fresh(session);
        assert_eq!(load(session).expect("first run"), None);

        let chain = "chain-a";
        assert!(matches!(
            try_consume(session, chain, default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
        assert!(matches!(
            try_consume(session, chain, default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
        assert!(matches!(
            try_consume(session, chain, default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
        assert!(!matches!(
            try_consume(session, chain, default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));

        let stored = load(session).expect("readable").expect("stored");
        assert_eq!(stored.chain_id, chain);
        assert_eq!(stored.attempts, default_max_attempts());
    }

    #[test]
    fn a_new_chain_restarts_at_zero() {
        let _home = TestHome::new();
        let session = "sess-new-chain";
        fresh(session);
        for _ in 0..default_max_attempts() {
            assert!(matches!(
                try_consume(session, "chain-old", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            ));
        }
        assert!(!matches!(
            try_consume(session, "chain-old", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));

        let began = begin_chain(session, "chain-next").expect("begin chain");
        assert_eq!(began.chain_id, "chain-next");
        assert_eq!(began.attempts, 0);
        assert!(matches!(
            try_consume(session, "chain-next", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
        assert_eq!(load(session).expect("readable").expect("stored").attempts, 1);
    }

    #[test]
    fn begin_chain_is_idempotent_for_the_same_chain() {
        let _home = TestHome::new();
        let session = "sess-idempotent";
        fresh(session);
        assert!(matches!(
            try_consume(session, "chain-same", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
        let again = begin_chain(session, "chain-same").expect("begin chain");
        assert_eq!(again.attempts, 1);
    }

    #[test]
    fn a_restart_restores_the_chain_and_its_attempts() {
        let _home = TestHome::new();
        let session = "sess-restart";
        fresh(session);
        for _ in 0..default_max_attempts() {
            assert!(matches!(
                try_consume(session, "chain-r", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            ));
        }

        // Simulated restart: a new process has nothing but the file.
        let restored = FailoverBudgetState::restored(load(session).expect("readable").expect("stored"));
        assert_eq!(restored.chain_id(), Some("chain-r"));
        assert_eq!(restored.attempts(), default_max_attempts());
        assert!(!restored.is_unknown());

        assert_eq!(
            try_consume(session, "chain-r", default_max_attempts()),
            ConsumeOutcome::Exhausted {
                attempts: default_max_attempts()
            }
        );
    }

    #[test]
    fn a_corrupt_file_is_an_error_and_is_never_reset() {
        let _home = TestHome::new();
        let session = "sess-corrupt";
        fresh(session);
        let path = budget_path(session).expect("path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let corrupt = "{\"session_id\":\"sess-corrupt\",\"chain_id\":\"chain-x\",\"attempts\":1";
        std::fs::write(&path, corrupt).expect("write corrupt");

        let error = load(session).expect_err("corrupt is an error");
        println!("load error: {error}");
        println!(
            "consume outcome: {:?}",
            try_consume(session, "chain-x", default_max_attempts())
        );
        assert!(matches!(error, BudgetLoadError::Corrupt { .. }));
        // Beginning a chain must not paper over the unknown state with a zero.
        assert!(matches!(
            begin_chain(session, "chain-new"),
            Err(BudgetLoadError::Corrupt { .. })
        ));
        assert_eq!(std::fs::read_to_string(&path).expect("still there"), corrupt);
        assert!(matches!(
            try_consume(session, "chain-x", default_max_attempts()),
            ConsumeOutcome::Unavailable(BudgetLoadError::Corrupt { .. })
        ));
        assert!(!matches!(
            try_consume(session, "chain-x", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
    }

    /// A stale `.bak` must never stand in for the attempt counter.
    ///
    /// `write_json` leaves a backup holding the state from one write earlier,
    /// so once the cap is reached the backup still names `attempts - 1`.
    /// Recovering it would refund an attempt and let the cap be bypassed by
    /// corrupting the primary, so a corrupt primary stays an error even when a
    /// parseable backup sits beside it.
    #[test]
    fn a_corrupt_primary_blocks_even_when_a_stale_backup_exists() {
        let _home = TestHome::new();
        let session = "sess-stale-backup";
        fresh(session);
        let path = budget_path(session).expect("path");
        let backup = path.with_extension("bak");

        for attempt in 1..=default_max_attempts() {
            assert!(
                matches!(
                    try_consume(session, "chain-stale", default_max_attempts()),
                    ConsumeOutcome::Allowed { .. }
                ),
                "attempt {attempt}"
            );
        }

        // The fixture only proves anything with a parseable backup on disk that
        // holds the previous write's count.
        let stale: FailoverBudget =
            serde_json::from_str(&std::fs::read_to_string(&backup).expect("a backup exists after the writes"))
                .expect("the backup parses");
        assert_eq!(stale.attempts, default_max_attempts() - 1);

        for round in 1..=2 {
            if round == 2 {
                // Repair the primary to an exhausted record, so the same bypass
                // is attempted again with the still-stale backup beside it.
                let repaired = FailoverBudget {
                    session_id: session.to_string(),
                    chain_id: "chain-stale".to_string(),
                    attempts: default_max_attempts(),
                    updated_at: Utc::now(),
                };
                std::fs::write(&path, serde_json::to_vec(&repaired).expect("json")).expect("repair the primary");
                assert!(
                    matches!(load(session), Ok(Some(_))),
                    "round {round}: repaired record loads"
                );
                assert_eq!(
                    serde_json::from_str::<FailoverBudget>(
                        &std::fs::read_to_string(&backup).expect("backup still there")
                    )
                    .expect("backup parses")
                    .attempts,
                    default_max_attempts() - 1,
                    "round {round}: the backup stays stale"
                );
            }

            std::fs::write(&path, "not json").expect("corrupt the primary");

            let error = load(session).expect_err("a corrupt primary is an error, not a refund");
            println!("round {round} load error: {error}");
            assert!(
                matches!(error, BudgetLoadError::Corrupt { .. }),
                "round {round}: expected Corrupt, got {error}"
            );
            assert!(
                matches!(
                    try_consume(session, "chain-stale", default_max_attempts()),
                    ConsumeOutcome::Unavailable(BudgetLoadError::Corrupt { .. })
                ),
                "round {round}: consume must be unavailable"
            );
            assert!(
                !matches!(
                    try_consume(session, "chain-stale", default_max_attempts()),
                    ConsumeOutcome::Allowed { .. }
                ),
                "round {round}: a corrupt primary must not allow an attempt"
            );
            // The stale backup is never copied over the corrupt primary.
            assert_eq!(
                std::fs::read_to_string(&path).expect("still there"),
                "not json",
                "round {round}: the primary is left untouched"
            );
        }
    }

    /// A surviving backup means the record was lost, not never written.
    ///
    /// `load` treats a missing primary as a first run, which restarts the chain
    /// at zero. If `write_json`'s backup is still on disk the state did exist
    /// and was deleted, so reporting a first run would refund every attempt the
    /// run already spent. Only when neither file exists is a fresh chain real.
    #[test]
    fn a_missing_primary_with_a_surviving_backup_is_not_a_first_run() {
        let _home = TestHome::new();
        let session = "sess-lost-primary";
        fresh(session);
        let path = budget_path(session).expect("path");
        let backup = path.with_extension("bak");

        // Two writes through the module's own API: the second one leaves the
        // first record on disk as the backup.
        assert!(matches!(
            try_consume(session, "chain-lost", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
        assert!(matches!(
            try_consume(session, "chain-lost", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
        assert!(backup.exists(), "the second write leaves a backup");

        std::fs::remove_file(&path).expect("delete only the primary");
        assert!(!path.exists());
        assert!(backup.exists());

        let error = load(session).expect_err("a lost record with a surviving backup is an error");
        println!("lost-primary load error: {error}");
        assert!(
            matches!(error, BudgetLoadError::Corrupt { .. }),
            "expected Corrupt, got {error}"
        );
        assert!(
            !matches!(
                try_consume(session, "chain-lost", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            ),
            "a lost record must not allow another attempt"
        );

        // Neither file is a genuine first run and still starts at zero.
        let fresh_session = "sess-genuine-first-run";
        fresh(fresh_session);
        let fresh_path = budget_path(fresh_session).expect("path");
        assert!(!fresh_path.exists());
        assert!(!fresh_path.with_extension("bak").exists());
        assert_eq!(load(fresh_session).expect("first run"), None);
        assert!(matches!(
            try_consume(fresh_session, "chain-first", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
    }

    /// A record whose body names another session is not this session's budget.
    ///
    /// Its attempts were spent by someone else, so reading it as this
    /// session's count would both steal that session's history and let this one
    /// write into a file it does not own.
    #[test]
    fn a_record_naming_another_session_is_corrupt_and_cannot_be_written() {
        let _home = TestHome::new();
        let session = "sess-owner";
        let other = "sess-other";
        fresh(session);
        fresh(other);
        let path = budget_path(session).expect("path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let foreign = FailoverBudget {
            session_id: other.to_string(),
            chain_id: "chain-foreign".to_string(),
            attempts: 1,
            updated_at: Utc::now(),
        };
        let foreign_json = serde_json::to_string(&foreign).expect("json");
        std::fs::write(&path, &foreign_json).expect("write the foreign record");

        let error = load(session).expect_err("a body naming another session is corrupt");
        println!("cross-session load error: {error}");
        match &error {
            BudgetLoadError::Corrupt { detail, .. } => {
                assert!(detail.contains(other), "detail names the body session: {detail}");
                assert!(detail.contains(session), "detail names the asked session: {detail}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
        assert!(matches!(
            try_consume(session, "chain-foreign", default_max_attempts()),
            ConsumeOutcome::Unavailable(BudgetLoadError::Corrupt { .. })
        ));
        assert!(!matches!(
            try_consume(session, "chain-foreign", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));

        // Writing is refused the same way, so an attempt cannot land on `other`.
        assert!(matches!(
            persist(session, &foreign),
            Err(BudgetLoadError::Corrupt { .. })
        ));
        assert!(
            !budget_path(other).expect("path").exists(),
            "the named session never receives this record"
        );
        // The foreign body is left as it was instead of being rewritten.
        assert_eq!(std::fs::read_to_string(&path).expect("still there"), foreign_json);
    }

    #[test]
    fn a_partially_written_file_is_not_a_valid_budget() {
        let _home = TestHome::new();
        let session = "sess-partial";
        fresh(session);
        let path = budget_path(session).expect("path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        // A torn write that happens to be valid JSON but is missing fields.
        std::fs::write(&path, "{\"session_id\":\"sess-partial\"}").expect("write torn");
        let error = load(session).expect_err("torn is an error");
        println!("torn load error: {error}");
        assert!(matches!(error, BudgetLoadError::Corrupt { .. }));
        assert!(!matches!(
            try_consume(session, "chain-torn", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
    }

    #[test]
    fn an_unreadable_path_is_an_io_error_and_blocks_failover() {
        let _home = TestHome::new();
        let session = "sess-unreadable";
        fresh(session);
        let path = budget_path(session).expect("path");
        // A directory where the record belongs cannot be read as a file.
        std::fs::create_dir_all(&path).expect("dir at record path");
        let error = load(session).expect_err("a directory is an error");
        println!("unreadable load error: {error}");
        assert!(matches!(error, BudgetLoadError::Io { .. }));
        assert!(!matches!(
            try_consume(session, "chain-u", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_permission_denied_record_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt;

        let _home = TestHome::new();
        let session = "sess-permission";
        fresh(session);
        let path = budget_path(session).expect("path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let valid = FailoverBudget {
            session_id: session.to_string(),
            chain_id: "chain-p".to_string(),
            attempts: 1,
            updated_at: Utc::now(),
        };
        std::fs::write(&path, serde_json::to_vec(&valid).expect("json")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let result = load(session);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod back");

        match result {
            Err(BudgetLoadError::Io { .. }) => {}
            // A privileged runner can read the file anyway; the case is then
            // not observable on this host rather than wrong.
            Ok(Some(budget)) => assert_eq!(budget.attempts, 1),
            other => panic!("expected an io error, got {other:?}"),
        }
    }

    #[test]
    fn the_session_id_cannot_escape_the_budget_directory() {
        let _home = TestHome::new();
        let path = budget_path("../../etc/passwd").expect("path");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("______etc_passwd.json")
        );
        assert_eq!(path.parent(), Some(budget_dir().expect("dir").as_path()));
    }

    #[tokio::test]
    async fn an_agent_keeps_its_budget_across_a_restart() {
        let _home = TestHome::new();
        let provider: Arc<dyn Provider> = Arc::new(IdleProvider);
        let mut agent = Agent::new(provider.clone(), Registry::empty());
        let session_id = agent.session.id.clone();
        let chain = agent.begin_failover_chain();

        assert!(agent.consume_failover_attempt(Some("req-1"), default_max_attempts()));
        assert!(agent.consume_failover_attempt(Some("req-2"), default_max_attempts()));
        assert!(agent.consume_failover_attempt(Some("req-3"), default_max_attempts()));
        assert!(!agent.consume_failover_attempt(Some("req-4"), default_max_attempts()));
        assert_eq!(agent.failover_attempts(), default_max_attempts());
        agent.session.save_prepared().expect("save session");

        // Restart: build a new Agent from the persisted session.
        let stored = crate::session::Session::load(&session_id).expect("load session");
        let mut restarted = Agent::new_with_session(provider, Registry::empty(), stored, None);
        assert_eq!(restarted.session.id, session_id);
        assert_eq!(restarted.failover_chain_id(), Some(chain.as_str()));
        assert_eq!(restarted.failover_attempts(), default_max_attempts());
        assert!(!restarted.failover_budget_unknown());
        assert!(!restarted.consume_failover_attempt(Some("req-5"), default_max_attempts()));
    }

    #[tokio::test]
    async fn restoring_a_session_adopts_its_stored_budget() {
        let _home = TestHome::new();
        let provider: Arc<dyn Provider> = Arc::new(IdleProvider);
        let mut agent = Agent::new(provider, Registry::empty());
        let target = "sess-restore-target";
        fresh(target);
        for _ in 0..default_max_attempts() {
            assert!(matches!(
                try_consume(target, "chain-restored", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            ));
        }
        let mut stored = crate::session::Session::create_with_id(target.to_string(), None, None);
        stored.save_prepared().expect("save session");

        agent.restore_session(target).expect("restore session");

        assert_eq!(agent.failover_chain_id(), Some("chain-restored"));
        assert_eq!(agent.failover_attempts(), default_max_attempts());
        assert!(!agent.consume_failover_attempt(Some("req-restored"), default_max_attempts()));
    }

    #[tokio::test]
    async fn an_exhausted_budget_surfaces_run_needs_human_resume() {
        let _home = TestHome::new();
        let provider: Arc<dyn Provider> = Arc::new(IdleProvider);
        let mut agent = Agent::new(provider, Registry::empty());
        let session_id = agent.session.id.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        crate::server::register_session_event_sink(&session_id, tx);

        for attempt in 1..=default_max_attempts() {
            assert!(
                agent.consume_failover_attempt(Some("req-ok"), default_max_attempts()),
                "attempt {attempt}"
            );
        }
        assert!(!agent.consume_failover_attempt(Some("req-stop"), default_max_attempts()));

        let event = rx.try_recv().expect("an event was delivered");
        println!("surfaced event: {}", serde_json::to_string(&event).expect("event json"));
        match event {
            ServerEvent::RunNeedsHumanResume {
                request_id,
                session_id: event_session,
                reason,
                attempts,
            } => {
                assert_eq!(request_id.as_deref(), Some("req-stop"));
                assert_eq!(event_session, session_id);
                assert_eq!(reason, "failover_budget_exhausted");
                assert_eq!(attempts, default_max_attempts());
            }
            other => panic!("expected RunNeedsHumanResume, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_corrupt_budget_blocks_the_agent_and_surfaces_it() {
        let _home = TestHome::new();
        let provider: Arc<dyn Provider> = Arc::new(IdleProvider);
        let mut agent = Agent::new(provider, Registry::empty());
        let session_id = agent.session.id.clone();
        let path = budget_path(&session_id).expect("path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "{not json").expect("write corrupt");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        crate::server::register_session_event_sink(&session_id, tx);

        // Reload the budget the way a restart would, then try to spend.
        agent.restore_failover_budget();
        println!("agent budget unknown after reload: {}", agent.failover_budget_unknown());
        assert!(agent.failover_budget_unknown());
        assert!(!agent.consume_failover_attempt(Some("req-corrupt"), default_max_attempts()));

        let event = rx.try_recv().expect("an event was delivered");
        println!("surfaced event: {}", serde_json::to_string(&event).expect("event json"));
        match event {
            ServerEvent::RunNeedsHumanResume { reason, .. } => {
                assert_eq!(reason, "failover_budget_unreadable");
            }
            other => panic!("expected RunNeedsHumanResume, got {other:?}"),
        }
        // The corrupt file is left exactly as it was.
        assert_eq!(std::fs::read_to_string(&path).expect("still there"), "{not json");
    }

    /// Manual QA, happy path. Run it with a directory that stays on disk so the
    /// persisted record can be inspected:
    /// `HORON_AGENT_QA_HOME=/tmp/fb-qa cargo test -p horon-agent-app-core --lib manual_qa_happy -- --nocapture`
    #[test]
    fn manual_qa_happy() {
        let _home = TestHome::new();
        let session = "qa-happy";
        fresh(session);
        println!("load on a first run: {:?}", load(session).expect("first run"));

        let chain = "qa-chain-1";
        for attempt in 1..=4 {
            println!(
                "consume #{attempt}: {}",
                matches!(
                    try_consume(session, chain, default_max_attempts()),
                    ConsumeOutcome::Allowed { .. }
                )
            );
        }
        let stored = load(session).expect("readable").expect("stored");
        println!("stored record: {}", serde_json::to_string(&stored).expect("json"));
        println!(
            "record on disk: {}",
            std::fs::read_to_string(budget_path(session).expect("path")).expect("read")
        );

        let began = begin_chain(session, "qa-chain-2").expect("begin chain");
        println!(
            "after a new chain: chain_id={} attempts={}",
            began.chain_id, began.attempts
        );
        println!(
            "consume on the new chain: {}",
            matches!(
                try_consume(session, "qa-chain-2", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            )
        );
    }

    /// Manual QA, failure paths.
    /// `HORON_AGENT_QA_HOME=/tmp/fb-qa cargo test -p horon-agent-app-core --lib manual_qa_failure -- --nocapture`
    #[test]
    fn manual_qa_failure() {
        let _home = TestHome::new();

        let corrupt_session = "qa-corrupt";
        fresh(corrupt_session);
        let corrupt_path = budget_path(corrupt_session).expect("path");
        std::fs::create_dir_all(corrupt_path.parent().expect("parent")).expect("dir");
        std::fs::write(&corrupt_path, "{\"chain_id\":\"qa\"").expect("write corrupt");
        println!("corrupt load: {:?}", load(corrupt_session));
        println!("corrupt begin_chain: {:?}", begin_chain(corrupt_session, "qa-new"));
        println!(
            "corrupt consume: {}",
            matches!(
                try_consume(corrupt_session, "qa", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            )
        );
        println!(
            "corrupt file after the attempt: {}",
            std::fs::read_to_string(&corrupt_path).expect("untouched")
        );

        let unreadable_session = "qa-unreadable";
        fresh(unreadable_session);
        let unreadable_path = budget_path(unreadable_session).expect("path");
        std::fs::create_dir_all(&unreadable_path).expect("dir at record path");
        println!("unreadable load: {:?}", load(unreadable_session));
        println!(
            "unreadable consume: {}",
            matches!(
                try_consume(unreadable_session, "qa", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            )
        );
    }

    /// Manual QA, restart recovery.
    /// `HORON_AGENT_QA_HOME=/tmp/fb-qa cargo test -p horon-agent-app-core --lib manual_qa_restart -- --nocapture`
    #[test]
    fn manual_qa_restart() {
        let _home = TestHome::new();
        let session = "qa-restart";
        fresh(session);
        let chain = "qa-chain-r";
        for attempt in 1..=3 {
            println!(
                "before the restart, consume #{attempt}: {}",
                matches!(
                    try_consume(session, chain, default_max_attempts()),
                    ConsumeOutcome::Allowed { .. }
                )
            );
        }

        // Nothing but the file survives a restart.
        let restored = FailoverBudgetState::restored(load(session).expect("readable").expect("stored"));
        println!(
            "restored: chain_id={:?} attempts={} unknown={}",
            restored.chain_id(),
            restored.attempts(),
            restored.is_unknown()
        );
        println!(
            "fourth attempt after the restart: {}",
            matches!(
                try_consume(session, chain, default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            )
        );
    }

    /// A dangling symlink at the backup path is lost state, not a first run:
    /// the link resolves to nothing, so a following probe must not refund the chain.
    #[test]
    fn a_dangling_backup_symlink_is_lost_state_not_a_first_run() {
        let _home = TestHome::new();
        let session = "sess-dangling-backup";
        fresh(session);
        let path = budget_path(session).expect("path");
        let backup = path.with_extension("bak");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        // A platform that cannot create the link without a privilege skips this
        // case; every other platform runs it.
        let target = std::path::Path::new("/nonexistent/failover-budget-record");
        let Ok(()) = horon_agent_platform::fs::symlink_file(target, &backup) else {
            return;
        };

        assert!(!path.exists(), "the primary is gone");
        assert!(std::fs::symlink_metadata(&backup).is_ok(), "the link itself is there");
        assert!(
            std::fs::metadata(&backup).is_err(),
            "the link resolves to nothing, so a following probe sees no file"
        );

        let error = load(session).expect_err("a dangling backup still proves the state existed");
        println!("dangling-backup load error: {error}");
        assert!(
            matches!(error, BudgetLoadError::Corrupt { .. }),
            "expected Corrupt, got {error}"
        );
        assert!(
            !matches!(
                try_consume(session, "chain-dangling", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            ),
            "a lost record must not allow an attempt"
        );
        assert!(matches!(
            try_consume(session, "chain-dangling", default_max_attempts()),
            ConsumeOutcome::Unavailable(BudgetLoadError::Corrupt { .. })
        ));

        // With neither file present the same directory is a genuine first run.
        std::fs::remove_file(&backup).expect("remove the dangling link");
        assert!(std::fs::symlink_metadata(&backup).is_err());
        assert_eq!(load(session).expect("first run"), None);
        assert!(matches!(
            try_consume(session, "chain-dangling", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
    }

    /// Eight writers spending ten times each must still stop at the cap.
    ///
    /// Without a lock across the read-modify-write, two writers read the same
    /// count and both persist `attempts + 1`, so the chain spends more than
    /// `max`. The count is repeated over several rounds so an unlucky first
    /// interleaving cannot hide the race.
    #[test]
    fn concurrent_consumers_cannot_exceed_the_cap() {
        let _home = TestHome::new();
        let session = "sess-concurrent";
        for round in 1..=5 {
            fresh(session);
            let mut handles = Vec::new();
            for _ in 0..8 {
                let session = session.to_string();
                handles.push(std::thread::spawn(move || {
                    let mut allowed = 0;
                    let mut unavailable = 0;
                    for _ in 0..10 {
                        match try_consume(&session, "chain-race", default_max_attempts()) {
                            ConsumeOutcome::Allowed { .. } => allowed += 1,
                            ConsumeOutcome::Unavailable(error) => {
                                unavailable += 1;
                                println!("round thread: unavailable: {error}");
                            }
                            ConsumeOutcome::Exhausted { .. } => {}
                        }
                    }
                    (allowed, unavailable)
                }));
            }
            let mut allowed = 0;
            let mut unavailable = 0;
            for handle in handles {
                let (thread_allowed, thread_unavailable) = handle.join().expect("thread");
                allowed += thread_allowed;
                unavailable += thread_unavailable;
            }
            let stored = load(session).expect("readable").expect("stored");
            println!(
                "round {round}: {allowed} allowed, {unavailable} unavailable, persisted attempts {}",
                stored.attempts
            );
            assert_eq!(
                unavailable, 0,
                "round {round}: the lock is not contended long enough to time out"
            );
            assert_eq!(allowed, default_max_attempts(), "round {round}: allowed attempts");
            assert_eq!(
                stored.attempts,
                default_max_attempts(),
                "round {round}: persisted attempts"
            );
        }
    }

    /// A lock that cannot be taken blocks the spend instead of allowing it.
    ///
    /// Another process holding the record lock means this attempt could not be
    /// accounted for. Treating that as "allow" is exactly the over-spend the cap
    /// exists to prevent, so the outcome must be `Unavailable`.
    #[test]
    fn a_consume_that_cannot_take_the_lock_is_unavailable() {
        let _home = TestHome::new();
        let session = "sess-locked-out";
        fresh(session);
        let lock_path = budget_path(session).expect("path").with_extension("lock");
        std::fs::create_dir_all(lock_path.parent().expect("parent")).expect("dir");
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("open the lock");
        held.try_lock().expect("hold the record lock");

        let outcome = try_consume(session, "chain-locked", default_max_attempts());
        println!("locked-out consume: {outcome:?}");
        assert!(
            matches!(outcome, ConsumeOutcome::Unavailable(BudgetLoadError::Io { .. })),
            "expected Unavailable, got {outcome:?}"
        );
        assert!(!matches!(
            try_consume(session, "chain-locked", default_max_attempts()),
            ConsumeOutcome::Allowed { .. }
        ));
        assert_eq!(load(session).expect("readable"), None, "no attempt was recorded");

        let _ = held.unlock();
        assert!(
            matches!(
                try_consume(session, "chain-locked", default_max_attempts()),
                ConsumeOutcome::Allowed { .. }
            ),
            "the lock frees the spend again"
        );
    }
}

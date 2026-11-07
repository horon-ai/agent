//! Test helpers shared by the skill modules.
//!
//! The perf counters are process-wide, so a test that asserts a delta must hold
//! [`counter_lock`] for its whole measured window and run with
//! `--test-threads=1`. The lock is not reentrant: a test that already holds it
//! must not take it again.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

use super::{Skill, body};

static COUNTER_LOCK: Mutex<()> = Mutex::new(());

/// Serialize the tests that reset or read the process-wide counters.
pub(crate) fn counter_lock() -> MutexGuard<'static, ()> {
    COUNTER_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

thread_local! {
    /// Directory scans performed by this test thread.
    ///
    /// The process-wide `skill_dir_reads` counter is shared with every test
    /// running in parallel, so a delta read from it can only be trusted while
    /// nothing else runs. Discovery also records each scan here, which gives a
    /// test its own measurement no sibling test can contaminate.
    static DIR_SCANS: Cell<u64> = const { Cell::new(0) };
}

pub(crate) fn record_dir_scan() {
    DIR_SCANS.with(|scans| scans.set(scans.get() + 1));
}

pub(crate) fn dir_scans() -> u64 {
    DIR_SCANS.with(Cell::get)
}

/// Root that keeps the skill files written by [`test_skill`] alive for the whole
/// test binary. Bodies load lazily, so a test skill has to be file-backed.
static TEST_ROOT: LazyLock<tempfile::TempDir> =
    LazyLock::new(|| tempfile::TempDir::new().expect("create test skill root"));

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Build a file-backed skill whose body is `content`.
pub(crate) fn test_skill(name: &str, description: &str, content: &str) -> Skill {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let directory = TEST_ROOT.path().join(format!("{id:06}"));
    std::fs::create_dir_all(&directory).expect("create test skill dir");
    let path = directory.join("SKILL.md");
    std::fs::write(
        &path,
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{content}\n"),
    )
    .expect("write test skill");
    Skill {
        name: name.to_string(),
        description: description.to_string(),
        allowed_tools: None,
        path,
        body: body::new_cache(),
    }
}

//! Manual QA for the persistent monitor: the two restart proofs plan task 11
//! names. Both run against real files, a real journal root, and the real
//! persisted wake queue, so nothing here is a mock.
//!
//! 1. A file monitor is created, the file changes while no runtime is open, the
//!    journal is reopened, and exactly ONE coalesced wake describes the change.
//! 2. A command monitor appends one line to a counter file; after the simulated
//!    restart the counter still holds ONE line and the monitor reports
//!    `needs_rearm`.

use horon_agent_base::monitor::{
    Clock, CommandPolicy, ManualClock, MonitorRuntime, MonitorSpec, MonitorStatus, PermissiveCommandPolicy,
    SoftInterruptWakeSink, WakeSink,
};
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn new(home: &Path) -> Self {
        let previous = std::env::var_os("HORON_AGENT_HOME");
        unsafe {
            std::env::set_var("HORON_AGENT_HOME", home);
        }
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var("HORON_AGENT_HOME", previous),
                None => std::env::remove_var("HORON_AGENT_HOME"),
            }
        }
    }
}

fn open(root: &Path, session: &str) -> MonitorRuntime {
    let sink: Arc<dyn WakeSink> = Arc::new(SoftInterruptWakeSink::new(session));
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new());
    let policy: Arc<dyn CommandPolicy> = Arc::new(PermissiveCommandPolicy);
    MonitorRuntime::open_in(root, session, clock, sink, policy).expect("open the monitor runtime")
}

fn queued_wakes(session: &str) -> Vec<(Option<String>, String)> {
    horon_agent_base::soft_interrupt_store::load(session)
        .expect("load the persisted wake queue")
        .into_iter()
        .map(|message| (message.client_id, message.content))
        .collect()
}

#[test]
fn file_monitor_restores_and_reports_one_coalesced_wake() {
    let home = TempDir::new().expect("home");
    let _guard = HomeGuard::new(home.path());
    let journal = TempDir::new().expect("journal");
    let work = TempDir::new().expect("work");
    let watched = work.path().join("qa-watched.log");
    std::fs::write(&watched, "before\n").expect("seed the watched file");
    let session = "ses_qa_file_monitor";

    let monitor_id = {
        let mut runtime = open(journal.path(), session);
        let created = runtime
            .create(
                MonitorSpec::file(watched.to_string_lossy()),
                Some("qa file watch".to_string()),
            )
            .expect("create the file monitor");
        runtime.tick().expect("take the baseline");
        let baselined = runtime.inspect(&created.id).expect("inspect");
        assert_eq!(baselined.changes, 0, "the baseline is not a change");
        assert!(queued_wakes(session).is_empty(), "a baseline wakes nobody");
        created.id
    };

    std::fs::write(&watched, "before\nafter the process went down\n").expect("change the file while down");

    let mut runtime = open(journal.path(), session);
    let restored = runtime.list();
    assert_eq!(restored.len(), 1, "the file monitor came back");
    assert_eq!(restored[0].id, monitor_id, "it is the same monitor");
    assert_eq!(restored[0].status, MonitorStatus::Running);

    runtime.tick().expect("tick after the restore");
    let wakes = queued_wakes(session);
    assert_eq!(wakes.len(), 1, "exactly ONE coalesced wake: {wakes:?}");
    let (source_id, text) = &wakes[0];
    assert!(source_id.is_some(), "the wake carries its id as the source id");
    assert!(text.contains("qa-watched.log"), "the wake names the file: {text}");
    assert!(text.contains("changed"), "the wake describes the change: {text}");
    assert!(text.contains("qa file watch"), "the wake echoes the intent: {text}");

    runtime.tick().expect("a second tick with nothing new");
    assert_eq!(
        queued_wakes(session).len(),
        1,
        "an unchanged file wakes nobody a second time"
    );
}

#[test]
fn command_monitor_restore_keeps_one_line_and_reports_needs_rearm() {
    let home = TempDir::new().expect("home");
    let _guard = HomeGuard::new(home.path());
    let journal = TempDir::new().expect("journal");
    let work = TempDir::new().expect("work");
    let counter = work.path().join("qa-counter.txt");
    let command = format!("printf 'started\\n' >> {}", counter.display());
    let session = "ses_qa_command_monitor";

    let monitor_id = {
        let mut runtime = open(journal.path(), session);
        let created = runtime
            .create(
                MonitorSpec::command(command.clone()),
                Some("qa command watch".to_string()),
            )
            .expect("create the command monitor");
        runtime.start_command(&created.id).expect("start it once");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if std::fs::read_to_string(&counter).is_ok_and(|body| body.lines().count() == 1) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        runtime.wait_command(&created.id).expect("wait for the exit");
        created.id
    };

    let lines_before_restart = std::fs::read_to_string(&counter)
        .expect("the counter exists")
        .lines()
        .count();
    assert_eq!(lines_before_restart, 1, "one start wrote one line");

    let mut runtime = open(journal.path(), session);
    let restored = runtime.inspect(&monitor_id).expect("the monitor came back");
    assert_eq!(
        restored.status,
        MonitorStatus::NeedsRearm,
        "a restore never restarts a command"
    );
    assert_eq!(restored.spec.restart_policy.as_str(), "never");

    let lines_after_restart = std::fs::read_to_string(&counter)
        .expect("the counter still exists")
        .lines()
        .count();
    assert_eq!(
        lines_after_restart, 1,
        "the counter file still holds exactly ONE line: no process ran on restore"
    );

    runtime.tick().expect("a tick observes no new output");
    assert_eq!(
        std::fs::read_to_string(&counter).expect("counter").lines().count(),
        1,
        "a tick never starts the command either"
    );
}

/// A wake must land in the queue a running session actually reads.
///
/// The legacy JSON file is imported once as a baseline and never read again, so
/// a sink that only wrote that file would record a wake that never reaches the
/// conversation. This asserts the journal path: the delivered wake id is a live
/// input, and recovering that journal returns the summary as pending.
#[test]
fn a_delivered_wake_is_a_pending_input_in_the_journal() {
    use horon_agent_base::durable_input::DurableInputStore;
    use horon_agent_base::monitor::{MonitorRuntime, MonitorSpec};

    let dir = tempfile::tempdir().expect("temp dir");
    let session = "monitor-wake-reaches-the-journal";
    let target = dir.path().join("watched.txt");
    std::fs::write(&target, "one").expect("seed the watched file");

    // The pacer needs a clock it can see move: a real wake is spaced from the
    // previous one, so a frozen clock would legitimately coalesce forever.
    let clock = Arc::new(ManualClock::new());
    let sink: Arc<dyn WakeSink> = Arc::new(SoftInterruptWakeSink::with_root(dir.path(), session));
    let policy: Arc<dyn CommandPolicy> = Arc::new(PermissiveCommandPolicy);
    let mut monitors =
        MonitorRuntime::open_in(dir.path(), session, clock.clone(), sink, policy).expect("open the monitor runtime");

    monitors
        .create(MonitorSpec::file(target.to_string_lossy().to_string()), None)
        .expect("create the file monitor");
    monitors.tick().expect("the baseline wakes nobody");
    std::fs::write(&target, "one two").expect("change the watched file");
    clock.advance(std::time::Duration::from_secs(120));
    let delivered = monitors.tick().expect("the change delivers a wake");
    assert_eq!(delivered.len(), 1, "one change produces one wake: {delivered:?}");

    let wake_id = delivered[0].clone();
    let mut inputs = DurableInputStore::open_in(dir.path(), session).expect("open the journal");
    let state = inputs.state().expect("read the journal state").clone();
    assert!(
        state.inputs.contains_key(&wake_id),
        "the wake id must be a live journal input, got {:?}",
        state.inputs.keys().collect::<Vec<_>>()
    );

    let recovery = inputs.recover(&[]).expect("recover");
    assert_eq!(recovery.pending.len(), 1, "the wake is pending: {recovery:?}");
    assert!(
        !recovery.pending[0].content.is_empty(),
        "the summary must carry text: {recovery:?}"
    );
}

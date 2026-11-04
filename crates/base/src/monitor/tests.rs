use super::*;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::TempDir;

fn temp_root() -> TempDir {
    TempDir::new().expect("temp dir")
}

fn session(name: &str) -> String {
    format!("ses_monitor_{name}")
}

struct RecordingSink {
    queued: Mutex<Vec<(String, String)>>,
    busy: Mutex<bool>,
}

impl RecordingSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queued: Mutex::new(Vec::new()),
            busy: Mutex::new(false),
        })
    }

    fn set_busy(&self, busy: bool) {
        *self.busy.lock().unwrap() = busy;
    }

    fn summaries(&self) -> Vec<String> {
        self.queued
            .lock()
            .unwrap()
            .iter()
            .map(|(_, summary)| summary.clone())
            .collect()
    }

    fn wake_ids(&self) -> Vec<String> {
        self.queued.lock().unwrap().iter().map(|(id, _)| id.clone()).collect()
    }
}

impl WakeSink for RecordingSink {
    fn enqueue(&self, wake_id: &str, summary: &str) -> Result<String, WakeError> {
        if *self.busy.lock().unwrap() {
            return Err(WakeError::Busy);
        }
        let mut queued = self.queued.lock().unwrap();
        if let Some(existing) = queued.iter().find(|(id, _)| id == wake_id) {
            return Ok(existing.0.clone());
        }
        queued.push((wake_id.to_string(), summary.to_string()));
        Ok(wake_id.to_string())
    }

    fn is_queued(&self, wake_id: &str) -> Result<bool, WakeError> {
        Ok(self.queued.lock().unwrap().iter().any(|(id, _)| id == wake_id))
    }
}

fn runtime_with(root: &Path, name: &str, clock: Arc<ManualClock>, sink: Arc<RecordingSink>) -> MonitorRuntime {
    MonitorRuntime::open_in(root, &session(name), clock, sink, Arc::new(PermissiveCommandPolicy))
        .expect("open monitor runtime")
}

fn write_file(path: &Path, content: &str) {
    std::fs::write(path, content).expect("write watched file");
}

/// Poll a file until it holds exactly `lines` lines, with a bounded deadline.
fn wait_for_lines(path: &Path, lines: usize) -> bool {
    for _ in 0..500 {
        if std::fs::read_to_string(path).is_ok_and(|body| body.lines().count() == lines) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn first_change(runtime: &mut MonitorRuntime, id: &str) -> MonitorRecord {
    runtime.tick().expect("tick");
    runtime.inspect(id).expect("monitor exists")
}

#[test]
fn persistent_monitor_file_baseline_and_modification() {
    let root = temp_root();
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let dir = temp_root();
    let path = dir.path().join("watched.log");
    write_file(&path, "one\n");

    let mut runtime = runtime_with(root.path(), "modify", Arc::clone(&clock), Arc::clone(&sink));
    let created = runtime
        .create(
            MonitorSpec::file(path.to_string_lossy()),
            Some("watch the log".to_string()),
        )
        .expect("create file monitor");
    assert_eq!(created.status, MonitorStatus::Running);
    assert_eq!(created.spec.restart_policy, RestartPolicy::Never);

    let after_baseline = first_change(&mut runtime, &created.id);
    assert_eq!(after_baseline.changes, 0, "the first sample is a silent baseline");
    assert!(sink.summaries().is_empty(), "a baseline is not a change");

    write_file(&path, "one\ntwo\n");
    let after_change = first_change(&mut runtime, &created.id);
    assert_eq!(after_change.changes, 1);
    assert!(
        after_change
            .last_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("changed")),
        "got {:?}",
        after_change.last_summary
    );
    let summaries = sink.summaries();
    assert_eq!(summaries.len(), 1, "one change is one wake: {summaries:?}");
    assert!(summaries[0].contains("watched.log"), "got {}", summaries[0]);
    assert!(
        summaries[0].contains("watch the log"),
        "the intent is echoed: {}",
        summaries[0]
    );
}

#[test]
fn persistent_monitor_same_size_same_mtime_is_detected() {
    let dir = temp_root();
    let path = dir.path().join("same.bin");
    write_file(&path, "AAAA");

    let mut fresh = FileWatcher::new(path.clone());
    assert!(
        fresh.observe(sample_file(&path)).is_none(),
        "the first sample of an existing file is the baseline"
    );

    let metadata = std::fs::metadata(&path).expect("metadata");
    let modified = metadata.modified().expect("mtime");

    write_file(&path, "BBBB");
    let restored = filetime_like(&path, modified);
    assert!(restored, "the mtime must be restored for this test to mean anything");
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").len(),
        metadata.len(),
        "the size must be unchanged for this test to mean anything"
    );

    let change = fresh.observe(sample_file(&path)).expect("a digest change is a change");
    assert_eq!(change.kind, ChangeKind::Modified);
    assert!(change.summary.contains("changed"), "got {}", change.summary);
    let digest = change.detail["digest"].as_str().unwrap_or_default();
    assert_ne!(digest, "", "the change names the new digest");
}

fn filetime_like(path: &Path, modified: std::time::SystemTime) -> bool {
    let file = match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let times = std::fs::FileTimes::new().set_modified(modified);
    file.set_times(times).is_ok()
}

#[test]
fn persistent_monitor_creation_is_detected_for_a_missing_file() {
    let dir = temp_root();
    let path = dir.path().join("later.log");

    let mut watcher = FileWatcher::new(path.clone());
    assert!(
        watcher.observe(sample_file(&path)).is_none(),
        "a missing file is allowed at start"
    );
    write_file(&path, "hello\n");
    let change = watcher.observe(sample_file(&path)).expect("creation is a change");
    assert_eq!(change.kind, ChangeKind::Created);
    assert!(change.summary.contains("appeared"), "got {}", change.summary);
    assert_eq!(change.detail["bytes"], 6);
}

#[test]
fn persistent_monitor_deletion_and_replacement_are_reported() {
    let dir = temp_root();
    let path = dir.path().join("swap.log");
    write_file(&path, "first\n");

    let mut watcher = FileWatcher::new(path.clone());
    assert!(watcher.observe(sample_file(&path)).is_none());

    let replacement = dir.path().join("replacement.log");
    write_file(&replacement, "second content\n");
    std::fs::rename(&replacement, &path).expect("rename over the watched path");
    let replaced = watcher.observe(sample_file(&path)).expect("a rename-over is a change");
    assert_eq!(replaced.kind, ChangeKind::Replaced);
    assert!(replaced.summary.contains("replaced"), "got {}", replaced.summary);
    assert_eq!(replaced.detail["bytes"], 15);

    std::fs::remove_file(&path).expect("delete the watched file");
    let deleted = watcher.observe(sample_file(&path)).expect("deletion is a change");
    assert_eq!(deleted.kind, ChangeKind::Deleted);
    assert_eq!(deleted.detail["previous_bytes"], 15);
}

#[cfg(unix)]
#[test]
fn persistent_monitor_symlink_retarget_is_a_replacement() {
    let dir = temp_root();
    let left = dir.path().join("left.log");
    let right = dir.path().join("right.log");
    write_file(&left, "left\n");
    write_file(&right, "right\n");
    let link = dir.path().join("current.log");
    std::os::unix::fs::symlink(&left, &link).expect("symlink");

    let mut watcher = FileWatcher::new(link.clone());
    assert!(watcher.observe(sample_file(&link)).is_none());
    std::fs::remove_file(&link).expect("remove link");
    std::os::unix::fs::symlink(&right, &link).expect("retarget symlink");

    let change = watcher.observe(sample_file(&link)).expect("a retarget is a change");
    assert_eq!(change.kind, ChangeKind::Replaced);
    assert_eq!(change.detail["bytes"], 6);
    assert!(
        watcher
            .last()
            .is_some_and(|snapshot| snapshot.target.ends_with("right.log")),
        "the new regular file is followed"
    );
}

#[test]
fn persistent_monitor_oversized_file_pauses_without_a_partial_compare() {
    let dir = temp_root();
    let path = dir.path().join("huge.bin");
    let payload = vec![b'x'; (MAX_WATCHED_FILE_BYTES + 1) as usize];
    std::fs::write(&path, &payload).expect("write the oversized file");

    let mut watcher = FileWatcher::new(path.clone());
    let change = watcher.observe(sample_file(&path)).expect("too_large is a change");
    assert_eq!(change.kind, ChangeKind::Paused);
    assert_eq!(change.pause, Some(PauseReason::TooLarge));
    assert_eq!(change.detail["cap"], MAX_WATCHED_FILE_BYTES);
    assert!(
        watcher.observe(sample_file(&path)).is_none(),
        "the pause is reported once"
    );

    std::fs::write(&path, "small\n").expect("shrink the file");
    assert!(
        watcher.observe(sample_file(&path)).is_none(),
        "shrinking re-baselines rather than reporting a bogus modification"
    );
    assert_eq!(watcher.pause(), None, "the pause clears when a stable sample returns");
}

#[cfg(unix)]
#[test]
fn persistent_monitor_permission_denied_pauses() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_root();
    let path = dir.path().join("secret.log");
    write_file(&path, "secret\n");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&path, permissions).expect("chmod 000");

    let mut watcher = FileWatcher::new(path.clone());
    let change = watcher.observe(sample_file(&path)).expect("denied is a change");
    assert_eq!(change.kind, ChangeKind::Paused);
    assert_eq!(change.pause, Some(PauseReason::Denied));

    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(&path, permissions).expect("restore permissions");
}

#[test]
fn persistent_monitor_rejects_non_regular_targets() {
    let dir = temp_root();
    let directory = dir.path().join("a-directory");
    std::fs::create_dir(&directory).expect("create dir");
    let error = canonical_watch_path(&directory).expect_err("a directory is refused");
    assert_eq!(error.kind(), MonitorErrorKind::Invalid);
    assert!(error.message().contains("directory"), "got {}", error.message());

    let relative = Path::new("relative.log");
    let error = canonical_watch_path(relative).expect_err("a relative path is refused");
    assert!(error.message().contains("absolute"), "got {}", error.message());

    #[cfg(unix)]
    {
        let socket_path = dir.path().join("a-socket");
        let _listener = std::os::unix::net::UnixListener::bind(&socket_path).expect("bind socket");
        let error = canonical_watch_path(&socket_path).expect_err("a socket is refused");
        assert!(error.message().contains("socket"), "got {}", error.message());
    }
}

#[test]
fn persistent_monitor_discards_a_sample_that_changed_mid_read() {
    let dir = temp_root();
    let path = dir.path().join("busy.log");
    write_file(&path, "before\n");

    let mut mutated = false;
    let outcome = sample_file_with(&path, &mut || {
        mutated = true;
        write_file(&path, "a much longer replacement body\n");
    });
    assert!(mutated, "the hook ran");
    assert_eq!(outcome, SampleOutcome::Unstable, "a mid-read change is discarded");

    let mut watcher = FileWatcher::new(path.clone());
    assert!(watcher.observe(sample_file(&path)).is_none());
    let stable = watcher.observe(sample_file_with(&path, &mut || {}));
    assert!(stable.is_none(), "an unchanged sample proves nothing");
    assert!(watcher.last().is_some());
}

#[test]
fn persistent_monitor_streams_the_digest_in_chunks() {
    let dir = temp_root();
    let path = dir.path().join("chunked.bin");
    let payload: Vec<u8> = (0..(SAMPLE_CHUNK_BYTES * 2 + 7))
        .map(|index| (index % 251) as u8)
        .collect();
    std::fs::write(&path, &payload).expect("write");

    let outcome = sample_file(&path);
    let SampleOutcome::Stable(snapshot) = outcome else {
        panic!("expected a stable sample, got {outcome:?}");
    };
    assert_eq!(snapshot.len, payload.len() as u64);
    assert_eq!(snapshot.digest.len(), 64, "sha-256 in hex");
    let again = sample_file(&path);
    assert_eq!(
        SampleOutcome::Stable(snapshot),
        again,
        "the same bytes give the same digest"
    );
}

#[test]
fn persistent_monitor_bounds_monitors_per_session() {
    let root = temp_root();
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let dir = temp_root();
    let path = dir.path().join("one.log");
    write_file(&path, "x\n");

    let mut runtime = runtime_with(root.path(), "bounds", clock, sink);
    for _ in 0..MAX_MONITORS_PER_SESSION {
        runtime
            .create(MonitorSpec::file(path.to_string_lossy()), None)
            .expect("create up to the ceiling");
    }
    let error = runtime
        .create(MonitorSpec::file(path.to_string_lossy()), None)
        .expect_err("the seventeenth monitor is refused");
    assert_eq!(error.kind(), MonitorErrorKind::Limit);
    assert!(error.message().contains("16"), "got {}", error.message());
}

#[test]
fn persistent_monitor_bounds_inline_output_on_a_utf8_boundary() {
    struct NullSpool;
    impl SpoolSink for NullSpool {
        fn write(&mut self, _bytes: &[u8]) -> Result<(), SpoolBlocked> {
            Ok(())
        }
        fn rotated_bytes(&self) -> u64 {
            0
        }
    }

    let mut output = BoundedOutput::new(Box::new(NullSpool));
    for _ in 0..4_000 {
        output.push("한글 줄입니다\n");
    }
    assert!(output.stats().inline_bytes <= INLINE_OUTPUT_BYTES);
    assert!(output.inline().is_char_boundary(0));
    assert!(
        !output.inline().contains('\u{FFFD}'),
        "the cut never splits a character"
    );
    assert!(output.drain() > 0, "a spool that never blocks drains the queued lines");
}

#[test]
fn persistent_monitor_ten_thousand_lines_stay_bounded() {
    let dir = temp_root();
    let spool_dir = dir.path().join("spool");
    let mut output = BoundedOutput::new(Box::new(FileSpool::new(spool_dir.clone()).expect("spool")));
    for index in 0..10_000 {
        output.push(&format!("line {index} with some payload to fill the spool\n"));
        if index % 512 == 0 {
            output.drain();
        }
    }
    let stats = output.stats();
    assert!(stats.inline_bytes <= INLINE_OUTPUT_BYTES, "inline is capped: {stats:?}");
    assert!(stats.queued <= OUTPUT_QUEUE_LINES, "the queue is capped: {stats:?}");
    assert!(!stats.overflowed, "a writable spool never overflows: {stats:?}");
    let spool_files = std::fs::read_dir(&spool_dir)
        .expect("read spool dir")
        .filter_map(Result::ok)
        .count();
    assert!(
        spool_files <= SPOOL_FILE_COUNT,
        "at most two spool files: {spool_files}"
    );
    let total: u64 = std::fs::read_dir(&spool_dir)
        .expect("read spool dir")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum();
    assert!(
        total <= SPOOL_FILE_BYTES * SPOOL_FILE_COUNT as u64,
        "spool bytes stay bounded: {total}"
    );
    assert!(
        stats.spooled_bytes + stats.dropped_bytes >= 10_000,
        "every line is either spooled or counted: {stats:?}"
    );
}

#[test]
fn persistent_monitor_overflow_records_the_dropped_count() {
    struct BlockedSpool {
        rotated: u64,
    }
    impl SpoolSink for BlockedSpool {
        fn write(&mut self, _bytes: &[u8]) -> Result<(), SpoolBlocked> {
            Err(SpoolBlocked)
        }
        fn rotated_bytes(&self) -> u64 {
            self.rotated
        }
    }

    let mut output = BoundedOutput::new(Box::new(BlockedSpool { rotated: 4_096 }));
    for index in 0..(OUTPUT_QUEUE_LINES * 4) {
        output.push(&format!("line {index}\n"));
    }
    let stats = output.stats();
    assert_eq!(stats.queued, OUTPUT_QUEUE_LINES, "the queue never grows past its bound");
    assert!(stats.overflowed, "a blocked spool raises the overflow state");
    assert!(stats.dropped_bytes > 0, "the dropped count is recorded: {stats:?}");
    assert_eq!(stats.rotated_bytes, 4_096, "rotated bytes are exposed, not hidden");
}

#[test]
fn persistent_monitor_wake_pacer_spends_a_token_then_coalesces() {
    let clock = ManualClock::new();
    let start = clock.now();
    let mut pacer = WakePacer::default();

    assert_eq!(
        pacer.decide("m1", start),
        WakeDecision::Emit,
        "the first change is immediate"
    );
    pacer.record("m1", start);
    assert_eq!(
        pacer.decide("m1", start),
        WakeDecision::Coalesce,
        "one per monitor per 30s"
    );

    clock.advance(Duration::from_secs(31));
    assert_eq!(
        pacer.decide("m1", clock.now()),
        WakeDecision::Emit,
        "30s later that monitor is free"
    );

    let mut emitted = 0;
    for step in 0..10 {
        clock.advance(Duration::from_secs(1));
        let id = format!("m{step}");
        if pacer.decide(&id, clock.now()) == WakeDecision::Emit {
            pacer.record(&id, clock.now());
            emitted += 1;
        }
    }
    assert_eq!(
        emitted, 5,
        "the first change plus five more fills the six-token window for this minute"
    );
    assert!(pacer.next_token_at(clock.now()).is_some(), "the window is full");

    clock.advance(Duration::from_secs(60));
    assert_eq!(
        pacer.decide("late", clock.now()),
        WakeDecision::Emit,
        "the window refills"
    );
    assert!(pacer.next_token_at(clock.now()).is_none(), "one token is free again");
}

#[test]
fn persistent_monitor_coalesced_wake_keeps_the_latest_and_a_count() {
    let mut wake = CoalescedWake::new(3, "first change".to_string());
    assert_eq!(wake.text("monitor m"), "monitor m: first change");
    wake.fold(9, "latest change".to_string());
    let text = wake.text("monitor m");
    assert!(text.contains("latest change"), "got {text}");
    assert!(text.contains("2 changes coalesced"), "got {text}");
    assert_eq!(wake.monitor_seq, 9);
}

#[test]
fn persistent_monitor_restores_a_file_watch_and_reports_the_offline_change() {
    let root = temp_root();
    let dir = temp_root();
    let path = dir.path().join("offline.log");
    write_file(&path, "before\n");
    let name = "restore_file";

    {
        let clock = Arc::new(ManualClock::new());
        let sink = RecordingSink::new();
        let mut runtime = runtime_with(root.path(), name, clock, sink);
        let created = runtime
            .create(MonitorSpec::file(path.to_string_lossy()), None)
            .expect("create");
        let baselined = first_change(&mut runtime, &created.id);
        assert_eq!(baselined.changes, 0, "the baseline is not a change");
    }

    write_file(&path, "before\nafter the process went down\n");

    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime = runtime_with(root.path(), name, Arc::clone(&clock), Arc::clone(&sink));
    let monitors = runtime.list();
    assert_eq!(monitors.len(), 1, "the file monitor is restored");
    assert_eq!(monitors[0].status, MonitorStatus::Running);

    runtime.tick().expect("tick after restore");
    let summaries = sink.summaries();
    assert_eq!(summaries.len(), 1, "one coalesced wake: {summaries:?}");
    assert!(summaries[0].contains("offline.log"), "got {}", summaries[0]);
    assert!(summaries[0].contains("changed"), "got {}", summaries[0]);
}

#[test]
fn persistent_monitor_command_restore_never_starts_a_process() {
    let root = temp_root();
    let dir = temp_root();
    let counter = dir.path().join("counter.txt");
    let command = format!("printf 'started\\n' >> {} && sleep 30", counter.display());
    let name = "restore_command";

    {
        let clock = Arc::new(ManualClock::new());
        let sink = RecordingSink::new();
        let mut runtime = runtime_with(root.path(), name, clock, sink);
        let created = runtime
            .create(MonitorSpec::command(command.clone()), None)
            .expect("create command monitor");
        runtime.start_command(&created.id).expect("start once");
        assert!(wait_for_lines(&counter, 1), "the started command appended its line");
        runtime.cancel(&created.id, "test finished").expect("cancel");
    }

    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let runtime = runtime_with(root.path(), name, clock, sink);
    assert!(
        runtime.list().is_empty(),
        "a cancelled monitor is not restored, so nothing can start again"
    );
    let stopped = runtime
        .store()
        .state()
        .monitors
        .values()
        .filter(|entry| entry.stopped.is_some())
        .count();
    assert_eq!(stopped, 1, "the journal records the stop");
    assert_eq!(
        std::fs::read_to_string(&counter).expect("counter").lines().count(),
        1,
        "the restore started no process"
    );
}

#[test]
fn persistent_monitor_command_reports_needs_rearm_and_one_line_after_rearm() {
    let root = temp_root();
    let dir = temp_root();
    let counter = dir.path().join("counter.txt");
    let command = format!("printf 'started\\n' >> {}", counter.display());
    let name = "rearm_command";

    let id = {
        let clock = Arc::new(ManualClock::new());
        let sink = RecordingSink::new();
        let mut runtime = runtime_with(root.path(), name, clock, sink);
        let created = runtime
            .create(MonitorSpec::command(command.clone()), None)
            .expect("create command monitor");
        created.id
    };

    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime = runtime_with(root.path(), name, Arc::clone(&clock), Arc::clone(&sink));
    let restored = runtime.inspect(&id).expect("restored monitor");
    assert_eq!(
        restored.status,
        MonitorStatus::NeedsRearm,
        "a restore never restarts a command"
    );
    assert!(
        !counter.exists(),
        "no process ran during the restore, so the counter file does not exist yet"
    );

    runtime.start_command(&id).expect("rearm starts the command");
    for _ in 0..200 {
        if std::fs::read_to_string(&counter).is_ok_and(|body| body.lines().count() == 1) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let body = std::fs::read_to_string(&counter).expect("counter exists");
    assert_eq!(
        body.lines().count(),
        1,
        "exactly ONE line after the restart and rearm: {body:?}"
    );
    runtime.cancel(&id, "test finished").expect("cancel");
}

#[test]
fn persistent_monitor_command_output_and_exit_are_observed() {
    let root = temp_root();
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime = runtime_with(root.path(), "command_output", clock, sink);

    let created = runtime
        .create(
            MonitorSpec::command("printf 'first\\nsecond\\n'; printf 'oops\\n' >&2; exit 3"),
            None,
        )
        .expect("create");
    runtime.start_command(&created.id).expect("start");

    let delivered = runtime.wait_command(&created.id).expect("wait for the exit");
    assert_eq!(delivered.len(), 1, "one wake for the whole run: {delivered:?}");

    let checkpoint = runtime
        .store()
        .state()
        .monitors
        .get(&created.id)
        .and_then(|entry| entry.last_checkpoint.clone())
        .expect("a checkpoint was journaled");
    assert!(checkpoint.summary.contains("exited"), "got {}", checkpoint.summary);
    let changes = checkpoint.detail["changes"].as_array().expect("changes");
    let kinds: Vec<&str> = changes.iter().filter_map(|change| change["kind"].as_str()).collect();
    assert!(kinds.contains(&"output"), "got {kinds:?}");
    assert!(kinds.contains(&"exited"), "got {kinds:?}");
    let exited = changes
        .iter()
        .find(|change| change["kind"] == "exited")
        .expect("an exit change");
    assert_eq!(exited["detail"]["exit_code"], 3);
    let output = exited["detail"]["output"].as_str().unwrap_or_default();
    assert!(output.contains("first"), "got {output}");
    assert!(output.contains("second"), "got {output}");
    assert!(output.contains("oops"), "stderr is observed too: {output}");
}

#[test]
fn persistent_monitor_command_permission_denial_starts_nothing() {
    struct DenyAll;

    impl CommandPolicy for DenyAll {
        fn check(&self, _command: &str, _cwd: Option<&Path>, _session_id: &str) -> Result<(), String> {
            Err("this session may not run commands".to_string())
        }

        fn working_dir(&self, requested: Option<&Path>, _session_id: &str) -> Option<std::path::PathBuf> {
            requested.map(Path::to_path_buf)
        }
    }

    let root = temp_root();
    let dir = temp_root();
    let marker = dir.path().join("should-not-exist");
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime =
        MonitorRuntime::open_in(root.path(), &session("denied"), clock, sink, Arc::new(DenyAll)).expect("open");

    let created = runtime
        .create(MonitorSpec::command(format!("touch {}", marker.display())), None)
        .expect("create");
    let error = runtime.start_command(&created.id).expect_err("the policy refuses");
    assert_eq!(error.kind(), MonitorErrorKind::Invalid);
    assert!(!marker.exists(), "a refused command never runs");
}

#[test]
fn persistent_monitor_busy_queue_keeps_the_pending_wake() {
    let root = temp_root();
    let dir = temp_root();
    let path = dir.path().join("busy.log");
    write_file(&path, "one\n");
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime = runtime_with(root.path(), "busy", Arc::clone(&clock), Arc::clone(&sink));

    let created = runtime
        .create(MonitorSpec::file(path.to_string_lossy()), None)
        .expect("create");
    runtime.tick().expect("baseline");

    sink.set_busy(true);
    write_file(&path, "one\ntwo\n");
    runtime.tick().expect("tick while busy");
    assert!(sink.summaries().is_empty(), "a busy queue accepts nothing");

    let pending: Vec<String> = runtime
        .store()
        .state()
        .pending_wakes()
        .map(|(_, wake)| wake.summary.clone())
        .collect();
    assert_eq!(pending.len(), 1, "the pending wake is preserved: {pending:?}");

    sink.set_busy(false);
    runtime.deliver().expect("deliver");
    assert_eq!(sink.summaries().len(), 1, "the preserved wake is delivered next");
    let state = runtime.store().state();
    let unacked = state.pending_wakes().count();
    assert_eq!(unacked, 0, "a delivered wake is acked");
    assert_eq!(state.wakes.len(), 1, "the busy attempt reused one wake row, not two");
    assert_eq!(
        runtime.inspect(&created.id).expect("monitor").status,
        MonitorStatus::Running
    );
}

#[test]
fn persistent_monitor_wake_ack_before_a_kill_is_redelivered_once() {
    let root = temp_root();
    let dir = temp_root();
    let path = dir.path().join("ack.log");
    write_file(&path, "one\n");
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime = runtime_with(root.path(), "ack_kill", Arc::clone(&clock), Arc::clone(&sink));

    let created = runtime
        .create(MonitorSpec::file(path.to_string_lossy()), None)
        .expect("create");
    runtime.tick().expect("baseline");

    let wake_id = crate::durable_runtime::new_id();
    let pending = crate::durable_runtime::Payload::WakePending(crate::durable_runtime::WakePending {
        monitor_id: created.id.clone(),
        monitor_seq: 7,
        summary: "queued but never acked".to_string(),
    });
    runtime.store_mut().append(&wake_id, pending).expect("pending");
    sink.enqueue(&wake_id, "queued but never acked").expect("enqueue");
    drop(runtime);

    let mut reopened = runtime_with(root.path(), "ack_kill", Arc::new(ManualClock::new()), Arc::clone(&sink));
    assert_eq!(
        sink.summaries(),
        vec!["queued but never acked".to_string()],
        "reopening redelivers the pending wake, and the dedup keeps it to one entry"
    );
    assert_eq!(sink.wake_ids(), vec![wake_id.clone()], "the recorded id is reused");
    assert_eq!(reopened.store().state().pending_wakes().count(), 0, "the ack now lands");
    let again = reopened.redeliver_pending().expect("redeliver again");
    assert!(again.is_empty(), "an acked wake is never redelivered");
    assert_eq!(
        reopened.inspect(&created.id).expect("monitor").status,
        MonitorStatus::Running
    );
}

#[test]
fn persistent_monitor_unacked_wake_is_redelivered_after_a_restart() {
    let root = temp_root();
    let dir = temp_root();
    let path = dir.path().join("pending.log");
    write_file(&path, "one\n");
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime = runtime_with(root.path(), "pending_restart", clock, sink);
    runtime
        .create(MonitorSpec::file(path.to_string_lossy()), None)
        .expect("create");
    runtime.tick().expect("baseline");

    let wake_id = crate::durable_runtime::new_id();
    let payload = crate::durable_runtime::Payload::WakePending(crate::durable_runtime::WakePending {
        monitor_id: "unused".to_string(),
        monitor_seq: 4,
        summary: "pending from before the crash".to_string(),
    });
    runtime.store_mut().append(&wake_id, payload).expect("pending");
    drop(runtime);

    let sink_again = RecordingSink::new();
    let mut reopened = runtime_with(
        root.path(),
        "pending_restart",
        Arc::new(ManualClock::new()),
        Arc::clone(&sink_again),
    );
    assert_eq!(
        sink_again.wake_ids(),
        vec![wake_id.clone()],
        "the recorded id is reused"
    );
    assert_eq!(
        sink_again.summaries(),
        vec!["pending from before the crash".to_string()]
    );

    let second = reopened.redeliver_pending().expect("redeliver again");
    assert!(second.is_empty(), "the ack makes the second pass a no-op");
    assert_eq!(sink_again.summaries().len(), 1, "the wake is delivered exactly once");
}

#[test]
fn persistent_monitor_cancel_wins_a_race_with_a_command_exit() {
    let root = temp_root();
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime = runtime_with(root.path(), "cancel_race", clock, sink);
    let created = runtime
        .create(MonitorSpec::command("printf 'done\\n'"), None)
        .expect("create");

    runtime.start_command(&created.id).expect("start");
    runtime
        .cancel(&created.id, "cancelled before the exit was observed")
        .expect("cancel");
    let cancelled = runtime.inspect(&created.id).expect("monitor");
    assert_eq!(cancelled.status, MonitorStatus::Stopped);

    runtime.tick().expect("a tick after the cancel observes nothing");
    let after = runtime.inspect(&created.id).expect("monitor");
    assert_eq!(
        after.status,
        MonitorStatus::Stopped,
        "the cancel is not overwritten by the exit"
    );
    assert_eq!(after.changes, cancelled.changes);
}

#[test]
fn persistent_monitor_journal_failure_publishes_no_success() {
    let root = temp_root();
    let dir = temp_root();
    let path = dir.path().join("fail.log");
    write_file(&path, "one\n");
    let clock = Arc::new(ManualClock::new());
    let sink = RecordingSink::new();
    let mut runtime = runtime_with(root.path(), "journal_fail", clock, Arc::clone(&sink));
    let created = runtime
        .create(MonitorSpec::file(path.to_string_lossy()), None)
        .expect("create");
    runtime.tick().expect("baseline");

    std::fs::write(runtime.store().journal_path(), b"{not json}\n").expect("corrupt the journal");
    write_file(&path, "one\ntwo\n");
    let error = runtime.tick().expect_err("a corrupt journal refuses the checkpoint");
    assert_eq!(error.kind(), MonitorErrorKind::Journal);
    assert!(
        sink.summaries().is_empty(),
        "no wake is delivered when the checkpoint could not be written"
    );
    assert_eq!(runtime.inspect(&created.id).expect("monitor").wakes, 0);
}

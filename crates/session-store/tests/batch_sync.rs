//! One log flush confirms a whole batched commit, and a failed flush publishes no head.
use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod support;

type Observations = Arc<Mutex<Vec<IoPoint>>>;

/// A writer over a fresh root with one committed creation event.
struct Fixture {
    paths: SessionPaths,
    id: SessionId,
    writer: BlockingWriter,
    root: SealedEvent,
    points: Observations,
    armed: Arc<AtomicBool>,
}

/// Opens the fixture, committing the creation event outside any batch so the
/// batch below owns every observation it is asked about.
fn fixture(failure: Option<IoPoint>) -> Fixture {
    let paths = SessionPaths::new(support::temp_root("batch-sync"));
    let id = SessionId::parse("batch").unwrap();
    let points: Observations = Arc::default();
    let observed = Arc::clone(&points);
    let armed = Arc::new(AtomicBool::new(false));
    let fail = Arc::clone(&armed);
    let mut writer = BlockingWriter::with_hook(
        paths.clone(),
        id.clone(),
        Box::new(move |point| {
            observed.lock().unwrap().push(point);
            if Some(point) == failure && fail.swap(false, Ordering::SeqCst) {
                return Err(std::io::Error::from_raw_os_error(28));
            }
            Ok(())
        }),
    )
    .unwrap();
    writer.enable_cache(ReadProbe::default());
    let root = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        None,
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        serde_json::json!({"metadata": SessionMetadata::default(), "branch_root": false}),
    )
    .seal()
    .unwrap();
    writer.append(&root).unwrap();
    points.lock().unwrap().clear();
    Fixture {
        paths,
        id,
        writer,
        root,
        points,
        armed,
    }
}

impl Fixture {
    fn message(&self, parent: &SealedEvent, text: &str) -> SealedEvent {
        EventDraft::new(
            parent.envelope().session_id.clone(),
            EventId::new_v4(),
            Some(parent.envelope().event_id.clone()),
            OriginId::new_v4(),
            chrono::DateTime::UNIX_EPOCH,
            EventKind::new(KnownEventKind::MessageAppended),
            serde_json::json!({"message": StoredMessage { original_input: None, arg_repairs: None,
            degradations: None,
                turn_start: false,
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::User,
                content: vec![ContentBlock::Text { text: text.into(), cache_control: None }],
                display_role: None, timestamp: None, tool_duration_ms: None, token_usage: None,
            }}),
        )
        .seal()
        .unwrap()
    }

    fn log(&self) -> PathBuf {
        self.paths.events_log(&self.id).unwrap()
    }

    fn head(&self) -> PathBuf {
        self.paths.session_dir(&self.id).unwrap().join("local-head.json")
    }

    fn count(&self, point: IoPoint) -> usize {
        self.points
            .lock()
            .unwrap()
            .iter()
            .filter(|&&observed| observed == point)
            .count()
    }

    /// Discards the observations collected so far so one step can be measured alone.
    fn forget(&self) {
        self.points.lock().unwrap().clear();
    }
}

/// Commits like a save does: every original, then one log flush, then the head.
///
/// The head is published only after the flush returned, so a head on disk never
/// names an event whose log bytes this store did not confirm.
fn commit(fixture: &mut Fixture, events: &[SealedEvent]) -> std::result::Result<(), StoreError> {
    let head = fixture.head();
    let selected = serde_json::to_vec(&events.last().unwrap().envelope().event_id).unwrap();
    fixture.writer.batched(|writer| {
        for event in events {
            writer.append(event)?;
        }
        writer.flush_pending_log()?;
        fs::write(&head, &selected)?;
        Ok(())
    })
}

fn log_lines(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn lines_naming(path: &Path, events: &[SealedEvent]) -> usize {
    let lines = log_lines(path);
    events
        .iter()
        .filter(|event| {
            lines
                .iter()
                .any(|line| line["event_id"] == event.envelope().event_id.as_str())
        })
        .count()
}

fn occurrences(path: &Path, event: &SealedEvent) -> usize {
    log_lines(path)
        .iter()
        .filter(|line| line["event_id"] == event.envelope().event_id.as_str())
        .count()
}

#[test]
fn a_three_event_batch_confirms_with_one_log_flush() {
    // Given a session with one committed event and a batch of three.
    let mut fixture = fixture(None);
    let first = fixture.message(&fixture.root.clone(), "first");
    let second = fixture.message(&first, "second");
    let third = fixture.message(&second, "third");
    let events = vec![first, second, third];
    let log = fixture.log();
    let prefix = fs::read(&log).unwrap();
    // When the batch commits the three originals behind one flush.
    commit(&mut fixture, &events).unwrap();
    // Then exactly one log flush confirmed all three.
    let log_sync = fixture.count(IoPoint::LogFileSync);
    println!("log_sync_per_batch={log_sync} events={}", events.len());
    assert_eq!(log_sync, 1, "a batch must confirm its log with one flush");
    assert_eq!(
        fixture.count(IoPoint::LogWrite),
        3,
        "every original must still be written"
    );
    assert_eq!(
        fixture.count(IoPoint::LogPublished),
        3,
        "every original must still be published"
    );
    assert_eq!(fixture.count(IoPoint::LogDirectorySync), 3);
    // And the durable log holds the untouched prefix plus one line per event.
    let log_bytes = fs::read(&log).unwrap();
    assert!(
        log_bytes.starts_with(&prefix),
        "the committed prefix must stay byte-identical"
    );
    assert_eq!(log_lines(&log).len(), 4);
    for event in &events {
        assert_eq!(occurrences(&log, event), 1, "each event must appear exactly once");
    }
    // And the head is published after the flush and names the batch's last event.
    assert_eq!(
        serde_json::from_slice::<EventId>(&fs::read(fixture.head()).unwrap()).unwrap(),
        events[2].envelope().event_id
    );

    // When the same events are replayed as a second batch.
    fixture.forget();
    commit(&mut fixture, &events).unwrap();
    // Then nothing is rewritten and the log stays byte-identical.
    assert_eq!(
        fs::read(&log).unwrap(),
        log_bytes,
        "a replay must not touch committed bytes"
    );
    assert_eq!(
        fixture.count(IoPoint::LogWrite),
        0,
        "a replay must not rewrite originals"
    );
    assert_eq!(
        fixture.count(IoPoint::LogFileSync),
        1,
        "a replay still costs at most one flush"
    );

    drop(fixture.writer);
    fs::remove_dir_all(fixture.paths.root()).unwrap();
}

#[test]
fn an_append_outside_a_batch_still_confirms_with_its_own_flush() {
    // Given a session with one committed event.
    let mut fixture = fixture(None);
    let next = fixture.message(&fixture.root.clone(), "alone");
    // When one event is appended outside any batch.
    fixture.forget();
    fixture.writer.append(&next).unwrap();
    // Then that append flushed the log itself.
    assert_eq!(fixture.count(IoPoint::LogFileSync), 1);
    // And it left no pending flush behind: an empty batch flushes nothing.
    fixture.forget();
    fixture
        .writer
        .batched(|_| -> std::result::Result<(), StoreError> { Ok(()) })
        .unwrap();
    assert_eq!(fixture.count(IoPoint::LogFileSync), 0, "an empty batch must not flush");
    assert_eq!(full_scan(&fixture.paths, &fixture.id).unwrap().events.len(), 2);

    drop(fixture.writer);
    fs::remove_dir_all(fixture.paths.root()).unwrap();
}

#[test]
fn a_failed_deferred_flush_returns_err_and_publishes_no_head() {
    // Given a batch of three events and a log flush that fails once.
    let mut fixture = fixture(Some(IoPoint::LogFileSync));
    let first = fixture.message(&fixture.root.clone(), "first");
    let second = fixture.message(&first, "second");
    let third = fixture.message(&second, "third");
    let events = vec![first, second, third];
    let log = fixture.log();
    fixture.armed.store(true, Ordering::SeqCst);
    // When the batch commits and its one flush fails.
    let attempt = commit(&mut fixture, &events);
    // Then the batch reports the failure and publishes no head.
    assert!(attempt.is_err(), "a failed log flush must not be acknowledged");
    assert!(!fixture.head().exists(), "no head may name an event whose flush failed");

    // When the same events are committed again.
    commit(&mut fixture, &events).unwrap();
    let retried = lines_naming(&log, &events);
    println!(
        "batched={} retry_lines={retried}",
        if attempt.is_err() { "Err" } else { "Ok" }
    );
    // Then each event is in the log exactly once and the head names the last one.
    assert_eq!(retried, 3);
    for event in &events {
        assert_eq!(occurrences(&log, event), 1, "a retry must not duplicate an event");
    }
    assert_eq!(log_lines(&log).len(), 4, "the log holds one line per distinct event");
    assert_eq!(
        serde_json::from_slice::<EventId>(&fs::read(fixture.head()).unwrap()).unwrap(),
        events[2].envelope().event_id
    );
    let scanned = full_scan(&fixture.paths, &fixture.id).unwrap();
    println!(
        "log_lines={} distinct_events={}",
        log_lines(&log).len(),
        scanned.events.len()
    );
    assert_eq!(scanned.events.len(), 4);

    drop(fixture.writer);
    fs::remove_dir_all(fixture.paths.root()).unwrap();
}

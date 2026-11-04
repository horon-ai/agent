//! Full-history pages cost the page, and never the session.
//!
//! Every page here is compared against the replay it replaces, so the index can
//! only make a page cheaper, never different.

use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use serde_json::json;
use std::path::Path;

mod support;

const TEXT: &str = "page fixture message with a stable, repeated body";

fn created(id: &SessionId) -> SealedEvent {
    EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        None,
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        json!({"metadata": SessionMetadata::default(), "branch_root": false}),
    )
    .seal()
    .unwrap()
}

fn message(id: &str, text: &str) -> StoredMessage {
    StoredMessage {
        turn_start: false,
        id: id.to_owned(),
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.into(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn appended(parent: &SealedEvent, index: usize, text: &str) -> SealedEvent {
    EventDraft::new(
        parent.envelope().session_id.clone(),
        EventId::new_v4(),
        Some(parent.envelope().event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(1),
        EventKind::new(KnownEventKind::MessageAppended),
        json!({"message": message(&format!("m{index}"), text)}),
    )
    .seal()
    .unwrap()
}

struct Seeded {
    paths: SessionPaths,
    id: SessionId,
    head: EventId,
    events: Vec<EventId>,
}

fn seed(directory: &Path, name: &str, messages: usize) -> Seeded {
    let paths = SessionPaths::new(directory);
    let id = SessionId::parse(name).unwrap();
    let session = paths.session_dir(&id).unwrap();
    std::fs::create_dir_all(&session).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let root = created(&id);
    let mut bytes = encode_record_line(root.envelope()).unwrap();
    let mut previous = root;
    let mut events = Vec::new();
    for index in 0..messages {
        let next = appended(&previous, index, &format!("{TEXT} {index}"));
        bytes.extend(encode_record_line(next.envelope()).unwrap());
        previous = next;
        events.push(previous.envelope().event_id.clone());
    }
    let log = paths.events_log(&id).unwrap();
    std::fs::write(&log, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    Seeded {
        paths,
        id,
        head: previous.envelope().event_id.clone(),
        events,
    }
}

fn replay_page(seeded: &Seeded, before: Option<&str>, limit: usize) -> Vec<StoredMessage> {
    let graph = EventGraph::open(&seeded.paths, &seeded.id).unwrap();
    let projection = graph.replay(Some(&seeded.head)).unwrap();
    let history: Vec<StoredMessage> = projection
        .messages()
        .iter()
        .map(|message| message.message.clone())
        .collect();
    let end = match before {
        Some(id) => history.iter().position(|message| message.id == id).unwrap(),
        None => history.len(),
    };
    history[end.saturating_sub(limit)..end].to_vec()
}

fn indexed_page(seeded: &Seeded, before: Option<&str>, limit: usize) -> Vec<StoredMessage> {
    let probe = ReadProbe::default();
    let pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &probe).unwrap();
    let end = match before {
        Some(id) => pages.position_of_message(id).unwrap().unwrap(),
        None => pages.len(),
    };
    pages
        .page(end.saturating_sub(limit), end)
        .unwrap()
        .into_iter()
        .map(|message| message.message)
        .collect()
}

fn digest(messages: &[StoredMessage]) -> String {
    let mut bytes = Vec::new();
    for message in messages {
        bytes.extend(serde_json::to_vec(message).unwrap());
    }
    sha256_hex(&bytes)
}

#[test]
fn first_middle_and_last_page_match_full_replay_exactly() {
    let directory = support::temp_root("history-page-parity");
    let seeded = seed(&directory, "parity", 500);

    let first = indexed_page(&seeded, None, 20);
    assert_eq!(digest(&first), digest(&replay_page(&seeded, None, 20)));
    assert_eq!(first.first().unwrap().id, "m480");
    assert_eq!(first.last().unwrap().id, "m499");

    let middle_cursor = "m300";
    let middle = indexed_page(&seeded, Some(middle_cursor), 20);
    assert_eq!(digest(&middle), digest(&replay_page(&seeded, Some(middle_cursor), 20)));
    assert_eq!(middle.first().unwrap().id, "m280");
    assert_eq!(middle.last().unwrap().id, "m299");

    let last = indexed_page(&seeded, Some("m20"), 20);
    assert_eq!(digest(&last), digest(&replay_page(&seeded, Some("m20"), 20)));
    assert_eq!(last.first().unwrap().id, "m0");
    assert_eq!(last.last().unwrap().id, "m19");

    let index = seeded
        .paths
        .session_dir(&seeded.id)
        .unwrap()
        .join("history-page-index.bin");
    std::fs::remove_file(&index).unwrap();
    assert_eq!(digest(&indexed_page(&seeded, Some(middle_cursor), 20)), digest(&middle));

    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn one_page_reads_only_its_own_records_and_not_the_session() {
    let small_dir = support::temp_root("history-page-small");
    let large_dir = support::temp_root("history-page-large");
    let small = seed(&small_dir, "small", 200);
    let large = seed(&large_dir, "large", 5_000);

    let measure = |seeded: &Seeded| -> (usize, usize) {
        let probe = ReadProbe::default();
        let cold = {
            let _pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &probe).unwrap();
            read_total(&probe)
        };
        let probe = ReadProbe::default();
        let pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &probe).unwrap();
        let end = pages.len();
        let page = pages.page(end - 20, end).unwrap();
        assert_eq!(page.len(), 20);
        (cold, read_total(&probe))
    };

    let (small_cold, small_page) = measure(&small);
    let (large_cold, large_page) = measure(&large);
    let small_log = std::fs::metadata(small.paths.events_log(&small.id).unwrap())
        .unwrap()
        .len();
    let large_log = std::fs::metadata(large.paths.events_log(&large.id).unwrap())
        .unwrap()
        .len();
    println!(
        "PAGE_READS small_log={small_log} small_cold={small_cold} small_page={small_page} large_log={large_log} large_cold={large_cold} large_page={large_page}"
    );

    assert!(
        small_cold as u64 > small_log / 2,
        "a cold index must replay the session"
    );
    assert!(
        large_page < large_cold / 4,
        "a warm page must read far less than the replay it replaces: {large_page} vs {large_cold}"
    );
    assert!(
        (large_page as f64) < (small_page as f64) * 2.0,
        "a page must not grow with the session: {large_page} vs {small_page}"
    );
    assert!(
        (large_page as u64) < large_log / 10,
        "a page must stay far below the whole session: {large_page} of {large_log}"
    );

    std::fs::remove_dir_all(&small_dir).unwrap();
    std::fs::remove_dir_all(&large_dir).unwrap();
}

fn read_total(probe: &ReadProbe) -> usize {
    probe.receipts().unwrap().iter().map(|receipt| receipt.bytes).sum()
}

#[test]
fn a_cursor_from_another_head_is_absent_rather_than_stale() {
    let directory = support::temp_root("history-page-heads");
    let seeded = seed(&directory, "heads", 60);

    let probe = ReadProbe::default();
    let pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &probe).unwrap();
    assert_eq!(pages.head(), &seeded.head);
    assert!(pages.position_of_message("m40").unwrap().is_some());

    let rewind = seeded.events[20].clone();
    select(&seeded, &rewind);
    let probe = ReadProbe::default();
    let pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &probe).unwrap();
    assert_eq!(pages.head(), &rewind, "a selection change must not be served stale");
    assert!(pages.position_of_message("m40").unwrap().is_none());
    assert_eq!(pages.len(), 21);

    let absent = EventId::parse("00000000-0000-4000-8000-000000000000").unwrap();
    let error = match HistoryPages::open(&seeded.paths, &seeded.id, Some(&absent), &ReadProbe::default()) {
        Ok(_) => panic!("a head outside the log must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.code, StoreErrorCode::MissingDependency);

    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn an_append_between_two_pages_keeps_the_cursor_consistent() {
    let directory = support::temp_root("history-page-append");
    let mut seeded = seed(&directory, "append", 40);

    let before = indexed_page(&seeded, None, 10);
    assert_eq!(before.last().unwrap().id, "m39");

    let log = seeded.paths.events_log(&seeded.id).unwrap();
    let mut writer = BlockingWriter::open(seeded.paths.clone(), seeded.id.clone()).unwrap();
    let next = EventDraft::new(
        seeded.id.clone(),
        EventId::new_v4(),
        Some(seeded.head.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(1),
        EventKind::new(KnownEventKind::MessageAppended),
        json!({"message": message("m40", &format!("{TEXT} 40"))}),
    )
    .seal()
    .unwrap();
    writer.append(&next).unwrap();
    drop(writer);
    seeded.events.push(next.envelope().event_id.clone());
    seeded.head = next.envelope().event_id.clone();

    let probe = ReadProbe::default();
    let pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &probe).unwrap();
    assert_eq!(pages.head(), &seeded.head);
    assert_eq!(pages.len(), 41);
    let cursor = "m30";
    let cursor_position = pages.position_of_message(cursor).unwrap().unwrap();
    let after = pages.page(cursor_position - 10, cursor_position).unwrap();
    assert_eq!(after.len(), 10);
    assert_eq!(after.last().unwrap().message.id, "m29");
    assert_eq!(
        digest(&after.iter().map(|m| m.message.clone()).collect::<Vec<_>>()),
        digest(&replay_page(&seeded, Some(cursor), 10)),
        "an append must keep older pages identical"
    );
    assert!(std::fs::metadata(&log).unwrap().len() > 0);

    std::fs::remove_dir_all(&directory).unwrap();
}

/// One message record appended to the log as a complete line.
fn append_line(paths: &SessionPaths, id: &SessionId, event: &SealedEvent) {
    use std::io::Write;
    let line = encode_record_line(event.envelope()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(paths.events_log(id).unwrap())
        .unwrap();
    file.write_all(&line).unwrap();
}

/// A message record chained onto an event identity rather than a sealed parent, so a writer can
/// append repeatedly without holding the whole chain.
fn appended_to(parent: &EventId, id: &SessionId, index: usize, text: &str) -> SealedEvent {
    EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        Some(parent.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(1),
        EventKind::new(KnownEventKind::MessageAppended),
        json!({"message": message(&format!("m{index}"), text)}),
    )
    .seal()
    .unwrap()
}

/// A page request must never fail `busy` because the log grew while the index was rebuilding.
///
/// The index is bound to a head and to the log stamp. An append adds events after that head, so
/// the rows the rebuild just produced still describe it exactly; the page is served, and the next
/// request rebuilds because the published stamp no longer matches the log. `busy` instead turned
/// an ordinary page request during a turn into an error nothing retries.
#[test]
fn a_page_request_survives_an_append_during_the_rebuild() {
    let directory = support::temp_root("history-page-append-during-build");
    let seeded = seed(&directory, "append-during-build", 5_000);
    let probe = ReadProbe::default();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let appender = {
        let stop = stop.clone();
        let probe = probe.clone();
        let paths = seeded.paths.clone();
        let id = seeded.id.clone();
        let watched = seeded.paths.events_log(&seeded.id).unwrap();
        let mut previous = seeded.head.clone();
        std::thread::spawn(move || {
            // Order the first append after the rebuild has actually opened the log, so the change
            // lands inside the window between the stamp `build` was handed and the one it re-read
            // at the end, rather than after an arbitrary delay.
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let started = probe
                    .receipts()
                    .is_ok_and(|receipts| receipts.iter().any(|receipt| receipt.path == watched));
                if started {
                    break;
                }
                std::thread::yield_now();
            }
            let mut count = 0usize;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let next = appended_to(&previous, &id, 10_000 + count, "appended during the rebuild");
                append_line(&paths, &id, &next);
                previous = next.envelope().event_id.clone();
                count += 1;
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            count
        })
    };

    let opened = HistoryPages::open(&seeded.paths, &seeded.id, None, &probe);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let appended = appender.join().unwrap();
    let pages = opened.expect("a page request must serve its page while the log grows, never fail busy");
    assert!(appended > 0, "no append landed, so this test would prove nothing");
    assert!(pages.built(), "the first load had to replay the session");

    let head = pages.head().clone();
    let end = pages.len();
    let page = pages.page(end - 5, end).unwrap();
    assert_eq!(page.len(), 5);
    let graph = EventGraph::open(&seeded.paths, &seeded.id).unwrap();
    let projection = graph.replay(Some(&head)).unwrap();
    let expected: Vec<StoredMessage> = projection.messages()[end - 5..end]
        .iter()
        .map(|message| message.message.clone())
        .collect();
    assert_eq!(
        digest(&page.iter().map(|page| page.message.clone()).collect::<Vec<_>>()),
        digest(&expected),
        "the served page must be the replay for the head the index was built for"
    );

    // The published index carries the stamp the replay started with. The log has moved since, so
    // the next request must rebuild instead of serving it - and that rebuild is also the proof
    // that the append landed inside the rebuild above rather than after it.
    let second = HistoryPages::open(&seeded.paths, &seeded.id, None, &ReadProbe::default()).unwrap();
    assert!(
        second.built(),
        "an index published under a superseded stamp must be rebuilt, never reused"
    );

    std::fs::remove_dir_all(&directory).unwrap();
}

fn select(seeded: &Seeded, head: &EventId) {
    let directory = seeded.paths.session_dir(&seeded.id).unwrap();
    std::fs::write(directory.join("local-head.json"), serde_json::to_vec(head).unwrap()).unwrap();
    std::fs::write(directory.join("local-head-explicit.json"), b"true").unwrap();
}

fn index_path(seeded: &Seeded) -> std::path::PathBuf {
    seeded
        .paths
        .session_dir(&seeded.id)
        .unwrap()
        .join("history-page-index.bin")
}

/// One numeric field of the index header block, read back from the published file.
fn header_number(index: &Path, field: &str) -> u64 {
    let bytes = std::fs::read(index).unwrap();
    let block: serde_json::Value = serde_json::from_slice(&bytes[..4096]).unwrap();
    block["value"][field].as_u64().unwrap()
}

/// Rewrites one `by_message` entry: offset, length, position.
fn corrupt_by_message_row(index: &Path, position: usize, offset: Option<u32>, length: Option<u32>) {
    let at = usize::try_from(header_number(index, "by_message_at")).unwrap() + position * 12;
    let mut bytes = std::fs::read(index).unwrap();
    if let Some(offset) = offset {
        bytes[at..at + 4].copy_from_slice(&offset.to_be_bytes());
    }
    if let Some(length) = length {
        bytes[at + 4..at + 8].copy_from_slice(&length.to_be_bytes());
    }
    std::fs::write(index, &bytes).unwrap();
}

/// Rewrites one row's string span: the last eight bytes of its 40-byte record.
fn corrupt_row_strings(index: &Path, position: usize, offset: Option<u32>, length: Option<u32>) {
    let at = usize::try_from(header_number(index, "rows_at")).unwrap() + position * 40;
    let mut bytes = std::fs::read(index).unwrap();
    if let Some(offset) = offset {
        bytes[at + 32..at + 36].copy_from_slice(&offset.to_be_bytes());
    }
    if let Some(length) = length {
        bytes[at + 36..at + 40].copy_from_slice(&length.to_be_bytes());
    }
    std::fs::write(index, &bytes).unwrap();
}

/// A corrupt index whose header still checks out must be refused, and the refusal
/// must come before the row's bytes are read: an unchecked row would ask for an
/// allocation as large as the 32-bit field allows.
fn refused_before_reading(
    seeded: &Seeded,
    read: impl FnOnce(&HistoryPages) -> Result<Option<usize>, StoreError>,
    target: u64,
) {
    let probe = ReadProbe::default();
    let pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &probe).unwrap();
    let error = match read(&pages) {
        Ok(_) => panic!("a row outside the string table must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.code, StoreErrorCode::CorruptLog, "{}", error.message);
    let reads = probe.receipts().unwrap();
    assert!(
        !reads.iter().any(|receipt| receipt.offset == target),
        "a refused row must never be read: offset {target} was read anyway"
    );
}

#[test]
fn a_row_length_past_the_string_table_is_refused_before_reading() {
    let directory = support::temp_root("history-page-row-length");
    let seeded = seed(&directory, "row-length", 40);
    let _pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &ReadProbe::default()).unwrap();
    let index = index_path(&seeded);
    let strings_at = header_number(&index, "strings_at");

    corrupt_by_message_row(&index, 0, None, Some(u32::MAX));

    refused_before_reading(&seeded, |pages| pages.position_of_message("m0"), strings_at);

    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_row_offset_past_the_string_table_is_refused_before_reading() {
    let directory = support::temp_root("history-page-row-offset");
    let seeded = seed(&directory, "row-offset", 40);
    let _pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &ReadProbe::default()).unwrap();
    let index = index_path(&seeded);
    let strings_at = header_number(&index, "strings_at");
    let strings_len = header_number(&index, "strings_len");

    let straddle = u32::try_from(strings_len - 1).unwrap();
    corrupt_by_message_row(&index, 0, Some(straddle), Some(8));

    refused_before_reading(
        &seeded,
        |pages| pages.position_of_message("m0"),
        strings_at + strings_len - 1,
    );

    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_row_message_span_past_the_string_table_is_refused_before_reading() {
    let directory = support::temp_root("history-page-row-span");
    let seeded = seed(&directory, "row-span", 40);
    let _pages = HistoryPages::open(&seeded.paths, &seeded.id, None, &ReadProbe::default()).unwrap();
    let index = index_path(&seeded);
    let strings_at = header_number(&index, "strings_at");

    corrupt_row_strings(&index, 0, None, Some(u32::MAX));

    refused_before_reading(
        &seeded,
        |pages| {
            pages
                .page(0, 1)
                .map(|page| page.first().map(|message| message.message.id.len()))
        },
        strings_at,
    );

    std::fs::remove_dir_all(&directory).unwrap();
}

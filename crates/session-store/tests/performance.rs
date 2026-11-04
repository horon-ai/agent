use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::checkpoint::SessionCache;
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

// allow: SIZE_OK - one assigned integration target owns every structural
// performance bound, its fixed fixtures and its failure-injection controls.

mod support;

const EVENT_COUNT: usize = 10_000;
const SESSION_COUNT: usize = 10_000;
const HISTORY_TARGET_BYTES: usize = 50 * 1024 * 1024;
const RETAINED_TAIL: usize = 100;
const FIXED_OVERHEAD_MESSAGES: usize = 1;
const LARGE_BLOB_BYTES: usize = 9 * 1024 * 1024;
const EXISTING_FIXTURE_MESSAGES: usize = 512;
const EXISTING_FIXTURE_TEXT: usize = 8188;
const SAMPLES: usize = 21;
const MESSAGE_TEXT_BYTES: usize = (HISTORY_TARGET_BYTES / EVENT_COUNT) - 110;

fn root(tag: &str) -> PathBuf {
    support::temp_root(&format!("perf-{tag}"))
}

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

fn appended(parent: &SealedEvent, text: &str) -> SealedEvent {
    EventDraft::new(
        parent.envelope().session_id.clone(),
        EventId::new_v4(),
        Some(parent.envelope().event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(1),
        EventKind::new(KnownEventKind::MessageAppended),
        json!({"message": StoredMessage {
            turn_start: false,
            id: uuid::Uuid::new_v4().to_string(), role: Role::User,
            content: vec![ContentBlock::Text { text: text.into(), cache_control: None }],
            display_role: None, timestamp: None, tool_duration_ms: None, token_usage: None,
        }}),
    )
    .seal()
    .unwrap()
}

fn write_private(path: &Path, bytes: &[u8]) {
    if path.exists() {
        overwrite_private(path, bytes);
        return;
    }
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn overwrite_private(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn published_bytes(probe: &ReadProbe, path: &Path) -> usize {
    probe
        .publications()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path == path)
        .map(|receipt| receipt.bytes)
        .sum()
}

fn read_bytes(probe: &ReadProbe, path: &Path) -> usize {
    probe
        .receipts()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path == path)
        .map(|receipt| receipt.bytes)
        .sum()
}

fn write_bytes(probe: &ReadProbe, path: &Path) -> usize {
    probe
        .writes()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path == path)
        .map(|receipt| receipt.bytes)
        .sum()
}

fn written_below(probe: &ReadProbe, path: &Path, boundary: u64) -> usize {
    probe
        .writes()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path == path && receipt.offset < boundary)
        .map(|receipt| receipt.bytes)
        .sum()
}

fn read_below(probe: &ReadProbe, path: &Path, boundary: u64) -> usize {
    probe
        .receipts()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path == path && receipt.offset < boundary)
        .map(|receipt| receipt.bytes)
        .sum()
}

fn transcript_read_bytes(probe: &ReadProbe) -> usize {
    probe
        .receipts()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path.file_name().is_some_and(|name| name == "events.jsonl"))
        .map(|receipt| receipt.bytes)
        .sum()
}

fn percentile(samples: &mut [Duration], fraction: f64) -> Duration {
    samples.sort_unstable();
    let index = ((samples.len() - 1) as f64 * fraction).round() as usize;
    samples[index]
}

fn median(samples: &mut [Duration]) -> Duration {
    percentile(samples, 0.5)
}

fn millis(value: Duration) -> f64 {
    value.as_micros() as f64 / 1000.0
}

fn resident_json_bytes(context: &checkpoint::RecentContext) -> usize {
    context
        .tail
        .iter()
        .map(|message| serde_json::to_vec(message).unwrap().len())
        .sum()
}

struct Large {
    paths: SessionPaths,
    id: SessionId,
    root_event: SealedEvent,
    head: SealedEvent,
    events: Vec<SealedEvent>,
    history_bytes: u64,
}

fn build_large(directory: &Path) -> Large {
    let paths = SessionPaths::new(directory);
    let id = SessionId::parse("history").unwrap();
    let session = paths.session_dir(&id).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&session).unwrap();
    let root_event = created(&id);
    let mut events = Vec::with_capacity(EVENT_COUNT);
    let mut bytes = encode_record_line(root_event.envelope()).unwrap();
    let mut previous = root_event.clone();
    for index in 0..EVENT_COUNT {
        let text = format!("{index:05}{}", "x".repeat(MESSAGE_TEXT_BYTES));
        let next = appended(&previous, &text);
        bytes.extend(encode_record_line(next.envelope()).unwrap());
        previous = next.clone();
        events.push(next);
    }
    let blob = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        Some(previous.envelope().event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::AuxiliaryRecorded),
        json!({"aux_kind": "legacy", "schema_version": 1, "value": "b".repeat(LARGE_BLOB_BYTES)}),
    )
    .seal()
    .unwrap();
    let SealedEvent::Blob {
        envelope,
        blob: payload,
        ..
    } = &blob
    else {
        panic!("the fixed large payload must be externalized");
    };
    let reference = envelope.payload_ref.as_ref().unwrap();
    assert_eq!(reference.size as usize, payload.len());
    bytes.extend(encode_record_line(blob.envelope()).unwrap());
    let log = paths.events_log(&id).unwrap();
    write_private(&log, &bytes);
    fs::File::open(&log).unwrap().sync_all().unwrap();
    let blob_dir = paths.blobs_dir(&id).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&blob_dir).unwrap();
    write_private(&paths.blob_file(&id, &reference.sha256).unwrap(), payload);
    Large {
        paths,
        id,
        root_event,
        head: blob,
        events,
        history_bytes: u64::try_from(bytes.len()).unwrap(),
    }
}

fn compaction_event(graph: &EventGraph, fixture: &Large, source: &SealedEvent, kept_first_index: usize) -> SealedEvent {
    let covered = &fixture.events[kept_first_index - 1];
    let first_kept = &fixture.events[kept_first_index];
    let source = source.envelope();
    let payload = CompactedPayload {
        summary_text: "summary of the covered prefix".into(),
        source_head_id: Some(source.event_id.clone()),
        covered_through_event_id: covered.envelope().event_id.clone(),
        source_prefix_digest: graph
            .prefix_digest(&source.event_id, &covered.envelope().event_id, None)
            .unwrap(),
        first_kept_event_id: Some(first_kept.envelope().event_id.clone()),
        prior_compaction_id: None,
        provider: "openai".into(),
        model: "model".into(),
        route: "route".into(),
        settings_digest: sha256_hex(b"settings"),
        generation: 1,
        input_tokens: None,
        output_tokens: None,
        outcome: CompactionOutcome::Summary,
        native_window: Vec::new(),
        restoration: None,
    };
    EventDraft::new(
        fixture.id.clone(),
        EventId::new_v4(),
        Some(source.event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(2),
        EventKind::new(KnownEventKind::Compacted),
        json!(payload),
    )
    .seal()
    .unwrap()
}

fn retained_tail_offset(fixture: &Large, kept_first_index: usize) -> u64 {
    let mut offset = u64::try_from(encode_record_line(fixture.root_event.envelope()).unwrap().len()).unwrap();
    for event in fixture.events.iter().take(kept_first_index) {
        offset += u64::try_from(encode_record_line(event.envelope()).unwrap().len()).unwrap();
    }
    offset
}

fn peak_rss(directory: &Path, session: &SessionId) -> Option<u64> {
    let exe = std::env::current_exe().unwrap();
    let output = Command::new("/usr/bin/time")
        .arg("-l")
        .arg(&exe)
        .args(["--exact", "peak_rss_is_measured_for_one_bounded_resume", "--nocapture"])
        .env("PERF_PHASE_ROOT", directory)
        .env("PERF_PHASE_SESSION", session.as_str())
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "the measured phase must complete: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr.lines().find(|line| line.contains("maximum resident set size"))?;
    let digits: String = line
        .split("maximum resident set size")
        .next()
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[test]
fn peak_rss_is_measured_for_one_bounded_resume() {
    let Ok(directory) = std::env::var("PERF_PHASE_ROOT") else {
        return;
    };
    let session = std::env::var("PERF_PHASE_SESSION").unwrap_or_else(|_| "history".into());
    let paths = SessionPaths::new(PathBuf::from(directory));
    let id = SessionId::parse(&session).unwrap();
    let context = SessionCache::new(paths, ReadProbe::default())
        .recent(&id, None)
        .unwrap();
    println!(
        "PEAK_PHASE resident_messages={} resident_json_bytes={}",
        context.tail.len(),
        resident_json_bytes(&context)
    );
}

#[test]
fn long_history_bounds_hold_with_failure_injection() {
    let directory = root("large");
    let fixture = build_large(&directory);
    let log = fixture.paths.events_log(&fixture.id).unwrap();
    println!(
        "FIXTURE_LARGE root={} events={} history_bytes={} blob_bytes={} retained_tail={}",
        directory.display(),
        EVENT_COUNT + 2,
        fixture.history_bytes,
        LARGE_BLOB_BYTES,
        RETAINED_TAIL
    );

    // Given a cold store holding the fixed long history.
    let cold_probe = ReadProbe::default();
    let cold_cache = SessionCache::new(fixture.paths.clone(), cold_probe.clone());
    let started = Instant::now();
    let cold = cold_cache.recent(&fixture.id, None).unwrap();
    let cold_ms = started.elapsed();
    let cold_read = read_bytes(&cold_probe, &log);
    println!(
        "COLD resume_ms={} original_read_bytes={} resident_messages={} resident_json_bytes={}",
        cold_ms.as_millis(),
        cold_read,
        cold.tail.len(),
        resident_json_bytes(&cold)
    );
    assert_eq!(cold.tail.len(), EVENT_COUNT);
    assert!(cold_read > 0, "a cold rebuild must read the original");

    // When the same session resumes from the published cache, repeatedly.
    let mut warm_samples = Vec::with_capacity(SAMPLES);
    let mut warm_reads = Vec::with_capacity(SAMPLES);
    let mut coverage = None;
    for _ in 0..SAMPLES {
        let probe = ReadProbe::default();
        let cache = SessionCache::new(fixture.paths.clone(), probe.clone());
        let started = Instant::now();
        let view = cache.tail_view(&fixture.id, None).unwrap();
        warm_samples.push(started.elapsed());
        warm_reads.push(read_bytes(&probe, &log));
        coverage = Some(view.coverage);
    }
    let coverage = coverage.unwrap();
    println!(
        "WARM_RESUME original_read_bytes={} max_sample_read_bytes={} active_messages={} inline_from={} inline_messages={} median_ms={} p95_ms={}",
        warm_reads.iter().sum::<usize>(),
        warm_reads.iter().copied().max().unwrap_or(0),
        coverage.active_messages,
        coverage.inline_from,
        coverage.inline_messages,
        millis(median(&mut warm_samples.clone())),
        millis(percentile(&mut warm_samples.clone(), 0.95))
    );
    // Then the warm resume reads none of the old prefix.
    assert_eq!(
        warm_reads.iter().sum::<usize>(),
        0,
        "a warm resume must read zero original bytes"
    );
    assert_eq!(coverage.active_messages, EVENT_COUNT);
    assert!(coverage.inline_messages < coverage.active_messages);
    // And the cold path is the injected violation of that same measurement.
    assert!(
        cold_read > warm_reads.iter().sum::<usize>(),
        "the read counter must distinguish a full replay from a warm resume"
    );

    // When the disposable cache is deleted, the next bounded resume rebuilds once.
    for name in ["checkpoint.json", "summary.json", "sparse-index.jsonl"] {
        fs::remove_file(fixture.paths.session_dir(&fixture.id).unwrap().join(name)).unwrap();
    }
    let mut rebuild_reads = Vec::new();
    for round in 0..3 {
        let probe = ReadProbe::default();
        let cache = SessionCache::new(fixture.paths.clone(), probe.clone());
        let view = cache.tail_view(&fixture.id, None).unwrap();
        let read = read_bytes(&probe, &log);
        rebuild_reads.push(read);
        println!(
            "REBUILD round={round} original_read_bytes={read} active_messages={} inline_from={} inline_messages={}",
            view.coverage.active_messages, view.coverage.inline_from, view.coverage.inline_messages
        );
        assert_eq!(view.coverage.active_messages, EVENT_COUNT);
    }
    // Then only the first resume after the deletion pays for the original.
    assert!(
        rebuild_reads[0] > 0,
        "deleting the cache must force exactly one rebuild"
    );
    assert_eq!(rebuild_reads[1], 0, "the rebuilt cache must be warm again");
    assert_eq!(rebuild_reads[2], 0, "warm state must stay warm");
    // And the complete-active-context reader is a separate, reported cost.
    let full_probe = ReadProbe::default();
    let full = SessionCache::new(fixture.paths.clone(), full_probe.clone())
        .recent(&fixture.id, None)
        .unwrap();
    println!(
        "FULL_MATERIALIZE original_read_bytes={} resident_messages={}",
        read_bytes(&full_probe, &log),
        full.tail.len()
    );
    assert_eq!(full.tail.len(), EVENT_COUNT);

    // Given a warm owning writer, when exactly one event is appended.
    let owner_probe = ReadProbe::default();
    let mut writer = BlockingWriter::open(fixture.paths.clone(), fixture.id.clone()).unwrap();
    writer.enable_cache(owner_probe.clone());
    writer.refresh_cache(None, owner_probe.clone()).unwrap();
    let prefix_len = fs::metadata(&log).unwrap().len();
    let prefix_bytes = fs::read(&log).unwrap();
    let append_probe = ReadProbe::default();
    writer.enable_cache(append_probe.clone());
    let next = appended(&fixture.head, "one more event");
    let line = u64::try_from(encode_record_line(next.envelope()).unwrap().len()).unwrap();
    let started = Instant::now();
    assert_eq!(writer.append(&next).unwrap(), AppendOutcome::Committed);
    let append_ms = started.elapsed();
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    let after_len = fs::metadata(&log).unwrap().len();
    let log_writes = write_bytes(&append_probe, &log);
    let prefix_writes = written_below(&append_probe, &log, prefix_len);
    let catalog = fixture.paths.root().join(".catalog.index.jsonl");
    let cache_written = append_probe
        .publications()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path != catalog)
        .map(|receipt| receipt.bytes)
        .sum::<usize>();
    println!(
        "APPEND append_ms={} log_written_bytes={} expected_line_bytes={} prefix_bytes_written={} catalog_written_bytes={} cache_written_bytes={}",
        append_ms.as_millis(),
        log_writes,
        line,
        prefix_writes,
        published_bytes(&append_probe, &catalog),
        cache_written
    );
    // Then the append only extends the original and never rewrites its prefix.
    assert_eq!(after_len, prefix_len + line);
    assert_eq!(log_writes as u64, line);
    assert_eq!(prefix_writes, 0, "an append must rewrite zero prefix bytes");
    assert_eq!(
        fs::read(&log).unwrap()[..prefix_bytes.len()],
        prefix_bytes[..],
        "the durable prefix must stay byte-identical"
    );
    drop(writer);

    // And a genuinely recreated log does move the same counter.
    let control_dir = root("prefix-control");
    let control_paths = SessionPaths::new(control_dir.clone());
    let control_id = SessionId::parse("control").unwrap();
    let control_event = created(&control_id);
    let mut control_writer = BlockingWriter::open(control_paths.clone(), control_id.clone()).unwrap();
    control_writer.append(&control_event).unwrap();
    drop(control_writer);
    let control_log = control_paths.events_log(&control_id).unwrap();
    let control_prefix = fs::metadata(&control_log).unwrap().len();
    fs::remove_file(&control_log).unwrap();
    let control_probe = ReadProbe::default();
    let mut control_writer = BlockingWriter::open(control_paths.clone(), control_id.clone()).unwrap();
    control_writer.enable_cache(control_probe.clone());
    control_writer.append(&control_event).unwrap();
    let control_prefix_writes = written_below(&control_probe, &control_log, control_prefix);
    println!("PREFIX_CONTROL recreated_prefix_len={control_prefix} prefix_bytes_written={control_prefix_writes}");
    assert!(
        control_prefix_writes > 0,
        "the prefix counter must report a real offset-zero rewrite"
    );
    drop(control_writer);
    fs::remove_dir_all(&control_dir).unwrap();

    let uncompacted_peak = peak_rss(&directory, &fixture.id);
    println!("PEAK_RSS uncompacted={uncompacted_peak:?}");

    // Given the same history with a validated compaction at its boundary.
    let graph = EventGraph::open(&fixture.paths, &fixture.id).unwrap();
    let kept_first_index = EVENT_COUNT - RETAINED_TAIL + 1;
    let compacted = compaction_event(&graph, &fixture, &next, kept_first_index);
    let kept_from = retained_tail_offset(&fixture, kept_first_index);
    let compact_probe = ReadProbe::default();
    let mut writer = BlockingWriter::open(fixture.paths.clone(), fixture.id.clone()).unwrap();
    writer.enable_cache(compact_probe.clone());
    assert_eq!(writer.append(&compacted).unwrap(), AppendOutcome::Committed);
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    drop(writer);

    // When the complete active context is hydrated from the warm cache.
    let hydrate_probe = ReadProbe::default();
    let cache = SessionCache::new(fixture.paths.clone(), hydrate_probe.clone());
    let compacted_context = cache.recent(&fixture.id, None).unwrap();
    let resident = compacted_context.tail.len();
    let summary_messages = usize::from(compacted_context.compaction.is_some());
    let covered_prefix_reads = read_below(&hydrate_probe, &log, kept_from);
    let resident_reads = read_bytes(&hydrate_probe, &log);
    println!(
        "COMPACTED resident_messages={resident} retained_tail={RETAINED_TAIL} summary_messages={summary_messages} resident_json_bytes={} original_read_bytes={resident_reads} covered_prefix_read_bytes={covered_prefix_reads}",
        resident_json_bytes(&compacted_context)
    );
    // Then resident messages are the summary plus the retained tail plus the
    // fixed boundary arithmetic, never the summarized prefix.
    let bound = summary_messages + RETAINED_TAIL + FIXED_OVERHEAD_MESSAGES;
    assert!(
        resident <= bound,
        "a compacted context must not keep the summarized prefix: {resident} > {bound}"
    );
    assert!(
        resident < EVENT_COUNT / 50,
        "the compacted resident context must be far below the history: {resident}"
    );
    assert_eq!(
        covered_prefix_reads, 0,
        "a compacted resume must not read the covered prefix"
    );
    assert!(resident_reads > 0, "the retained tail is hydrated from the original");
    // And the uncompacted context is the injected violation of that same bound.
    assert!(
        cold.tail.len() > summary_messages + RETAINED_TAIL,
        "the resident bound must distinguish a compacted context from an uncompacted one"
    );

    let compacted_peak = peak_rss(&directory, &fixture.id);
    println!("PEAK_RSS compacted={compacted_peak:?}");
    if let (Some(before), Some(after)) = (uncompacted_peak, compacted_peak) {
        println!(
            "PEAK_RSS_DELTA uncompacted={before} compacted={after} saved={}",
            before.saturating_sub(after)
        );
        assert!(
            after < before,
            "a compacted resume must peak below the uncompacted one: {after} vs {before}"
        );
    }

    fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn listing_parses_no_transcript_across_ten_thousand_sessions() {
    let directory = root("listing");
    let paths = SessionPaths::new(directory.clone());
    let seed = SessionId::parse("session-00000").unwrap();
    let seed_event = created(&seed);
    let mut writer = BlockingWriter::open(paths.clone(), seed.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.append(&seed_event).unwrap();
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    drop(writer);
    let seed_summary = fs::read(paths.session_dir(&seed).unwrap().join("summary.json")).unwrap();
    let catalog_path = directory.join(".catalog.index.jsonl");
    let mut seed_row = seed_summary.clone();
    seed_row.push(b'\n');
    assert_eq!(
        fs::read(&catalog_path).unwrap(),
        seed_row,
        "one published summary record is exactly the catalog row"
    );
    let template = String::from_utf8(seed_summary).unwrap();

    // The crafted record must decode through the real catalog reader before
    // ten thousand of them are written.
    let check_dir = root("listing-check");
    let check_paths = SessionPaths::new(check_dir.clone());
    let mut check_rows = Vec::new();
    for index in 0..2 {
        let id = SessionId::parse(format!("check-{index:05}")).unwrap();
        let event = created(&id);
        let session = check_paths.session_dir(&id).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&session).unwrap();
        let log = session.join("events.jsonl");
        write_private(&log, &encode_record_line(event.envelope()).unwrap());
        let record = catalog_record(&template, &id, &event, &log);
        write_private(&session.join("summary.json"), &record);
        check_rows.extend_from_slice(&record);
        check_rows.push(b'\n');
    }
    write_private(&check_dir.join(".catalog.index.jsonl"), &check_rows);
    let check_probe = ReadProbe::default();
    let checked = catalog::list(&check_paths, check_probe.clone()).unwrap();
    println!(
        "FIXTURE_LISTING_CHECK sessions={} transcript_bytes={}",
        checked.len(),
        transcript_read_bytes(&check_probe)
    );
    assert_eq!(checked.len(), 2, "the crafted catalog record must decode as-is");
    assert_eq!(
        transcript_read_bytes(&check_probe),
        0,
        "the crafted record must take the warm listing path"
    );
    fs::remove_dir_all(&check_dir).unwrap();

    let mut rows = Vec::new();
    rows.extend_from_slice(&seed_row);
    for index in 1..SESSION_COUNT {
        let id = SessionId::parse(format!("session-{index:05}")).unwrap();
        let event = created(&id);
        let session = paths.session_dir(&id).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&session).unwrap();
        let log = session.join("events.jsonl");
        write_private(&log, &encode_record_line(event.envelope()).unwrap());
        let record = catalog_record(&template, &id, &event, &log);
        write_private(&session.join("summary.json"), &record);
        rows.extend_from_slice(&record);
        rows.push(b'\n');
    }
    write_private(&catalog_path, &rows);
    println!(
        "FIXTURE_LISTING root={} sessions={SESSION_COUNT} catalog_bytes={}",
        directory.display(),
        rows.len()
    );

    // When the complete catalog is listed from the warm state.
    let probe = ReadProbe::default();
    let started = Instant::now();
    let listed = catalog::list(&paths, probe.clone()).unwrap();
    let listed_ms = started.elapsed();
    let parsed = transcript_read_bytes(&probe);
    let summaries: usize = probe
        .receipts()
        .unwrap()
        .iter()
        .filter(|receipt| receipt.path.file_name().is_some_and(|name| name == "summary.json"))
        .map(|receipt| receipt.bytes)
        .sum();
    println!(
        "LISTING sessions={} listed={} listed_ms={} transcript_bytes={} catalog_read_bytes={} summary_read_bytes={}",
        SESSION_COUNT,
        listed.len(),
        millis(listed_ms),
        parsed,
        read_bytes(&probe, &catalog_path),
        summaries
    );
    // Then listing never parses a transcript.
    assert_eq!(listed.len(), SESSION_COUNT);
    assert_eq!(parsed, 0, "listing must parse zero transcripts");
    assert!(summaries > 0, "listing reads bounded summary records instead");

    // And one damaged summary forces exactly that transcript to be parsed.
    let damaged = SessionId::parse("session-00042").unwrap();
    let summary_path = paths.session_dir(&damaged).unwrap().join("summary.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&summary_path).unwrap()).unwrap();
    record["digest"] = json!("0".repeat(64));
    overwrite_private(&summary_path, &serde_json::to_vec(&record).unwrap());
    let repair_probe = ReadProbe::default();
    let repaired = catalog::list(&paths, repair_probe.clone()).unwrap();
    let repaired_bytes = read_bytes(&repair_probe, &paths.events_log(&damaged).unwrap());
    println!(
        "LISTING_CONTROL damaged_session={} repaired={} damaged_transcript_bytes={repaired_bytes}",
        damaged.as_str(),
        repaired.len()
    );
    assert_eq!(repaired.len(), SESSION_COUNT);
    assert!(
        repaired_bytes > 0,
        "a damaged summary must force a real transcript read"
    );

    fs::remove_dir_all(&directory).unwrap();
}

fn catalog_record(template: &str, id: &SessionId, event: &SealedEvent, log: &Path) -> Vec<u8> {
    let meta = fs::metadata(log).unwrap();
    let stamp = format!(
        "{{\"device\":{},\"inode\":{},\"len\":{},\"modified\":[{},{}],\"changed\":[{},{}]}}",
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec()
    );
    let value = value_text(template);
    let value = replace_json_value(value, "\"session_id\":", &format!("\"{}\"", id.as_str()));
    let value = replace_json_value(
        &value,
        "\"head\":",
        &format!("\"{}\"", event.envelope().event_id.as_str()),
    );
    let value = replace_json_value(&value, "\"source\":", &stamp);
    let digest = sha256_hex(value.as_bytes());
    format!("{{\"digest\":\"{digest}\",\"value\":{value}}}").into_bytes()
}

fn value_text(record: &str) -> &str {
    let start = record.find("\"value\":").expect("value field") + "\"value\":".len();
    record[start..record.len() - 1].trim_end()
}

fn replace_json_value(text: &str, key: &str, replacement: &str) -> String {
    let start = text.find(key).expect("field present") + key.len();
    let rest = &text[start..];
    let end = match rest.as_bytes()[0] {
        b'{' | b'[' => {
            let (open, close) = (rest.as_bytes()[0], if rest.as_bytes()[0] == b'{' { b'}' } else { b']' });
            let mut depth = 0i32;
            let mut end = 0;
            for (index, byte) in rest.bytes().enumerate() {
                if byte == open {
                    depth += 1;
                } else if byte == close {
                    depth -= 1;
                    if depth == 0 {
                        end = index + 1;
                        break;
                    }
                }
            }
            assert!(end > 0, "the object at {key} must be complete");
            end
        }
        b'"' => {
            let mut end = 0;
            for (index, byte) in rest.bytes().enumerate().skip(1) {
                if byte == b'"' {
                    end = index + 1;
                    break;
                }
            }
            assert!(end > 0, "the string at {key} must be complete");
            end
        }
        _ => panic!("unsupported json value at {key}"),
    };
    format!("{}{}{}", &text[..start], replacement, &rest[end..])
}

#[test]
fn existing_fixture_reports_median_p95_and_rss() {
    let directory = root("existing");
    let paths = SessionPaths::new(directory.clone());
    let id = SessionId::parse("existing").unwrap();
    let session = paths.session_dir(&id).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&session).unwrap();
    let root_event = created(&id);
    let mut bytes = encode_record_line(root_event.envelope()).unwrap();
    let mut previous = root_event;
    for index in 0..EXISTING_FIXTURE_MESSAGES {
        let text = format!("{index:04}{}", "y".repeat(EXISTING_FIXTURE_TEXT - 4));
        let next = appended(&previous, &text);
        bytes.extend(encode_record_line(next.envelope()).unwrap());
        previous = next;
    }
    let log = paths.events_log(&id).unwrap();
    write_private(&log, &bytes);
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    writer.refresh_cache(None, ReadProbe::default()).unwrap();
    assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
    drop(writer);

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let probe = ReadProbe::default();
        let cache = SessionCache::new(paths.clone(), probe.clone());
        let started = Instant::now();
        cache.tail_view(&id, None).unwrap();
        samples.push(started.elapsed());
        assert_eq!(read_bytes(&probe, &log), 0);
    }
    let list_probe = ReadProbe::default();
    let started = Instant::now();
    let listed = catalog::list(&paths, list_probe.clone()).unwrap();
    let list_ms = started.elapsed();
    let peak = peak_rss(&directory, &id);
    println!(
        "FIXTURE_EXISTING root={} messages={} history_bytes={} warm_resume_median_ms={} warm_resume_p95_ms={} list_ms={} list_transcript_bytes={} peak_rss={peak:?}",
        directory.display(),
        EXISTING_FIXTURE_MESSAGES + 1,
        bytes.len(),
        millis(median(&mut samples.clone())),
        millis(percentile(&mut samples.clone(), 0.95)),
        millis(list_ms),
        transcript_read_bytes(&list_probe)
    );
    assert_eq!(listed.len(), 1);
    assert_eq!(transcript_read_bytes(&list_probe), 0);
    fs::remove_dir_all(&directory).unwrap();
}

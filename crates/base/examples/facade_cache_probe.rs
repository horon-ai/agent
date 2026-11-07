//! Measures the facade cache at its 1 MiB threshold, seeded by the todo-1 auxiliary fixtures: for
//! each fixture it reports the fixture bytes, the fitted canonical record size, the save and
//! restore wall time, the peak temporary allocation, and whether the entry was written and read
//! back whole. Run: `cargo run -p horon-agent-base --example facade_cache_probe -- <under.json>
//! <over.json>`. It writes only inside its own temp home.
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use horon_agent_base::message::{ContentBlock, Role};
use horon_agent_base::session::{EnvSnapshot, Session};
use serde_json::json;

/// Mirrors `event_cache::FACADE_LIMIT`, which is private to the session module.
const FACADE_LIMIT: usize = 1024 * 1024;

/// Fixed part of the facade record (identifiers, counters, tool chain, JSON punctuation), used
/// only as the starting pad estimate; the bracket below measures the real boundary.
const ESTIMATED_OVERHEAD: usize = 1100;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let grown = unsafe { System.realloc(pointer, layout, new_size) };
        if grown.is_null() {
            return grown;
        }
        if new_size >= layout.size() {
            let live = LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed) + new_size - layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        grown
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measured<T> {
    value: T,
    millis: f64,
    peak_bytes: usize,
}

fn measure<T>(run: impl FnOnce() -> T) -> Measured<T> {
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let started = Instant::now();
    let value = run();
    let millis = started.elapsed().as_secs_f64() * 1000.0;
    Measured {
        value,
        millis,
        peak_bytes: PEAK.load(Ordering::Relaxed).saturating_sub(baseline),
    }
}

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }]
}

/// Bytes `publish` adds around the canonical payload: `{"payload":…,"sha256":"<64 hex>"}`.
fn wrapper_bytes() -> usize {
    let encoded = serde_json::to_vec(&json!({"payload": "x", "sha256": "a".repeat(64)})).expect("wrapper encodes");
    encoded.len() - 1
}

fn cache_file(session: &Session) -> PathBuf {
    session
        .event_log_path()
        .expect("a saved session has an event log")
        .parent()
        .expect("the log sits in the session directory")
        .join("facade-cache.json")
}

fn count_bytes<T: serde::Serialize>(value: &T) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0 += buffer.len();
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).expect("value encodes");
    counter.0
}

/// The canonical record size, read back from the cache file's own payload so it is measured, not
/// inferred from a wrapper constant.
fn record_bytes(session: &Session) -> Option<usize> {
    let bytes = std::fs::read(cache_file(session)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("payload").map(count_bytes)
}

fn set_pad(session: &mut Session, pad: usize) {
    let id = session.id.clone();
    session.env_snapshots.clear();
    session.record_env_snapshot(EnvSnapshot {
        captured_at: chrono::Utc::now(),
        reason: "p".repeat(pad),
        session_id: id,
        working_dir: None,
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        horon_agent_version: "probe".to_string(),
        horon_agent_git_hash: None,
        horon_agent_git_dirty: None,
        os: "macos".to_string(),
        arch: "arm64".to_string(),
        pid: std::process::id(),
        is_debug: false,
        working_git: None,
    });
}

fn restored_pad(session: &Session) -> usize {
    session
        .env_snapshots
        .first()
        .map(|snapshot| snapshot.reason.len())
        .unwrap_or(0)
}

/// The auxiliary fields the facade record carries, rebuilt from the public session surface.
fn auxiliary(session: &Session) -> serde_json::Value {
    json!({
        "env_snapshots": session.env_snapshots,
        "memory_injections": session.memory_injections,
        "replay_events": session.replay_events,
        "compaction": session.compaction,
        "is_debug": session.is_debug,
    })
}

fn auxiliary_bytes(session: &Session) -> usize {
    serde_json::to_vec(&auxiliary(session))
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

/// The largest pad whose record still fits the facade limit. Each probe costs one save, so the pad
/// starts from an estimate and a short bracket closes the last few bytes; the record's size drifts
/// by a few bytes between saves because the source stamp's numeric fields change length.
fn largest_cached_pad(mut session: Session) -> anyhow::Result<(Session, usize, Option<usize>)> {
    let mut pad = FACADE_LIMIT.saturating_sub(ESTIMATED_OVERHEAD);
    let mut low: Option<usize> = None;
    let mut high: Option<usize> = None;
    for _ in 0..12 {
        set_pad(&mut session, pad);
        session.save()?;
        match record_bytes(&session) {
            Some(measured) if measured == FACADE_LIMIT - 1 => return Ok((session, pad, Some(measured))),
            Some(measured) => {
                low = Some(pad);
                let step = (FACADE_LIMIT - 1).saturating_sub(measured).max(1);
                let next = match high {
                    Some(high) if pad + step >= high => pad + (high - pad) / 2,
                    _ => pad + step,
                };
                if next == pad {
                    return Ok((session, pad, Some(measured)));
                }
                pad = next;
            }
            None => {
                high = Some(pad);
                let next = match low {
                    Some(low) if pad - low <= 1 => low,
                    Some(low) => low + (pad - low) / 2,
                    None => pad.saturating_sub(64),
                };
                if next == pad {
                    return Ok((session, pad, None));
                }
                pad = next;
            }
        }
        if let (Some(low), Some(high)) = (low, high)
            && high - low <= 1
        {
            set_pad(&mut session, low);
            session.save()?;
            let record = record_bytes(&session);
            return Ok((session, low, record));
        }
    }
    anyhow::bail!("the facade boundary did not settle near pad {pad}")
}

fn fixture_pad(path: &str) -> anyhow::Result<usize> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(value.get("pad").and_then(|pad| pad.as_str()).map(str::len).unwrap_or(0))
}

fn threshold_report(session_id: &str, fixture: &str, past_boundary: bool) -> anyhow::Result<serde_json::Value> {
    let fixture_bytes = std::fs::metadata(fixture)?.len();
    let fixture_pad = fixture_pad(fixture)?;
    let mut session = Session::create_with_id(session_id.to_string(), None, None);
    session.add_message(Role::User, text("facade threshold probe"));
    let (mut session, cached_pad, cached_record) = largest_cached_pad(session)?;
    // The over case keeps a margin: the source stamp shifts a couple of bytes per save, so a
    // one-byte pad step would sometimes land back under the limit.
    let pad = if past_boundary { cached_pad + 64 } else { cached_pad };
    set_pad(&mut session, pad);
    session.save()?;
    let measured_record = record_bytes(&session);
    // When the entry is skipped the cache file is gone, so the size the decision rejected is
    // derived from the fixed overhead measured at the last cached pad.
    let overhead = cached_record.map(|record| record.saturating_sub(cached_pad));
    let derived_record = overhead.map(|overhead| overhead + pad);
    let auxiliary_serialized_bytes = auxiliary_bytes(&session);

    let save = measure(|| session.save());
    let cached = cache_file(&session).exists();
    let cache_bytes = std::fs::metadata(cache_file(&session)).ok().map(|meta| meta.len());

    let restore = measure(|| Session::load(session_id));
    let restored = restore.value.as_ref().ok();
    let restored_pad = restored.map(restored_pad);
    let restored_auxiliary_bytes = restored.map(auxiliary_bytes);
    let log_bytes = std::fs::metadata(session.event_log_path()?).ok().map(|meta| meta.len());

    let auxiliary = auxiliary(&session);
    let buffered = measure(|| serde_json::to_vec(&auxiliary));
    let counted = measure(|| count_bytes(&auxiliary));
    let canonical = measure(|| horon_agent_session_store::canonical_json(&auxiliary));

    Ok(json!({
        "fixture": fixture,
        "fixture_bytes": fixture_bytes,
        "fixture_pad_bytes": fixture_pad,
        "largest_cached_pad": cached_pad,
        "largest_cached_record_bytes": cached_record,
        "record_bytes_measured": measured_record,
        "record_bytes_derived": derived_record,
        "record_over_limit": derived_record.is_some_and(|bytes| bytes > FACADE_LIMIT),
        "record_bytes_measured": record_bytes(&session),
        "cache_entry_written": cached,
        "cache_file_bytes": cache_bytes,
        "pad_bytes": pad,
        "auxiliary_serialized_bytes": auxiliary_serialized_bytes,
        "event_log_bytes": log_bytes,
        "save_ms": save.millis,
        "save_peak_bytes": save.peak_bytes,
        "save_ok": save.value.is_ok(),
        "restore_ms": restore.millis,
        "restore_peak_bytes": restore.peak_bytes,
        "restore_ok": restored.is_some(),
        "restored_pad_bytes": restored_pad,
        "restored_auxiliary_serialized_bytes": restored_auxiliary_bytes,
        "auxiliary_intact": restored_pad == Some(pad),
        "decision_buffered_ms": buffered.millis,
        "decision_buffered_peak_bytes": buffered.peak_bytes,
        "decision_buffered_bytes": buffered.value.map(|bytes| bytes.len()).unwrap_or(0),
        "decision_counted_ms": counted.millis,
        "decision_counted_peak_bytes": counted.peak_bytes,
        "decision_counted_bytes": counted.value,
        "decision_canonical_ms": canonical.millis,
        "decision_canonical_peak_bytes": canonical.peak_bytes,
        "decision_canonical_bytes": canonical.value.map(|bytes| bytes.len()).unwrap_or(0),
    }))
}

fn main() -> anyhow::Result<()> {
    let under_fixture = std::env::args().nth(1).expect("under-1MiB fixture path");
    let over_fixture = std::env::args().nth(2).expect("over-1MiB fixture path");
    let home = tempfile::Builder::new().prefix("horon-agent-facade-probe-").tempdir()?;
    // The store refuses an agent home that is not 0700; `TempDir` already created it that way.
    unsafe { std::env::set_var("HORON_AGENT_HOME", home.path()) };

    let under = threshold_report("facade_probe_under", &under_fixture, false)?;
    let over = threshold_report("facade_probe_over", &over_fixture, true)?;
    let report = json!({
        "operation": "facade-threshold",
        "facade_limit_bytes": FACADE_LIMIT,
        "wrapper_bytes": wrapper_bytes(),
        "just_under": under,
        "just_over": over,
        "home": home.path().display().to_string(),
    });
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

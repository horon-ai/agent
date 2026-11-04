//! Measures the source hash both ways - a whole-file read and the streamed digest - and prints each digest, its wall time, its peak temporary allocation and its read volume, plus the save path's pending clone, intended bytes and prefix read. Run it against a real log; it never writes to the measured file.
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use horon_agent_base::message::{ContentBlock, Role};
use horon_agent_base::session::Session;
use horon_agent_session_store as store;
use serde_json::json;

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

struct Counted<R> {
    inner: R,
    bytes: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.bytes += u64::try_from(read).unwrap_or(u64::MAX);
        Ok(read)
    }
}

fn whole_file_digest(path: &Path) -> (String, u64, f64, usize) {
    let measured = measure(|| {
        let mut counted = Counted {
            inner: std::fs::File::open(path).expect("log opens"),
            bytes: 0,
        };
        let mut bytes = Vec::new();
        counted.read_to_end(&mut bytes).expect("log reads");
        (store::sha256_hex(&bytes), counted.bytes)
    });
    (measured.value.0, measured.value.1, measured.millis, measured.peak_bytes)
}

fn streamed_digest(path: &Path) -> (String, u64, f64, usize) {
    let measured = measure(|| {
        let mut counted = Counted {
            inner: std::fs::File::open(path).expect("log opens"),
            bytes: 0,
        };
        let digest = store::sha256_hex_stream(&mut counted).expect("log streams");
        (digest.hex, digest.bytes)
    });
    (measured.value.0, measured.value.1, measured.millis, measured.peak_bytes)
}

fn hash_report(path: &Path) -> serde_json::Value {
    let (whole_hex, whole_read, whole_ms, whole_peak) = whole_file_digest(path);
    let (stream_hex, stream_read, stream_ms, stream_peak) = streamed_digest(path);
    json!({
        "operation": "hash",
        "path": path.display().to_string(),
        "size_bytes": std::fs::metadata(path).expect("log stats").len(),
        "whole_file": {"sha256": whole_hex, "ms": whole_ms, "peak_bytes": whole_peak, "read_bytes": whole_read},
        "streamed": {"sha256": stream_hex, "ms": stream_ms, "peak_bytes": stream_peak, "read_bytes": stream_read},
        "digests_equal": whole_hex == stream_hex,
        "read_bytes_equal": whole_read == stream_read,
        "hash_buffer_bytes": store::HASH_STREAM_BUFFER,
    })
}

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }]
}

fn save_report(messages: usize) -> anyhow::Result<serde_json::Value> {
    let mut session = Session::create_with_id(format!("save_profile_{messages}"), None, None);
    for index in 0..messages {
        session.add_message(Role::User, text(&format!("profile message {index}")));
    }
    session.save()?;
    let log = session.event_log_path()?;
    let id = store::SessionId::parse(&session.id)?;
    let paths = store::SessionPaths::new(
        log.parent()
            .and_then(Path::parent)
            .expect("sessions root")
            .to_path_buf(),
    );
    let log_bytes = std::fs::metadata(&log)?.len();
    let scan = store::full_scan(&paths, &id)?;

    let prefix_read = measure(|| std::fs::read(&log));
    let last = scan.events.last().expect("a saved session has events");
    let one_clone = measure(|| vec![last.clone()]);
    let one_line = measure(|| store::encode_record_line(last).expect("record encodes"));
    let append = measure(|| {
        session.add_message(Role::Assistant, text("profile answer"));
        session.save()
    });

    Ok(json!({
        "operation": "save-profile",
        "messages": messages,
        "events_in_log": scan.events.len(),
        "log_bytes": log_bytes,
        "commit_prefix_read_peak_bytes": prefix_read.peak_bytes,
        "commit_prefix_read_bytes": prefix_read.value.as_ref().map(Vec::len).unwrap_or(0),
        "one_pending_event_clone_peak_bytes": one_clone.peak_bytes,
        "one_pending_event_line_bytes": one_line.value.len(),
        "one_append_save_ms": append.millis,
        "one_append_save_peak_bytes": append.peak_bytes,
        "one_append_save_ok": append.value.is_ok(),
    }))
}

fn main() -> anyhow::Result<()> {
    let command = std::env::args().nth(1).unwrap_or_default();
    let value = match command.as_str() {
        "hash" => {
            let path = std::env::args().nth(2).expect("log path required");
            hash_report(Path::new(&path))
        }
        "save-profile" => save_report(
            std::env::args()
                .nth(2)
                .unwrap_or_else(|| "100".into())
                .parse()
                .expect("message count"),
        )?,
        other => anyhow::bail!("unknown command {other}"),
    };
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

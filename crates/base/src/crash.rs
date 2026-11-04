//! Local crash bundles: one redacted directory per panic.
//!
//! With the hook installed and `features.crash_bundles` enabled, every panic
//! writes its own directory under `<agent dir>/crashes/` holding `bundle.json`
//! and a readable `report.txt`: the panic message, location, and thread, plus
//! the most recent tool dispatches from an in-memory ring of 64. Secret-shaped
//! values are redacted and long fields truncated before the write, and nothing
//! is transmitted anywhere.
//!
//! What this module does NOT do: it does not capture a backtrace beyond what
//! the panic hook reports, does not deduplicate panics (each one gets a new
//! bundle), and cannot write a bundle for a panic that happens before
//! [`install_panic_hook`] runs. Tool operations are recorded only while bundles
//! are enabled, so the ring stays empty otherwise.

use crate::message::redact_secrets;
use serde::Serialize;
use std::collections::VecDeque;
use std::io;
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

pub const OPERATION_RING_CAPACITY: usize = 64;

const BUNDLE_SCHEMA: u32 = 1;
const MAX_FIELD_CHARS: usize = 2_048;
const MAX_TOOL_CHARS: usize = 128;

static OPERATION_SEQ: AtomicU64 = AtomicU64::new(1);

static OPERATIONS: LazyLock<Mutex<VecDeque<OperationRecord>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(OPERATION_RING_CAPACITY)));

static BUNDLES_ENABLED: AtomicBool = AtomicBool::new(false);

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// One tool dispatch as seen by the crash ring, with input already redacted.
#[derive(Debug, Clone, Serialize)]
pub struct OperationRecord {
    pub seq: u64,
    pub tool: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
struct CrashBundle {
    schema: u32,
    captured_at: String,
    pid: u32,
    panic: PanicRecord,
    operations: Vec<OperationRecord>,
}

#[derive(Debug, Serialize)]
struct PanicRecord {
    message: String,
    location: Option<String>,
    thread: String,
}

/// Install the crash-bundle hook, honouring `features.crash_bundles`.
///
/// Later calls only refresh the enabled flag; the hook itself is installed once.
pub fn install_panic_hook() {
    install_panic_hook_with_enabled(crate::config::config().features.crash_bundles);
}

/// Install the crash-bundle hook with an explicit enabled flag.
pub fn install_panic_hook_with_enabled(enabled: bool) {
    BUNDLES_ENABLED.store(enabled, Ordering::SeqCst);
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        previous(info);
        if BUNDLES_ENABLED.load(Ordering::Relaxed) {
            let _ = write_bundle(info);
        }
    }));
}

/// Record one tool dispatch in the process-wide ring.
///
/// The input is attacker-influenced text: it is redacted and truncated here and
/// later written as inert data only, never parsed or executed.
pub fn record_tool_operation(tool: &str, input: &serde_json::Value) {
    if !BUNDLES_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let serialized = serde_json::to_string(input).unwrap_or_else(|_| "<unserializable input>".to_string());
    let record = OperationRecord {
        seq: OPERATION_SEQ.fetch_add(1, Ordering::Relaxed),
        tool: truncate_chars(&redact_secrets(tool), MAX_TOOL_CHARS),
        detail: truncate_chars(&redact_secrets(&serialized), MAX_FIELD_CHARS),
    };

    let mut operations = operations();
    if operations.len() == OPERATION_RING_CAPACITY {
        operations.pop_front();
    }
    operations.push_back(record);
}

fn operations() -> MutexGuard<'static, VecDeque<OperationRecord>> {
    OPERATIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn operations_snapshot() -> Vec<OperationRecord> {
    match OPERATIONS.try_lock() {
        Ok(operations) => operations.iter().cloned().collect(),
        Err(_) => Vec::new(),
    }
}

fn crash_root() -> io::Result<PathBuf> {
    crate::storage::horon_agent_dir()
        .map(|dir| dir.join("crashes"))
        .map_err(|error| io::Error::other(error.to_string()))
}

fn write_bundle(info: &PanicHookInfo<'_>) -> io::Result<PathBuf> {
    let root = crash_root()?;
    let stamp = bundle_stamp();
    let directory = create_bundle_dir(&root, &stamp)?;

    let bundle = CrashBundle {
        schema: BUNDLE_SCHEMA,
        captured_at: redact_field(&stamp),
        pid: std::process::id(),
        panic: PanicRecord {
            message: redact_field(&panic_message(info)),
            location: info.location().map(|location| {
                redact_field(&format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                ))
            }),
            thread: redact_field(&thread_name(info)),
        },
        operations: operations_snapshot(),
    };

    let json = serde_json::to_string_pretty(&bundle).map_err(io::Error::other)?;
    std::fs::write(directory.join("bundle.json"), json)?;
    std::fs::write(directory.join("report.txt"), render_report(&bundle))?;

    Ok(directory)
}

fn bundle_stamp() -> String {
    format!(
        "{}-p{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        std::process::id()
    )
}

fn create_bundle_dir(root: &Path, stamp: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    for attempt in 0..1_000u32 {
        let candidate = if attempt == 0 {
            root.join(stamp)
        } else {
            root.join(format!("{stamp}-{attempt}"))
        };
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique crash bundle directory",
    ))
}

fn panic_message(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "<non-string panic payload>".to_string()
}

fn thread_name(_info: &PanicHookInfo<'_>) -> String {
    let thread = std::thread::current();
    thread
        .name()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{:?}", thread.id()))
}

fn render_report(bundle: &CrashBundle) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "horon-agent crash bundle (local only; nothing was transmitted)");
    let _ = writeln!(out, "schema: {}", bundle.schema);
    let _ = writeln!(out, "captured_at: {}", bundle.captured_at);
    let _ = writeln!(out, "pid: {}", bundle.pid);
    let _ = writeln!(out, "thread: {}", bundle.panic.thread);
    match bundle.panic.location.as_deref() {
        Some(location) => {
            let _ = writeln!(out, "location: {location}");
        }
        None => {
            let _ = writeln!(out, "location: <unknown>");
        }
    }
    let _ = writeln!(out, "panic: {}", bundle.panic.message);
    let _ = writeln!(out);
    let _ = writeln!(out, "last {} tool operations (oldest first):", bundle.operations.len());
    for operation in &bundle.operations {
        let _ = writeln!(
            out,
            "  #{} {} :: {}",
            operation.seq,
            single_line(&operation.tool),
            single_line(&operation.detail)
        );
    }
    out
}

fn redact_field(text: &str) -> String {
    truncate_chars(&redact_secrets(text), MAX_FIELD_CHARS)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((index, _)) => {
            let mut truncated = text[..index].to_string();
            truncated.push_str("[truncated]");
            truncated
        }
        None => text.to_string(),
    }
}

fn single_line(text: &str) -> String {
    text.replace('\r', "\\r").replace('\n', "\\n")
}

#[cfg(test)]
#[path = "crash_tests.rs"]
mod crash_tests;

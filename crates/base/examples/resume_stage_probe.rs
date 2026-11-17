//! Cold resume decomposition probe: which part of a resume pays for what.
//!
//! `loading_probe` reports one number for `Session::load` and one for
//! `Session::history_messages`; both are whole-function wall clocks, so neither
//! says whether the time went into moving bytes, decoding records, building the
//! projection, or serialising the transcript back out. This probe calls the same
//! public store functions the load path calls, in the same order, and times each
//! one. The product path is untouched: no timer is added to it, and the only new
//! cost is this probe's own.
//!
//! A cold `Session::load` runs `SessionCache::recent` (a rebuild that the load
//! then discards), then `graph()` and `replay()`, then a JSON round trip of the
//! whole transcript, then `source_hash()`. `history_messages()` runs `graph()`
//! and `replay()` again. The stages below cut that into read (`log_read`), decode
//! (`full_scan`), index (`graph_replay`), serialise (`metadata_json`,
//! `messages_json`, `messages_decode`) and the passes that repeat work
//! (`checkpoint_recent`, `history_rescan`), plus the bounded `page_index`.
//!
//! Neither read stage is a disk measurement: the fixture was just copied and
//! `checkpoint_recent` already streamed the log, so `log_read` measures the copy
//! and `full_scan` measures JSON work over page-cached bytes. The stages are
//! attribution, not a latency budget: they run one after another in this process,
//! so their sum is not `attach_history` and must never be quoted as one.
//!
//! What it writes: `checkpoint_recent`, `history_rescan` and `page_index` publish
//! the same derived caches the product does. `events.jsonl` is never written and
//! its SHA-256 is reported before and after; blobs are only read. Damage or a
//! torn tail is a failure, never cleaned up.
//!
//! Exit codes: `0` every stage ran; `1` the fixture or a stage disagreed; `2`
//! preflight (bad arguments, `HORON_HOME` mismatch, counters disabled).

use anyhow::{Context, Result, bail, ensure};
use horon_agent_base::session::StoredMessage;
use horon_agent_perf_counters::{Snapshot, snapshot};
use horon_agent_session_store as store;
use serde_json::{Value, json};
use std::time::Instant;

/// Fixed stage order. Callers assert it instead of trusting the JSON order.
pub const ORDER: [&str; 12] = [
    "selection",
    "checkpoint_recent",
    "log_read",
    "full_scan",
    "blob_read",
    "graph_replay",
    "metadata_json",
    "messages_json",
    "messages_decode",
    "source_hash",
    "history_rescan",
    "page_index",
];

/// Every stage ran and every counter was reported.
const EXIT_OK: i32 = 0;
/// A stage value or a fixture expectation disagreed with the run.
const EXIT_MISMATCH: i32 = 1;
/// The probe could not measure at all.
const EXIT_PREFLIGHT: i32 = 2;

const USAGE: &str = "\
resume_stage_probe - cold resume decomposition probe

usage: resume_stage_probe --session <id> --expect-events <n> --expect-messages <n>
                          [--page-limit <n>]

  --session          session id inside $HORON_HOME/sessions
  --expect-events    records the fixture log must contain
  --expect-messages  projected messages every stage must produce
  --page-limit       messages the bounded first-page stage returns (default 50)

Prints one JSON report. events.jsonl is never written; its SHA-256 is reported
before and after. Derived caches are published exactly where the product does.
";

struct Options {
    session: String,
    expect_events: usize,
    expect_messages: usize,
    page_limit: usize,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut session = None;
        let mut expect_events = None;
        let mut expect_messages = None;
        let mut page_limit = 50usize;
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let mut value = || args.next().context(format!("{flag} needs a value"));
            match flag.as_str() {
                "--session" => session = Some(value()?),
                "--expect-events" => expect_events = Some(value()?.parse()?),
                "--expect-messages" => expect_messages = Some(value()?.parse()?),
                "--page-limit" => page_limit = value()?.parse()?,
                other => bail!("unknown argument {other}"),
            }
        }
        ensure!(page_limit > 0, "--page-limit must be positive");
        Ok(Self {
            session: session.context("--session is required")?,
            expect_events: expect_events.context("--expect-events is required")?,
            expect_messages: expect_messages.context("--expect-messages is required")?,
            page_limit,
        })
    }
}

/// Counters reported for every stage. The probe always emits all six; a disabled
/// counter set is a preflight failure, never a row of zeros.
const REQUIRED_COUNTERS: [&str; 6] = [
    "store_read_bytes",
    "blob_bytes_read",
    "hash_bytes_read",
    "full_source_hashes",
    "full_source_hash_bytes",
    "io_read_bytes",
];

fn delta(before: &Snapshot, after: &Snapshot) -> Value {
    json!({
        "store_read_bytes": after.store_read_bytes.saturating_sub(before.store_read_bytes),
        "blob_bytes_read": after.blob_bytes_read.saturating_sub(before.blob_bytes_read),
        "hash_bytes_read": after.hash_bytes_read.saturating_sub(before.hash_bytes_read),
        "full_source_hashes": after.full_source_hashes.saturating_sub(before.full_source_hashes),
        "full_source_hash_bytes": after.full_source_hash_bytes.saturating_sub(before.full_source_hash_bytes),
        "io_read_bytes": after.io_read_bytes.saturating_sub(before.io_read_bytes),
    })
}

struct Stages {
    entries: Vec<Value>,
    failures: Vec<String>,
    preflight: bool,
}

impl Stages {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            failures: Vec::new(),
            preflight: false,
        }
    }

    /// Run one stage; record its time, detail and counters, and keep going.
    fn stage(&mut self, name: &'static str, body: impl FnOnce() -> Result<Value>) {
        let before = snapshot();
        let started = Instant::now();
        let outcome = body();
        let millis = started.elapsed().as_secs_f64() * 1000.0;
        let moved = delta(&before, &snapshot());
        let entry = match outcome {
            Ok(detail) => json!({"name": name, "ms": millis, "ok": true, "detail": detail, "delta": moved}),
            Err(error) => {
                let message = format!("{name}: {error:#}");
                // A store path the probe may not touch is a refusal, not a value.
                self.preflight |= message.contains("unsafe");
                self.failures.push(message.clone());
                json!({"name": name, "ms": millis, "ok": false, "failure": message, "delta": moved})
            }
        };
        self.entries.push(entry);
    }

    fn exit_code(&self) -> i32 {
        if self.preflight {
            EXIT_PREFLIGHT
        } else if self.failures.is_empty() {
            EXIT_OK
        } else {
            EXIT_MISMATCH
        }
    }
}

/// The selection `event_store::selected` resolves.
struct Selection {
    head: Option<store::EventId>,
    /// `Some(false)` when the flag file says there is no selection at all.
    explicit: Option<bool>,
}

/// Read the selection the way `event_store::selected` does: the flag decides
/// first, and only then is `local-head.json` read. The readers are module-private
/// to the session module, so the two file reads are repeated here; the fixture
/// writes neither file, so a pristine home resolves to no selection and the
/// projection follows the graph's shared head, as the product does.
fn selected(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Selection> {
    let directory = paths.session_dir(id)?;
    let explicit = match std::fs::read(directory.join("local-head-explicit.json")) {
        Ok(bytes) => Some(serde_json::from_slice::<bool>(&bytes).context("explicit flag parse")?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if explicit == Some(false) {
        return Ok(Selection { head: None, explicit });
    }
    let head = match std::fs::read(directory.join("local-head.json")) {
        Ok(bytes) => Some(serde_json::from_slice::<store::EventId>(&bytes).context("local head parse")?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(Selection { head, explicit })
}

fn sha256_of(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(store::sha256_hex(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn blob_error(message: &str) -> store::StoreError {
    store::StoreError::internal(store::StoreErrorCode::Internal, message)
}

/// Measure one fixture and print the report; returns the process exit code.
fn run(options: &Options) -> Result<i32> {
    let home = horon_agent_base::storage::horon_agent_dir()?;
    let id = store::SessionId::parse(&options.session)?;
    let paths = horon_agent_base::session::store_paths()?;
    let log = paths.events_log(&id)?;
    if !log.is_file() {
        bail!(
            "fixture log {} is absent; this probe measures the cold path",
            log.display()
        );
    }
    let before_sha = sha256_of(&log)?;
    let before_bytes = std::fs::metadata(&log).map(|m| m.len()).ok();

    let mut stages = Stages::new();
    // A stage that failed must not feed the ones after it: a later number taken
    // from a different head, or from a scan this run never made, would read as
    // evidence for work that did not happen.
    let mut head: Option<store::EventId> = None;
    let mut head_resolved = false;
    let mut scanned = None;

    stages.stage("selection", || {
        let found = selected(&paths, &id)?;
        head = found.head;
        head_resolved = true;
        Ok(json!({"head": head.as_ref().map(store::EventId::as_str), "explicit": found.explicit}))
    });

    stages.stage("checkpoint_recent", || {
        ensure!(head_resolved, "selection did not resolve");
        let cache = store::checkpoint::SessionCache::new(paths.clone(), store::ReadProbe::default());
        let recent = cache.recent(&id, head.as_ref())?;
        ensure!(
            recent.tail.len() == options.expect_messages,
            "checkpoint context has {} messages, expected {}",
            recent.tail.len(),
            options.expect_messages
        );
        Ok(json!({"messages": recent.tail.len(), "head": recent.head.as_str()}))
    });

    // The read floor: the same bytes moved once into one buffer. `checkpoint_recent`
    // already streamed the log, so this copy comes from the page cache.
    stages.stage("log_read", || {
        let bytes = std::fs::read(&log).context("read the fixture log")?;
        ensure!(
            bytes.len() as u64 == before_bytes.unwrap_or(0),
            "read {} bytes, metadata said {}",
            bytes.len(),
            before_bytes.unwrap_or(0)
        );
        Ok(json!({"bytes": bytes.len()}))
    });

    stages.stage("full_scan", || {
        let scan = store::full_scan(&paths, &id)?;
        if let Some(error) = &scan.damage {
            bail!("fixture log is damaged: {error}");
        }
        ensure!(scan.torn_tail.is_empty(), "fixture log has a torn tail");
        ensure!(
            scan.events.len() == options.expect_events,
            "scan found {} records, expected {}",
            scan.events.len(),
            options.expect_events
        );
        let records: u64 = scan.lengths.iter().sum();
        scanned = Some(scan);
        Ok(json!({"events": options.expect_events, "record_bytes": records}))
    });

    // Blob resolution, apart from graph construction. This fixture is fully
    // inline, so a zero here is a measured zero next to the reference count.
    let mut blobs: std::collections::BTreeMap<String, Vec<u8>> = std::collections::BTreeMap::new();
    stages.stage("blob_read", || {
        let scan = scanned.as_ref().context("full_scan did not run")?;
        let (mut bytes, mut count) = (0u64, 0usize);
        for event in &scan.events {
            let Some(reference) = &event.payload_ref else {
                continue;
            };
            if blobs.contains_key(&reference.sha256) {
                continue;
            }
            let path = paths.blob_file(&id, &reference.sha256)?;
            let body = std::fs::read(&path).with_context(|| format!("read blob {}", path.display()))?;
            bytes += body.len() as u64;
            count += 1;
            horon_agent_perf_counters::record_blob_bytes_read(body.len() as u64);
            horon_agent_perf_counters::record_store_bytes_read(body.len() as u64);
            blobs.insert(reference.sha256.clone(), body);
        }
        Ok(json!({"blobs": count, "bytes": bytes}))
    });

    let mut projection = None;
    stages.stage("graph_replay", || {
        let scan = scanned.as_ref().context("full_scan did not run")?;
        let graph = store::EventGraph::with_blobs(&id, &scan.events, |event| {
            let reference = event.payload_ref.as_ref().ok_or_else(|| blob_error("missing blob"))?;
            blobs
                .get(&reference.sha256)
                .cloned()
                .ok_or_else(|| blob_error("blob not read by blob_read"))
        })?;
        let replayed = graph.replay(head.as_ref())?;
        ensure!(
            replayed.active_messages().len() == options.expect_messages,
            "projection has {} active messages, expected {}",
            replayed.active_messages().len(),
            options.expect_messages
        );
        let messages = replayed.messages().len();
        projection = Some(replayed);
        Ok(json!({"messages": messages, "active": options.expect_messages}))
    });

    stages.stage("metadata_json", || {
        let projection = projection.as_ref().context("graph_replay did not run")?;
        let metadata = serde_json::to_value(projection.metadata())?;
        Ok(json!({"fields": metadata.as_object().map(|object| object.len())}))
    });

    let mut messages = Value::Null;
    stages.stage("messages_json", || {
        let projection = projection.as_ref().context("graph_replay did not run")?;
        let owned = projection
            .active_messages()
            .iter()
            .map(|message| &message.message)
            .collect::<Vec<&StoredMessage>>();
        messages = serde_json::to_value(owned)?;
        Ok(json!({"messages": messages.as_array().map(|array| array.len())}))
    });

    // The decode half of the load's round trip. The load decodes the whole
    // `Session` from the tree it just built; that is this decode plus a few dozen
    // scalar fields, so the message array is what it costs.
    stages.stage("messages_decode", || {
        ensure!(messages.is_array(), "messages_json did not produce an array");
        let decoded: Vec<StoredMessage> = serde_json::from_value(messages.take())?;
        ensure!(
            decoded.len() == options.expect_messages,
            "decoded {} messages, expected {}",
            decoded.len(),
            options.expect_messages
        );
        Ok(json!({"messages": decoded.len()}))
    });

    stages.stage("source_hash", || {
        let mut file = std::fs::File::open(&log).context("open the fixture log for hashing")?;
        let digest = store::sha256_hex_stream(&mut file)?;
        horon_agent_perf_counters::record_hash_bytes_read(digest.bytes);
        horon_agent_perf_counters::record_store_bytes_read(digest.bytes);
        horon_agent_perf_counters::record_full_source_hash(digest.bytes);
        ensure!(
            before_sha.as_deref() == Some(digest.hex.as_str()),
            "streamed digest differs from the whole-file digest"
        );
        Ok(json!({"bytes": digest.bytes}))
    });

    // What `history_messages()` pays: the scan and the replay again.
    stages.stage("history_rescan", || {
        ensure!(head_resolved, "selection did not resolve");
        let scan = store::full_scan(&paths, &id)?;
        let graph = store::EventGraph::with_blobs(&id, &scan.events, |event| {
            let reference = event.payload_ref.as_ref().ok_or_else(|| blob_error("missing blob"))?;
            match blobs.get(&reference.sha256) {
                Some(body) => Ok(body.clone()),
                None => Ok(std::fs::read(paths.blob_file(&id, &reference.sha256)?)?),
            }
        })?;
        let replayed = graph.replay(head.as_ref())?;
        ensure!(
            replayed.messages().len() == options.expect_messages,
            "history rescan has {} messages, expected {}",
            replayed.messages().len(),
            options.expect_messages
        );
        Ok(json!({"events": scan.events.len(), "messages": replayed.messages().len()}))
    });

    // What a first page costs once the head-bound index exists. A pristine home
    // has no index, so this stage pays its one build and says so.
    stages.stage("page_index", || {
        ensure!(head_resolved, "selection did not resolve");
        let probe = store::ReadProbe::default();
        let pages = store::HistoryPages::open(&paths, &id, head.as_ref(), &probe)?;
        let end = options.page_limit.min(pages.len());
        let page = pages.page(0, end)?;
        ensure!(
            page.len() == end,
            "first page returned {} messages, expected {end}",
            page.len()
        );
        Ok(json!({"built": pages.built(), "messages": pages.len(), "page": page.len()}))
    });

    let after_sha = sha256_of(&log)?;
    if after_sha != before_sha {
        stages.failures.push("the probe rewrote the fixture log".to_string());
    }
    let counters = snapshot();
    if !counters.enabled {
        stages.preflight = true;
        stages.failures.push(format!(
            "counters are disabled by {}; zeros are not measurements",
            horon_agent_perf_counters::ENV_ENABLE
        ));
    }
    let exit_code = stages.exit_code();
    let failures = std::mem::take(&mut stages.failures);
    let output = json!({
        "schema_version": 1,
        "probe": "resume_stage_probe",
        "additive": false,
        "additive_note": "stages run in one process, one after another; their sum is not attach_history - the only measured latency is the anchor in bench_loading",
        "build": {
            "profile": horon_agent_build_meta::build_profile(),
            "git_hash": horon_agent_build_meta::git_hash(),
            "release_build": horon_agent_build_meta::is_release_build(),
        },
        "home": home.display().to_string(),
        "session": options.session.clone(),
        "expect_events": options.expect_events,
        "expect_messages": options.expect_messages,
        "page_limit": options.page_limit,
        "fixture": {
            "log_path": log.display().to_string(),
            "log_bytes": before_bytes,
            "log_sha256": before_sha,
            "log_sha256_after": after_sha,
        },
        "order": ORDER,
        "counters_required": REQUIRED_COUNTERS,
        "stages": stages.entries,
        "counters": counters,
        "failures": failures,
        "exit_code": exit_code,
        "outcome": if failures.is_empty() { "ok" } else { "failure" },
    });
    println!("{output}");
    Ok(exit_code)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{USAGE}");
        return;
    }
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("resume_stage_probe: {error:#}");
            eprint!("{USAGE}");
            std::process::exit(EXIT_PREFLIGHT);
        }
    };
    match run(&options) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("resume_stage_probe: {error:#}");
            std::process::exit(EXIT_PREFLIGHT);
        }
    }
}

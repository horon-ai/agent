//! Process-wide stage counters: queue depth and buffered bytes, bridge requests
//! awaiting a reply, history page hydrations, store blob and hash bytes,
//! interrupt-poll wakeups, and context condensation (spills, read dedup, aging).
//!
//! The loading baseline lives here too: [`record_store_bytes_read`] counts bytes
//! a session store read (log, cache, or blob) actually pulled from disk,
//! [`record_full_source_hash`] counts whole-log digests, and
//! [`record_request_deep_copy`] counts owned copies of the provider projection
//! built for a request. [`record_skill_dir_read`] counts skill-directory scans
//! made while discovering the skill catalog, [`record_skill_file_read`] counts
//! SKILL.md files opened and the bytes read from them, and
//! [`record_mcp_config_read`] counts MCP configuration files parsed and their bytes,
//! and [`record_hook_spawn`] counts lifecycle-hook child processes actually spawned.
//!
//! Each recording call is one relaxed atomic read (the gate) plus one relaxed
//! atomic add; high-water marks add a compare-and-swap that only spins when a new
//! maximum is seen. Nothing is formatted, allocated, locked, or persisted, and
//! only numbers are stored - never a payload, path, or credential. Snapshots are
//! built on demand, so a request path pays nothing for the reporting surface.
//!
//! Counters are on by default; `HORON_AGENT_PERF_COUNTERS=0|off|false|no` makes
//! every recording call return immediately and the snapshot reports
//! `enabled: false`. Read them with `horon-agent debug 'server:memory'`
//! (`process.stage_counters`) or with
//! `cargo run -p horon-agent-base --example perf_counters_surface`, which needs
//! no running daemon. FD and thread counts stay in `horon_agent_base::process_memory`.
//!
//! Values are cumulative since process start; [`reset`] exists for tests and
//! benchmark harnesses.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use serde::Serialize;

pub const ENV_ENABLE: &str = "HORON_AGENT_PERF_COUNTERS";

static ENABLED: OnceLock<bool> = OnceLock::new();

static QUEUE_DEPTH_HIGH_WATER: AtomicU64 = AtomicU64::new(0);
static QUEUE_BYTES_BUFFERED: AtomicU64 = AtomicU64::new(0);
static QUEUE_BYTES_HIGH_WATER: AtomicU64 = AtomicU64::new(0);
static PENDING_REQUESTS: AtomicU64 = AtomicU64::new(0);
static PENDING_REQUESTS_HIGH_WATER: AtomicU64 = AtomicU64::new(0);
static PAGE_HYDRATIONS: AtomicU64 = AtomicU64::new(0);
static PAGE_MESSAGES_HYDRATED: AtomicU64 = AtomicU64::new(0);
static HISTORY_PAGE_REPLAYED: AtomicU64 = AtomicU64::new(0);
static HISTORY_PAGE_FLATTENED: AtomicU64 = AtomicU64::new(0);
static HISTORY_PAGE_WIRE_BYTES: AtomicU64 = AtomicU64::new(0);
static HISTORY_INDEX_BUILDS: AtomicU64 = AtomicU64::new(0);
static BLOB_BYTES_READ: AtomicU64 = AtomicU64::new(0);
static HASH_BYTES_READ: AtomicU64 = AtomicU64::new(0);
static INTERRUPT_POLL_WAKEUPS: AtomicU64 = AtomicU64::new(0);
static IO_READ_BYTES: AtomicU64 = AtomicU64::new(0);
static IO_WRITE_BYTES: AtomicU64 = AtomicU64::new(0);
static IO_PUBLISHED_BYTES: AtomicU64 = AtomicU64::new(0);
static STORE_READ_BYTES: AtomicU64 = AtomicU64::new(0);
static FULL_SOURCE_HASHES: AtomicU64 = AtomicU64::new(0);
static FULL_SOURCE_HASH_BYTES: AtomicU64 = AtomicU64::new(0);
static REQUEST_DEEP_COPIES: AtomicU64 = AtomicU64::new(0);
static REQUEST_DEEP_COPY_BYTES: AtomicU64 = AtomicU64::new(0);
static REQUEST_MESSAGES_COPIED: AtomicU64 = AtomicU64::new(0);
static SKILL_DIR_READS: AtomicU64 = AtomicU64::new(0);
static SKILL_FILE_READS: AtomicU64 = AtomicU64::new(0);
static SKILL_BYTES_READ: AtomicU64 = AtomicU64::new(0);
static MCP_CONFIG_READS: AtomicU64 = AtomicU64::new(0);
static MCP_CONFIG_BYTES: AtomicU64 = AtomicU64::new(0);
static TOOL_OUTPUT_SPILLS: AtomicU64 = AtomicU64::new(0);
static TOOL_OUTPUT_SPILL_ORIGINAL_BYTES: AtomicU64 = AtomicU64::new(0);
static TOOL_OUTPUT_SPILL_KEPT_BYTES: AtomicU64 = AtomicU64::new(0);
static READ_UNCHANGED_HITS: AtomicU64 = AtomicU64::new(0);
static READ_DELTA_HITS: AtomicU64 = AtomicU64::new(0);
static READ_BYTES_AVOIDED: AtomicU64 = AtomicU64::new(0);
static TOOL_RESULT_AGING_BATCHES: AtomicU64 = AtomicU64::new(0);
static TOOL_RESULT_AGED_RESULTS: AtomicU64 = AtomicU64::new(0);
static TOOL_RESULT_AGED_BYTES: AtomicU64 = AtomicU64::new(0);
static HOOK_PROCESS_SPAWNS: AtomicU64 = AtomicU64::new(0);

/// Per-tool counts of calls a schema repair rewrote. The only counter keyed by
/// a name, because "how often is this model's writer wrong about `bash`" is the
/// question this exists to answer.
static TOOL_ARG_REPAIRS: LazyLock<Mutex<BTreeMap<String, u64>>> = LazyLock::new(Mutex::default);

/// Read once from [`ENV_ENABLE`] and cached, so the gate stays a single load and
/// changing the variable mid-process cannot make counters inconsistent.
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        let Ok(value) = std::env::var(ENV_ENABLE) else {
            return true;
        };
        !matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "off" | "false" | "no")
    })
}

#[inline]
fn raise_high_water(slot: &AtomicU64, value: u64) {
    let mut current = slot.load(Ordering::Relaxed);
    while value > current {
        match slot.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

/// Call after a queue mutation, so the high-water marks describe the deepest
/// depth reached rather than a periodic sample.
pub fn observe_queue(depth: u64, bytes: u64) {
    if !enabled() {
        return;
    }
    raise_high_water(&QUEUE_DEPTH_HIGH_WATER, depth);
    raise_high_water(&QUEUE_BYTES_HIGH_WATER, bytes);
    QUEUE_BYTES_BUFFERED.store(bytes, Ordering::Relaxed);
}

pub fn observe_pending_requests(pending: u64) {
    if !enabled() {
        return;
    }
    PENDING_REQUESTS.store(pending, Ordering::Relaxed);
    raise_high_water(&PENDING_REQUESTS_HIGH_WATER, pending);
}

pub fn record_page_hydration(messages: u64) {
    if !enabled() {
        return;
    }
    PAGE_HYDRATIONS.fetch_add(1, Ordering::Relaxed);
    PAGE_MESSAGES_HYDRATED.fetch_add(messages, Ordering::Relaxed);
}

/// One history page request, split into the three costs that decide it.
///
/// `replayed` counts the messages this request had to replay because no index
/// matched its head; a warm page reports zero however large the session is.
/// `flattened` counts the messages serialized into the reply, and `wire` its
/// serialized bytes.
pub fn record_history_page(replayed: u64, flattened: u64, wire: u64) {
    if !enabled() {
        return;
    }
    HISTORY_PAGE_REPLAYED.fetch_add(replayed, Ordering::Relaxed);
    HISTORY_PAGE_FLATTENED.fetch_add(flattened, Ordering::Relaxed);
    HISTORY_PAGE_WIRE_BYTES.fetch_add(wire, Ordering::Relaxed);
}

/// One page-index rebuild, and how many messages it had to replay to build it.
pub fn record_history_index_build(messages: u64) {
    if !enabled() {
        return;
    }
    HISTORY_INDEX_BUILDS.fetch_add(1, Ordering::Relaxed);
    HISTORY_PAGE_REPLAYED.fetch_add(messages, Ordering::Relaxed);
}

pub fn record_blob_bytes_read(bytes: u64) {
    if !enabled() {
        return;
    }
    BLOB_BYTES_READ.fetch_add(bytes, Ordering::Relaxed);
}

pub fn record_hash_bytes_read(bytes: u64) {
    if !enabled() {
        return;
    }
    HASH_BYTES_READ.fetch_add(bytes, Ordering::Relaxed);
}

pub fn record_interrupt_poll_wakeup() {
    if !enabled() {
        return;
    }
    INTERRUPT_POLL_WAKEUPS.fetch_add(1, Ordering::Relaxed);
}

/// `read`/`write` are the original log's stream bytes; `published` is whole-file
/// derived publications (cache, index, catalog), which replace their file and so
/// are not part of that append stream.
pub fn record_io(read: u64, write: u64, published: u64) {
    if !enabled() {
        return;
    }
    if read != 0 {
        IO_READ_BYTES.fetch_add(read, Ordering::Relaxed);
    }
    if write != 0 {
        IO_WRITE_BYTES.fetch_add(write, Ordering::Relaxed);
    }
    if published != 0 {
        IO_PUBLISHED_BYTES.fetch_add(published, Ordering::Relaxed);
    }
}

/// Bytes the session store actually read from disk.
///
/// One count per observed read syscall, including the log, derived caches and
/// original blobs. This is the store's own read volume, so a caller can tell a
/// full replay from a page without subtracting unrelated I/O.
pub fn record_store_bytes_read(bytes: u64) {
    if !enabled() {
        return;
    }
    STORE_READ_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

/// One whole-log digest, and the bytes it had to read to produce it.
///
/// A loading path that re-digests the same log twice reports two here; that is
/// the duplication this counter exists to expose.
pub fn record_full_source_hash(bytes: u64) {
    if !enabled() {
        return;
    }
    FULL_SOURCE_HASHES.fetch_add(1, Ordering::Relaxed);
    FULL_SOURCE_HASH_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

/// One owned copy of the provider projection built for a request.
///
/// `bytes` is the serialized size of the messages duplicated by the copy, and
/// `messages` their count. A request path that borrows the projection instead of
/// cloning it records nothing.
pub fn record_request_deep_copy(bytes: u64, messages: u64) {
    if !enabled() {
        return;
    }
    REQUEST_DEEP_COPIES.fetch_add(1, Ordering::Relaxed);
    REQUEST_DEEP_COPY_BYTES.fetch_add(bytes, Ordering::Relaxed);
    REQUEST_MESSAGES_COPIED.fetch_add(messages, Ordering::Relaxed);
}

/// One directory scan performed while discovering the skill catalog.
///
/// Counted per `read_dir` call, never per skill file, so a reload of the same
/// directory tree reports the same count however many skills it holds.
pub fn record_skill_dir_read() {
    if !enabled() {
        return;
    }
    SKILL_DIR_READS.fetch_add(1, Ordering::Relaxed);
}

/// One SKILL.md file read while building or consulting the skill catalog.
///
/// `bytes` is the file length that read consumed, so a discovery path that only
/// reads a frontmatter prefix reports the prefix bytes, not the whole body.
pub fn record_skill_file_read(bytes: u64) {
    if !enabled() {
        return;
    }
    SKILL_FILE_READS.fetch_add(1, Ordering::Relaxed);
    SKILL_BYTES_READ.fetch_add(bytes, Ordering::Relaxed);
}

/// One MCP configuration file parsed, with the bytes it contained.
///
/// Counted per file read, whether the source is a user-level file or a
/// project-local config the workspace trust check authorized.
pub fn record_mcp_config_read(bytes: u64) {
    if !enabled() {
        return;
    }
    MCP_CONFIG_READS.fetch_add(1, Ordering::Relaxed);
    MCP_CONFIG_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

/// One oversized tool output condensed for the model's context, with the full
/// text saved as a session artifact. `kept_bytes` is what the model received.
pub fn record_tool_output_spill(original_bytes: u64, kept_bytes: u64) {
    if !enabled() {
        return;
    }
    TOOL_OUTPUT_SPILLS.fetch_add(1, Ordering::Relaxed);
    TOOL_OUTPUT_SPILL_ORIGINAL_BYTES.fetch_add(original_bytes, Ordering::Relaxed);
    TOOL_OUTPUT_SPILL_KEPT_BYTES.fetch_add(kept_bytes, Ordering::Relaxed);
}

/// One tool call whose arguments a schema-directed repair rewrote, counted
/// against the tool that ran them. A repaired call is the exception, so the
/// per-tool map stays small and no name is formatted unless one happens.
pub fn record_tool_arg_repair(tool: &str) {
    if !enabled() {
        return;
    }
    let mut repairs = TOOL_ARG_REPAIRS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match repairs.get_mut(tool) {
        Some(count) => *count += 1,
        None => {
            repairs.insert(tool.to_string(), 1);
        }
    }
}

/// A repeated `read` of unchanged lines answered with a one-line notice.
pub fn record_read_unchanged(avoided_bytes: u64) {
    if !enabled() {
        return;
    }
    READ_UNCHANGED_HITS.fetch_add(1, Ordering::Relaxed);
    READ_BYTES_AVOIDED.fetch_add(avoided_bytes, Ordering::Relaxed);
}

/// A repeated `read` of a changed file answered with a diff.
pub fn record_read_delta(avoided_bytes: u64) {
    if !enabled() {
        return;
    }
    READ_DELTA_HITS.fetch_add(1, Ordering::Relaxed);
    READ_BYTES_AVOIDED.fetch_add(avoided_bytes, Ordering::Relaxed);
}

/// One batch that condensed older tool results; one batch is one prompt-cache
/// prefix rewrite, however many results it touched.
pub fn record_tool_result_aging(results: u64, removed_bytes: u64) {
    if !enabled() {
        return;
    }
    TOOL_RESULT_AGING_BATCHES.fetch_add(1, Ordering::Relaxed);
    TOOL_RESULT_AGED_RESULTS.fetch_add(results, Ordering::Relaxed);
    TOOL_RESULT_AGED_BYTES.fetch_add(removed_bytes, Ordering::Relaxed);
}

/// One hook child process actually spawned.
///
/// Every lifecycle hook is an external command, so this is the counter that
/// shows whether a hook is free when it is unset: a request path that checks
/// its configuration first spawns nothing and leaves the count alone.
pub fn record_hook_spawn() {
    if !enabled() {
        return;
    }
    HOOK_PROCESS_SPAWNS.fetch_add(1, Ordering::Relaxed);
}

/// Tests and benchmark harnesses only; nothing in the daemon calls it.
pub fn reset() {
    for slot in [
        &QUEUE_DEPTH_HIGH_WATER,
        &QUEUE_BYTES_BUFFERED,
        &QUEUE_BYTES_HIGH_WATER,
        &PENDING_REQUESTS,
        &PENDING_REQUESTS_HIGH_WATER,
        &PAGE_HYDRATIONS,
        &PAGE_MESSAGES_HYDRATED,
        &HISTORY_PAGE_REPLAYED,
        &HISTORY_PAGE_FLATTENED,
        &HISTORY_PAGE_WIRE_BYTES,
        &HISTORY_INDEX_BUILDS,
        &BLOB_BYTES_READ,
        &HASH_BYTES_READ,
        &INTERRUPT_POLL_WAKEUPS,
        &IO_READ_BYTES,
        &IO_WRITE_BYTES,
        &IO_PUBLISHED_BYTES,
        &STORE_READ_BYTES,
        &FULL_SOURCE_HASHES,
        &FULL_SOURCE_HASH_BYTES,
        &REQUEST_DEEP_COPIES,
        &REQUEST_DEEP_COPY_BYTES,
        &REQUEST_MESSAGES_COPIED,
        &SKILL_DIR_READS,
        &SKILL_FILE_READS,
        &SKILL_BYTES_READ,
        &MCP_CONFIG_READS,
        &MCP_CONFIG_BYTES,
        &TOOL_OUTPUT_SPILLS,
        &TOOL_OUTPUT_SPILL_ORIGINAL_BYTES,
        &TOOL_OUTPUT_SPILL_KEPT_BYTES,
        &READ_UNCHANGED_HITS,
        &READ_DELTA_HITS,
        &READ_BYTES_AVOIDED,
        &TOOL_RESULT_AGING_BATCHES,
        &TOOL_RESULT_AGED_RESULTS,
        &TOOL_RESULT_AGED_BYTES,
        &HOOK_PROCESS_SPAWNS,
    ] {
        slot.store(0, Ordering::Relaxed);
    }
    TOOL_ARG_REPAIRS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    /// False when [`ENV_ENABLE`] disabled recording; every counter then reads 0,
    /// which is "not measured" rather than "measured zero".
    pub enabled: bool,
    pub queue_depth_high_water: u64,
    pub queue_bytes_buffered: u64,
    pub queue_bytes_high_water: u64,
    pub pending_requests: u64,
    pub pending_requests_high_water: u64,
    pub page_hydrations: u64,
    pub page_messages_hydrated: u64,
    pub history_page_replayed: u64,
    pub history_page_flattened: u64,
    pub history_page_wire_bytes: u64,
    pub history_index_builds: u64,
    pub blob_bytes_read: u64,
    pub hash_bytes_read: u64,
    pub interrupt_poll_wakeups: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub io_published_bytes: u64,
    pub store_read_bytes: u64,
    pub full_source_hashes: u64,
    pub full_source_hash_bytes: u64,
    pub request_deep_copies: u64,
    pub request_deep_copy_bytes: u64,
    pub request_messages_copied: u64,
    pub skill_dir_reads: u64,
    pub skill_file_reads: u64,
    pub skill_bytes_read: u64,
    pub mcp_config_reads: u64,
    pub mcp_config_bytes: u64,
    pub tool_output_spills: u64,
    pub tool_output_spill_original_bytes: u64,
    pub tool_output_spill_kept_bytes: u64,
    pub read_unchanged_hits: u64,
    pub read_delta_hits: u64,
    pub read_bytes_avoided: u64,
    pub tool_result_aging_batches: u64,
    pub tool_result_aged_results: u64,
    pub tool_result_aged_bytes: u64,
    pub hook_process_spawns: u64,
    /// Repaired calls per tool, ready to read without another lookup.
    pub tool_arg_repairs: BTreeMap<String, u64>,
}

impl Snapshot {
    /// FD and thread high-water marks are deliberately absent: those are process
    /// facts read from the OS, and one process's peak is never summed into another's.
    pub fn resource_high_water(&self) -> ResourceHighWater {
        ResourceHighWater {
            queue_depth: self.queue_depth_high_water,
            queue_bytes: self.queue_bytes_high_water,
            pending_requests: self.pending_requests_high_water,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourceHighWater {
    pub queue_depth: u64,
    pub queue_bytes: u64,
    pub pending_requests: u64,
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        enabled: enabled(),
        queue_depth_high_water: QUEUE_DEPTH_HIGH_WATER.load(Ordering::Relaxed),
        queue_bytes_buffered: QUEUE_BYTES_BUFFERED.load(Ordering::Relaxed),
        queue_bytes_high_water: QUEUE_BYTES_HIGH_WATER.load(Ordering::Relaxed),
        pending_requests: PENDING_REQUESTS.load(Ordering::Relaxed),
        pending_requests_high_water: PENDING_REQUESTS_HIGH_WATER.load(Ordering::Relaxed),
        page_hydrations: PAGE_HYDRATIONS.load(Ordering::Relaxed),
        page_messages_hydrated: PAGE_MESSAGES_HYDRATED.load(Ordering::Relaxed),
        history_page_replayed: HISTORY_PAGE_REPLAYED.load(Ordering::Relaxed),
        history_page_flattened: HISTORY_PAGE_FLATTENED.load(Ordering::Relaxed),
        history_page_wire_bytes: HISTORY_PAGE_WIRE_BYTES.load(Ordering::Relaxed),
        history_index_builds: HISTORY_INDEX_BUILDS.load(Ordering::Relaxed),
        blob_bytes_read: BLOB_BYTES_READ.load(Ordering::Relaxed),
        hash_bytes_read: HASH_BYTES_READ.load(Ordering::Relaxed),
        interrupt_poll_wakeups: INTERRUPT_POLL_WAKEUPS.load(Ordering::Relaxed),
        io_read_bytes: IO_READ_BYTES.load(Ordering::Relaxed),
        io_write_bytes: IO_WRITE_BYTES.load(Ordering::Relaxed),
        io_published_bytes: IO_PUBLISHED_BYTES.load(Ordering::Relaxed),
        store_read_bytes: STORE_READ_BYTES.load(Ordering::Relaxed),
        full_source_hashes: FULL_SOURCE_HASHES.load(Ordering::Relaxed),
        full_source_hash_bytes: FULL_SOURCE_HASH_BYTES.load(Ordering::Relaxed),
        request_deep_copies: REQUEST_DEEP_COPIES.load(Ordering::Relaxed),
        request_deep_copy_bytes: REQUEST_DEEP_COPY_BYTES.load(Ordering::Relaxed),
        request_messages_copied: REQUEST_MESSAGES_COPIED.load(Ordering::Relaxed),
        skill_dir_reads: SKILL_DIR_READS.load(Ordering::Relaxed),
        skill_file_reads: SKILL_FILE_READS.load(Ordering::Relaxed),
        skill_bytes_read: SKILL_BYTES_READ.load(Ordering::Relaxed),
        mcp_config_reads: MCP_CONFIG_READS.load(Ordering::Relaxed),
        mcp_config_bytes: MCP_CONFIG_BYTES.load(Ordering::Relaxed),
        tool_output_spills: TOOL_OUTPUT_SPILLS.load(Ordering::Relaxed),
        tool_output_spill_original_bytes: TOOL_OUTPUT_SPILL_ORIGINAL_BYTES.load(Ordering::Relaxed),
        tool_output_spill_kept_bytes: TOOL_OUTPUT_SPILL_KEPT_BYTES.load(Ordering::Relaxed),
        read_unchanged_hits: READ_UNCHANGED_HITS.load(Ordering::Relaxed),
        read_delta_hits: READ_DELTA_HITS.load(Ordering::Relaxed),
        read_bytes_avoided: READ_BYTES_AVOIDED.load(Ordering::Relaxed),
        tool_result_aging_batches: TOOL_RESULT_AGING_BATCHES.load(Ordering::Relaxed),
        tool_result_aged_results: TOOL_RESULT_AGED_RESULTS.load(Ordering::Relaxed),
        tool_result_aged_bytes: TOOL_RESULT_AGED_BYTES.load(Ordering::Relaxed),
        hook_process_spawns: HOOK_PROCESS_SPAWNS.load(Ordering::Relaxed),
        tool_arg_repairs: TOOL_ARG_REPAIRS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_water_never_decreases() {
        reset();
        observe_queue(3, 30);
        observe_queue(1, 10);
        let snapshot = snapshot();
        assert_eq!(snapshot.queue_depth_high_water, 3);
        assert_eq!(snapshot.queue_bytes_high_water, 30);
        assert_eq!(
            snapshot.queue_bytes_buffered, 10,
            "the gauge follows the latest reading"
        );
    }

    #[test]
    fn counters_are_cumulative() {
        reset();
        record_blob_bytes_read(100);
        record_blob_bytes_read(25);
        record_hash_bytes_read(64);
        record_page_hydration(2);
        record_history_page(0, 20, 4_096);
        record_history_index_build(9_999);
        record_interrupt_poll_wakeup();
        record_io(10, 20, 30);
        record_store_bytes_read(700);
        record_store_bytes_read(7);
        record_full_source_hash(512);
        record_request_deep_copy(1_024, 4);
        record_skill_dir_read();
        record_skill_dir_read();
        record_skill_file_read(4_096);
        record_skill_file_read(128);
        record_mcp_config_read(300);
        record_mcp_config_read(20);
        record_tool_output_spill(1_000, 100);
        record_tool_output_spill(500, 50);
        record_read_unchanged(40);
        record_read_delta(60);
        record_tool_result_aging(3, 9_000);
        let snapshot = snapshot();
        assert_eq!(snapshot.history_page_replayed, 9_999);
        assert_eq!(snapshot.history_index_builds, 1);
        assert_eq!(snapshot.history_page_flattened, 20);
        assert_eq!(snapshot.history_page_wire_bytes, 4_096);
        assert_eq!(snapshot.blob_bytes_read, 125);
        assert_eq!(snapshot.hash_bytes_read, 64);
        assert_eq!(snapshot.page_hydrations, 1);
        assert_eq!(snapshot.page_messages_hydrated, 2);
        assert_eq!(snapshot.interrupt_poll_wakeups, 1);
        assert_eq!(snapshot.io_read_bytes, 10);
        assert_eq!(snapshot.io_write_bytes, 20);
        assert_eq!(snapshot.io_published_bytes, 30);
        assert_eq!(snapshot.store_read_bytes, 707);
        assert_eq!(snapshot.full_source_hashes, 1);
        assert_eq!(snapshot.full_source_hash_bytes, 512);
        assert_eq!(snapshot.request_deep_copies, 1);
        assert_eq!(snapshot.request_deep_copy_bytes, 1_024);
        assert_eq!(snapshot.request_messages_copied, 4);
        assert_eq!(snapshot.skill_dir_reads, 2);
        assert_eq!(snapshot.skill_file_reads, 2);
        assert_eq!(snapshot.skill_bytes_read, 4_224);
        assert_eq!(snapshot.mcp_config_reads, 2);
        assert_eq!(snapshot.mcp_config_bytes, 320);
        assert_eq!(snapshot.tool_output_spills, 2);
        assert_eq!(snapshot.tool_output_spill_original_bytes, 1_500);
        assert_eq!(snapshot.tool_output_spill_kept_bytes, 150);
        assert_eq!(snapshot.read_unchanged_hits, 1);
        assert_eq!(snapshot.read_delta_hits, 1);
        assert_eq!(snapshot.read_bytes_avoided, 100);
        assert_eq!(snapshot.tool_result_aging_batches, 1);
        assert_eq!(snapshot.tool_result_aged_results, 3);
        assert_eq!(snapshot.tool_result_aged_bytes, 9_000);
    }

    #[test]
    fn snapshot_serializes_every_counter_by_name() {
        reset();
        let value = serde_json::to_value(snapshot()).expect("snapshot serializes");
        for key in [
            "enabled",
            "queue_depth_high_water",
            "queue_bytes_buffered",
            "queue_bytes_high_water",
            "pending_requests",
            "pending_requests_high_water",
            "page_hydrations",
            "page_messages_hydrated",
            "history_page_replayed",
            "history_page_flattened",
            "history_page_wire_bytes",
            "history_index_builds",
            "blob_bytes_read",
            "hash_bytes_read",
            "interrupt_poll_wakeups",
            "io_read_bytes",
            "io_write_bytes",
            "io_published_bytes",
            "store_read_bytes",
            "full_source_hashes",
            "full_source_hash_bytes",
            "request_deep_copies",
            "request_deep_copy_bytes",
            "request_messages_copied",
            "skill_dir_reads",
            "skill_file_reads",
            "skill_bytes_read",
            "mcp_config_reads",
            "mcp_config_bytes",
            "tool_output_spills",
            "tool_output_spill_original_bytes",
            "tool_output_spill_kept_bytes",
            "read_unchanged_hits",
            "read_delta_hits",
            "read_bytes_avoided",
            "tool_result_aging_batches",
            "tool_result_aged_results",
            "tool_result_aged_bytes",
            "tool_arg_repairs",
        ] {
            assert!(value.get(key).is_some(), "missing counter {key}");
        }
    }
}

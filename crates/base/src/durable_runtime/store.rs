use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use chrono::Utc;
use uuid::Uuid;

use super::record::{Checkpoint, JOURNAL_VERSION, JournalRecord, Payload, RecordKind, RuntimeState};
use super::replay::{self, RetentionCaps, ScanReport, ScanStart};
use super::{JournalError, JournalResult};

pub const JOURNAL_DIR_NAME: &str = "durable-runtime";
pub const JOURNAL_FILE_NAME: &str = "runtime.jsonl";
pub const WRITER_LOCK_NAME: &str = "writer.lock";
pub const RECOVERY_PREFIX: &str = ".recovery-";
const TEMP_PREFIX: &str = ".runtime.tmp-";

/// Hard ceiling for one session journal, recovery tails excluded.
pub const MAX_JOURNAL_BYTES: u64 = 32 * 1024 * 1024;
/// Bytes of the journal that admissions never enter.
///
/// The reserve belongs to every lifecycle row, not only to completion and
/// cancellation: completion, cancellation, return, in-doubt, run, control,
/// monitor, wake, and checkpoint rows can all be written inside it after the
/// admission ceiling is reached. Nothing else is kept out of it, so a store that
/// has stopped accepting new work can still record how the admitted work ended.
pub const LIFECYCLE_RESERVE_BYTES: u64 = 1024 * 1024;
pub const COMPACT_AFTER_BYTES: u64 = 4 * 1024 * 1024;
pub const COMPACT_AFTER_RECORDS: u64 = 4_096;
pub const MAX_LIVE_INPUTS: usize = 1_000;
pub const MAX_LIVE_INPUT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_INPUT_PAYLOAD_BYTES: u64 = 1024 * 1024;
/// Most recent terminal (stopped) runs a checkpoint keeps.
///
/// 256 keeps the recent run history a runtime-state view or a diagnosis reads
/// while holding the checkpoint's run section to a few tens of kilobytes, far
/// below the 4 MiB compaction threshold. An older run is pruned, so a lookup of
/// it is unknown rather than a fabricated answer.
pub const TERMINAL_RUN_CAP: usize = 256;
/// Most recent terminal input receipts, terminal controls, stopped monitors, and
/// acknowledged wakes a checkpoint keeps.
///
/// 1,024 is the plan's receipt bound for control and answer receipts: an id
/// whose result a client may still ask for keeps that result, an older id is
/// unknown and the SDK never retries it automatically.
pub const TERMINAL_RECEIPT_CAP: usize = 1_024;

/// Syscall boundaries a store can observe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalIoPoint {
    WriterLockAcquire,
    WriterLockRelease,
    RecordWrite,
    RecordFileSync,
    RecordDirectorySync,
    RecordRollbackFileSync,
    /// The parent of a journal directory level this open created, flushed so the new entry survives.
    DirectoryEntrySync,
    RecoveryTailWrite,
    RecoveryTailFileSync,
    RecoveryTailDirectorySync,
    RewriteTempWrite,
    RewriteFileSync,
    RewriteRename,
    RewriteDirectorySync,
}

/// Test-only fault injection and instrumentation handle.
///
/// The hook runs immediately before the named syscall, so returning an error
/// prevents that syscall. It can never make a failing syscall report success,
/// which is why an injected failure is evidence about ordering and rollback,
/// never evidence that data is durable.
#[cfg(any(test, feature = "test-support"))]
pub type JournalHook = Box<dyn FnMut(JournalIoPoint) -> std::io::Result<()> + Send>;

#[cfg(any(test, feature = "test-support"))]
type HookSlot = Option<JournalHook>;
#[cfg(not(any(test, feature = "test-support")))]
type HookSlot = ();

#[cfg(any(test, feature = "test-support"))]
pub fn hook(observe: impl FnMut(JournalIoPoint) -> std::io::Result<()> + Send + 'static) -> JournalHook {
    Box::new(observe)
}

/// The budgets one store enforces. The defaults are the contract values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalLimits {
    pub max_journal_bytes: u64,
    /// Bytes admissions may not enter; every lifecycle row may.
    pub reserve_bytes: u64,
    pub compact_bytes: u64,
    pub compact_records: u64,
    pub max_live_inputs: usize,
    pub max_live_input_bytes: u64,
    pub max_input_payload_bytes: u64,
    pub terminal_receipt_cap: usize,
    pub terminal_run_cap: usize,
}

impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            max_journal_bytes: MAX_JOURNAL_BYTES,
            reserve_bytes: LIFECYCLE_RESERVE_BYTES,
            compact_bytes: COMPACT_AFTER_BYTES,
            compact_records: COMPACT_AFTER_RECORDS,
            max_live_inputs: MAX_LIVE_INPUTS,
            max_live_input_bytes: MAX_LIVE_INPUT_BYTES,
            max_input_payload_bytes: MAX_INPUT_PAYLOAD_BYTES,
            terminal_receipt_cap: TERMINAL_RECEIPT_CAP,
            terminal_run_cap: TERMINAL_RUN_CAP,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct JournalOptions {
    pub limits: JournalLimits,
    pub hooks: Option<JournalHook>,
}

/// What one repair preserved and rewrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryReport {
    pub source_generation: u64,
    pub preserved_tail: PathBuf,
    pub preserved_bytes: u64,
    pub rewritten_bytes: u64,
    pub rewritten_seq: u64,
}

/// A fresh internal identity for an input, run, monitor, wake, or operation.
pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// One session's durable runtime journal.
///
/// The store is a synchronous writer: every call that can touch the disk blocks,
/// so the async layer must run it on a blocking thread rather than inside a
/// Tokio worker. No lock is held across an await, a provider call, or a wait for
/// a user answer.
pub struct DurableRuntimeStore {
    session_id: String,
    dir: PathBuf,
    journal_path: PathBuf,
    lock_path: PathBuf,
    process_lock: Arc<Mutex<()>>,
    state: RuntimeState,
    generation: u64,
    last_seq: u64,
    file_len: u64,
    record_count: u64,
    limits: JournalLimits,
    read_only: bool,
    poisoned: Option<String>,
    recoveries: Vec<RecoveryReport>,
    hooks: HookSlot,
}

impl DurableRuntimeStore {
    /// Open the journal for one session under the agent home.
    pub fn open(session_id: &str) -> JournalResult<Self> {
        let root = crate::storage::horon_agent_dir()
            .map_err(|error| JournalError::new(super::JournalErrorKind::Io, format!("no agent home: {error}")))?
            .join(JOURNAL_DIR_NAME);
        Self::open_in(&root, session_id)
    }

    /// Open the journal for one session under an explicit journal root.
    ///
    /// Tests, the manual QA binary, and any caller with its own durable root use
    /// this instead of redirecting the process home.
    pub fn open_in(root: &Path, session_id: &str) -> JournalResult<Self> {
        #[cfg(any(test, feature = "test-support"))]
        {
            Self::open_inner(root, session_id, JournalLimits::default(), None)
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            Self::open_inner(root, session_id, JournalLimits::default(), ())
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open_in_with(root: &Path, session_id: &str, options: JournalOptions) -> JournalResult<Self> {
        Self::open_inner(root, session_id, options.limits, options.hooks)
    }

    fn open_inner(root: &Path, session_id: &str, limits: JournalLimits, mut hooks: HookSlot) -> JournalResult<Self> {
        validate_session_id(session_id)?;
        let dir = root.join(session_id);
        ensure_private_dir(&dir, &mut hooks)?;
        let mut store = Self {
            session_id: session_id.to_string(),
            journal_path: dir.join(JOURNAL_FILE_NAME),
            lock_path: dir.join(WRITER_LOCK_NAME),
            process_lock: process_lock_for(&dir),
            dir,
            state: RuntimeState::default(),
            generation: 0,
            last_seq: 0,
            file_len: 0,
            record_count: 0,
            limits,
            read_only: false,
            poisoned: None,
            recoveries: Vec::new(),
            hooks,
        };
        store.with_writer_lock(|store| store.load_locked())?;
        Ok(store)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    pub fn directory(&self) -> &Path {
        &self.dir
    }

    /// The generation of the file on disk; 0 means no journal exists yet.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Bytes the journal occupies after the last confirmed write.
    pub fn journal_bytes(&self) -> u64 {
        self.file_len
    }

    pub fn record_count(&self) -> u64 {
        self.record_count
    }

    pub fn limits(&self) -> &JournalLimits {
        &self.limits
    }

    pub fn state(&self) -> &RuntimeState {
        &self.state
    }

    pub fn recoveries(&self) -> &[RecoveryReport] {
        &self.recoveries
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Replay everything other writers appended, then hand back typed state.
    pub fn replay(&mut self) -> JournalResult<&RuntimeState> {
        self.with_writer_lock(|store| {
            store.catch_up_locked()?;
            Ok(())
        })?;
        Ok(&self.state)
    }

    /// Replay and clone the state, for a caller that outlives the borrow.
    pub fn snapshot(&mut self) -> JournalResult<RuntimeState> {
        self.replay()?;
        Ok(self.state.clone())
    }

    /// Append one transition and return it only after it is durable.
    ///
    /// `id` is the entity the row refers to. The row's sequence is assigned
    /// here; one call is exactly one row.
    pub fn append(&mut self, id: &str, payload: Payload) -> JournalResult<JournalRecord> {
        validate_record_id(id)?;
        self.with_writer_lock(|store| store.append_locked(id, payload))
    }

    /// Fold the replayed state into one checkpoint row.
    pub fn compact(&mut self) -> JournalResult<()> {
        self.with_writer_lock(|store| store.compact_locked())
    }

    fn with_writer_lock<T>(&mut self, body: impl FnOnce(&mut Self) -> JournalResult<T>) -> JournalResult<T> {
        let process_lock = Arc::clone(&self.process_lock);
        let _process_guard = process_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure_private_dir(&self.dir, &mut self.hooks)?;
        let lock_path = self.lock_path.clone();
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| JournalError::io_context("open the writer lock", &lock_path, error))?;
        self.hit(JournalIoPoint::WriterLockAcquire)?;
        lock.lock().map_err(|error| {
            JournalError::lock(format!("cannot take the writer lock {}: {error}", lock_path.display()))
        })?;
        let outcome = body(self);
        let released = lock.unlock();
        let _ = self.hit(JournalIoPoint::WriterLockRelease);
        match (outcome, released) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => Err(JournalError::lock(format!(
                "cannot release the writer lock {}: {error}",
                lock_path.display()
            ))),
            (Err(error), _) => Err(error),
        }
    }

    fn hit(&mut self, point: JournalIoPoint) -> std::io::Result<()> {
        hit_slot(&mut self.hooks, point)
    }

    fn load_locked(&mut self) -> JournalResult<()> {
        let path = self.journal_path.clone();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(JournalError::io_context("read the journal", &path, error)),
        };
        if bytes.is_empty() {
            self.reset_fields();
            return Ok(());
        }
        let report = replay::scan(
            &bytes,
            ScanStart::Fresh {
                session_id: &self.session_id,
            },
            self.retention(),
        )?;
        self.adopt_locked(&report)?;
        if report.has_torn_tail() {
            self.repair_tail_locked(&report)?;
        }
        Ok(())
    }

    fn reset_fields(&mut self) {
        self.state = RuntimeState::default();
        self.generation = 0;
        self.last_seq = 0;
        self.file_len = 0;
        self.record_count = 0;
        self.read_only = false;
    }

    fn adopt_locked(&mut self, report: &ScanReport) -> JournalResult<()> {
        self.state = report.state.clone();
        self.generation = report.generation;
        self.last_seq = report.last_seq;
        self.record_count = report.records;
        self.file_len = report.valid_len;
        if self.file_len >= self.limits.max_journal_bytes {
            self.read_only = true;
        }
        Ok(())
    }

    fn catch_up_locked(&mut self) -> JournalResult<()> {
        let path = self.journal_path.clone();
        let len = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.file_len == 0 {
                    return Ok(());
                }
                return Err(JournalError::corrupt("the journal disappeared after it was replayed"));
            }
            Err(error) => return Err(JournalError::io_context("inspect the journal", &path, error)),
        };
        if len == self.file_len {
            return Ok(());
        }
        if len < self.file_len || self.generation == 0 {
            return self.reload_locked();
        }
        let mut file = File::open(&path).map_err(|error| JournalError::io_context("read the journal", &path, error))?;
        file.seek(SeekFrom::Start(self.file_len))
            .map_err(|error| JournalError::io_context("seek in the journal", &path, error))?;
        let mut delta = Vec::new();
        file.read_to_end(&mut delta)
            .map_err(|error| JournalError::io_context("read appended rows", &path, error))?;
        let report = replay::scan(
            &delta,
            ScanStart::Continuation {
                session_id: &self.session_id,
                state: &self.state,
                generation: self.generation,
                last_seq: self.last_seq,
            },
            self.retention(),
        )?;
        let torn = report.has_torn_tail();
        self.state = report.state.clone();
        self.last_seq = report.last_seq;
        self.record_count = self.record_count.saturating_add(report.records);
        self.file_len = self.file_len.saturating_add(report.valid_len);
        if self.file_len >= self.limits.max_journal_bytes {
            self.read_only = true;
        }
        if torn {
            self.repair_tail_locked(&report)?;
        }
        Ok(())
    }

    fn reload_locked(&mut self) -> JournalResult<()> {
        let path = self.journal_path.clone();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(JournalError::io_context("read the journal", &path, error)),
        };
        if bytes.is_empty() {
            let previous_seq = self.last_seq;
            self.reset_fields();
            if previous_seq > 0 {
                return Err(JournalError::corrupt(format!(
                    "the journal was truncated after seq {previous_seq}"
                )));
            }
            return Ok(());
        }
        let report = replay::scan(
            &bytes,
            ScanStart::Fresh {
                session_id: &self.session_id,
            },
            self.retention(),
        )?;
        if report.last_seq < self.last_seq {
            return Err(JournalError::corrupt(format!(
                "the journal now ends at seq {} but this store already replayed {}",
                report.last_seq, self.last_seq
            )));
        }
        self.adopt_locked(&report)?;
        if report.has_torn_tail() {
            self.repair_tail_locked(&report)?;
        }
        Ok(())
    }

    fn repair_tail_locked(&mut self, report: &ScanReport) -> JournalResult<()> {
        let source_generation = report.generation;
        let tail_path = self.recovery_path(source_generation);
        self.preserve_tail_locked(&tail_path, &report.torn_tail)?;
        let generation = source_generation.saturating_add(1);
        let seq = report.last_seq.saturating_add(1);
        let bounded = self.bounded_state(&report.state);
        let checkpoint = checkpoint_record(&self.session_id, generation, seq, report.last_seq, &bounded)?;
        let line = checkpoint.to_line()?;
        self.atomic_replace_locked(line.as_bytes())?;
        self.recoveries.push(RecoveryReport {
            source_generation,
            preserved_tail: tail_path,
            preserved_bytes: report.torn_tail.len() as u64,
            rewritten_bytes: line.len() as u64,
            rewritten_seq: seq,
        });
        self.state = bounded;
        self.generation = generation;
        self.last_seq = seq;
        self.file_len = line.len() as u64;
        self.record_count = 1;
        Ok(())
    }

    fn recovery_path(&self, generation: u64) -> PathBuf {
        let mut candidate = generation;
        loop {
            let path = self.dir.join(format!("{RECOVERY_PREFIX}{candidate}"));
            if !path.exists() {
                return path;
            }
            candidate = candidate.saturating_add(1);
        }
    }

    fn preserve_tail_locked(&mut self, path: &Path, bytes: &[u8]) -> JournalResult<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        horon_agent_platform::fs::request_owner_only_mode(&mut options);
        let mut file = options
            .open(path)
            .map_err(|error| JournalError::io_context("create the recovery tail", path, error))?;
        self.hit(JournalIoPoint::RecoveryTailWrite)?;
        file.write_all(bytes)?;
        self.hit(JournalIoPoint::RecoveryTailFileSync)?;
        horon_agent_platform::sync_file(&file, crate::config::config().storage_sync_mode())?;
        self.hit(JournalIoPoint::RecoveryTailDirectorySync)?;
        let dir = self.dir.clone();
        sync_directory_entry(&dir)
    }

    fn atomic_replace_locked(&mut self, bytes: &[u8]) -> JournalResult<()> {
        let temp = self
            .dir
            .join(format!("{TEMP_PREFIX}{}.{}", std::process::id(), Uuid::new_v4()));
        let outcome = self.write_and_rename_locked(&temp, bytes);
        if outcome.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        outcome
    }

    fn write_and_rename_locked(&mut self, temp: &Path, bytes: &[u8]) -> JournalResult<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        horon_agent_platform::fs::request_owner_only_mode(&mut options);
        let mut file = options
            .open(temp)
            .map_err(|error| JournalError::io_context("create the journal temp file", temp, error))?;
        self.hit(JournalIoPoint::RewriteTempWrite)?;
        file.write_all(bytes)?;
        self.hit(JournalIoPoint::RewriteFileSync)?;
        horon_agent_platform::sync_file(&file, crate::config::config().storage_sync_mode())?;
        drop(file);
        self.hit(JournalIoPoint::RewriteRename)?;
        let journal = self.journal_path.clone();
        std::fs::rename(temp, &journal)
            .map_err(|error| JournalError::io_context("replace the journal", &journal, error))?;
        self.hit(JournalIoPoint::RewriteDirectorySync)?;
        let dir = self.dir.clone();
        sync_directory_entry(&dir)
    }

    fn append_locked(&mut self, id: &str, payload: Payload) -> JournalResult<JournalRecord> {
        let kind = payload.kind();
        if kind == RecordKind::Checkpoint {
            return Err(JournalError::invalid(
                "checkpoints are written by the store, not by a caller",
            ));
        }
        self.catch_up_locked()?;
        if self.read_only {
            return Err(self.read_only_error());
        }
        if kind.is_admission() {
            let size = payload.retained_bytes();
            if size > self.limits.max_input_payload_bytes {
                return Err(JournalError::invalid(format!(
                    "input payload is {size} bytes; the limit is {}",
                    self.limits.max_input_payload_bytes
                )));
            }
        }
        if self.should_compact() {
            self.compact_locked()?;
        }
        let mut compacted_for_capacity = false;
        loop {
            let prepared = self.prepare_locked(id, payload.clone(), kind)?;
            match self.ensure_capacity_locked(kind, &payload, prepared.bytes.len() as u64) {
                Ok(()) => {
                    self.publish_locked(&prepared.bytes, prepared.previous_len)?;
                    self.state = prepared.state;
                    self.last_seq = prepared.record.seq;
                    if prepared.wrote_checkpoint {
                        self.generation = 1;
                    }
                    self.record_count = self.record_count.saturating_add(prepared.rows);
                    return Ok(prepared.record);
                }
                Err(error) if error.kind() == super::JournalErrorKind::Busy && !compacted_for_capacity => {
                    compacted_for_capacity = true;
                    self.compact_locked()?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn prepare_locked(&self, id: &str, payload: Payload, kind: RecordKind) -> JournalResult<PreparedAppend> {
        let wrote_checkpoint = self.generation == 0;
        debug_assert!(
            !wrote_checkpoint || self.state == RuntimeState::default(),
            "a store without a checkpoint on disk must start from empty state"
        );
        let mut bytes = Vec::new();
        let mut rows = 0u64;
        if wrote_checkpoint {
            let checkpoint = checkpoint_record(&self.session_id, 1, 1, 0, &RuntimeState::default())?;
            bytes.extend_from_slice(checkpoint.to_line()?.as_bytes());
            rows += 1;
        }
        let seq = if wrote_checkpoint {
            2
        } else {
            self.last_seq.saturating_add(1)
        };
        let record = JournalRecord {
            v: JOURNAL_VERSION,
            seq,
            session_id: self.session_id.clone(),
            kind,
            id: id.to_string(),
            at: Utc::now(),
            payload,
        };
        let mut state = self.state.clone();
        replay::apply_record(&mut state, &record, self.retention())
            .map_err(|violation| JournalError::invalid(violation.to_string()))?;
        bytes.extend_from_slice(record.to_line()?.as_bytes());
        rows += 1;
        Ok(PreparedAppend {
            record,
            bytes,
            state,
            rows,
            wrote_checkpoint,
            previous_len: self.file_len,
        })
    }

    fn ensure_capacity_locked(&mut self, kind: RecordKind, payload: &Payload, bytes: u64) -> JournalResult<()> {
        let max = self.limits.max_journal_bytes;
        let reserve = self.limits.reserve_bytes.min(max);
        let total = self.file_len.saturating_add(bytes);
        if self.file_len >= max {
            self.read_only = true;
            return Err(self.read_only_error());
        }
        if kind.is_admission() {
            let live = self.state.inputs.len();
            if live >= self.limits.max_live_inputs {
                return Err(JournalError::busy(format!(
                    "{live} live inputs are already retained; the limit is {}",
                    self.limits.max_live_inputs
                )));
            }
            let retained = self.state.live_input_bytes().saturating_add(payload.retained_bytes());
            if retained > self.limits.max_live_input_bytes {
                return Err(JournalError::busy(format!(
                    "live input payload would reach {retained} bytes; the limit is {}",
                    self.limits.max_live_input_bytes
                )));
            }
            if total > max.saturating_sub(reserve) {
                return Err(JournalError::busy(format!(
                    "the journal would reach {total} bytes, past the {} byte admission ceiling that keeps {} bytes for lifecycle rows",
                    max.saturating_sub(reserve),
                    reserve
                )));
            }
            return Ok(());
        }
        if total > max {
            self.read_only = true;
            return Err(self.read_only_error());
        }
        Ok(())
    }

    fn read_only_error(&self) -> JournalError {
        match &self.poisoned {
            Some(reason) => {
                JournalError::read_only(format!("the journal is read-only after a failed rollback: {reason}"))
            }
            None => JournalError::read_only(format!(
                "the journal reached its {} byte ceiling; only explicit recovery can reopen it",
                self.limits.max_journal_bytes
            )),
        }
    }

    /// The terminal-history caps this store enforces.
    fn retention(&self) -> RetentionCaps {
        RetentionCaps::new(self.limits.terminal_run_cap, self.limits.terminal_receipt_cap)
    }

    /// The state a checkpoint may carry: the same unfinished work, with the
    /// terminal history held to the retention caps.
    ///
    /// Every checkpoint this build writes goes through here, so a checkpoint
    /// read from disk cannot reintroduce history that a later write would keep
    /// forever: the bound holds for the file, not only for in-memory state.
    fn bounded_state(&self, state: &RuntimeState) -> RuntimeState {
        let mut bounded = state.clone();
        replay::prune(&mut bounded, self.retention(), true);
        bounded
    }

    fn should_compact(&self) -> bool {
        self.generation > 0
            && (self.file_len >= self.limits.compact_bytes || self.record_count >= self.limits.compact_records)
    }

    fn compact_locked(&mut self) -> JournalResult<()> {
        if self.generation == 0 && self.file_len == 0 {
            return Ok(());
        }
        self.catch_up_locked()?;
        let generation = self.generation.saturating_add(1);
        let seq = self.last_seq.saturating_add(1);
        let bounded = self.bounded_state(&self.state);
        let checkpoint = checkpoint_record(&self.session_id, generation, seq, self.last_seq, &bounded)?;
        let line = checkpoint.to_line()?;
        let ceiling = self.limits.max_journal_bytes.saturating_sub(self.limits.reserve_bytes);
        if line.len() as u64 > ceiling {
            return Err(JournalError::busy(format!(
                "a compacted checkpoint needs {} bytes, past the {} byte ceiling",
                line.len(),
                ceiling
            )));
        }
        self.atomic_replace_locked(line.as_bytes())?;
        self.state = bounded;
        self.generation = generation;
        self.last_seq = seq;
        self.file_len = line.len() as u64;
        self.record_count = 1;
        Ok(())
    }

    fn publish_locked(&mut self, bytes: &[u8], previous_len: u64) -> JournalResult<()> {
        match self.try_publish_locked(bytes, previous_len) {
            Ok(()) => {
                self.file_len = previous_len.saturating_add(bytes.len() as u64);
                Ok(())
            }
            Err(error) => self.rollback_locked(previous_len, error),
        }
    }

    fn try_publish_locked(&mut self, bytes: &[u8], previous_len: u64) -> JournalResult<()> {
        let path = self.journal_path.clone();
        let mut options = OpenOptions::new();
        options.create(true).append(true).truncate(false);
        horon_agent_platform::fs::request_owner_only_mode(&mut options);
        let mut file = options
            .open(&path)
            .map_err(|error| JournalError::io_context("open the journal for append", &path, error))?;
        self.hit(JournalIoPoint::RecordWrite)?;
        file.write_all(bytes)?;
        self.hit(JournalIoPoint::RecordFileSync)?;
        horon_agent_platform::sync_file(&file, crate::config::config().storage_sync_mode())?;
        if previous_len == 0 {
            self.hit(JournalIoPoint::RecordDirectorySync)?;
            let dir = self.dir.clone();
            sync_directory_entry(&dir)?;
        }
        Ok(())
    }

    fn rollback_locked(&mut self, previous_len: u64, error: JournalError) -> JournalResult<()> {
        let path = self.journal_path.clone();
        let rolled_back = match OpenOptions::new().write(true).open(&path) {
            Ok(file) => match file.set_len(previous_len) {
                Ok(()) => match self.hit(JournalIoPoint::RecordRollbackFileSync) {
                    Ok(()) => horon_agent_platform::sync_file(&file, crate::config::config().storage_sync_mode()),
                    Err(injected) => Err(injected),
                },
                Err(truncate_error) => Err(truncate_error),
            },
            Err(open_error) if open_error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(open_error) => Err(open_error),
        };
        match rolled_back {
            Ok(()) => {
                self.file_len = previous_len;
            }
            Err(rollback_error) => {
                self.read_only = true;
                self.poisoned = Some(format!(
                    "{} could not be rolled back: {rollback_error}",
                    error.message()
                ));
            }
        }
        Err(error)
    }
}

impl std::fmt::Debug for DurableRuntimeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableRuntimeStore")
            .field("session_id", &self.session_id)
            .field("generation", &self.generation)
            .field("last_seq", &self.last_seq)
            .field("journal_bytes", &self.file_len)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

struct PreparedAppend {
    record: JournalRecord,
    bytes: Vec<u8>,
    state: RuntimeState,
    rows: u64,
    wrote_checkpoint: bool,
    previous_len: u64,
}

fn checkpoint_record(
    session_id: &str,
    generation: u64,
    seq: u64,
    last_seq: u64,
    state: &RuntimeState,
) -> JournalResult<JournalRecord> {
    Ok(JournalRecord {
        v: JOURNAL_VERSION,
        seq,
        session_id: session_id.to_string(),
        kind: RecordKind::Checkpoint,
        id: Uuid::new_v4().to_string(),
        at: Utc::now(),
        payload: Payload::Checkpoint(Checkpoint {
            generation,
            start_seq: seq,
            last_seq,
            state: state.clone(),
        }),
    })
}

fn validate_session_id(raw: &str) -> JournalResult<()> {
    horon_agent_session_store::SessionId::parse(raw)
        .map(|_| ())
        .map_err(|error| JournalError::invalid(format!("invalid session id {raw:?}: {error}")))
}

fn validate_record_id(raw: &str) -> JournalResult<()> {
    let parsed = Uuid::parse_str(raw)
        .map_err(|error| JournalError::invalid(format!("record id {raw:?} is not a uuid: {error}")))?;
    if parsed.to_string() != raw {
        return Err(JournalError::invalid(format!(
            "record id {raw:?} must be the lowercase hyphenated form {parsed}"
        )));
    }
    Ok(())
}

fn process_lock_for(dir: &Path) -> Arc<Mutex<()>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let key = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut locks = registry.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = locks.get(&key).and_then(Weak::upgrade) {
        return existing;
    }
    let fresh = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&fresh));
    fresh
}

/// Create the journal directory, flushing the entry of every level this call created.
///
/// The first record's file is confirmed by flushing this directory, but a crash
/// before the new directory's own entry reaches its parent loses the directory
/// and the confirmed journal with it. An existing directory needs no flush: its
/// entry was confirmed when it was created.
fn ensure_private_dir(path: &Path, hooks: &mut HookSlot) -> JournalResult<()> {
    let mut created = Vec::new();
    let mut level = Some(path);
    while let Some(current) = level {
        match std::fs::symlink_metadata(current) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => created.push(current.to_path_buf()),
            Err(error) => {
                return Err(JournalError::io_context(
                    "inspect the journal directory",
                    current,
                    error,
                ));
            }
        }
        level = current.parent();
    }
    horon_agent_platform::create_private_dir(path, true)
        .map_err(|error| JournalError::io_context("create the journal directory", path, error))?;
    horon_agent_platform::set_permissions_private_dir(path)
        .map_err(|error| JournalError::io_context("restrict the journal directory", path, error))?;
    for directory in created.iter().rev() {
        if let Some(parent) = directory.parent() {
            hit_slot(hooks, JournalIoPoint::DirectoryEntrySync)
                .map_err(|error| JournalError::io_context("sync the journal directory", parent, error))?;
            sync_directory_entry(parent)?;
        }
    }
    Ok(())
}

/// Flush one directory entry so a journal file created or replaced in it survives.
fn sync_directory_entry(path: &Path) -> JournalResult<()> {
    horon_agent_platform::sync_dir(path, crate::config::config().storage_sync_mode())
        .map_err(|error| JournalError::io_context("sync the journal directory", path, error))
}

#[cfg(any(test, feature = "test-support"))]
fn hit_slot(hooks: &mut HookSlot, point: JournalIoPoint) -> std::io::Result<()> {
    match hooks.as_mut() {
        Some(observe) => observe(point),
        None => Ok(()),
    }
}

#[cfg(not(any(test, feature = "test-support")))]
fn hit_slot(_hooks: &mut HookSlot, _point: JournalIoPoint) -> std::io::Result<()> {
    Ok(())
}

//! Session-lifetime exclusive writer. Use WriterHandle outside blocking workers.
use crate::blobs::{Disk, check_file, checked_file, io_error, log_file, new_file, verify_blob};
use crate::recovery::{corrupt, quarantine};
use crate::*;
use std::fs::File;
use std::io::Write;
use std::sync::mpsc::{self, Receiver, SyncSender};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Committed,
    AlreadyDurable,
}

/// Blocking storage boundary. All IDs and timestamps are supplied before I/O.
/// The lock file is never unlinked or replaced, including on failed opens.
pub struct BlockingWriter {
    pub(crate) paths: SessionPaths,
    pub(crate) id: SessionId,
    lock: File,
    pub(crate) disk: Disk,
    pub(crate) state: crate::cache_state::WriterState,
    pending: Option<(EventId, Vec<u8>)>,
    // Last import page's positively acknowledged IDs, not merely complete log lines.
    pub(crate) import_acknowledged: Option<(String, Vec<String>)>,
}

pub(crate) fn lock_exclusive(file: &File) -> Result<(), StoreError> {
    lock_exclusive_named(file, "session writer")
}

/// Acquire an exclusive guard, naming the resource its Busy diagnostic is about.
///
/// The typed code stays StoreErrorCode::Busy for every caller; only the message
/// names which lock was contended, so a log reader can tell the shared catalog
/// index from a session's own writer without matching prose.
pub(crate) fn lock_exclusive_named(file: &File, owner: &str) -> Result<(), StoreError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(StoreError::busy(format!("{owner} is already owned"))),
        Err(std::fs::TryLockError::Error(error)) => Err(io_error(error)),
    }
}

impl BlockingWriter {
    pub fn open(paths: SessionPaths, id: SessionId) -> Result<Self, StoreError> {
        Self::with_hook(paths, id, Box::new(|_| Ok(())))
    }

    pub fn with_hook(paths: SessionPaths, id: SessionId, hook: IoHook) -> Result<Self, StoreError> {
        Self::with_observation(paths, id, (hook, ReadProbe::default()))
    }

    pub(crate) fn with_observation(
        paths: SessionPaths,
        id: SessionId,
        observation: (IoHook, ReadProbe),
    ) -> Result<Self, StoreError> {
        let mut writer = Self::lock_for_exchange(paths, id, observation)?;
        writer.reconcile()?;
        Ok(writer)
    }

    /// Acquire the exclusive guard without reading or recovering the original log.
    ///
    /// Use `import_events` or `import_events_resuming` on this writer to validate
    /// the whole incoming page before any reconciliation or original mutation.
    ///
    /// # Errors
    /// Returns Busy for an owned writer, and path, permission or durability errors
    /// when the exclusive guard cannot be safely acquired.
    pub fn lock_for_exchange(
        paths: SessionPaths,
        id: SessionId,
        observation: (IoHook, ReadProbe),
    ) -> Result<Self, StoreError> {
        let (hook, probe) = observation;
        let mode = paths.sync_mode();
        let mut disk = Disk::new(hook, mode);
        disk.directory(paths.root())?;
        // Canonicalizing rewrites the root, so the durability level is carried
        // over explicitly; the writer and every cache opened from these paths
        // then flush originals the same way.
        let paths = SessionPaths::new(paths.root().canonicalize()?).with_sync_mode(mode);
        let directory = paths.session_dir(&id)?;
        // The session directory's entry is what has to survive a crash; it is
        // flushed when this call creates it.
        disk.ensure_dir(&directory)?;
        let lock_path = directory.join(".writer.lock");
        let lock = match new_file(&lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => checked_file(&lock_path, true)?,
            Err(error) => return Err(io_error(error)),
        };
        lock_exclusive(&lock)?;
        // The writer lock is not an original: it carries no data that has to
        // survive a crash and the operating system releases it when the process
        // dies, so it is closed over without a flush. The point stays as the
        // observable end of the acquire sequence.
        disk.hit(IoPoint::LockFileSync)?;
        let writer = Self {
            paths,
            id,
            lock,
            disk,
            state: crate::cache_state::WriterState {
                probe,
                ..Default::default()
            },
            pending: None,
            import_acknowledged: None,
        };
        Ok(writer)
    }

    fn reconcile(&mut self) -> Result<(), StoreError> {
        self.check_lock()?;
        if self.state.unchanged(&self.paths, &self.id)? {
            return Ok(());
        }
        let report = crate::recovery::scan_observed(&self.paths, &self.id, &self.state.probe)?;
        if let Some(error) = &report.damage {
            return Err(error.clone());
        }
        if !report.torn_tail.is_empty() {
            self.dirty()?;
            quarantine(&mut self.disk, &self.paths.events_log(&self.id)?, &report)?;
        }
        self.state.replace(&report, (&self.paths, &self.id))?;
        self.state.source = crate::cache_io::Stamp::optional(&self.paths.events_log(&self.id)?)?;
        self.state.context = None;
        Ok(())
    }

    pub(crate) fn pending_id(&self) -> Option<&EventId> {
        self.pending.as_ref().map(|(id, _)| id)
    }

    pub(crate) fn check_lock(&self) -> Result<(), StoreError> {
        check_file(&self.paths.session_dir(&self.id)?.join(".writer.lock"), &self.lock)?;
        Ok(())
    }

    pub(crate) fn dirty(&mut self) -> Result<(), StoreError> {
        let directory = self.paths.root().join(".catalog.dirty");
        let path = directory.join(self.id.as_str());
        // One unchanged, already-durable marker covers every original in this
        // batch. Recheck its identity and safety: publication may have removed it
        // or a path change may have replaced it since the previous append. This
        // check runs before the marker directory is touched, so a batch pays the
        // directory work once instead of once per append.
        if self.state.batch
            && let Some(durable) = &self.state.batch_dirty
            && crate::cache_io::Stamp::optional(&path)?.as_ref() == Some(durable)
        {
            return Ok(());
        }
        self.disk.ensure_dir(&directory)?;
        let mut file = match new_file(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => checked_file(&path, true)?,
            Err(error) => return Err(error.into()),
        };
        file.write_all(b"dirty\n")?;
        self.disk.sync_file(&file, IoPoint::DirtyFileSync)?;
        self.disk.sync_dir(&directory, IoPoint::DirtyDirectorySync)?;
        if self.state.batch {
            self.state.batch_dirty = Some(crate::cache_io::Stamp::read(&path)?);
        }
        Ok(())
    }

    pub fn append(&mut self, sealed: &SealedEvent) -> Result<AppendOutcome, StoreError> {
        let event = sealed.envelope();
        let line = encode_record_line(event)?;
        if event.session_id != self.id {
            return Err(corrupt("wrong session"));
        }
        if let Some((id, pending)) = &self.pending
            && *pending != line
        {
            if *id == event.event_id {
                return Err(StoreError::hash_conflict(
                    "reserved event id has different content",
                    Vec::new(),
                ));
            }
            return Err(StoreError::busy("retry the reserved event before submitting another"));
        }
        // Reserve exact canonical bytes before any commit I/O.
        self.pending = Some((event.event_id.clone(), line.clone()));
        let result = self.commit(sealed, &line);
        if result.is_ok() {
            self.pending = None;
        }
        result
    }

    /// Confirms the log bytes a batched commit wrote but deferred.
    ///
    /// `batched` skips the per-append flush, so one commit pays one flush instead
    /// of one per event. Whoever publishes a head must call this after the last
    /// append and before that head, so a confirmed head never names an event whose
    /// bytes were not flushed. Outside a batch a successful append has already
    /// flushed and cleared the debt, so the call is a no-op there; the one case it
    /// still acts on is a non-batched append that failed at its own flush, whose
    /// pending debt stays recorded for the retry.
    /// # Errors
    /// Returns the path or I/O error of the flush. The pending debt stays recorded,
    /// so a retry flushes the same bytes instead of acknowledging them.
    pub fn flush_pending_log(&mut self) -> Result<(), StoreError> {
        if !self.state.pending_log_sync {
            return Ok(());
        }
        let path = self.paths.events_log(&self.id)?;
        // Opened writable because a platform flush may be `fdatasync`.
        let file = checked_file(&path, true)?;
        self.disk.sync_file(&file, IoPoint::LogFileSync)?;
        self.state.pending_log_sync = false;
        Ok(())
    }

    fn commit(&mut self, sealed: &SealedEvent, line: &[u8]) -> Result<AppendOutcome, StoreError> {
        let event = sealed.envelope();
        self.reconcile()?;
        let existing = match self.state.check_event((&self.paths, &self.id), event) {
            Ok(existing) => existing,
            Err(error) => {
                if matches!(
                    error.code,
                    StoreErrorCode::HashConflict | StoreErrorCode::MissingDependency
                ) {
                    self.pending = None;
                }
                return Err(error);
            }
        };
        self.dirty()?;
        if let Some(reference) = &event.payload_ref {
            let path = self.paths.blob_file(&self.id, &reference.sha256)?;
            match sealed.blob() {
                Some(bytes) => self.disk.publish_blob(&path, (event, bytes))?,
                None => {
                    verify_blob(&self.paths, &self.id, event)?;
                    let blob = checked_file(&path, false)?;
                    self.disk.sync_file(&blob, IoPoint::BlobFileSync)?;
                    self.disk
                        .sync_dir(&self.paths.blobs_dir(&self.id)?, IoPoint::BlobDirectorySync)?;
                }
            }
        }
        let log_path = self.paths.events_log(&self.id)?;
        let (mut file, created) = log_file(&log_path)?;
        if created {
            self.disk.hit(IoPoint::LogCreated)?;
        }
        if !existing {
            self.disk.hit(IoPoint::LogWrite)?;
            // The newline is a completion marker; readers ignore the body until it exists.
            let (newline, body) = line.split_last().ok_or_else(|| corrupt("empty canonical line"))?;
            self.state.probe.append(&mut file, (&log_path, body))?;
            self.disk.hit(IoPoint::LogBodyWritten)?;
            self.state.probe.append(&mut file, (&log_path, &[*newline]))?;
        }
        // One platform flush confirms both stages: the adapter maps the data and
        // the full flush to the same call on every supported OS, so a second one
        // would repeat the syscall. The point stays observable for failure
        // injection at the data boundary.
        //
        // The debt is recorded before the boundary is observed, so a failure here
        // still leaves the written bytes covered by the next flush. Inside a batch
        // that flush is deferred, so one commit pays one flush instead of one per
        // event; the batch's exit and Self::flush_pending_log both settle it.
        self.state.pending_log_sync = true;
        self.disk.hit(IoPoint::LogDataSync)?;
        if !self.state.batch {
            // Also on retries: a prior failed attempt may have created this file.
            self.disk.sync_file(&file, IoPoint::LogFileSync)?;
            self.state.pending_log_sync = false;
        }
        // The directory entry only changes when the log is created; after that
        // the name is already durable and only the file's contents move.
        if created || !self.state.log_directory_durable {
            self.disk
                .sync_dir(&self.paths.session_dir(&self.id)?, IoPoint::LogDirectorySync)?;
            self.state.log_directory_durable = true;
        } else {
            self.disk.hit(IoPoint::LogDirectorySync)?;
        }
        self.disk.hit(IoPoint::LogPublished)?;
        self.finish_original(sealed);
        Ok(if existing {
            AppendOutcome::AlreadyDurable
        } else {
            AppendOutcome::Committed
        })
    }
}

type Reply = mpsc::Sender<Result<AppendOutcome, StoreError>>;
type WriterStartup = (WriterHandle, Receiver<Result<(), StoreError>>, Receiver<()>);

/// Bounded dedicated blocking thread. Startup and append return receivers, so
/// an async facade never performs filesystem work or waits on the calling turn.
/// Dropping the handle closes the queue; `finished` observes lock release.
pub struct WriterHandle {
    queue: SyncSender<(SealedEvent, Reply)>,
}

impl WriterHandle {
    pub fn spawn(paths: SessionPaths, id: SessionId) -> Result<WriterStartup, StoreError> {
        let (queue, requests) = mpsc::sync_channel::<(SealedEvent, Reply)>(1);
        let (ready, started) = mpsc::channel();
        let (finished, stopped) = mpsc::channel();
        std::thread::Builder::new()
            .name("session-writer".into())
            .spawn(move || {
                match BlockingWriter::open(paths, id) {
                    Ok(mut writer) => {
                        if ready.send(Ok(())).is_ok() {
                            for (event, reply) in requests {
                                let result = writer.append(&event);
                                if reply.send(result).is_err() {
                                    // Cancellation cannot roll back a durable original.
                                    continue;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if ready.send(Err(error)).is_err() {
                            return;
                        }
                    }
                }
                finished.send(()).unwrap_or_default();
            })?;
        Ok((Self { queue }, started, stopped))
    }

    pub fn append(&self, event: SealedEvent) -> Result<Receiver<Result<AppendOutcome, StoreError>>, StoreError> {
        let (reply, response) = mpsc::channel();
        self.queue.try_send((event, reply)).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => StoreError::busy("writer request queue is full"),
            mpsc::TrySendError::Disconnected(_) => StoreError::internal(StoreErrorCode::Io, "writer stopped"),
        })?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "writer_sync_mode_tests.rs"]
mod sync_mode_tests;

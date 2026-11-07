//! Session-lifetime writer: all commit I/O for a session runs on one pool worker.
//!
//! The facade is unchanged for callers: `call` still blocks until its job ran,
//! jobs of one session still run in submission order, and dropping the last
//! facade still drains queued jobs before the session's file lock is released.
//! What changed is where they run: a fixed set of long-lived pool threads
//! (`event_pool`) instead of a thread per session.
use anyhow::{Context, Result, anyhow};
use horon_agent_session_store as store;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError, Weak, mpsc};

/// One registry key per session directory.
///
/// The writer's file lock uses the canonicalized root, so keying the registry on
/// the raw root would let `/tmp/x` and `/private/tmp/x` (or a symlinked home)
/// become two writers for one session; the second would then fail Busy against
/// the first one's lock. Canonicalize the deepest existing ancestor and re-attach
/// the missing components, so the key is the same before and after the first
/// writer creates the directory.
fn registry_key(root: &std::path::Path, id: &str) -> std::path::PathBuf {
    let mut existing = root;
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            let mut key = canonical;
            for part in missing.iter().rev() {
                key.push(part);
            }
            return key.join(id);
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                existing = parent;
            }
            _ => return root.join(id),
        }
    }
}

enum Startup {
    Recover,
    Exchange,
}

pub(super) struct Writer {
    _slot: Arc<WriterSlot>,
    /// Handle of this session in its worker's map; never reused.
    session: u64,
    commands: Option<mpsc::Sender<super::event_pool::Command>>,
    operation: Mutex<Operation>,
    #[cfg(any(test, feature = "test-support"))]
    preparation_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    drop_started: Mutex<Option<mpsc::Sender<()>>>,
}

pub(super) struct Operation {
    incarnation: store::EventId,
    generation: u64,
    pub retry_required: bool,
}

impl Operation {
    pub(super) fn snapshot(&self) -> super::SessionGeneration {
        super::SessionGeneration {
            incarnation: self.incarnation.clone(),
            generation: self.generation,
        }
    }

    pub(super) fn advance(&mut self) -> Result<()> {
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            store::StoreError::internal(store::StoreErrorCode::Internal, "session generation exhausted")
        })?;
        Ok(())
    }
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionWriter")
    }
}

// A slot survives the last facade until its writer releases the file lock.
type Key = std::path::PathBuf;
type Writers = std::collections::HashMap<Key, Weak<WriterSlot>>;
static WRITERS: std::sync::LazyLock<Mutex<Writers>> = std::sync::LazyLock::new(|| Mutex::new(Writers::new()));

pub(super) struct WriterSlot {
    key: Key,
    state: Mutex<SlotState>,
    stopped: Condvar,
}

#[derive(Default)]
struct SlotState {
    writer: Weak<Writer>,
    running: bool,
}

impl Drop for WriterSlot {
    fn drop(&mut self) {
        let this = self as *const WriterSlot;
        let mut writers = WRITERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if writers
            .get(&self.key)
            .is_some_and(|slot| std::ptr::eq(slot.as_ptr(), this))
        {
            writers.remove(&self.key);
        }
    }
}

/// Marks the session slot as running for as long as its writer holds the lock.
///
/// The pool worker keeps this next to the `BlockingWriter`, and declares the
/// writer first, so the lock is released before this drop notifies a waiting
/// opener of the same session.
pub(super) struct WriterSlotLease(pub(super) Arc<WriterSlot>);

impl Drop for WriterSlotLease {
    fn drop(&mut self) {
        // The BlockingWriter was dropped first, including its OS file lock.
        let mut state = self.0.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.writer = Weak::new();
        state.running = false;
        self.0.stopped.notify_all();
    }
}

impl Writer {
    pub(super) fn registered(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Option<Arc<Self>>> {
        let key = registry_key(paths.root(), id.as_str());
        let slot = {
            let writers = WRITERS.lock().map_err(|_| anyhow!("writer registry poisoned"))?;
            writers.get(&key).and_then(Weak::upgrade)
        };
        let Some(slot) = slot else {
            return Ok(None);
        };
        let state = slot.state.lock().map_err(|_| anyhow!("writer slot poisoned"))?;
        Ok(state.writer.upgrade())
    }

    pub(super) fn open(paths: store::SessionPaths, id: store::SessionId) -> Result<Arc<Self>> {
        Self::open_with_hook(paths, id, Box::new(|_| Ok(())))
    }

    pub(super) fn open_with_hook(
        paths: store::SessionPaths,
        id: store::SessionId,
        hook: store::IoHook,
    ) -> Result<Arc<Self>> {
        Self::start(paths, id, hook, Startup::Recover)
    }

    pub(super) fn open_for_import(paths: store::SessionPaths, id: store::SessionId) -> Result<Arc<Self>> {
        Self::start(paths, id, Box::new(|_| Ok(())), Startup::Exchange)
    }

    fn start(
        paths: store::SessionPaths,
        id: store::SessionId,
        hook: store::IoHook,
        startup: Startup,
    ) -> Result<Arc<Self>> {
        // Opening from a worker would queue the open behind the job that is
        // asking for it. No caller does that; report it instead of deadlocking.
        if super::event_pool::on_worker_thread() {
            return Err(anyhow!("session writer cannot be opened from a pool worker"));
        }
        // The root does not exist before the first writer creates it.
        // SessionId is already validated; BlockingWriter validates all paths before I/O.
        let key = registry_key(paths.root(), id.as_str());
        let requested_root = paths.root().display().to_string();
        let canonical_root = paths
            .root()
            .canonicalize()
            .unwrap_or_else(|_| paths.root().to_path_buf())
            .display()
            .to_string();
        let slot = {
            let mut writers = WRITERS.lock().map_err(|_| anyhow!("writer registry poisoned"))?;
            match writers.get(&key).and_then(Weak::upgrade) {
                Some(slot) => slot,
                None => {
                    let slot = Arc::new(WriterSlot {
                        key: key.clone(),
                        state: Mutex::new(SlotState::default()),
                        stopped: Condvar::new(),
                    });
                    writers.insert(key.clone(), Arc::downgrade(&slot));
                    slot
                }
            }
        };
        // Only this session serializes startup and the previous writer's drain.
        // The process-wide registry is never held during filesystem work.
        let mut state = slot.state.lock().map_err(|_| anyhow!("writer slot poisoned"))?;
        loop {
            if let Some(writer) = state.writer.upgrade() {
                return Ok(writer);
            }
            if !state.running {
                break;
            }
            #[cfg(test)]
            tests::note_waiting(&key);
            state = slot.stopped.wait(state).map_err(|_| anyhow!("writer slot poisoned"))?;
        }
        let session = super::event_pool::next_session();
        let (ready, started) = mpsc::channel();
        let open_paths = paths.clone();
        let open_id = id.clone();
        let open: super::event_pool::OpenAttempt = Box::new(move || {
            store::with_cache_repair(&open_paths, &open_id, || match startup {
                Startup::Recover => store::BlockingWriter::with_hook(open_paths.clone(), open_id.clone(), hook),
                Startup::Exchange => store::BlockingWriter::lock_for_exchange(
                    open_paths.clone(),
                    open_id.clone(),
                    (hook, store::ReadProbe::default()),
                ),
            })
        });
        state.running = true;
        let submitted = super::event_pool::submit(
            &key,
            super::event_pool::Command::Open {
                session,
                lease: WriterSlotLease(Arc::clone(&slot)),
                open,
                ready,
            },
        );
        let commands = match submitted {
            Ok(commands) => commands,
            Err(failed) => {
                state.running = false;
                slot.stopped.notify_all();
                drop(state);
                // Dropping the command drops the lease, which takes the slot
                // state lock this function just released.
                drop(failed);
                return Err(anyhow!("session writer pool stopped"));
            }
        };
        let opened = started.recv().context("session writer startup stopped")?;
        if let Err(error) = &opened
            && error.code == store::StoreErrorCode::Busy
        {
            // Distinguish a real second owner from two spellings of one root:
            // the file lock canonicalizes, the registry key does too, so this
            // line tells the next occurrence which of the two it was.
            crate::logging::warn(&format!(
                "session writer busy: registry_key={} requested_root={requested_root} canonical_root={canonical_root}",
                key.display(),
            ));
        }
        opened.map_err(anyhow::Error::from)?;
        let writer = Arc::new(Self {
            _slot: Arc::clone(&slot),
            session,
            commands: Some(commands),
            operation: Mutex::new(Operation {
                incarnation: store::EventId::new_v4(),
                generation: 0,
                retry_required: false,
            }),
            #[cfg(any(test, feature = "test-support"))]
            preparation_hook: Mutex::new(None),
            #[cfg(test)]
            drop_started: Mutex::new(None),
        });
        state.writer = Arc::downgrade(&writer);
        Ok(writer)
    }

    pub(super) fn operation(&self) -> Result<MutexGuard<'_, Operation>> {
        self.operation.lock().map_err(|_| {
            store::StoreError::internal(store::StoreErrorCode::Internal, "session operation poisoned").into()
        })
    }

    pub(super) fn try_operation(&self) -> Result<MutexGuard<'_, Operation>> {
        self.operation.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => store::StoreError::busy("session save or import is active").into(),
            TryLockError::Poisoned(_) => {
                store::StoreError::internal(store::StoreErrorCode::Internal, "session operation poisoned").into()
            }
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn set_preparation_hook(&self, hook: Box<dyn FnOnce() + Send>) -> Result<()> {
        *self
            .preparation_hook
            .lock()
            .map_err(|_| anyhow!("preparation hook poisoned"))? = Some(hook);
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn prepare(&self) -> Result<()> {
        let hook = self
            .preparation_hook
            .lock()
            .map_err(|_| anyhow!("preparation hook poisoned"))?
            .take();
        if let Some(hook) = hook {
            hook();
        }
        Ok(())
    }

    pub(super) fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut store::BlockingWriter) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        // A job that waits for the worker running it never returns; other
        // sessions on that worker would wait with it.
        if super::event_pool::on_worker_thread() {
            return Err(anyhow!("session writer cannot be called from a pool worker"));
        }
        let (reply, result) = mpsc::channel();
        self.commands
            .as_ref()
            .context("writer already stopped")?
            .send(super::event_pool::Command::Run {
                session: self.session,
                job: Box::new(move |writer| {
                    if reply.send(job(writer)).is_err() {
                        crate::logging::warn("Session commit reply was cancelled; durable originals retained");
                    }
                }),
            })
            .map_err(|_| anyhow!("session writer stopped"))?;
        result.recv().context("session writer reply lost")?
    }

    /// Queue a job without waiting for it, so a test can have work in flight.
    #[cfg(test)]
    pub(super) fn enqueue_job(&self, job: impl FnOnce(&mut store::BlockingWriter) + Send + 'static) -> Result<()> {
        self.commands
            .as_ref()
            .context("writer already stopped")?
            .send(super::event_pool::Command::Run {
                session: self.session,
                job: Box::new(job),
            })
            .map_err(|_| anyhow!("session writer stopped"))
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        #[cfg(test)]
        tests::note_drop_started(self);
        let Some(commands) = self.commands.take() else {
            return;
        };
        let (done, closed) = mpsc::channel();
        if commands
            .send(super::event_pool::Command::Close {
                session: self.session,
                done,
            })
            .is_err()
        {
            return;
        }
        if super::event_pool::on_worker_thread() {
            // The close is queued behind the job this thread is running; waiting
            // for its ack would wait for ourselves.
            return;
        }
        if closed.recv().is_err() {
            crate::logging::warn("Session writer pool stopped before this session drained");
        }
    }
}

#[cfg(test)]
#[path = "event_actor_tests.rs"]
mod tests;

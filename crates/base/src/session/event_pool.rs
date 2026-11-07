//! Fixed pool of session writer threads.
//!
//! A session used to own one `session-events` thread so that its commits were
//! serialized and its `BlockingWriter` (and the file lock inside it) lived on a
//! single stack. The pool keeps both properties without one thread per session:
//! a session is assigned to a worker by the hash of its registry key, the worker
//! runs that session's jobs in submission order, and the `BlockingWriter` lives
//! in the worker's session map, so the file lock is released exactly when the
//! map entry is dropped.
//!
//! Two properties of the old per-session thread are load-bearing and preserved:
//!
//! * a job panic closes only the session whose job panicked. The worker catches
//!   the unwind, drops that one map entry (releasing its lock) and keeps serving
//!   its other sessions.
//! * a `Writer` drop drains the jobs already queued ahead of it before its
//!   `BlockingWriter` is released. `Close` is queued behind them and acked after
//!   the entry is gone, which is what the old thread `join()` provided.

use super::event_actor::WriterSlotLease;
use horon_agent_session_store as store;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, mpsc};

/// One registry key per session directory, shared with the actor registry.
pub(super) type Key = std::path::PathBuf;

/// One unit of commit I/O. The reply rendezvous lives inside the closure, so a
/// job that panics drops its reply and the waiting caller sees a lost reply.
pub(super) type Job = Box<dyn FnOnce(&mut store::BlockingWriter) + Send>;

/// Opening a writer performs filesystem work, so it runs on the worker too.
pub(super) type OpenAttempt = Box<dyn FnOnce() -> std::result::Result<store::BlockingWriter, store::StoreError> + Send>;

/// Lowest worker count: even a single-core machine keeps a commit from blocking
/// an unrelated session's commit on a second thread.
const MIN_WORKERS: usize = 2;
/// Highest worker count: commits are fsync-bound, and more than four concurrent
/// writers only adds lock and stack pressure without shortening the wall time.
const MAX_WORKERS: usize = 4;

/// Thread name of every pool worker. Tests count threads by this name.
pub(super) const WORKER_NAME: &str = "session-writer";

pub(super) enum Command {
    Open {
        session: u64,
        /// Released only after the `BlockingWriter` in the same map entry, so a
        /// waiting opener learns about the lock release and not earlier.
        lease: WriterSlotLease,
        open: OpenAttempt,
        ready: mpsc::Sender<std::result::Result<(), store::StoreError>>,
    },
    Run {
        session: u64,
        job: Job,
    },
    Close {
        session: u64,
        done: mpsc::Sender<()>,
    },
}

struct Worker {
    commands: mpsc::Sender<Command>,
}

static POOL: LazyLock<Vec<Worker>> = LazyLock::new(start_workers);

/// Session handles are unique forever, so a `Run` queued for a closed session
/// can never reach a session that opened later.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static ON_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Number of writer threads this process will use.
pub(super) fn pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1)
        .clamp(MIN_WORKERS, MAX_WORKERS)
}

fn start_workers() -> Vec<Worker> {
    let mut workers = Vec::with_capacity(pool_size());
    for index in 0..pool_size() {
        let (commands, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name(format!("{WORKER_NAME}-{index}"))
            .spawn(move || {
                ON_WORKER.with(|flag| flag.set(true));
                serve(receiver);
            });
        match spawned {
            Ok(_thread) => workers.push(Worker { commands }),
            Err(error) => {
                // A process that cannot start a writer thread cannot commit at
                // all; the remaining workers still serve, and a fully failed
                // pool reports an error from every `submit`.
                crate::logging::error(&format!("Session writer worker {index} did not start: {error}"));
            }
        }
    }
    workers
}

/// Worker index that owns `key`, or `None` when no worker could be started.
fn slot(key: &Key) -> Option<usize> {
    let workers = &*POOL;
    if workers.is_empty() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    Some((hasher.finish() % workers.len() as u64) as usize)
}

/// Worker that owns `key`.
///
/// The registry key is already canonicalized, so two spellings of one session
/// directory cannot land on different workers and open two `BlockingWriter`s
/// for one lock. `DefaultHasher::new()` is keyed identically for every instance,
/// so a key maps to the same worker for the life of the process; a `RandomState`
/// here would re-bucket on every call.
fn worker(key: &Key) -> Option<&'static Worker> {
    slot(key).map(|index| &POOL[index])
}

/// Index of the worker that owns `key`, for tests that need two sessions on
/// different workers (or on the same one).
#[cfg(test)]
pub(super) fn worker_index(key: &Key) -> usize {
    slot(key).unwrap_or(0)
}

/// Queue `command` on the worker that owns `key` and hand back that worker's
/// sender for the facade's later jobs.
///
/// On failure the command comes back inside the error, so the caller can settle
/// state it moved into it (the slot lease) after releasing the locks it holds.
pub(super) fn submit(
    key: &Key,
    command: Command,
) -> std::result::Result<mpsc::Sender<Command>, mpsc::SendError<Command>> {
    let Some(worker) = worker(key) else {
        return Err(mpsc::SendError(command));
    };
    worker.commands.send(command).map(|()| worker.commands.clone())
}

/// True on a pool worker. Blocking there would wait on the very thread that is
/// running the caller, so `Writer::call` and `Writer::drop` turn it into an
/// error instead of a deadlock.
pub(super) fn on_worker_thread() -> bool {
    ON_WORKER.with(std::cell::Cell::get)
}

/// Register a new session handle for the worker's map.
pub(super) fn next_session() -> u64 {
    NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
}

struct SessionWriter {
    /// Declared before the lease so the file lock is released before the lease
    /// notifies a waiting opener of the same slot.
    writer: store::BlockingWriter,
    _lease: WriterSlotLease,
}

fn serve(commands: mpsc::Receiver<Command>) {
    let mut sessions: HashMap<u64, SessionWriter> = HashMap::new();
    // The channel is owned by the process-wide pool and never closes, so this
    // loop runs for the life of the process.
    for command in commands {
        match command {
            Command::Open {
                session,
                lease,
                open,
                ready,
            } => match catch_unwind(AssertUnwindSafe(open)) {
                Ok(Ok(mut writer)) => {
                    writer.enable_cache(store::ReadProbe::default());
                    sessions.insert(session, SessionWriter { writer, _lease: lease });
                    if let Err(error) = ready.send(Ok(())) {
                        // The opener went away before it could take the writer;
                        // drop the entry so the session directory is not locked
                        // by a writer nobody owns.
                        sessions.remove(&session);
                        crate::logging::warn(&format!("Session writer open reply was dropped: {error}"));
                    }
                }
                Ok(Err(error)) => {
                    if let Err(dropped) = ready.send(Err(error)) {
                        crate::logging::warn(&format!("Session writer open reply was dropped: {dropped}"));
                    }
                }
                Err(panic) => {
                    // The opener sees an error and the lease is dropped with the
                    // command, so the slot is free again.
                    crate::logging::error(&format!(
                        "Session writer startup panicked: {}",
                        panic_message(panic.as_ref())
                    ));
                    let failed =
                        store::StoreError::internal(store::StoreErrorCode::Internal, "session writer startup panicked");
                    if let Err(dropped) = ready.send(Err(failed)) {
                        crate::logging::warn(&format!("Session writer open reply was dropped: {dropped}"));
                    }
                }
            },
            Command::Run { session, job } => {
                // A closed or panicked session has no entry. Dropping the job
                // drops its reply sender, so the waiting caller fails with a
                // lost reply rather than hanging.
                let Some(entry) = sessions.get_mut(&session) else {
                    crate::logging::warn("Session writer is gone; queued commit failed");
                    continue;
                };
                if catch_unwind(AssertUnwindSafe(move || job(&mut entry.writer))).is_err() {
                    // Drop this one entry: the BlockingWriter releases its file
                    // lock before the lease notifies the slot.
                    sessions.remove(&session);
                    crate::logging::error(&format!("Session writer job panicked; closed only session {session}"));
                }
            }
            Command::Close { session, done } => {
                sessions.remove(&session);
                if let Err(error) = done.send(()) {
                    crate::logging::warn(&format!("Session writer close reply was dropped: {error}"));
                }
            }
        }
    }
}

/// Panic payload as text, for the log line only.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_owned()
}

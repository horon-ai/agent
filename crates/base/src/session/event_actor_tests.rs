use super::*;
use crate::session::event_pool;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

// The pool keeps a `BlockingWriter` in a worker-owned map instead of a stack
// owned by a thread spawned for one session; `Send` is what allows the move.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<store::BlockingWriter>();
};

#[derive(Debug)]
enum ReopenEvent {
    Waiting,
    Finished(Result<Arc<Writer>>),
}

static WAITERS: LazyLock<Mutex<HashMap<Key, mpsc::Sender<ReopenEvent>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

struct WaitObserver(Key);

impl Drop for WaitObserver {
    fn drop(&mut self) {
        WAITERS.lock().expect("wait observer registry").remove(&self.0);
    }
}

pub(super) fn note_drop_started(writer: &mut Writer) {
    if let Ok(observer) = writer.drop_started.get_mut()
        && let Some(observer) = observer.take()
    {
        let _ = observer.send(());
    }
}

pub(super) fn note_waiting(key: &Key) {
    let observer = WAITERS.lock().expect("wait observer registry").remove(key);
    if let Some(observer) = observer {
        let _ = observer.send(ReopenEvent::Waiting);
    }
}

/// First session id whose registry key lands on `worker`.
///
/// The pool assigns sessions to workers by key hash, so a test that needs two
/// sessions on different workers (or on the same one) has to probe for ids
/// instead of assuming a particular hash.
fn id_on_worker(root: &std::path::Path, worker: usize, prefix: &str) -> Result<store::SessionId> {
    for attempt in 0..4096 {
        let id = store::SessionId::parse(format!("{prefix}_{attempt}"))?;
        if event_pool::worker_index(&registry_key(root, id.as_str())) == worker {
            return Ok(id);
        }
    }
    Err(anyhow!("no candidate id maps to worker {worker}"))
}

#[test]
fn starting_one_writer_does_not_block_another_session() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let first_id = id_on_worker(paths.root(), 0, "session_actor_start_a")?;
    let other_id = id_on_worker(paths.root(), 1, "session_actor_start_b")?;
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let first_paths = paths.clone();
    let first = std::thread::spawn(move || {
        let mut held = false;
        Writer::open_with_hook(
            first_paths,
            first_id,
            Box::new(move |point| {
                if point == store::IoPoint::DirectorySync && !held {
                    held = true;
                    entered.send(()).map_err(std::io::Error::other)?;
                    release_rx.recv().map_err(std::io::Error::other)?;
                }
                Ok(())
            }),
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(10))?;

    let (completed, completed_rx) = mpsc::channel();
    let other = std::thread::spawn(move || {
        let opened = Writer::open(paths, other_id);
        let _ = completed.send(opened);
    });
    let progressed = completed_rx.recv_timeout(Duration::from_secs(10));
    release.send(())?;
    let first_writer = first.join().expect("first opener joins")?;
    other.join().expect("other opener joins");
    let other_writer = progressed.context("session B must open while session A's startup I/O is held")??;
    first_writer.call(|_| Ok(()))?;
    other_writer.call(|_| Ok(()))?;
    Ok(())
}

#[test]
fn reopening_waits_until_the_previous_writer_releases_its_file_lock() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let id = store::SessionId::parse("session_actor_reopen")?;
    let writer = Writer::open(paths.clone(), id.clone())?;
    let (job_entered, job_entered_rx) = mpsc::channel();
    let (release_job, release_job_rx) = mpsc::channel();
    writer.enqueue_job(move |_| {
        let _ = job_entered.send(());
        let _ = release_job_rx.recv();
    })?;
    job_entered_rx.recv_timeout(Duration::from_secs(10))?;

    let (dropping, dropping_rx) = mpsc::channel();
    *writer.drop_started.lock().expect("drop observer") = Some(dropping);
    let weak = Arc::downgrade(&writer);
    let dropper = std::thread::spawn(move || drop(writer));
    dropping_rx.recv_timeout(Duration::from_secs(10))?;
    assert_eq!(weak.strong_count(), 0, "the old facade is already in Drop");

    let key = registry_key(paths.root(), id.as_str());
    let (events, events_rx) = mpsc::channel();
    WAITERS
        .lock()
        .expect("wait observer registry")
        .insert(key.clone(), events.clone());
    let _observer = WaitObserver(key);
    let opener = std::thread::spawn(move || {
        let _ = events.send(ReopenEvent::Finished(Writer::open(paths, id)));
    });

    let first_event = events_rx.recv_timeout(Duration::from_secs(10))?;
    let waited = matches!(first_event, ReopenEvent::Waiting);
    release_job.send(())?;
    dropper.join().expect("old writer drains");
    let reopened = match first_event {
        ReopenEvent::Waiting => match events_rx.recv_timeout(Duration::from_secs(10))? {
            ReopenEvent::Finished(result) => result,
            ReopenEvent::Waiting => panic!("one wait observation per reopen"),
        },
        ReopenEvent::Finished(result) => result,
    };
    opener.join().expect("new opener joins");
    assert!(
        waited,
        "reopening must wait for the known closing owner, got {reopened:?}"
    );
    reopened?.call(|_| Ok(()))?;
    Ok(())
}

#[test]
fn live_import_and_registered_facades_share_the_same_operation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let id = store::SessionId::parse("session_actor_import")?;
    let key = registry_key(paths.root(), id.as_str());
    let writer = Writer::open(paths.clone(), id.clone())?;
    writer.operation()?.retry_required = true;

    let imported = Writer::open_for_import(paths.clone(), id.clone())?;
    assert!(Arc::ptr_eq(&writer, &imported));
    assert!(imported.operation()?.retry_required);
    let registered = Writer::registered(&paths, &id)?.expect("the live facade is registered");
    assert!(Arc::ptr_eq(&writer, &registered));
    drop(registered);
    drop(imported);
    drop(writer);
    assert!(!WRITERS.lock().expect("writer registry").contains_key(&key));
    Ok(())
}

#[test]
fn failed_startup_releases_the_slot_for_the_next_owner() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let id = store::SessionId::parse("session_actor_start_error")?;
    let key = registry_key(paths.root(), id.as_str());
    let failed = Writer::open_with_hook(
        paths.clone(),
        id.clone(),
        Box::new(|_| Err(std::io::Error::other("injected startup failure"))),
    );
    assert!(failed.is_err());
    let writer = Writer::open(paths, id)?;
    writer.call(|_| Ok(()))?;
    drop(writer);
    assert!(!WRITERS.lock().expect("writer registry").contains_key(&key));
    Ok(())
}

#[test]
fn startup_panic_closes_the_reply_before_releasing_the_slot() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let id = store::SessionId::parse("session_actor_start_panic")?;
    let failing_id = id.clone();
    let failing_paths = paths.clone();
    let failed = Writer::open_with_hook(
        failing_paths,
        failing_id,
        Box::new(|_| panic!("injected startup panic")),
    );
    assert!(failed.is_err());
    // The worker that ran the panicking startup is still serving its queue.
    let next_id = id_on_worker(
        paths.root(),
        event_pool::worker_index(&registry_key(paths.root(), id.as_str())),
        "session_actor_start_panic_next",
    )?;
    let writer = Writer::open(paths, next_id)?;
    writer.call(|_| Ok(()))?;
    Ok(())
}

#[test]
fn pool_preserves_per_session_submission_order() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let id = store::SessionId::parse("session_pool_order")?;
    let writer = Writer::open(paths, id)?;
    const JOBS: usize = 64;
    let (seen, seen_rx) = mpsc::channel();
    for index in 0..JOBS {
        let seen = seen.clone();
        writer.enqueue_job(move |_| {
            let _ = seen.send(index);
        })?;
    }
    // A blocking call is the barrier: everything queued before it has run.
    writer.call(|_| Ok(()))?;
    let observed: Vec<usize> = std::iter::from_fn(|| seen_rx.try_recv().ok()).collect();
    assert_eq!(
        observed,
        (0..JOBS).collect::<Vec<_>>(),
        "one session's jobs keep submission order"
    );
    Ok(())
}

#[test]
fn pool_job_panic_closes_only_that_session() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let first_id = id_on_worker(paths.root(), 0, "session_pool_panic_a")?;
    let second_id = id_on_worker(paths.root(), 0, "session_pool_panic_b")?;
    let first = Writer::open(paths.clone(), first_id)?;
    let second = Writer::open(paths.clone(), second_id)?;

    let panicked = first.call(|_| -> Result<()> { panic!("injected job panic") });
    assert!(panicked.is_err(), "the panicking call reports the lost reply");
    assert!(
        first.call(|_| Ok(())).is_err(),
        "the panicked session's writer is closed"
    );
    second.call(|_| Ok(()))?;

    // The worker survived: another session hashed to it still opens and commits.
    let third_id = id_on_worker(paths.root(), 0, "session_pool_panic_c")?;
    let third = Writer::open(paths, third_id)?;
    third.call(|_| Ok(()))?;
    Ok(())
}

#[test]
fn pool_reentrant_call_from_worker_errors_instead_of_deadlocking() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let id = store::SessionId::parse("session_pool_reentrant")?;
    let writer = Writer::open(paths, id)?;
    let inner = Arc::clone(&writer);
    let nested = writer.call(move |_| Ok::<_, anyhow::Error>(inner.call(|_| Ok(()))))?;
    let error = nested.expect_err("a job must not wait on its own worker");
    assert!(error.to_string().contains("pool worker"), "unexpected error: {error}");
    // The worker still serves the session afterwards.
    writer.call(|_| Ok(()))?;
    Ok(())
}

#[test]
fn pool_drop_drains_queued_jobs_before_releasing_lock() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let id = store::SessionId::parse("session_pool_drain")?;
    let writer = Writer::open(paths.clone(), id.clone())?;
    const JOBS: usize = 8;
    let (ran, ran_rx) = mpsc::channel();
    for index in 0..JOBS {
        let ran = ran.clone();
        writer.enqueue_job(move |_| {
            let _ = ran.send(index);
        })?;
    }
    drop(writer);
    let observed: Vec<usize> = std::iter::from_fn(|| ran_rx.try_recv().ok()).collect();
    assert_eq!(observed, (0..JOBS).collect::<Vec<_>>(), "drop drains every queued job");
    // The file lock is free as soon as drop returns.
    let reopened = Writer::open(paths, id)?;
    reopened.call(|_| Ok(()))?;
    Ok(())
}

#[test]
fn writers_do_not_spawn_a_thread_per_session() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    // Warm the pool so its own threads are not counted as growth.
    let warmup = Writer::open(paths.clone(), store::SessionId::parse("session_pool_warmup")?)?;
    warmup.call(|_| Ok(()))?;
    drop(warmup);

    let before_named = named_worker_threads();
    if let Some(named) = before_named {
        assert_eq!(
            named,
            event_pool::pool_size() as u64,
            "the warm-up session created the whole pool once"
        );
    }
    let before_total = process_thread_count();

    let mut writers = Vec::new();
    for index in 0..32 {
        let id = store::SessionId::parse(format!("session_pool_thread_{index:02}"))?;
        let writer = Writer::open(paths.clone(), id)?;
        writer.call(|_| Ok(()))?;
        writers.push(writer);
    }

    let after_total = process_thread_count();
    let after_named = named_worker_threads();
    if let Some(named) = after_named {
        assert_eq!(
            named,
            event_pool::pool_size() as u64,
            "32 live sessions must run on exactly the pool's threads"
        );
    }
    if let (Some(before), Some(after)) = (before_total, after_total) {
        let grown = after.saturating_sub(before);
        // Not one thread per session. The pool is already warm here, so a quiet
        // process grows by zero; the bound leaves room for threads other tests
        // spawn in the same binary at the same moment.
        assert!(grown < 16, "32 sessions grew the process by {grown} threads");
    }
    drop(writers);
    Ok(())
}

/// Head-of-line measurement for the pool's one failure mode.
///
/// A save reads the whole event log three times, so a big session holds its
/// worker for hundreds of milliseconds. This measures how long a second session
/// on the same worker waits behind a 30 MB write + fsync, and confirms a session
/// on another worker is not delayed. Ignored by default: it is a measurement,
/// not a correctness check.
#[test]
#[ignore = "latency measurement; run with --ignored for QA evidence"]
fn pool_head_of_line_latency_is_measured() -> Result<()> {
    use std::io::Write;

    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let long_id = id_on_worker(paths.root(), 0, "session_hol_long")?;
    let same_id = id_on_worker(paths.root(), 0, "session_hol_same")?;
    let other_id = id_on_worker(paths.root(), 1, "session_hol_other")?;
    let long = Writer::open(paths.clone(), long_id)?;
    let same = Writer::open(paths.clone(), same_id)?;
    let other = Writer::open(paths.clone(), other_id)?;

    let idle = timed_job(&same)?;
    println!("idle small job: {idle:?}");

    let (holding, holding_rx) = mpsc::channel();
    let (written, written_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let big_path = root.path().join("head-of-line-30mb.bin");
    long.enqueue_job(move |_| {
        let _ = holding.send(());
        let started = std::time::Instant::now();
        if let Ok(mut file) = std::fs::File::create(&big_path) {
            let chunk = vec![0u8; 1 << 20];
            for _ in 0..30 {
                if file.write_all(&chunk).is_err() {
                    break;
                }
            }
            let _ = file.sync_all();
        }
        let _ = written.send(started.elapsed());
        let _ = release_rx.recv();
    })?;
    holding_rx.recv_timeout(Duration::from_secs(10))?;

    let (same_done, same_done_rx) = mpsc::channel();
    let same_start = std::time::Instant::now();
    same.enqueue_job(move |_| {
        let _ = same_done.send(same_start.elapsed());
    })?;
    let (other_done, other_done_rx) = mpsc::channel();
    let other_start = std::time::Instant::now();
    other.enqueue_job(move |_| {
        let _ = other_done.send(other_start.elapsed());
    })?;

    let other_latency = other_done_rx.recv_timeout(Duration::from_secs(10))?;
    let write_duration = written_rx.recv_timeout(Duration::from_secs(60))?;
    release.send(())?;
    let same_latency = same_done_rx.recv_timeout(Duration::from_secs(60))?;
    println!(
        "30 MB write+fsync: {write_duration:?}; same-worker job waits {same_latency:?}; \
         other-worker job waits {other_latency:?}"
    );
    assert!(
        other_latency < same_latency,
        "a session on another worker must not wait behind this one: {other_latency:?} vs {same_latency:?}"
    );
    Ok(())
}

fn timed_job(writer: &Writer) -> Result<Duration> {
    let (done, done_rx) = mpsc::channel();
    let start = std::time::Instant::now();
    writer.enqueue_job(move |_| {
        let _ = done.send(start.elapsed());
    })?;
    done_rx.recv_timeout(Duration::from_secs(30)).map_err(Into::into)
}

/// Threads whose name starts with the pool worker name, from `/proc`.
///
/// The count is exact where `/proc/self/task` exists and `None` where it does
/// not, which is what makes this assertion immune to threads other tests spawn
/// in the same process. macOS has no `/proc`, so the test falls back to the
/// process-wide thread count there.
fn named_worker_threads() -> Option<u64> {
    let tasks = std::fs::read_dir("/proc/self/task").ok()?;
    let count = tasks
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim_end().starts_with(event_pool::WORKER_NAME))
        })
        .count();
    Some(count as u64)
}

fn process_thread_count() -> Option<u64> {
    horon_agent_platform::process::process_memory_facts().thread_count
}

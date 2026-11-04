use super::*;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

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

#[test]
fn starting_one_writer_does_not_block_another_session() -> Result<()> {
    let root = tempfile::tempdir()?;
    let paths = store::SessionPaths::new(root.path().join("sessions"));
    let first_id = store::SessionId::parse("session_actor_start_a")?;
    let other_id = store::SessionId::parse("session_actor_start_b")?;
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
    writer
        .queue
        .as_ref()
        .expect("live writer queue")
        .send(Box::new(move |_| {
            let _ = job_entered.send(());
            let _ = release_job_rx.recv();
        }))
        .map_err(|_| anyhow!("test writer queue stopped"))?;
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
    let failing_paths = paths.clone();
    let failing_id = id.clone();
    let (result, result_rx) = mpsc::channel();
    let opener = std::thread::spawn(move || {
        let failed = Writer::open_with_hook(
            failing_paths,
            failing_id,
            Box::new(|_| panic!("injected startup panic")),
        );
        let _ = result.send(failed);
    });
    let failed = result_rx.recv_timeout(Duration::from_secs(10))?;
    opener.join().expect("failed opener joins");
    assert!(failed.is_err());
    let writer = Writer::open(paths, id)?;
    writer.call(|_| Ok(()))?;
    Ok(())
}

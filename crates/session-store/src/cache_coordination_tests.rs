use super::*;
use crate::checkpoint::SessionCache;
use crate::{
    BlockingWriter, EventDraft, EventId, EventKind, IoPoint, KnownEventKind, OriginId, ReadProbe, SessionMetadata,
};
use std::sync::{TryLockError, mpsc};
use std::time::Duration;

#[path = "../tests/support/mod.rs"]
mod support;

static OBSERVERS: LazyLock<Mutex<HashMap<PathBuf, mpsc::Sender<()>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn before_lock(gate: &Gate) {
    if matches!(gate.held.try_lock(), Err(TryLockError::WouldBlock))
        && let Some(observer) = OBSERVERS.lock().expect("observers").remove(&gate.key)
    {
        let _ = observer.send(());
    }
}

fn seeded() -> Result<(SessionPaths, SessionId), StoreError> {
    let paths = SessionPaths::new(support::temp_root("cache-coordination"));
    let id = SessionId::parse("cache")?;
    let root = EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        None,
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        serde_json::json!({"metadata": SessionMetadata::default(), "branch_root": false}),
    )
    .seal()?;
    let mut writer = BlockingWriter::open(paths.clone(), id.clone())?;
    writer.enable_cache(ReadProbe::default());
    writer.append(&root)?;
    drop(writer);
    Ok((paths, id))
}

fn cache_reader_case(fail_repair: bool) -> Result<(), StoreError> {
    let (paths, id) = seeded()?;
    let prefix = std::fs::read(paths.events_log(&id)?)?;
    std::fs::remove_file(paths.session_dir(&id)?.join("checkpoint.json"))?;
    let cache = SessionCache::new(paths.clone(), ReadProbe::default());
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let mut held = false;
    *cache.repair_hook.lock().expect("repair hook") = Some(Box::new(move |point| {
        if point == IoPoint::CacheStaged && !held {
            held = true;
            entered
                .send(())
                .map_err(|_| std::io::Error::other("observer stopped"))?;
            release_rx
                .recv()
                .map_err(|_| std::io::Error::other("release stopped"))?;
            if fail_repair {
                return Err(std::io::Error::from_raw_os_error(28));
            }
        }
        Ok(())
    }));
    let read_id = id.clone();
    let reader = std::thread::spawn(move || cache.recent(&read_id, None));
    let started = entered_rx.recv_timeout(Duration::from_secs(10));
    if started.is_err() {
        drop(release);
        reader.join().expect("reader joins")?;
        return Err(StoreError::internal(
            StoreErrorCode::Internal,
            "reader did not enter repair",
        ));
    }

    // This real cache reader owns an OS lease outside the base Actor registry.
    let ungated = BlockingWriter::open(paths.clone(), id.clone());
    assert_eq!(ungated.err().map(|error| error.code), Some(StoreErrorCode::Busy));

    let gate_key = key(&paths, &id)?;
    let (waiting, waiting_rx) = mpsc::channel();
    OBSERVERS.lock().expect("observers").insert(gate_key.clone(), waiting);
    let alias = SessionPaths::new(paths.root().join("."));
    let open_id = id.clone();
    let opener = std::thread::spawn(move || {
        with_cache_repair(&alias, &open_id, || {
            BlockingWriter::open(alias.clone(), open_id.clone())
        })
    });

    // Another session must progress while this session's actual repair is held.
    let other_paths = paths.clone();
    let (other_done, other_rx) = mpsc::channel();
    let other = std::thread::spawn(move || {
        let other_id = SessionId::parse("other")?;
        let writer = with_cache_repair(&other_paths, &other_id, || {
            BlockingWriter::open(other_paths.clone(), other_id.clone())
        })?;
        drop(writer);
        let _ = other_done.send(());
        Ok::<(), StoreError>(())
    });
    let waited = waiting_rx.recv_timeout(Duration::from_secs(10));
    let unrelated = other_rx.recv_timeout(Duration::from_secs(10));
    let _ = release.send(());
    let read = reader.join().expect("reader joins");
    let opened = opener.join().expect("opener joins");
    other.join().expect("other joins")?;
    OBSERVERS.lock().expect("observers").remove(&gate_key);
    assert!(waited.is_ok(), "startup must wait on the known temporary reader");
    assert!(unrelated.is_ok(), "unrelated sessions must not wait on this repair");
    assert_eq!(read.is_err(), fail_repair);
    let writer = opened?;
    // An ordinary second writer still fails nonblocking, including in-process.
    assert_eq!(
        BlockingWriter::open(paths.clone(), id.clone())
            .err()
            .map(|error| error.code),
        Some(StoreErrorCode::Busy)
    );
    drop(writer);
    assert_eq!(std::fs::read(paths.events_log(&id)?)?, prefix);
    assert!(!GATES.lock().expect("registry").contains_key(&gate_key));
    std::fs::remove_dir_all(paths.root())?;
    Ok(())
}

#[test]
fn actual_cache_reader_and_writer_startup_share_the_session_barrier() -> Result<(), StoreError> {
    cache_reader_case(false)?;
    cache_reader_case(true)
}

#[test]
fn a_regular_writer_remains_busy_and_failed_startup_leaves_no_gate() -> Result<(), StoreError> {
    let (paths, id) = seeded()?;
    let owner = BlockingWriter::open(paths.clone(), id.clone())?;
    let result = with_cache_repair(&paths, &id, || BlockingWriter::open(paths.clone(), id.clone()));
    assert_eq!(result.err().map(|error| error.code), Some(StoreErrorCode::Busy));
    let gate_key = key(&paths, &id)?;
    assert!(!GATES.lock().expect("registry").contains_key(&gate_key));
    drop(owner);
    drop(with_cache_repair(&paths, &id, || {
        BlockingWriter::open(paths.clone(), id.clone())
    })?);
    assert!(!GATES.lock().expect("registry").contains_key(&gate_key));
    std::fs::remove_dir_all(paths.root())?;
    Ok(())
}

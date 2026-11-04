//! Coordinates temporary cache-repair owners with persistent-writer startup.
use crate::{SessionId, SessionPaths, StoreError, StoreErrorCode};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, Weak};

struct Gate {
    key: PathBuf,
    held: Mutex<()>,
}

static GATES: LazyLock<Mutex<HashMap<PathBuf, Weak<Gate>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn poisoned() -> StoreError {
    StoreError::internal(StoreErrorCode::Internal, "cache repair coordination poisoned")
}

fn key(paths: &SessionPaths, id: &SessionId) -> Result<PathBuf, StoreError> {
    SessionId::parse(id.as_str())?;
    // Startup creates a missing root; the real writer still validates every
    // filesystem component before touching it. This key only coordinates it.
    let mut ancestor = paths.root().join(id.as_str());
    let mut suffix = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut path) => {
                for component in suffix.into_iter().rev() {
                    path.push(component);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| StoreError::internal(StoreErrorCode::Internal, "session path has no ancestor"))?
                        .to_owned(),
                );
                ancestor.pop();
            }
            Err(error) => return Err(error.into()),
        }
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let this = self as *const Gate;
        let mut gates = GATES.lock().unwrap_or_else(|error| error.into_inner());
        if gates
            .get(&self.key)
            .is_some_and(|gate| std::ptr::eq(gate.as_ptr(), this))
        {
            gates.remove(&self.key);
        }
    }
}

/// Serializes temporary cache repair and persistent-writer acquisition for one session.
///
/// Cache repair must drop its temporary writer before `work` returns. Persistent
/// startup may return a writer: its subsequent lifetime is fenced by the OS lock.
/// The registry never holds filesystem I/O, and unrelated sessions do not wait.
/// This is not reentrant. Callers must not acquire a catalog lock before entering.
/// Ordinary `BlockingWriter` constructors keep their nonblocking Busy contract.
/// # Errors
/// Returns path/coordination errors or the error returned by `work`.
pub fn with_cache_repair<T>(
    paths: &SessionPaths,
    id: &SessionId,
    work: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let key = key(paths, id)?;
    let gate = {
        let mut gates = GATES.lock().map_err(|_| poisoned())?;
        match gates.get(&key).and_then(Weak::upgrade) {
            Some(gate) => gate,
            None => {
                let gate = Arc::new(Gate {
                    key: key.clone(),
                    held: Mutex::new(()),
                });
                gates.insert(key, Arc::downgrade(&gate));
                gate
            }
        }
    };
    #[cfg(all(test, unix))]
    tests::before_lock(&gate);
    let _held = gate.held.lock().map_err(|_| poisoned())?;
    work()
}

#[cfg(all(test, unix))]
#[path = "cache_coordination_tests.rs"]
mod tests;

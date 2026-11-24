//! The sessions a reader keeps at the top of the session list, in a local file.
//!
//! A pin is the reader's own preference, so it is stored by this client and
//! never written into a session or sent to the harness. The file is
//! `$HORON_HOME/tui-pins.json`, created owner-only, and a save is a
//! read-merge-write under an exclusive OS lock on a persistent lock file, so a
//! second `horon-tui` writing at the same moment keeps its own endpoint's pins
//! without either writer breaking the other's lock.
//!
//! Session ids are unique only inside the daemon that issued them, and this
//! client can be pointed at another daemon with `--socket` or a forwarded
//! socket. The file therefore keeps one list per endpoint, named by the socket
//! the connection used: an id that names a session on another server is never
//! read as this one's. A file from a newer build is reported and left alone,
//! and a file this build cannot parse is moved aside rather than overwritten.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The schema this build writes.
const VERSION: u64 = 1;

/// How long a save waits for another writer's lock before reporting the wait.
const LOCK_WAIT: Duration = Duration::from_millis(500);
const LOCK_PAUSE: Duration = Duration::from_millis(20);

/// Every endpoint's pinned ids, in the shape the file holds them.
type ByScope = BTreeMap<String, BTreeSet<String>>;

/// Why a pin could not be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinFault(String);

impl PinFault {
    /// A fault carrying the sentence to show.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for PinFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The pinned sessions of one endpoint, and the file they come from.
///
/// A change is written before the reader sees it: [`Self::set_pinned`] answers
/// `Ok(true)` only once the file holds the new list, so the screen never says a
/// pin took effect when the write failed.
#[derive(Debug)]
pub struct PinStore {
    /// The file the pins are read from and written to; `None` keeps them in
    /// memory only, which is what a test or an embedded `App` uses.
    path: Option<PathBuf>,
    /// The endpoint whose list this store reads and writes.
    scope: String,
    loaded: bool,
    /// The file holds a schema this build must not overwrite.
    foreign: bool,
    /// Every endpoint's list, as the file holds them.
    scopes: ByScope,
}

impl PinStore {
    /// A store that never touches the disk.
    pub fn memory() -> Self {
        Self {
            path: None,
            scope: String::new(),
            loaded: true,
            foreign: false,
            scopes: BTreeMap::new(),
        }
    }

    /// A store backed by `path`, for the endpoint named by `scope`.
    pub fn at(path: impl Into<PathBuf>, scope: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            scope: scope.into(),
            loaded: false,
            foreign: false,
            scopes: BTreeMap::new(),
        }
    }

    /// The file the client reads when it owns the terminal.
    pub fn default_path() -> PathBuf {
        horon_agent_sdk::user_horon_agent_home().join("tui-pins.json")
    }

    /// The store this client owns, for the endpoint it connected to.
    pub fn owned(scope: impl Into<String>) -> Self {
        Self::at(Self::default_path(), scope)
    }

    /// Read the file once, reporting anything the reader should know.
    ///
    /// A missing file is the ordinary first run. A file that is not this
    /// schema is moved aside so the pins in it are not silently destroyed by
    /// the next save; a file from a newer build is reported and left in place,
    /// and saving to it is refused.
    pub fn ensure_loaded(&mut self) -> Vec<String> {
        if self.loaded {
            return Vec::new();
        }
        self.loaded = true;
        let mut warnings = Vec::new();
        let Some(path) = self.path.clone() else {
            return warnings;
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return warnings,
            Err(error) => {
                self.foreign = true;
                warnings.push(format!(
                    "cannot read {}: {error}; pins are not saved this run",
                    path.display()
                ));
                return warnings;
            }
        };
        match parse(&text) {
            Ok(Parsed::Pins(scopes, malformed)) => {
                self.scopes = scopes;
                if malformed > 0 {
                    warnings.push(format!(
                        "{} held {malformed} unreadable pin entries; they were skipped",
                        path.display()
                    ));
                }
            }
            Ok(Parsed::Newer(version)) => {
                self.foreign = true;
                warnings.push(format!(
                    "{} uses schema version {version}, which this build does not write; pins are left alone",
                    path.display()
                ));
            }
            Err(reason) => self.set_aside(&path, &reason, &mut warnings),
        }
        warnings
    }

    /// Whether this endpoint has `session` pinned.
    pub fn is_pinned(&self, session: &str) -> bool {
        self.scopes
            .get(&self.scope)
            .is_some_and(|pinned| pinned.contains(session))
    }

    /// Pin or unpin one session, answering whether the list changed.
    ///
    /// The file is written before the change is kept in memory: when the write
    /// fails the error reaches the caller and both this store and the screen
    /// still show the list they had. A set that already reads the way `pinned`
    /// asks writes nothing and answers `false`.
    ///
    /// Two clients pinning different sessions at the same moment must both
    /// keep their pin, so a file-backed change is applied to the list read
    /// under the save lock rather than writing a whole set decided before it.
    /// The answer is `true` whenever the caller's screen needs the new state,
    /// which is also when another client changed it first.
    pub fn set_pinned(&mut self, session: &str, pinned: bool) -> Result<bool, PinFault> {
        let session = session.trim();
        if session.is_empty() {
            return Err(PinFault::new("the session id is empty"));
        }
        let wanted = self.is_pinned(session);
        if self.path.is_none() {
            if wanted == pinned {
                return Ok(false);
            }
            let mut candidate = self.scopes.get(&self.scope).cloned().unwrap_or_else(BTreeSet::new);
            if pinned {
                candidate.insert(session.to_string());
            } else {
                candidate.remove(session);
            }
            self.commit(candidate);
            return Ok(true);
        }
        if self.foreign {
            return Err(PinFault::new(
                "the pins file is not one this build writes; nothing was saved",
            ));
        }
        let (merged, changed) = self.write_change(session, pinned)?;
        self.scopes = merged;
        Ok(changed || wanted != self.is_pinned(session))
    }

    /// Keep a changed list for this endpoint.
    fn commit(&mut self, candidate: BTreeSet<String>) {
        if candidate.is_empty() {
            self.scopes.remove(&self.scope);
        } else {
            self.scopes.insert(self.scope.clone(), candidate);
        }
    }

    /// Apply one change to this endpoint's list in the file, answering the
    /// merged lists and whether the file now differs from what was read.
    ///
    /// The lock keeps two clients from replacing each other's endpoints, and
    /// the merge reads the file again under it because another client may have
    /// written since this one loaded. Only the one session this press names is
    /// changed there, so a pin another client took in the meantime survives.
    fn write_change(&mut self, session: &str, pinned: bool) -> Result<(ByScope, bool), PinFault> {
        let Some(path) = self.path.clone() else {
            return Ok((self.scopes.clone(), false));
        };
        // The lock file and the pins file share a directory, and the first
        // save on a fresh home has neither: create it before the lock, or the
        // lock is the first thing that fails on a directory that only the save
        // would have made.
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)
                .map_err(|error| PinFault::new(format!("cannot create {}: {error}", directory.display())))?;
        }
        let _lock = SaveLock::acquire(&lock_path(&path))?;
        let mut merged = read_for_merge(&path)?;
        let mut set = merged.get(&self.scope).cloned().unwrap_or_else(BTreeSet::new);
        let was = set.contains(session);
        if pinned {
            set.insert(session.to_string());
        } else {
            set.remove(session);
        }
        let changed = was != set.contains(session);
        if set.is_empty() {
            merged.remove(&self.scope);
        } else {
            merged.insert(self.scope.clone(), set);
        }
        if changed {
            write_atomic(&path, &merged)?;
        }
        Ok((merged, changed))
    }

    /// Move an unreadable file out of the way so the pins in it survive.
    fn set_aside(&mut self, path: &Path, reason: &PinFault, warnings: &mut Vec<String>) {
        let aside = corrupt_path(path);
        if let Some(error) = crate::app::state_file::discard(&aside) {
            self.foreign = true;
            warnings.push(format!(
                "{} is not readable ({reason}) and {}; pins are not saved this run",
                path.display(),
                error
            ));
            return;
        }
        match fs::rename(path, &aside) {
            Ok(()) => warnings.push(format!(
                "{} is not readable ({reason}); moved it to {}",
                path.display(),
                aside.display()
            )),
            Err(error) => {
                self.foreign = true;
                warnings.push(format!(
                    "{} is not readable ({reason}) and could not be moved aside: {error}; pins are not saved this run",
                    path.display()
                ));
            }
        }
    }
}

/// What reading the file produced.
enum Parsed {
    /// The lists, and how many entries were skipped as unreadable.
    Pins(ByScope, usize),
    /// The schema version, from a build that writes a newer one.
    Newer(u64),
}

/// Read the file's text into endpoint lists.
fn parse(text: &str) -> Result<Parsed, PinFault> {
    let value: Value = serde_json::from_str(text).map_err(|error| PinFault::new(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| PinFault::new("the file is not a JSON object"))?;
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| PinFault::new("the file names no schema version"))?;
    if version > VERSION {
        return Ok(Parsed::Newer(version));
    }
    if version < VERSION {
        return Err(PinFault::new(format!("schema version {version} is no longer read")));
    }
    let mut scopes = BTreeMap::new();
    let mut malformed = 0;
    let Some(stored) = object.get("scopes") else {
        return Ok(Parsed::Pins(scopes, malformed));
    };
    let stored = stored
        .as_object()
        .ok_or_else(|| PinFault::new("`scopes` is not an object"))?;
    for (scope, ids) in stored {
        if scope.trim().is_empty() {
            malformed += 1;
            continue;
        }
        let Some(ids) = ids.as_array() else {
            malformed += 1;
            continue;
        };
        let mut pinned = BTreeSet::new();
        for id in ids {
            match id.as_str() {
                Some(id) if !id.trim().is_empty() => {
                    pinned.insert(id.trim().to_string());
                }
                _ => malformed += 1,
            }
        }
        if !pinned.is_empty() {
            scopes.insert(scope.clone(), pinned);
        }
    }
    Ok(Parsed::Pins(scopes, malformed))
}

/// The file's contents for a set of endpoint lists.
fn encode_pins(scopes: &ByScope) -> serde_json::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(128 + scopes.len() * 64);
    out.extend_from_slice(b"{\n  \"scopes\": {");
    for (scope_index, (scope, pinned)) in scopes.iter().enumerate() {
        if scope_index > 0 {
            out.push(b',');
        }
        out.extend_from_slice(b"\n    ");
        serde_json::to_writer(&mut out, scope)?;
        if pinned.is_empty() {
            out.extend_from_slice(b": []");
        } else {
            out.extend_from_slice(b": [");
            for (id_index, id) in pinned.iter().enumerate() {
                if id_index > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(b"\n      ");
                serde_json::to_writer(&mut out, id)?;
            }
            out.extend_from_slice(b"\n    ]");
        }
    }
    if !scopes.is_empty() {
        out.extend_from_slice(b"\n  ");
    }
    out.extend_from_slice(b"},\n  \"version\": ");
    serde_json::to_writer(&mut out, &VERSION)?;
    out.extend_from_slice(b"\n}");
    Ok(out)
}

/// The file as another writer left it, read under the save lock.
fn read_for_merge(path: &Path) -> Result<ByScope, PinFault> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(PinFault::new(format!("cannot read {}: {error}", path.display()))),
    };
    match parse(&text) {
        Ok(Parsed::Pins(scopes, _)) => Ok(scopes),
        Ok(Parsed::Newer(version)) => Err(PinFault::new(format!(
            "{} uses schema version {version}; nothing was saved",
            path.display()
        ))),
        Err(reason) => Err(PinFault::new(format!(
            "{} is not readable ({reason}); nothing was saved",
            path.display()
        ))),
    }
}

/// `<path>.corrupt`: where an unreadable pins file is kept.
fn corrupt_path(path: &Path) -> PathBuf {
    crate::app::state_file::corrupt_path(path, "tui-pins")
}

/// `<path>.lock`: the file that keeps two savers apart.
fn lock_path(path: &Path) -> PathBuf {
    crate::app::state_file::lock_path(path, "tui-pins")
}

/// Write `scopes` beside `path` and rename it over the file.
///
/// The temporary file is created owner-only and flushed before the rename, which
/// is [`crate::app::state_file::write_atomic`]'s job: the pins file shares its
/// durability rule with the notes file, so the sequence is written once.
fn write_atomic(path: &Path, scopes: &ByScope) -> Result<(), PinFault> {
    let bytes = encode_pins(scopes).map_err(|error| PinFault::new(error.to_string()))?;
    crate::app::state_file::write_atomic(path, &bytes, "tui-pins").map_err(PinFault::new)
}

/// A held save lock, released when its file handle closes.
///
/// The lock file is never removed: deleting it would let a second writer lock a
/// new file while this one still holds the old one, which is the race a lock
/// decided by the file's existence cannot survive. The OS releases this lock
/// when the handle closes and when a writer dies, so a crashed writer never has
/// to be broken by hand — and a live writer is never bypassed.
struct SaveLock {
    _file: File,
}

impl SaveLock {
    /// Take the exclusive lock, waiting briefly for a live writer.
    ///
    /// A writer still holding the lock when the wait ends is reported rather
    /// than bypassed: the caller keeps its old state and the next save tries
    /// again.
    fn acquire(path: &Path) -> Result<Self, PinFault> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        horon_agent_sdk::private_fs::harden_credential_open(&mut options, path)
            .map_err(|error| PinFault::new(format!("cannot prepare {}: {error}", path.display())))?;
        let file = options
            .open(path)
            .map_err(|error| PinFault::new(format!("cannot open {}: {error}", path.display())))?;
        horon_agent_sdk::private_fs::set_permissions_owner_only_file(&file)
            .map_err(|error| PinFault::new(format!("cannot make {} private: {error}", path.display())))?;
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(PinFault::new(format!(
                            "another horon-tui is writing {}; the pin is kept for the next save",
                            path.display()
                        )));
                    }
                    std::thread::sleep(LOCK_PAUSE);
                }
                Err(TryLockError::Error(error)) => {
                    return Err(PinFault::new(format!("cannot lock {}: {error}", path.display())));
                }
            }
        }
    }
}

//! Local review notes attached to the conversation, kept in a TUI-only file.
//!
//! A note is the reader's own line about a message: "this answer missed the
//! return value", "check whether the second call was needed". It is never sent
//! to the harness and never written into a session file, which is why it lives
//! in a file this client owns and why the note is drawn beside the message
//! rather than pushed into it.
//!
//! A note belongs to a session and, when the harness provides it, a stored
//! message id. Live rows and older daemons can lack that id. For those rows
//! the fallback is the visible answer's text fingerprint and its occurrence.
//! Addresses such as `a3` are how readers select a row in the current view.
//!
//! The file is `$HORON_HOME/tui-notes.json`, written by this client alone. It is
//! created owner-only, a file found wider is narrowed, and a save is a
//! read-merge-write under a lock file so a second `horon-tui` writing at the
//! same moment keeps its sessions. A file this build cannot read is moved
//! aside as `<name>.corrupt` rather than overwritten, and a file from a newer
//! schema is reported and left untouched.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::app::App;
use crate::transcript::{Item, Transcript};

/// Longest note kept, in characters after normalisation.
pub(crate) const MAX_NOTE_CHARS: usize = 500;

/// The schema this build writes.
const VERSION: u64 = 1;

/// How long a save lock may sit before another writer treats it as abandoned.
const LOCK_STALE: Duration = Duration::from_secs(10);

/// How many times a save waits for another writer's lock, and for how long.
const LOCK_TRIES: usize = 20;
const LOCK_PAUSE: Duration = Duration::from_millis(20);

/// Which side of the conversation a noted message is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// A message the user sent.
    User,
    /// A message the model produced.
    Assistant,
}

impl Role {
    /// The letter the stored key and the address start with.
    fn code(self) -> char {
        match self {
            Self::User => 'u',
            Self::Assistant => 'a',
        }
    }

    fn from_code(code: char) -> Option<Self> {
        match code {
            'u' | 'U' => Some(Self::User),
            'a' | 'A' => Some(Self::Assistant),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::User => 0,
            Self::Assistant => 1,
        }
    }
}

/// What the reader types to name a message: its place among the messages of
/// its role, counted from one.
///
/// The address is a view, not an identity: it moves when the conversation
/// before it changes, so the store never keys a note by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address {
    role: Role,
    ordinal: usize,
}

impl Address {
    /// The address of the `ordinal`-th message of `role`, counted from one.
    pub fn new(role: Role, ordinal: usize) -> Self {
        Self { role, ordinal }
    }

    /// Parse `a3`, `u12` and the like. Zero is not a message and is refused.
    pub fn parse(text: &str) -> Option<Self> {
        let mut characters = text.chars();
        let role = Role::from_code(characters.next()?)?;
        let digits = characters.as_str();
        if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
            return None;
        }
        let ordinal = match digits.parse::<usize>() {
            Ok(ordinal) => ordinal,
            Err(_) => return None,
        };
        (ordinal > 0).then_some(Self { role, ordinal })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.role.code(), self.ordinal)
    }
}

/// What a note is stored under: the message's role and content.
///
/// The fingerprint is of the text with runs of whitespace collapsed, so a
/// re-wrapped history row still matches; a different sentence does not. Two
/// messages with the same text are told apart by `occurrence`, counted in
/// conversation order among the messages of that role with that fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageKey {
    role: Role,
    fingerprint: u64,
    occurrence: usize,
}

impl MessageKey {
    /// The identity of the `occurrence`-th message with this role and text.
    pub fn of(role: Role, text: &str, occurrence: usize) -> Self {
        Self {
            role,
            fingerprint: fingerprint(text),
            occurrence,
        }
    }

    /// The form the file stores: `a1@0123456789abcdef`.
    fn token(self) -> String {
        format!("{}{}@{:016x}", self.role.code(), self.occurrence, self.fingerprint)
    }

    /// Read a stored token. Anything malformed, including occurrence zero, is
    /// not an identity and is skipped.
    fn parse_token(text: &str) -> Option<Self> {
        let (head, hex) = text.split_once('@')?;
        if hex.len() != 16 || !hex.chars().all(|character| character.is_ascii_hexdigit()) {
            return None;
        }
        let mut characters = head.chars();
        let role = Role::from_code(characters.next()?)?;
        let digits = characters.as_str();
        if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
            return None;
        }
        let occurrence = match digits.parse::<usize>() {
            Ok(occurrence) => occurrence,
            Err(_) => return None,
        };
        let fingerprint = match u64::from_str_radix(hex, 16) {
            Ok(fingerprint) => fingerprint,
            Err(_) => return None,
        };
        (occurrence > 0).then_some(Self {
            role,
            fingerprint,
            occurrence,
        })
    }
}

impl fmt::Display for MessageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.token())
    }
}

/// One note and when it last changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The note's text, one line.
    pub text: String,
    /// Where the message sat when the note was written, for listing a note
    /// whose text the conversation no longer has.
    pub address: Address,
    /// Milliseconds since the Unix epoch, when the text was written.
    pub updated_ms: u64,
    /// The stored message the note was written on, when the harness named one.
    ///
    /// Two messages can read the same and still be different answers: a replay,
    /// a compaction, or a repeated sentence. Once both the note and the row name
    /// a stored message, the note is drawn only under the one it was written on.
    /// A note written before this field existed, or on a row from a harness that
    /// reports no ids, has none and keeps matching by text alone.
    pub stored_id: Option<String>,
}

/// Why a note could not be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteError {
    /// The session id was empty, so a note could not be placed in a session.
    NoSession,
    /// The text was empty after normalisation.
    Empty,
}

impl fmt::Display for NoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSession => f.write_str("the session has no id to attach a note to"),
            Self::Empty => f.write_str("the note has no text"),
        }
    }
}

/// Why a notes operation could not be finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteFault(String);

impl NoteFault {
    /// A fault carrying the sentence to show.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for NoteFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Default)]
struct SessionNotes {
    by_text: BTreeMap<MessageKey, Note>,
    stored: BTreeMap<String, (MessageKey, Note)>,
}

impl SessionNotes {
    fn len(&self) -> usize {
        self.by_text.len() + self.stored.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn insert(&mut self, key: MessageKey, note: Note) {
        if let Some(id) = &note.stored_id {
            self.stored.insert(id.clone(), (key, note));
        } else {
            self.by_text.insert(key, note);
        }
    }

    fn iter(&self) -> impl Iterator<Item = (MessageKey, &Note)> {
        self.by_text
            .iter()
            .map(|(key, note)| (*key, note))
            .chain(self.stored.values().map(|(key, note)| (*key, note)))
    }

    fn stored_fallback(&self, key: MessageKey) -> Option<(&str, &Note)> {
        let mut candidates = self.stored.iter().filter(|(_, (text, _))| *text == key);
        let (id, (_, note)) = candidates.next()?;
        candidates.next().is_none().then_some((id.as_str(), note))
    }
}

/// Every session's notes, in the shape the file holds them.
type AllNotes = BTreeMap<String, SessionNotes>;

/// The notes of every session, and the file they come from.
#[derive(Debug)]
pub struct NoteStore {
    /// The file notes are read from and written to; `None` keeps them in memory
    /// only, which is what an app built outside a real terminal uses.
    path: Option<PathBuf>,
    loaded: bool,
    /// The file holds a schema this build must not overwrite.
    foreign: bool,
    sessions: AllNotes,
    /// Sessions changed here and not yet written; a save merges exactly these.
    dirty: BTreeSet<String>,
}

impl NoteStore {
    /// A store that never touches the disk.
    pub fn memory() -> Self {
        Self {
            path: None,
            loaded: true,
            foreign: false,
            sessions: BTreeMap::new(),
            dirty: BTreeSet::new(),
        }
    }

    /// A store backed by `path`, read on first use.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            loaded: false,
            foreign: false,
            sessions: BTreeMap::new(),
            dirty: BTreeSet::new(),
        }
    }

    /// The file the client reads when it owns the terminal.
    pub fn default_path() -> PathBuf {
        horon_agent_sdk::user_horon_agent_home().join("tui-notes.json")
    }

    /// The store this client owns, backed by [`Self::default_path`].
    ///
    /// It is opened only where the client owns the terminal, so a test or an
    /// embedded `App` never writes to `$HORON_HOME`; the notes are read there
    /// so a replay's notes are on screen with the conversation they describe.
    pub fn owned() -> Self {
        Self::at(Self::default_path())
    }

    /// Read the file once, reporting anything the reader should know.
    ///
    /// A missing file is the ordinary first run. A file that is not this
    /// schema is moved aside so the notes in it are not silently destroyed by
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
                    "cannot read {}: {error}; notes are not saved this run",
                    path.display()
                ));
                return warnings;
            }
        };
        match parse(&text) {
            Ok(Parsed::Notes(sessions, malformed)) => {
                self.sessions = sessions;
                if malformed > 0 {
                    warnings.push(format!(
                        "{} held {malformed} unreadable note(s); they were skipped",
                        path.display()
                    ));
                }
            }
            Ok(Parsed::Newer(version)) => {
                self.foreign = true;
                warnings.push(format!(
                    "{} uses schema version {version}, which this build does not write; notes are left alone",
                    path.display()
                ));
            }
            Err(reason) => {
                self.set_aside(&path, &reason, &mut warnings);
            }
        }
        if let Some(warning) = tighten(&path) {
            warnings.push(warning);
        }
        warnings
    }

    /// The note on one message, if any.
    pub fn note(&self, session: &str, key: MessageKey) -> Option<&str> {
        self.note_record(session, key, None).map(|note| note.text.as_str())
    }

    /// The whole note record on one message, with the stored message it was
    /// written on.
    pub fn note_record(&self, session: &str, key: MessageKey, id: Option<&str>) -> Option<&Note> {
        let notes = self.sessions.get(session)?;
        if let Some(id) = id {
            return notes
                .stored
                .get(id)
                .map(|(_, note)| note)
                .or_else(|| notes.by_text.get(&key));
        }
        notes
            .by_text
            .get(&key)
            .or_else(|| notes.stored_fallback(key).map(|(_, note)| note))
    }

    /// Every note of one session, ordered by content identity.
    pub fn notes(&self, session: &str) -> impl Iterator<Item = (MessageKey, &Note)> {
        self.sessions.get(session).into_iter().flat_map(|notes| notes.iter())
    }

    /// The sessions holding at least one note.
    pub fn session_ids(&self) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|(_, notes)| !notes.is_empty())
            .map(|(session, _)| session.clone())
            .collect()
    }

    /// How many notes one session holds.
    pub fn count(&self, session: &str) -> usize {
        self.sessions.get(session).map_or(0, |notes| notes.len())
    }

    /// Attach a note to a message, or replace the one already there.
    ///
    /// The text is normalised to one line and capped at [`MAX_NOTE_CHARS`]: a
    /// note is drawn beside a message, so a multi-paragraph note would push
    /// the conversation it is about off the screen.
    pub fn set(&mut self, session: &str, key: MessageKey, address: Address, text: &str) -> Result<(), NoteError> {
        self.set_stored(session, key, address, None, text)
    }

    /// [`Self::set`], also recording the stored message the note was written on.
    pub fn set_stored(
        &mut self,
        session: &str,
        key: MessageKey,
        address: Address,
        stored_id: Option<&str>,
        text: &str,
    ) -> Result<(), NoteError> {
        let session = session.trim();
        if session.is_empty() {
            return Err(NoteError::NoSession);
        }
        let text = normalise(text);
        if text.is_empty() {
            return Err(NoteError::Empty);
        }
        let notes = self.sessions.entry(session.to_string()).or_default();
        if stored_id.is_some() {
            notes.by_text.remove(&key);
        }
        let stored_id = stored_id.map(str::to_owned).or_else(|| {
            if notes.by_text.contains_key(&key) {
                return None;
            }
            notes.stored_fallback(key).map(|(id, _)| id.to_owned())
        });
        notes.insert(
            key,
            Note {
                text,
                address,
                updated_ms: now_ms(),
                stored_id,
            },
        );
        self.dirty.insert(session.to_string());
        Ok(())
    }

    /// Remove the note on one message, answering whether there was one.
    pub fn remove(&mut self, session: &str, key: MessageKey, id: Option<&str>) -> bool {
        let Some(notes) = self.sessions.get_mut(session) else {
            return false;
        };
        let removed = if let Some(id) = id {
            notes.stored.remove(id).is_some() || notes.by_text.remove(&key).is_some()
        } else if notes.by_text.remove(&key).is_some() {
            true
        } else if let Some((id, _)) = notes.stored_fallback(key) {
            let id = id.to_owned();
            notes.stored.remove(&id).is_some()
        } else {
            false
        };
        if !removed {
            return false;
        }
        self.tidy(session);
        true
    }

    /// Remove a note by the address it was written at.
    ///
    /// This is how a note whose message the conversation no longer has is
    /// cleaned up: the content identity cannot be computed for a message that
    /// is gone, but the address it was stored at still names it.
    pub fn remove_at(&mut self, session: &str, address: Address) -> bool {
        let Some(notes) = self.sessions.get_mut(session) else {
            return false;
        };
        let Some((key, id)) = notes
            .iter()
            .find(|(_, note)| note.address == address)
            .map(|(key, note)| (key, note.stored_id.clone()))
        else {
            return false;
        };
        let removed = if let Some(id) = id {
            notes.stored.remove(&id).is_some()
        } else {
            notes.by_text.remove(&key).is_some()
        };
        if removed {
            self.tidy(session);
        }
        removed
    }

    /// Drop an empty session and mark the change for the next save.
    fn tidy(&mut self, session: &str) {
        if self.sessions.get(session).is_some_and(|notes| notes.is_empty()) {
            self.sessions.remove(session);
        }
        self.dirty.insert(session.to_string());
    }

    /// Drop every note of `session`, answering how many were removed.
    fn forget_session(&mut self, session: &str) -> usize {
        let Some(notes) = self.sessions.remove(session) else {
            return 0;
        };
        self.dirty.insert(session.to_string());
        notes.len()
    }

    /// Drop the notes of the sessions named, for sessions the harness no longer
    /// stores. Answers the sessions and notes removed.
    pub fn forget_sessions(&mut self, gone: &[String]) -> (usize, usize) {
        let mut sessions = 0;
        let mut notes = 0;
        for session in gone {
            let removed = self.forget_session(session);
            if removed > 0 {
                sessions += 1;
                notes += removed;
            }
        }
        (sessions, notes)
    }

    /// Write the sessions changed here, merging what another writer stored.
    ///
    /// Answers the warnings a reader should see. The lock keeps two clients
    /// from replacing each other's sessions, and the merge reads the file
    /// again under it because another client may have written since this one
    /// loaded. Errors leave the changes in memory: the next save tries again.
    pub fn save(&mut self) -> Result<Vec<String>, NoteFault> {
        if self.dirty.is_empty() {
            return Ok(Vec::new());
        }
        if self.foreign {
            return Err(NoteFault::new(
                "the notes file is not one this build writes; nothing was saved",
            ));
        }
        let Some(path) = self.path.clone() else {
            self.dirty.clear();
            return Ok(Vec::new());
        };
        let mut warnings = Vec::new();
        let _lock = SaveLock::acquire(&lock_path(&path))?;
        let mut merged = self.read_for_merge(&path, &mut warnings)?;
        let changed: Vec<String> = self.dirty.iter().cloned().collect();
        for session in changed {
            match self.sessions.get(&session) {
                Some(notes) if !notes.is_empty() => {
                    merged.insert(session, notes.clone());
                }
                _ => {
                    merged.remove(&session);
                }
            }
        }
        write_atomic(&path, &merged)?;
        self.sessions = merged;
        self.dirty.clear();
        Ok(warnings)
    }

    /// The file as another writer left it, read under the save lock.
    fn read_for_merge(&mut self, path: &Path, warnings: &mut Vec<String>) -> Result<AllNotes, NoteFault> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(error) => return Err(NoteFault::new(format!("cannot read {}: {error}", path.display()))),
        };
        match parse(&text) {
            Ok(Parsed::Notes(sessions, _)) => Ok(sessions),
            Ok(Parsed::Newer(version)) => Err(NoteFault::new(format!(
                "{} uses schema version {version}; nothing was saved",
                path.display()
            ))),
            Err(reason) => {
                self.set_aside(path, &reason, warnings);
                Ok(BTreeMap::new())
            }
        }
    }

    /// Move an unreadable file out of the way so the notes in it survive.
    fn set_aside(&mut self, path: &Path, reason: &NoteFault, warnings: &mut Vec<String>) {
        let aside = corrupt_path(path);
        if let Some(error) = crate::app::state_file::discard(&aside) {
            self.foreign = true;
            warnings.push(format!(
                "{} is not readable ({reason}) and {}; notes are not saved this run",
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
                    "{} is not readable ({reason}) and could not be moved aside: {error}; notes are not saved this run",
                    path.display()
                ));
            }
        }
    }
}

/// One message that can hold a note, with both of its names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Keyed {
    /// What the note is stored under.
    pub(crate) key: MessageKey,
    /// What the reader types to reach it.
    pub(crate) address: Address,
    /// The stored message the row came from, when the harness named one.
    pub(crate) stored_id: Option<String>,
}

/// The message identity and address of each item that can hold a note.
///
/// Only committed user and assistant messages are addressable: they are the
/// rows a replay restores, so both names describe a message the reader can
/// still reach after a reattach. `stored` holds the harness's stored message id
/// per item, aligned with `items`; it is empty or short whenever the caller has
/// none, and a row without one is keyed exactly as before.
pub(crate) fn keyed(items: &[Item], stored: &[Option<String>]) -> Vec<Option<Keyed>> {
    let mut out = Vec::with_capacity(items.len());
    walk(items, |index, key, address| {
        let (Some(key), Some(address)) = (key, address) else {
            out.push(None);
            return;
        };
        out.push(Some(Keyed {
            key,
            address,
            stored_id: stored.get(index).cloned().flatten(),
        }));
    });
    out
}

/// Read every row's identity and address, in conversation order.
///
/// Both listings are built from this one walk: the pane needs only the
/// identities and the commands need both, so neither repeats the fingerprint
/// and occurrence rule, and the frame path never copies a stored message id it
/// does not read.
fn walk(items: &[Item], mut visit: impl FnMut(usize, Option<MessageKey>, Option<Address>)) {
    let mut ordinals = [0usize; 2];
    // Counted per role and text rather than per index: a repeated answer is the
    // second occurrence of its own key, wherever it sits. The map is read one
    // entry at a time, so its iteration order never reaches the output.
    let mut occurrences: HashMap<(Role, u64), usize> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        let Some((role, text)) = addressable(item) else {
            visit(index, None, None);
            continue;
        };
        ordinals[role.index()] += 1;
        let fingerprint = fingerprint(text);
        let seen = occurrences.entry((role, fingerprint)).or_insert(0);
        *seen += 1;
        visit(
            index,
            Some(MessageKey {
                role,
                fingerprint,
                occurrence: *seen,
            }),
            Some(Address {
                role,
                ordinal: ordinals[role.index()],
            }),
        );
    }
}

/// The message a note can be attached to, or `None` for any other row.
fn addressable(item: &Item) -> Option<(Role, &str)> {
    match item {
        Item::User(text) => Some((Role::User, text)),
        Item::Assistant(text) => Some((Role::Assistant, text)),
        _ => None,
    }
}

/// The note of the block at `index`, if the session and message have one.
///
/// A note that records a stored message is drawn only under a row that names
/// the same one: the text key alone would also match an identical answer in
/// another place, and a note belongs to the answer it was written on. When
/// either side has no id the text key decides, which is the whole rule for a
/// harness that reports none.
pub(crate) fn note_at<'a>(
    store: &'a NoteStore,
    session: Option<&str>,
    keys: Option<&[Option<MessageKey>]>,
    ids: Option<&[Option<String>]>,
    index: usize,
) -> Option<&'a str> {
    let session = session?;
    let key = keys?.get(index).copied().flatten()?;
    let current = ids.and_then(|ids| ids.get(index)).and_then(Option::as_deref);
    store.note_record(session, key, current).map(|note| note.text.as_str())
}

/// The notes one frame can hang under the blocks it draws.
///
/// A conversation with no notes is the ordinary case, and it should not pay for
/// a fingerprint per item on every frame, so the message keys are built only
/// when the session has a note; one lookup against the session's notes says
/// whether the keys are needed at all. The pane then asks this index once per
/// block instead of carrying the store, the session and the keys through its
/// own code.
pub(crate) struct Frame<'a> {
    store: &'a NoteStore,
    session: Option<&'a str>,
    keys: Option<Cow<'a, [Option<MessageKey>]>>,
    ids: &'a [Option<String>],
}

impl<'a> Frame<'a> {
    /// The notes of the app's current session.
    pub(crate) fn new(app: &'a App) -> Self {
        Self::from_parts(&app.notes, app.session_id.as_deref(), &app.transcript, &app.note_keys)
    }

    /// The notes of `session` over `transcript`.
    ///
    /// The measuring walk has the store, the session and the transcript apart
    /// rather than through an `App`, because it holds those fields of one
    /// borrowed across the scroll's own mutable borrow.
    pub(crate) fn from_parts(
        store: &'a NoteStore,
        session: Option<&'a str>,
        transcript: &'a Transcript,
        cache: &'a KeyCache,
    ) -> Self {
        let keys = session
            .filter(|session| store.count(session) > 0)
            .map(|_| cache.keys_for(transcript));
        Self {
            store,
            session,
            keys,
            ids: transcript.stored_ids(),
        }
    }

    /// The note of the block at `index`, if its message has one.
    pub(crate) fn at(&self, index: usize) -> Option<&'a str> {
        note_at(self.store, self.session, self.keys.as_deref(), Some(self.ids), index)
    }
}

/// The message keys of the conversation the last frame drew.
///
/// The keys depend on the user and assistant rows alone, and those change only
/// when the transcript's item generation moves, so a frame that draws the same
/// conversation again (a scroll step, a redraw, a spinner tick) reuses them
/// instead of fingerprinting every message once more.
#[derive(Debug, Default)]
pub(crate) struct KeyCache {
    generation: Option<u64>,
    keys: Vec<Option<MessageKey>>,
}

impl KeyCache {
    /// Bring the kept keys up to date with `transcript`.
    pub(crate) fn refresh(&mut self, transcript: &Transcript) {
        let generation = transcript.items_generation();
        if self.generation != Some(generation) {
            self.keys = message_keys(transcript.items());
            self.generation = Some(generation);
        }
        debug_assert_eq!(
            self.keys,
            message_keys(transcript.items()),
            "an unchanged item generation must leave the message keys unchanged"
        );
    }

    /// The keys of `transcript`: the kept ones while they are current, and a
    /// fresh walk for a frame drawn before [`Self::refresh`] caught up.
    fn keys_for<'k>(&'k self, transcript: &Transcript) -> Cow<'k, [Option<MessageKey>]> {
        if self.generation == Some(transcript.items_generation()) {
            Cow::Borrowed(&self.keys)
        } else {
            Cow::Owned(message_keys(transcript.items()))
        }
    }
}

/// The identity of each row that can hold a note, aligned with `items`.
///
/// The frame asks only for these, so it takes the shared walk's identities and
/// drops the addresses: no stored message id is copied to answer a draw.
fn message_keys(items: &[Item]) -> Vec<Option<MessageKey>> {
    let mut keys = Vec::with_capacity(items.len());
    walk(items, |_, key, _| keys.push(key));
    keys
}

/// The text a note's listing quotes for one message.
pub(crate) fn excerpt(item: &Item) -> Option<&str> {
    match item {
        Item::User(text) | Item::Assistant(text) => Some(text),
        _ => None,
    }
}

/// One note line for a listing, cut to `chars` characters.
pub(crate) fn summary(text: &str, chars: usize) -> String {
    let mut out: String = text.chars().take(chars).collect();
    if text.chars().count() > chars {
        out.push('…');
    }
    out
}

/// The content fingerprint a note is stored under.
///
/// FNV-1a over the text with runs of whitespace collapsed to one space and the
/// ends trimmed, so a row that a replay re-wrapped still matches while a
/// different sentence does not. The same function answers for the live item and
/// for the stored row, which is what keeps a note on its own message.
fn fingerprint(text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut pending_space = false;
    let mut started = false;
    let mut buffer = [0u8; 4];
    for character in text.chars() {
        if character.is_whitespace() {
            if started {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            hash ^= u64::from(b' ');
            hash = hash.wrapping_mul(PRIME);
            pending_space = false;
        }
        for byte in character.encode_utf8(&mut buffer).bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
        started = true;
    }
    hash
}

/// Normalise a note to one line and the length limit.
fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_NOTE_CHARS));
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_control() || character.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(character);
    }
    out.trim_end().chars().take(MAX_NOTE_CHARS).collect()
}

/// Milliseconds since the Unix epoch, or zero on a clock before it.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// What reading the file produced.
enum Parsed {
    /// The notes, and how many entries were skipped as unreadable.
    Notes(AllNotes, usize),
    /// The schema version, from a build that writes a newer one.
    Newer(u64),
}

/// Read the file's text into notes.
fn parse(text: &str) -> Result<Parsed, NoteFault> {
    let value: Value = serde_json::from_str(text).map_err(|error| NoteFault::new(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| NoteFault::new("the file is not a JSON object"))?;
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| NoteFault::new("the file names no schema version"))?;
    if version > VERSION {
        return Ok(Parsed::Newer(version));
    }
    if version < VERSION {
        return Err(NoteFault::new(format!("schema version {version} is no longer read")));
    }
    let mut sessions = BTreeMap::new();
    let mut malformed = 0;
    let Some(stored) = object.get("sessions") else {
        return Ok(Parsed::Notes(sessions, malformed));
    };
    let stored = stored
        .as_object()
        .ok_or_else(|| NoteFault::new("`sessions` is not an object"))?;
    for (session, holding) in stored {
        if session.trim().is_empty() {
            malformed += 1;
            continue;
        }
        let Some(items) = holding.get("items").and_then(Value::as_object) else {
            malformed += 1;
            continue;
        };
        let mut notes = SessionNotes::default();
        for (token, entry) in items {
            let key = entry.get("fingerprint").and_then(Value::as_str).unwrap_or(token);
            let Some(key) = MessageKey::parse_token(key) else {
                malformed += 1;
                continue;
            };
            let Some(text) = entry.get("text").and_then(Value::as_str) else {
                malformed += 1;
                continue;
            };
            let Some(address) = entry.get("address").and_then(Value::as_str).and_then(Address::parse) else {
                malformed += 1;
                continue;
            };
            let updated_ms = entry.get("updated_ms").and_then(Value::as_u64).unwrap_or(0);
            // Absent on every note written before stored ids were recorded,
            // and on notes written from a harness that reports none.
            let stored_id = entry.get("stored_id").and_then(Value::as_str).map(str::to_string);
            notes.insert(
                key,
                Note {
                    text: text.to_string(),
                    address,
                    updated_ms,
                    stored_id,
                },
            );
        }
        if !notes.is_empty() {
            sessions.insert(session.clone(), notes);
        }
    }
    Ok(Parsed::Notes(sessions, malformed))
}

/// The file's contents for a set of notes.
fn to_value(sessions: &AllNotes) -> Value {
    let mut stored = Map::new();
    for (session, notes) in sessions {
        let mut items = Map::new();
        for (key, note) in notes.iter() {
            let mut entry = Map::new();
            entry.insert("text".to_string(), Value::String(note.text.clone()));
            entry.insert("address".to_string(), Value::String(note.address.to_string()));
            entry.insert("updated_ms".to_string(), Value::from(note.updated_ms));
            let token = if let Some(stored_id) = &note.stored_id {
                entry.insert("stored_id".to_string(), Value::String(stored_id.clone()));
                entry.insert("fingerprint".to_string(), Value::String(key.token()));
                format!("id:{stored_id}")
            } else {
                key.token()
            };
            items.insert(token, Value::Object(entry));
        }
        let mut holding = Map::new();
        holding.insert("items".to_string(), Value::Object(items));
        stored.insert(session.clone(), Value::Object(holding));
    }
    let mut root = Map::new();
    root.insert("version".to_string(), Value::from(VERSION));
    root.insert("sessions".to_string(), Value::Object(stored));
    Value::Object(root)
}

/// `<path>.corrupt`: where an unreadable notes file is kept.
fn corrupt_path(path: &Path) -> PathBuf {
    crate::app::state_file::corrupt_path(path, "tui-notes")
}

/// `<path>.lock`: the file that keeps two savers apart.
fn lock_path(path: &Path) -> PathBuf {
    crate::app::state_file::lock_path(path, "tui-notes")
}

/// Write `sessions` beside `path` and rename it over the file.
///
/// The temporary file is created owner-only and flushed before the rename, which
/// is [`crate::app::state_file::write_atomic`]'s job: the notes file shares its
/// durability rule with the pins file, so the sequence is written once.
fn write_atomic(path: &Path, sessions: &AllNotes) -> Result<(), NoteFault> {
    let bytes = serde_json::to_vec_pretty(&to_value(sessions)).map_err(|error| NoteFault::new(error.to_string()))?;
    crate::app::state_file::write_atomic(path, &bytes, "tui-notes").map_err(NoteFault::new)
}

/// A held save lock, released when it is dropped.
struct SaveLock {
    path: PathBuf,
}

impl SaveLock {
    /// Take the lock, waiting briefly for a live writer and breaking a stale one.
    fn acquire(path: &Path) -> Result<Self, NoteFault> {
        for attempt in 0..LOCK_TRIES {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            horon_agent_sdk::private_fs::harden_credential_open(&mut options, path)
                .map_err(|error| NoteFault::new(format!("cannot prepare {}: {error}", path.display())))?;
            match options.open(path) {
                Ok(file) => {
                    horon_agent_sdk::private_fs::set_permissions_owner_only_file(&file).map_err(|error| {
                        let error = format!("cannot make {} private: {error}", path.display());
                        NoteFault::new(match crate::app::state_file::discard(path) {
                            Some(cleanup) => format!("{error}; {cleanup}"),
                            None => error,
                        })
                    })?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if stale(path) {
                        if let Some(cleanup) = crate::app::state_file::discard(path) {
                            return Err(NoteFault::new(format!("cannot break the stale notes lock: {cleanup}")));
                        }
                        continue;
                    }
                    if attempt + 1 == LOCK_TRIES {
                        return Err(NoteFault::new(format!(
                            "another horon-tui is writing {}; the note is kept for the next save",
                            path.display()
                        )));
                    }
                    std::thread::sleep(LOCK_PAUSE);
                }
                Err(error) => {
                    return Err(NoteFault::new(format!("cannot lock {}: {error}", path.display())));
                }
            }
        }
        Err(NoteFault::new(format!(
            "cannot lock {}: too many writers",
            path.display()
        )))
    }
}

impl Drop for SaveLock {
    fn drop(&mut self) {
        // A lock that cannot be removed is broken by the next writer once it is
        // older than `LOCK_STALE`, so a destructor has no reader to report to.
        let _unlocked = fs::remove_file(&self.path);
    }
}

/// Whether a lock file is old enough to treat its owner as gone.
fn stale(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age > LOCK_STALE,
        // A clock that cannot place the lock's time leaves it to its owner.
        Err(_) => false,
    }
}

/// Narrow an existing notes file to its owner, reporting when it was not.
fn tighten(path: &Path) -> Option<String> {
    let Ok(metadata) = fs::metadata(path) else {
        return None;
    };
    if horon_agent_sdk::private_fs::is_private_file(&metadata) {
        return None;
    }
    let before = horon_agent_sdk::private_fs::mode_bits(&metadata)
        .map_or_else(|| "wider than owner-only".to_string(), |mode| format!("{mode:o}"));
    match horon_agent_sdk::private_fs::set_file_permissions_owner_only(path) {
        Ok(()) => Some(format!(
            "{} was readable by other users ({before}); it is now owner-only",
            path.display()
        )),
        Err(error) => Some(format!(
            "{} is readable by other users ({before}) and could not be narrowed: {error}",
            path.display()
        )),
    }
}

#[cfg(test)]
#[path = "notes_tests.rs"]
mod notes_tests;

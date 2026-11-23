//! Opt-in local recording of the conversation, and timed playback of one.
//!
//! A recording is a JSON Lines file under `$HORON_HOME/recordings` unless the
//! user names another path. The first line is the format marker; every later
//! line is one semantic event with the milliseconds since recording started.
//!
//! The boundary is deliberate. A recording keeps what the conversation showed —
//! a message the user sent, an answer that was published, that a tool call ran
//! and whether it failed, a warning — and nothing else. Approval answers, the
//! arguments of a parked call, an unsent draft and a tool's body never reach the
//! file, so a recording can be read or shared without carrying a secret or an
//! output the user never chose to keep. Keystrokes are not recorded either: a
//! replay is a transcript played back, not a terminal session, and replaying a
//! keystroke would run it again.
//!
//! A replay is a [`Player`] over a loaded [`Recording`]: the caller feeds it
//! elapsed milliseconds, it answers the events that became due, and it holds a
//! paused flag, a speed and a seek position.

use std::borrow::Cow;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Largest recording this client writes, in bytes.
///
/// A recording is a cursory capture of a conversation, not an archive: the cap
/// bounds what a long turn full of answers can cost on disk, and a file that
/// reaches it stops rather than growing without a limit.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The directory recordings are written to and read from by default.
pub fn recordings_dir() -> PathBuf {
    horon_agent_sdk::user_horon_agent_home().join("recordings")
}

/// A fresh path under [`recordings_dir`], named by the clock.
///
/// Two recordings started in the same millisecond would collide; [`Recorder`]
/// refuses an existing file, so the second start fails loudly instead of
/// overwriting the first.
pub fn new_recording_path() -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    recordings_dir().join(format!("tui-{millis}.jsonl"))
}

/// The newest `.jsonl` file under [`recordings_dir`], if there is one.
pub fn newest_recording_path() -> Option<PathBuf> {
    let entries = match fs::read_dir(recordings_dir()) {
        Ok(entries) => entries,
        // No directory means no recordings; the caller says so with the path.
        Err(_) => return None,
    };
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(at, _)| modified > *at) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// A path typed on the command line, with a leading `~/` resolved.
pub fn expand_path(argument: &str) -> PathBuf {
    if let Some(rest) = argument.strip_prefix("~/")
        && let Some(home) = home_directory()
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(argument)
}

/// The home directory `~/` is resolved against.
fn home_directory() -> Option<String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| home.to_string_lossy().into_owned())
}

/// What one recorded line says happened, without a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A message the user sent, as the line they typed.
    User(String),
    /// An answer the conversation published.
    Assistant(String),
    /// A warning line the client wrote.
    ///
    /// The line itself is not kept: a warning can name a path or echo a
    /// harness message, and a recording is read on its own. A replay draws that
    /// a warning was shown, which is the part the conversation carried.
    Warning,
    /// A tool call that ran: its name and the outcome, never its arguments or
    /// output.
    Tool { name: String, outcome: Outcome },
}

/// How a recorded call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It ran and reported a result.
    Done,
    /// It ran and reported an error.
    Failed,
}

impl Outcome {
    /// The word the file stores for this outcome.
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// The outcome a stored word names, when it names one.
    fn parse(word: &str) -> Option<Self> {
        match word {
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Why the recorder stopped taking events.
#[derive(Debug)]
pub enum RecordError {
    /// The file could not be made.
    Create(String),
    /// The file reached its byte limit; nothing more was written.
    Full(u64),
    /// The file could not be written.
    Io(String),
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create(message) | Self::Io(message) => formatter.write_str(message),
            Self::Full(limit) => write!(formatter, "the file reached its {limit} byte limit"),
        }
    }
}

/// Why a file could not be read back as a recording.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplayError {
    /// The file could not be read.
    Io(String),
    /// The file is empty.
    Empty,
    /// The first line is not this format's marker.
    NotARecording,
    /// A line is not an event this build understands.
    Line { line: usize, problem: String },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(message) => formatter.write_str(message),
            Self::Empty => write!(formatter, "the file is empty"),
            Self::NotARecording => write!(formatter, "the file is not a horon-tui recording"),
            Self::Line { line, problem } => write!(formatter, "line {line}: {problem}"),
        }
    }
}

/// One event and when it happened, in milliseconds since recording started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timed {
    pub at_ms: u64,
    pub event: Event,
}

/// A recording read back from disk, validated.
#[derive(Debug, Clone)]
pub struct Recording {
    /// The events in the order they were written.
    pub events: Vec<Timed>,
    /// When the last event happened.
    pub duration_ms: u64,
}

impl Recording {
    /// Read and validate a recording file.
    ///
    /// Validation is strict on purpose: a line this build cannot read is an
    /// error naming it, not an event silently dropped, so a truncated or
    /// hand-edited file fails where the user can see it rather than playing
    /// back a different conversation.
    pub fn load(path: &Path) -> Result<Self, ReplayError> {
        let metadata = fs::metadata(path).map_err(|error| ReplayError::Io(format!("{}: {error}", path.display())))?;
        if metadata.len() > MAX_BYTES {
            return Err(ReplayError::Io(format!(
                "{}: the file is larger than the {MAX_BYTES} byte recording limit",
                path.display()
            )));
        }
        let text = fs::read_to_string(path).map_err(|error| ReplayError::Io(format!("{}: {error}", path.display())))?;
        let mut lines = text.lines().enumerate();
        let Some((_, header)) = lines.next() else {
            return Err(ReplayError::Empty);
        };
        let header: serde_json::Value = serde_json::from_str(header).map_err(|error| ReplayError::Line {
            line: 1,
            problem: format!("the header is not JSON: {error}"),
        })?;
        if header.get("format").and_then(|value| value.as_str()) != Some(FORMAT) {
            return Err(ReplayError::NotARecording);
        }
        if header.get("version").and_then(|value| value.as_u64()) != Some(VERSION) {
            return Err(ReplayError::Line {
                line: 1,
                problem: format!("this build reads version {VERSION} recordings only"),
            });
        }
        let mut events: Vec<Timed> = Vec::new();
        let mut last_at = 0;
        for (index, line) in lines {
            let number = index + 1;
            if line.trim().is_empty() {
                continue;
            }
            let mut value: serde_json::Value = serde_json::from_str(line).map_err(|error| ReplayError::Line {
                line: number,
                problem: format!("not JSON: {error}"),
            })?;
            let at_ms = value
                .get("at")
                .and_then(|value| value.as_u64())
                .ok_or_else(|| ReplayError::Line {
                    line: number,
                    problem: "no `at` in milliseconds".to_string(),
                })?;
            if at_ms < last_at {
                return Err(ReplayError::Line {
                    line: number,
                    problem: "the timestamp is earlier than the line before it".to_string(),
                });
            }
            let event = decode(&mut value).map_err(|problem| ReplayError::Line {
                line: number,
                problem: problem.0,
            })?;
            last_at = at_ms;
            events.push(Timed { at_ms, event });
        }
        Ok(Self {
            duration_ms: last_at,
            events,
        })
    }
}

/// The first line's format name and version.
const FORMAT: &str = "horon-tui-recording";
const VERSION: u64 = 1;

/// One event as a file line.
///
/// Text is scrubbed of terminal control sequences here, at the one place every
/// event is written: a pasted escape sequence is not conversation, and a file is
/// read back and drawn long after the terminal that produced it is gone.
fn encode_line(buf: &mut Vec<u8>, at_ms: u64, event: &Event) -> std::io::Result<()> {
    buf.clear();
    match event {
        Event::User(text) => {
            write!(buf, "{{\"at\":{at_ms},\"kind\":\"user\",\"text\":")?;
            let clean = scrub_cow(text);
            serde_json::to_writer(&mut *buf, clean.as_ref()).map_err(std::io::Error::other)?;
            buf.extend_from_slice(b"}\n");
        }
        Event::Assistant(text) => {
            write!(buf, "{{\"at\":{at_ms},\"kind\":\"assistant\",\"text\":")?;
            let clean = scrub_cow(text);
            serde_json::to_writer(&mut *buf, clean.as_ref()).map_err(std::io::Error::other)?;
            buf.extend_from_slice(b"}\n");
        }
        Event::Warning => {
            writeln!(buf, "{{\"at\":{at_ms},\"kind\":\"warning\"}}")?;
        }
        Event::Tool { name, outcome } => {
            write!(buf, "{{\"at\":{at_ms},\"kind\":\"tool\",\"name\":")?;
            let clean = scrub_cow(name);
            serde_json::to_writer(&mut *buf, clean.as_ref()).map_err(std::io::Error::other)?;
            writeln!(buf, ",\"outcome\":\"{}\"}}", outcome.as_str())?;
        }
    }
    Ok(())
}

/// Whether `text` contains any control character that [`scrub`] would remove.
fn needs_scrub(text: &str) -> bool {
    if text.is_ascii() {
        text.bytes()
            .any(|byte| (byte < 0x20 && byte != b'\n' && byte != b'\t') || byte == 0x7f)
    } else {
        text.chars().any(|ch| ch != '\n' && ch != '\t' && ch.is_control())
    }
}

/// Borrow `text` when it already carries no control sequences; otherwise scrub it.
fn scrub_cow(text: &str) -> Cow<'_, str> {
    if needs_scrub(text) {
        Cow::Owned(scrub(text))
    } else {
        Cow::Borrowed(text)
    }
}

/// The printable text of `text`, with terminal control sequences removed.
///
/// A CSI sequence runs to its final byte, an OSC string to BEL or ESC `\`, and
/// any other escape ends with the character after it. Control characters that
/// are left are dropped, except the line break and the tab, which are text.
fn scrub(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if ch == '\n' || ch == '\t' || !ch.is_control() {
            clean.push(ch);
        }
    }
    clean
}

/// Why one line was not an event this build understands.
struct Problem(String);

impl Problem {
    fn new(problem: impl Into<String>) -> Self {
        Self(problem.into())
    }
}

/// One file line as an event, or the reason it is not one.
fn take_scrubbed_text(value: &mut serde_json::Value, field: &str) -> Result<String, Problem> {
    let Some(serde_json::Value::String(text)) = value.as_object_mut().and_then(|map| map.remove(field)) else {
        return Err(Problem::new(format!("no `{field}` text")));
    };
    if needs_scrub(&text) { Ok(scrub(&text)) } else { Ok(text) }
}

fn decode(value: &mut serde_json::Value) -> Result<Event, Problem> {
    enum Kind {
        User,
        Assistant,
        Warning,
        Tool,
    }
    let kind = match value.get("kind").and_then(|value| value.as_str()) {
        Some("user") => Kind::User,
        Some("assistant") => Kind::Assistant,
        Some("warning") => Kind::Warning,
        Some("tool") => Kind::Tool,
        Some(other) => return Err(Problem::new(format!("unknown event kind `{other}`"))),
        None => return Err(Problem::new("no `kind`")),
    };
    match kind {
        Kind::User => Ok(Event::User(take_scrubbed_text(value, "text")?)),
        Kind::Assistant => Ok(Event::Assistant(take_scrubbed_text(value, "text")?)),
        // The line itself is not kept, so an event carrying one adds nothing.
        Kind::Warning => Ok(Event::Warning),
        Kind::Tool => {
            let outcome = value
                .get("outcome")
                .and_then(|value| value.as_str())
                .and_then(Outcome::parse)
                .ok_or_else(|| Problem::new("a tool line needs outcome `done` or `failed`"))?;
            Ok(Event::Tool {
                name: take_scrubbed_text(value, "name")?,
                outcome,
            })
        }
    }
}

/// A recording being written.
#[derive(Debug)]
pub struct Recorder {
    path: PathBuf,
    file: fs::File,
    started: std::time::Instant,
    bytes: u64,
    limit: u64,
    events: usize,
    buf: Vec<u8>,
}

impl Recorder {
    /// Create a recording at `path`, keeping at most `limit` bytes.
    ///
    /// The file is made by the `tempfile` crate, which creates it for its owner
    /// alone (`0600` where the platform has file modes, an owner-only location
    /// where it has ACLs), and then moved to `path` without replacing anything:
    /// a name that already means something keeps meaning it. The directory is
    /// made the same way, so this client needs no operating-system branch of its
    /// own.
    pub fn create(path: &Path, limit: u64) -> Result<Self, RecordError> {
        let directory = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                ensure_private_dir(parent)
                    .map_err(|error| RecordError::Create(format!("{}: {error}", parent.display())))?;
                parent
            }
            _ => Path::new("."),
        };
        // Staged beside the target and moved with "do not replace", so a name
        // that is already taken fails the way `create_new` would.
        let staging = tempfile::Builder::new()
            .prefix(".recording-")
            .suffix(".tmp")
            .tempfile_in(directory)
            .map_err(|error| RecordError::Create(format!("{}: {error}", path.display())))?;
        let file = staging
            .persist_noclobber(path)
            .map_err(|error| RecordError::Create(format!("{}: {}", path.display(), error.error)))?;
        let mut recorder = Self {
            path: path.to_path_buf(),
            file,
            started: std::time::Instant::now(),
            bytes: 0,
            limit,
            events: 0,
            buf: Vec::with_capacity(256),
        };
        let header = serde_json::json!({"format": FORMAT, "version": VERSION}).to_string();
        recorder
            .write_line(&header)
            .map_err(|error| RecordError::Io(format!("{}: {error}", path.display())))?;
        Ok(recorder)
    }

    /// Append one event with the time since the recording started.
    ///
    /// A line that would cross the limit is not written; the caller stops
    /// recording and says so, so a truncated event never reaches the file.
    pub fn push(&mut self, event: &Event) -> Result<(), RecordError> {
        let at_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        encode_line(&mut self.buf, at_ms, event)
            .map_err(|error| RecordError::Io(format!("{}: {error}", self.path.display())))?;
        let needed = self.buf.len() as u64;
        if self.bytes.saturating_add(needed) > self.limit {
            return Err(RecordError::Full(self.limit));
        }
        self.file
            .write_all(&self.buf)
            .map_err(|error| RecordError::Io(format!("{}: {error}", self.path.display())))?;
        self.bytes += needed;
        self.events += 1;
        Ok(())
    }

    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.bytes += line.len() as u64 + 1;
        Ok(())
    }

    /// Where the recording is being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many events have been written.
    pub fn events(&self) -> usize {
        self.events
    }
}

/// Make `path` a directory only its owner may enter.
///
/// `tempfile` creates a scratch directory for its owner alone, and it is
/// renamed into place: the caller states what it needs and the crate picks the
/// primitive, so no mode bits and no operating-system branch appear here. A
/// directory that already exists is left exactly as it is.
fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;
    // The staging directory is a `TempDir`: it removes itself if the rename
    // fails, so no cleanup branch appears here. After a successful rename its
    // own name is gone, and its removal finds nothing to do.
    let staging = tempfile::Builder::new().prefix(".recordings-").tempdir_in(parent)?;
    match fs::rename(staging.path(), path) {
        Ok(()) => Ok(()),
        // Another client may have created the same directory meanwhile.
        Err(_) if path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

/// Speeds the pause key steps through.
const SPEEDS: [f64; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];

/// A recording being played back.
///
/// Playback is driven by elapsed milliseconds rather than by a thread of its
/// own: the caller's frame loop asks how much real time passed, and the player
/// answers the events that became due. That keeps a replay deterministic in a
/// test and on screen at the same time.
#[derive(Debug)]
pub struct Player {
    events: Vec<Timed>,
    next: usize,
    position_ms: u64,
    duration_ms: u64,
    speed: usize,
    carry_ms: f64,
    paused: bool,
}

impl Player {
    /// A player at the start of `recording`.
    pub fn new(recording: Recording) -> Self {
        Self {
            events: recording.events,
            next: 0,
            position_ms: 0,
            duration_ms: recording.duration_ms,
            // Index 1 is 1x in `SPEEDS`.
            speed: 1,
            carry_ms: 0.0,
            paused: false,
        }
    }

    /// Advance by `real_ms` of wall clock and answer the events now due.
    ///
    /// A paused or finished player answers nothing, and the position never
    /// passes the end of the recording.
    pub fn advance(&mut self, real_ms: u64) -> Vec<Event> {
        if self.paused || self.is_finished() {
            return Vec::new();
        }
        self.carry_ms += real_ms as f64 * self.speed();
        let step = self.carry_ms.floor();
        self.carry_ms -= step;
        let gained = step.min(u64::MAX as f64) as u64;
        self.position_ms = self.position_ms.saturating_add(gained).min(self.duration_ms);
        self.due()
    }

    /// Jump to `target_ms` and answer every event up to it.
    ///
    /// The answer is the whole prefix, not the difference, so a caller can
    /// rebuild the screen from scratch and a seek backwards reads exactly as
    /// the recording did at that point. The target is clamped to the end.
    pub fn seek(&mut self, target_ms: u64) -> Vec<Event> {
        self.position_ms = target_ms.min(self.duration_ms);
        self.carry_ms = 0.0;
        self.next = 0;
        self.due()
    }

    fn due(&mut self) -> Vec<Event> {
        let mut due = Vec::new();
        while let Some(timed) = self.events.get(self.next) {
            if timed.at_ms > self.position_ms {
                break;
            }
            due.push(timed.event.clone());
            self.next += 1;
        }
        due
    }

    /// Turn playback on or off, answering the state now in force.
    pub fn toggle_pause(&mut self) -> bool {
        self.paused = !self.paused;
        self.paused
    }

    /// Whether playback is paused.
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Whether every event has been played and the position reached the end.
    pub fn is_finished(&self) -> bool {
        self.next >= self.events.len() && self.position_ms >= self.duration_ms
    }

    /// Where playback stands.
    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    /// The length of the whole recording.
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// How many events have been played.
    pub fn played(&self) -> usize {
        self.next
    }

    /// How many events the recording holds.
    pub fn total(&self) -> usize {
        self.events.len()
    }

    /// The current speed.
    pub fn speed(&self) -> f64 {
        SPEEDS[self.speed]
    }

    /// Step one speed faster, answering the speed now in force.
    pub fn speed_up(&mut self) -> f64 {
        self.speed = (self.speed + 1).min(SPEEDS.len() - 1);
        self.speed()
    }

    /// Step one speed slower, answering the speed now in force.
    pub fn speed_down(&mut self) -> f64 {
        self.speed = self.speed.saturating_sub(1);
        self.speed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorder_refuses_an_existing_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("one.jsonl");
        let first = Recorder::create(&path, MAX_BYTES).expect("first recording");
        assert_eq!(first.events(), 0);
        let error = Recorder::create(&path, MAX_BYTES).expect_err("a second recording overwrote the first");
        let shown = error.to_string();
        assert!(shown.contains("exists"), "unexpected error: {shown}");
    }

    #[test]
    fn a_recording_makes_its_own_directory_and_keeps_the_file_there() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("recordings").join("two.jsonl");
        let recorder = Recorder::create(&path, MAX_BYTES).expect("recording");
        assert!(path.is_file(), "the file was not written where it was named");
        assert_eq!(recorder.path(), path.as_path());
        // The staging name the crate used is gone: only the recording is left.
        let left: Vec<String> = std::fs::read_dir(directory.path().join("recordings"))
            .expect("list the directory")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["two.jsonl".to_string()]);
    }

    #[test]
    fn terminal_control_sequences_are_scrubbed_from_text() {
        assert_eq!(scrub("\u{1b}[31mred\u{1b}[0m and \u{7}bell"), "red and bell");
        assert_eq!(scrub("two\nlines\tkept"), "two\nlines\tkept");
        assert_eq!(scrub("\u{1b}]0;a title\u{7}text"), "text");
        assert_eq!(scrub_cow("한글\u{85}본문"), "한글본문");
        assert_eq!(scrub_cow("plain 한국어\nline\t2"), "plain 한국어\nline\t2");
        let mut line = Vec::new();
        encode_line(
            &mut line,
            42,
            &Event::Tool {
                name: "apply_patch".into(),
                outcome: Outcome::Done,
            },
        )
        .expect("encode tool line");
        assert_eq!(
            String::from_utf8(line).expect("utf8"),
            "{\"at\":42,\"kind\":\"tool\",\"name\":\"apply_patch\",\"outcome\":\"done\"}\n"
        );
    }

    #[test]
    fn a_speed_steps_through_the_ladder_and_stops_at_its_ends() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("speed.jsonl");
        let mut recorder = Recorder::create(&path, MAX_BYTES).expect("recording");
        recorder.push(&Event::User("hello".into())).expect("first event");
        let mut player = Player::new(Recording::load(&path).expect("load"));
        assert_eq!(player.speed(), 1.0);
        assert_eq!(player.speed_up(), 2.0);
        assert_eq!(player.speed_down(), 1.0);
        assert_eq!(player.speed_down(), 0.5);
        assert_eq!(player.speed_down(), 0.5);
    }
}

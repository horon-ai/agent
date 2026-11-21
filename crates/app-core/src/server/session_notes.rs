//! Bounded, per-session notes about the work a turn is doing.
//!
//! These notes feed the client's own status view: which lifecycle hooks fired
//! and which background work moved. They live here, outside the conversation,
//! and nothing reads them back into a request: they are facts about the turn,
//! not something the turn is told. Recording is per session and stays off until
//! that session asks for it, so a daemon nobody asked keeps no entry at all.
//!
//! One owner holds at most [`NOTE_CAP`] lines. A line that repeats the one
//! before it folds into that line's `repeats` count instead of taking another
//! slot, so a hook that fires on every tool call cannot push everything else
//! out. A distinct line that arrives after the cap evicts the oldest line and
//! is counted as dropped, which keeps
//! `recorded = retained repeats + repeated + dropped` true no matter how long
//! the session runs.

use serde::Serialize;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

/// Most note lines one session keeps.
pub(crate) const NOTE_CAP: usize = 24;

/// Longest note text kept, in characters after normalisation.
pub(crate) const NOTE_TEXT_MAX: usize = 160;

/// The lifecycle hooks a status view reports, in the order the documentation
/// lists them.
const HOOK_NAMES: [&str; 9] = [
    "session_start",
    "session_end",
    "turn_start",
    "turn_end",
    "pre_tool",
    "post_tool",
    "pre_tool_transform",
    "post_tool_feedback",
    "pre_request",
];

/// Which kind of signal a note records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteKind {
    /// Background work moved: a task started, progressed or finished.
    Work,
    /// A lifecycle hook fired.
    Hook,
}

impl NoteKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Hook => "hook",
        }
    }

    fn counter(self, counts: &mut NoteCounts) -> &mut BTreeMap<String, u64> {
        match self {
            Self::Work => &mut counts.work,
            Self::Hook => &mut counts.hooks,
        }
    }
}

/// One retained note line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Note {
    kind: NoteKind,
    text: String,
    /// How many times this exact line arrived in a row.
    repeats: u64,
}

/// A note as a client reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteView {
    pub kind: &'static str,
    pub note: String,
    pub repeats: u64,
}

/// What has been recorded, whether or not it is still retained.
///
/// `recorded` counts every note accepted; `retained` is the lines still held;
/// `repeated` counts arrivals folded into an existing line; `dropped` counts
/// distinct lines evicted by the cap. The four satisfy
/// `recorded = sum(retained repeats) + repeated + dropped`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct NoteCounts {
    pub recorded: u64,
    pub retained: usize,
    pub repeated: u64,
    pub dropped: u64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hooks: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub work: BTreeMap<String, u64>,
}

/// The fixed bounds a client can print beside the counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteLimits {
    pub notes: usize,
    pub chars: usize,
}

impl Default for NoteLimits {
    fn default() -> Self {
        Self {
            notes: NOTE_CAP,
            chars: NOTE_TEXT_MAX,
        }
    }
}

/// The session the notes belong to, and the hooks that can feed them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SessionView {
    pub id: String,
    pub configured_hooks: Vec<&'static str>,
}

/// A session's view at the moment it was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NotesView {
    /// The only scalar at the top level, so a client's report prints
    /// `recording  no`/`recording  yes` with one fixed gap.
    pub recording: bool,
    pub session: SessionView,
    pub notes: Vec<NoteView>,
    pub counts: NoteCounts,
    pub limits: NoteLimits,
}

/// One session's notes and the switch that decided whether to keep them.
#[derive(Debug, Default)]
pub(crate) struct SessionNotes {
    recording: bool,
    notes: VecDeque<Note>,
    counts: NoteCounts,
}

impl SessionNotes {
    /// Turn recording on or off. Turning it on again keeps what was kept.
    pub(crate) fn set_recording(&mut self, on: bool) {
        self.recording = on;
    }

    /// Record one signal, when recording is on.
    ///
    /// A `detail` is joined to `name` with a middle dot; blank details add
    /// nothing. The text is normalised to one line and capped, and an empty
    /// result is not a note.
    pub(crate) fn record(&mut self, kind: NoteKind, name: &str, detail: Option<&str>) {
        if !self.recording {
            return;
        }
        let detail = detail.map(str::trim).filter(|detail| !detail.is_empty());
        let raw = match detail {
            Some(detail) => format!("{name} · {detail}"),
            None => name.to_string(),
        };
        self.record_text(kind, name, capped(one_line(&raw)));
    }

    /// Keep one already-built text, with no further switch check.
    ///
    /// The caller has checked that this session is recording, which is what
    /// lets a background call site build its detail only when it will be kept.
    fn record_text(&mut self, kind: NoteKind, name: &str, text: String) {
        if text.is_empty() {
            return;
        }
        self.counts.recorded += 1;
        *kind.counter(&mut self.counts).entry(name.to_string()).or_default() += 1;
        if let Some(last) = self.notes.back_mut()
            && last.kind == kind
            && last.text == text
        {
            last.repeats += 1;
            self.counts.repeated += 1;
            return;
        }
        if self.notes.len() == NOTE_CAP {
            self.notes.pop_front();
            self.counts.dropped += 1;
        }
        self.notes.push_back(Note { kind, text, repeats: 1 });
        self.counts.retained = self.notes.len();
    }

    /// Drop every note and counter, keeping the recording switch as it is.
    pub(crate) fn clear(&mut self) {
        self.notes.clear();
        self.counts = NoteCounts::default();
    }

    fn view(&self, session_id: &str) -> NotesView {
        NotesView {
            recording: self.recording,
            session: SessionView {
                id: session_id.to_string(),
                configured_hooks: configured_hooks(),
            },
            notes: self
                .notes
                .iter()
                .map(|note| NoteView {
                    kind: note.kind.as_str(),
                    note: note.text.clone(),
                    repeats: note.repeats,
                })
                .collect(),
            counts: self.counts.clone(),
            limits: NoteLimits::default(),
        }
    }
}

/// One note line, whitespace collapsed and control characters turned into a
/// break rather than passed through to a terminal.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_control() || ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out
}

/// `text` cut to [`NOTE_TEXT_MAX`] characters, with an ellipsis when cut.
fn capped(text: String) -> String {
    if text.chars().count() <= NOTE_TEXT_MAX {
        return text;
    }
    let mut out: String = text.chars().take(NOTE_TEXT_MAX.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The hooks configured in this process, in documentation order.
///
/// A status view can say which hooks are active without exposing their
/// commands, which the daemon deliberately keeps out of client reports.
pub(crate) fn configured_hooks() -> Vec<&'static str> {
    HOOK_NAMES
        .into_iter()
        .filter(|name| crate::hooks::hook_configured(name))
        .collect()
}

static NOTES: LazyLock<Mutex<HashMap<String, Arc<Mutex<SessionNotes>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether any session is recording right now.
///
/// False is the ordinary case, and it is what keeps a hot call site from taking
/// the registry lock or building a detail string when nobody asked for notes.
/// It may stay true for a moment after the last session turns off; that only
/// costs the slow path, never a wrong note.
static RECORDING_ANY: AtomicBool = AtomicBool::new(false);

fn registry() -> MutexGuard<'static, HashMap<String, Arc<Mutex<SessionNotes>>>> {
    NOTES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn lock(notes: &Mutex<SessionNotes>) -> MutexGuard<'_, SessionNotes> {
    notes.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Recompute [`RECORDING_ANY`] from the registry while its lock is held.
///
/// The caller holds the registry lock, so no session can turn on or off during
/// the scan: an enable stores `true` itself, and a disable or removal is the
/// only thing that can turn the flag back off.
fn recompute_recording_any(sessions: &HashMap<String, Arc<Mutex<SessionNotes>>>, except: &str) {
    let any = sessions
        .iter()
        .any(|(session_id, notes)| session_id != except && lock(notes).recording);
    RECORDING_ANY.store(any, Ordering::Relaxed);
}

/// The owner for a session that already opted in.
fn existing(session_id: &str) -> Option<Arc<Mutex<SessionNotes>>> {
    registry().get(session_id).cloned()
}

/// The owner for a session, created on first use, while the registry is held.
fn owner_in<'a>(
    sessions: &'a mut HashMap<String, Arc<Mutex<SessionNotes>>>,
    session_id: &str,
) -> &'a Arc<Mutex<SessionNotes>> {
    sessions
        .entry(session_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(SessionNotes::default())))
}

/// Record a hook note for `session_id`, dropping it when nobody is recording.
///
/// The fast path reads one atomic, so a default-off daemon pays no registry
/// lock and no string build for any hook that fires.
pub(crate) fn note_hook(session_id: &str, name: &'static str, detail: Option<&str>) {
    record(session_id, NoteKind::Hook, name, detail);
}

/// Record a note for `session_id`, dropping it when the session never opted in.
pub(crate) fn record(session_id: &str, kind: NoteKind, name: &str, detail: Option<&str>) {
    if session_id.trim().is_empty() || !RECORDING_ANY.load(Ordering::Relaxed) {
        return;
    }
    let Some(notes) = existing(session_id) else {
        return;
    };
    lock(&notes).record(kind, name, detail);
}

/// Record a work note whose detail is built only when it will be kept.
///
/// Background progress reaches this on every tick, so the detail closure must
/// not run while no session is recording.
pub(crate) fn record_work(session_id: &str, name: &'static str, detail: impl FnOnce() -> String) {
    if session_id.trim().is_empty() || !RECORDING_ANY.load(Ordering::Relaxed) {
        return;
    }
    let Some(notes) = existing(session_id) else {
        return;
    };
    let mut guard = lock(&notes);
    if !guard.recording {
        return;
    }
    let detail = detail();
    let text = if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name} · {detail}")
    };
    guard.record_text(NoteKind::Work, name, capped(one_line(&text)));
}

/// Turn recording on or off for `session_id` and report the state it now holds.
pub(crate) fn set_recording(session_id: &str, on: bool) -> NotesView {
    // The registry lock spans the switch and the flag update, so a concurrent
    // switch cannot leave the fast path off while a session records.
    let mut sessions = registry();
    let notes = Arc::clone(owner_in(&mut sessions, session_id));
    let (view, changed) = {
        let mut guard = lock(&notes);
        let was = guard.recording;
        guard.set_recording(on);
        (guard.view(session_id), was != on)
    };
    if changed {
        if on {
            RECORDING_ANY.store(true, Ordering::Relaxed);
        } else {
            recompute_recording_any(&sessions, session_id);
        }
    }
    view
}

/// Drop every note and counter for `session_id`, keeping its switch.
pub(crate) fn clear(session_id: &str) -> NotesView {
    match existing(session_id) {
        Some(notes) => {
            let mut guard = lock(&notes);
            guard.clear();
            guard.view(session_id)
        }
        None => SessionNotes::default().view(session_id),
    }
}

/// The session's view, without creating an owner for a session that has none.
pub(crate) fn view(session_id: &str) -> NotesView {
    match existing(session_id) {
        Some(notes) => lock(&notes).view(session_id),
        None => SessionNotes::default().view(session_id),
    }
}

/// Forget a removed session's notes.
pub(crate) fn forget_session(session_id: &str) {
    let mut sessions = registry();
    let Some(notes) = sessions.remove(session_id) else {
        return;
    };
    let was_recording = lock(&notes).recording;
    if was_recording {
        recompute_recording_any(&sessions, session_id);
    }
}

/// Run `/observations` for one session.
///
/// This answers from the notes registry rather than the agent, so the view is
/// available while a turn is running and never becomes model input.
pub(crate) fn command(session_id: &str, args: &[String]) -> anyhow::Result<(serde_json::Value, String)> {
    let view = match args {
        [] => view(session_id),
        [arg] => match arg.as_str() {
            "show" | "status" => view(session_id),
            "on" | "enable" | "enabled" => set_recording(session_id, true),
            "off" | "disable" | "disabled" => set_recording(session_id, false),
            "clear" => clear(session_id),
            _ => anyhow::bail!("Usage: /observations [show|on|off|clear]"),
        },
        _ => anyhow::bail!("Usage: /observations [show|on|off|clear]"),
    };
    let data = serde_json::to_value(&view)?;
    let message = serde_json::to_string_pretty(&data)?;
    Ok((data, message))
}

#[cfg(test)]
#[path = "session_notes_tests.rs"]
mod tests;

//! `/note`: the reader's own notes on the conversation.
//!
//! A note belongs to one message of one session and lives in the client's own
//! file ([`crate::notes`]), never in a message and never in a harness request.
//! The command answers every part of the workflow: adding, editing, removing,
//! listing and cleaning up after a session the harness no longer stores.
//!
//! A message is addressed as `u<n>` or `a<n>`, its place among the user or
//! assistant messages of the session. With no address the newest message is
//! the target, which is what the reader usually means. The address is resolved
//! against the transcript in force, and the note is stored under the message's
//! content, so it never follows a different message that later takes the same
//! place.

use super::{Action, App};
use crate::notes::{self, Address, NoteFault};

/// The line a listing shows per note, before the note's own text.
const EXCERPT_CHARS: usize = 60;

impl App {
    /// Read the notes file once, reporting what the first read found.
    pub(crate) fn load_notes(&mut self) {
        for warning in self.notes.ensure_loaded() {
            self.warn(format!("notes: {warning}"));
        }
    }

    /// Answer `/note`.
    pub(crate) fn run_note(&mut self, arguments: &str) -> Option<Action> {
        self.load_notes();
        let Some(session) = self.session_id.clone() else {
            self.warn("notes: not attached to a session, so there is nothing to note");
            return None;
        };
        let trimmed = arguments.trim();
        let (verb, rest) = match trimmed.split_once(char::is_whitespace) {
            Some((word, rest)) => (word, rest.trim_start()),
            None => (trimmed, ""),
        };
        match verb {
            "" | "list" => {
                self.list_notes(&session);
                None
            }
            "add" => {
                self.add_note(&session, rest, false);
                None
            }
            "edit" => {
                self.add_note(&session, rest, true);
                None
            }
            "remove" | "delete" => {
                self.remove_note(&session, rest);
                None
            }
            "prune" => {
                if self.disconnected {
                    self.warn("notes: the connection is gone; cleanup needs the harness");
                    return None;
                }
                let ids = self.notes.session_ids();
                if ids.is_empty() {
                    self.transcript.push_notice("notes: there are none to clean up");
                    return None;
                }
                Some(Action::CheckSessions(ids))
            }
            // Shorthand: `/note <text>` notes the newest message.
            _ => {
                self.add_note(&session, trimmed, false);
                None
            }
        }
    }

    /// Attach or replace a note on one message.
    fn add_note(&mut self, session: &str, raw: &str, must_exist: bool) {
        let target = match target(self, raw) {
            Ok(target) => target,
            Err(message) => {
                self.warn(format!("notes: {message}"));
                return;
            }
        };
        let (target, text) = target;
        let notes::Keyed {
            key,
            address,
            stored_id,
        } = target;
        if text.trim().is_empty() {
            self.warn(format!(
                "notes: the note for {address} has no text; /note add {address} <text>"
            ));
            return;
        }
        let existing = self.notes.note_record(session, key, stored_id.as_deref());
        if must_exist && existing.is_none() {
            self.warn(format!(
                "notes: {address} has no note to edit; /note add {address} <text> adds one"
            ));
            return;
        }
        let existed = existing.is_some();
        match self
            .notes
            .set_stored(session, key, address, stored_id.as_deref(), &text)
        {
            Ok(()) => {}
            Err(error) => {
                self.warn(format!("notes: {error}"));
                return;
            }
        }
        self.save_notes();
        let what = if existed { "updated" } else { "added" };
        self.transcript.push_notice(format!("note {what} on {address}"));
    }

    /// Remove the note on one message.
    fn remove_note(&mut self, session: &str, raw: &str) {
        let trimmed = raw.trim();
        let (address, key) = if trimmed.is_empty() {
            match newest(self) {
                Ok(target) => (Some(target.address), Some(target)),
                Err(message) => {
                    self.warn(format!("notes: {message}"));
                    return;
                }
            }
        } else {
            let word = trimmed.split_whitespace().next().unwrap_or(trimmed);
            match address(word) {
                Ok(Some(address)) => (Some(address), key_at(self, address)),
                Ok(None) => {
                    self.warn(format!(
                        "notes: `{word}` is not a message address; use u1, a1 and so on"
                    ));
                    return;
                }
                Err(message) => {
                    self.warn(format!("notes: {message}"));
                    return;
                }
            }
        };
        let mut removed = match key {
            Some(target) => self.notes.remove(session, target.key, target.stored_id.as_deref()),
            None => false,
        };
        // A note written on a message this transcript no longer holds — a
        // compaction, a retracted turn — is reached by the address it was
        // written at, because the content identity it is listed by cannot be
        // computed for a message that is gone. It is removed from there only
        // while no live row holds it: the same address can now name another
        // answer, and that answer's note is a different one.
        if !removed && let Some(address) = address {
            removed = self.remove_unheld_note(session, address);
        }
        if !removed {
            let named = address.map_or_else(|| "the newest message".to_string(), |address| address.to_string());
            self.transcript
                .push_notice(format!("notes: {named} has no note to remove"));
            return;
        }
        self.save_notes();
        let named = address.map_or_else(|| "the newest message".to_string(), |address| address.to_string());
        self.transcript.push_notice(format!("note removed from {named}"));
    }

    /// Remove the note recorded at `address` when no live row holds it.
    ///
    /// Answers whether a note was removed. The note is drawn wherever a row
    /// still matches its identity, and deleting it through an address that has
    /// moved on to another message would take the text off the row the reader
    /// is looking at, so a held note is left alone and the caller says there is
    /// nothing to remove here.
    fn remove_unheld_note(&mut self, session: &str, address: Address) -> bool {
        let Some((key, stored_id)) = self
            .notes
            .notes(session)
            .find(|(_, note)| note.address == address)
            .map(|(key, note)| (key, note.stored_id.clone()))
        else {
            return false;
        };
        let keyed = notes::keyed(self.transcript.items(), self.transcript.stored_ids());
        // A row holds the note when it names the same stored message, or when
        // it carries no id of its own and the note's text identity, which is
        // the fallback `NoteStore::remove` makes. A note written before stored
        // ids existed is held by its text identity alone.
        let held = keyed.iter().flatten().any(|row| match stored_id.as_deref() {
            Some(id) => row.stored_id.as_deref() == Some(id) || (row.stored_id.is_none() && row.key == key),
            None => row.key == key,
        });
        if held {
            return false;
        }
        self.notes.remove_at(session, address)
    }

    /// Draw this session's notes, in the order the conversation holds them.
    fn list_notes(&mut self, session: &str) {
        let count = self.notes.count(session);
        if count == 0 {
            self.transcript
                .push_notice("notes: this session has none; /note add <text> notes the newest message");
            return;
        }
        let keyed = notes::keyed(self.transcript.items(), self.transcript.stored_ids());
        let mut rows = vec![format!("notes in this session ({count}):")];
        let mut shown = 0;
        let mut live = Vec::new();
        for (item, entry) in self.transcript.items().iter().zip(&keyed) {
            let Some(entry) = entry else {
                continue;
            };
            let Some(note) = self.notes.note_record(session, entry.key, entry.stored_id.as_deref()) else {
                continue;
            };
            live.push(note);
            let quote = notes::excerpt(item).map_or_else(String::new, |text| {
                format!("\"{}\"  ", notes::summary(text.trim(), EXCERPT_CHARS))
            });
            rows.push(format!("  {}  {quote}— {}", entry.address, note.text));
            shown += 1;
        }
        // A note whose message this transcript no longer holds is still the
        // reader's text; hiding it would look like the note was lost.
        if shown < count {
            for (_, note) in self.notes.notes(session) {
                if !live.iter().any(|shown| std::ptr::eq(*shown, note)) {
                    rows.push(format!(
                        "  {}  (message not in this transcript)  — {}",
                        note.address, note.text
                    ));
                }
            }
        }
        self.transcript.push_notice(rows.join("\n"));
    }

    /// Prune the notes of sessions the harness reported as no longer stored.
    pub(crate) fn sessions_checked(&mut self, gone: &[String], unverified: usize) {
        let current = self.session_id.clone();
        let gone: Vec<String> = gone
            .iter()
            .filter(|session| current.as_deref() != Some(session.as_str()))
            .cloned()
            .collect();
        let (sessions, notes) = self.notes.forget_sessions(&gone);
        if notes > 0 {
            self.save_notes();
        }
        if notes == 0 {
            self.transcript.push_notice("notes: no deleted session held a note");
        } else {
            self.transcript.push_notice(format!(
                "note cleanup: removed {notes} note(s) of {sessions} session(s) the harness no longer stores"
            ));
        }
        if unverified > 0 {
            self.warn(format!(
                "notes: {unverified} noted session(s) could not be checked and kept their notes"
            ));
        }
    }

    /// Write the notes file, reporting a failure without losing the change.
    fn save_notes(&mut self) {
        match self.notes.save() {
            Ok(warnings) => {
                for warning in warnings {
                    self.warn(format!("notes: {warning}"));
                }
            }
            Err(error) => self.warn(format!("notes: {error}")),
        }
    }
}

/// The message and text an `add`/`edit` line names.
///
/// The first word is an address when it parses as one; otherwise the whole
/// line is the note's text and the newest message is the target.
fn target(app: &App, raw: &str) -> Result<(notes::Keyed, String), NoteFault> {
    let raw = raw.trim();
    let (first, rest) = match raw.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (raw, ""),
    };
    if let Some(address) = address(first)? {
        let keyed =
            key_at(app, address).ok_or_else(|| NoteFault::new(format!("this session has no message {address}")))?;
        return Ok((keyed, rest.to_string()));
    }
    let newest = newest(app)?;
    Ok((newest, raw.to_string()))
}

/// The keyed message the newest addressable row of `app` holds.
fn newest(app: &App) -> Result<notes::Keyed, NoteFault> {
    notes::keyed(app.transcript.items(), app.transcript.stored_ids())
        .into_iter()
        .flatten()
        .next_back()
        .ok_or_else(|| NoteFault::new("this session has no user or assistant message to note"))
}

/// The current identity of `address`, when the transcript holds it.
fn key_at(app: &App, address: Address) -> Option<notes::Keyed> {
    notes::keyed(app.transcript.items(), app.transcript.stored_ids())
        .into_iter()
        .flatten()
        .find(|entry| entry.address == address)
}

/// Read a word as an address.
///
/// A word shaped like an address (`a3`, `u0`) that does not parse is an error,
/// not note text: a mistyped address would otherwise attach the note to the
/// newest message, which is how a note ends up on the wrong row.
fn address(word: &str) -> Result<Option<Address>, NoteFault> {
    let Some(address) = Address::parse(word) else {
        let mut characters = word.chars();
        let looks = matches!(characters.next(), Some('u' | 'U' | 'a' | 'A')) && !characters.as_str().is_empty();
        if looks && characters.as_str().chars().all(|character| character.is_ascii_digit()) {
            return Err(NoteFault::new(format!(
                "`{word}` is not a message address; messages start at 1"
            )));
        }
        return Ok(None);
    };
    Ok(Some(address))
}

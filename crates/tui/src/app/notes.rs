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
use crate::notes::{self, Address, MessageKey, NoteFault};

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
        let (key, address, text) = target;
        if text.trim().is_empty() {
            self.warn(format!(
                "notes: the note for {address} has no text; /note add {address} <text>"
            ));
            return;
        }
        if must_exist && self.notes.note(session, key).is_none() {
            self.warn(format!(
                "notes: {address} has no note to edit; /note add {address} <text> adds one"
            ));
            return;
        }
        let existed = self.notes.note(session, key).is_some();
        match self.notes.set(session, key, address, &text) {
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
                Ok(target) => (Some(target.address), Some(target.key)),
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
        let mut removed = false;
        if let Some(key) = key {
            removed = self.notes.remove(session, key);
        }
        // A note whose message the conversation no longer has cannot be found
        // by content; the address it was stored at is the only handle left.
        if !removed && let Some(address) = address {
            removed = self.notes.remove_at(session, address);
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

    /// Draw this session's notes, in the order the conversation holds them.
    fn list_notes(&mut self, session: &str) {
        let count = self.notes.count(session);
        if count == 0 {
            self.transcript
                .push_notice("notes: this session has none; /note add <text> notes the newest message");
            return;
        }
        let keyed = notes::keyed(self.transcript.items());
        let mut rows = vec![format!("notes in this session ({count}):")];
        let mut shown = 0;
        for (item, entry) in self.transcript.items().iter().zip(&keyed) {
            let Some(entry) = entry else {
                continue;
            };
            let Some(note) = self.notes.note(session, entry.key) else {
                continue;
            };
            let quote = notes::excerpt(item).map_or_else(String::new, |text| {
                format!("\"{}\"  ", notes::summary(text.trim(), EXCERPT_CHARS))
            });
            rows.push(format!("  {}  {quote}— {note}", entry.address));
            shown += 1;
        }
        // A note whose message this transcript no longer holds is still the
        // reader's text; hiding it would look like the note was lost.
        if shown < count {
            let live: Vec<MessageKey> = keyed.iter().flatten().map(|entry| entry.key).collect();
            for (key, note) in self.notes.notes(session) {
                if !live.contains(&key) {
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
fn target(app: &App, raw: &str) -> Result<(MessageKey, Address, String), NoteFault> {
    let raw = raw.trim();
    let (first, rest) = match raw.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (raw, ""),
    };
    if let Some(address) = address(first)? {
        let key =
            key_at(app, address).ok_or_else(|| NoteFault::new(format!("this session has no message {address}")))?;
        return Ok((key, address, rest.to_string()));
    }
    let newest = newest(app)?;
    Ok((newest.key, newest.address, raw.to_string()))
}

/// The keyed message the newest addressable row of `app` holds.
fn newest(app: &App) -> Result<notes::Keyed, NoteFault> {
    notes::keyed(app.transcript.items())
        .into_iter()
        .flatten()
        .next_back()
        .ok_or_else(|| NoteFault::new("this session has no user or assistant message to note"))
}

/// The current content identity of `address`, when the transcript holds it.
fn key_at(app: &App, address: Address) -> Option<MessageKey> {
    notes::keyed(app.transcript.items())
        .into_iter()
        .flatten()
        .find(|entry| entry.address == address)
        .map(|entry| entry.key)
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

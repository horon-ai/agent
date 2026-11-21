//! The actions a keystroke can mean, and the map from keys to actions.
//!
//! An action id has the form `<scope>.<verb>`. The scope decides which part of
//! the interface answers it: [`Scope::App`] and [`Scope::Editor`] belong to the
//! input line and the session, [`Scope::Prompt`] belongs to the approval and
//! question prompt that takes every key while it is open.
//!
//! The defaults in [`ACTIONS`] are the keys the interface shipped with, so a
//! configuration file that mentions nothing changes nothing. A file entry
//! replaces that action's keys; `[]` turns the action off. Two actions in one
//! scope that claim the same key are a mistake in the file, not a reason to
//! refuse to start: the first in [`ACTIONS`] order keeps the key, and the
//! warning names both actions.

use std::collections::BTreeMap;

use toml::{Table, Value};

use super::key::Key;

/// The action ids the interface answers to.
pub mod ids {
    /// Send the typed message.
    pub const APP_SUBMIT: &str = "app.submit";
    /// Cancel the running turn, or leave when nothing is running.
    pub const APP_CANCEL: &str = "app.cancel";
    /// Send the queued messages into the running turn; with nothing queued,
    /// interrupt it. No action when idle.
    pub const APP_INTERRUPT: &str = "app.interrupt";
    /// Leave.
    pub const APP_QUIT: &str = "app.quit";
    /// Hold the transcript view one page higher.
    pub const APP_SCROLL_PAGE_UP: &str = "app.scroll_page_up";
    /// Move the transcript view one page back towards the end.
    pub const APP_SCROLL_PAGE_DOWN: &str = "app.scroll_page_down";
    /// Hold the transcript view one row higher.
    pub const APP_SCROLL_UP: &str = "app.scroll_up";
    /// Move the transcript view one row back towards the end.
    pub const APP_SCROLL_DOWN: &str = "app.scroll_down";
    /// Follow the end of the conversation again.
    pub const APP_SCROLL_BOTTOM: &str = "app.scroll_bottom";
    /// Show the whole output of the selected tool call whose preview was cut,
    /// and the preview again on the next press.
    pub const APP_EXPAND_TOOL: &str = "app.expand_tool";
    /// Draw or hide the record of finished tool calls for this session.
    pub const APP_TOGGLE_TOOL_ACTIVITY: &str = "app.toggle_tool_activity";
    /// Step to the next reasoning-effort level, wrapping at the last one.
    pub const APP_CYCLE_EFFORT: &str = "app.cycle_effort";
    /// Open the list of models this session can switch to.
    pub const APP_PICK_MODEL: &str = "app.pick_model";
    /// Copy the whole draft to the terminal clipboard.
    pub const APP_COPY_DRAFT: &str = "app.copy_draft";
    /// Copy the line the cursor is on to the terminal clipboard.
    pub const APP_COPY_LINE: &str = "app.copy_line";
    /// Copy the newest answer to the terminal clipboard.
    pub const APP_COPY_LAST: &str = "app.copy_last";
    /// Copy the newest answer's last code block to the terminal clipboard.
    pub const APP_COPY_CODE: &str = "app.copy_code";
    /// Step forward to the next model in `models.shortlist`, wrapping.
    pub const APP_CYCLE_MODEL: &str = "app.cycle_model";
    /// Step backward to the previous model in `models.shortlist`, wrapping.
    pub const APP_CYCLE_MODEL_BACK: &str = "app.cycle_model_back";
    /// Paint the whole screen again, discarding the frame the terminal holds.
    pub const APP_REPAINT: &str = "app.repaint";
    /// Show or hide the model's reasoning for the rest of the session.
    pub const APP_TOGGLE_THINKING: &str = "app.toggle_thinking";
    /// Start or finish on-device voice input.
    pub const APP_VOICE: &str = "app.voice";
    /// Delete the character before the cursor.
    pub const EDITOR_BACKSPACE: &str = "editor.backspace";
    /// Delete the character at the cursor.
    pub const EDITOR_DELETE: &str = "editor.delete";
    /// Move the cursor one character left.
    pub const EDITOR_LEFT: &str = "editor.left";
    /// Move the cursor one character right.
    pub const EDITOR_RIGHT: &str = "editor.right";
    /// Move the cursor to the start of its line.
    pub const EDITOR_HOME: &str = "editor.home";
    /// Move the cursor to the end of its line.
    pub const EDITOR_END: &str = "editor.end";
    /// Start a new line without sending the draft.
    pub const EDITOR_NEWLINE: &str = "editor.newline";
    /// Delete from the start of the line to the cursor.
    pub const EDITOR_KILL_TO_START: &str = "editor.kill_to_start";
    /// Delete from the cursor to the end of the line.
    pub const EDITOR_KILL_TO_END: &str = "editor.kill_to_end";
    /// Delete the word before the cursor.
    pub const EDITOR_DELETE_WORD: &str = "editor.delete_word";
    /// Move to the start of the word before the cursor.
    pub const EDITOR_WORD_LEFT: &str = "editor.word_left";
    /// Move past the word after the cursor.
    pub const EDITOR_WORD_RIGHT: &str = "editor.word_right";
    /// Step back through the drafts sent before this one.
    pub const EDITOR_HISTORY_PREV: &str = "editor.history_prev";
    /// Step forward, and back to the draft being typed.
    pub const EDITOR_HISTORY_NEXT: &str = "editor.history_next";
    /// Complete the slash command being typed.
    pub const EDITOR_COMPLETE: &str = "editor.complete";
    /// Fill the draft with the local suggestion shown after the caret.
    pub const EDITOR_ACCEPT_SUGGESTION: &str = "editor.accept_suggestion";
    /// Search the drafts sent before this one.
    pub const EDITOR_HISTORY_SEARCH: &str = "editor.history_search";
    /// Hand the draft to `VISUAL` or `EDITOR` and load what it saves.
    pub const EDITOR_EXTERNAL: &str = "editor.external";
    /// Approve a parked tool call once.
    pub const PROMPT_ALLOW: &str = "prompt.allow";
    /// Approve a parked tool call and every later call like it.
    pub const PROMPT_ALLOW_ALWAYS: &str = "prompt.allow_always";
    /// Deny a parked tool call.
    pub const PROMPT_DENY: &str = "prompt.deny";
    /// Answer a parked tool call with a denial; a question stays open, since
    /// there is nothing to deny and the tool has its own deadline.
    pub const PROMPT_DISMISS: &str = "prompt.dismiss";
    /// Confirm a multi-choice question.
    pub const PROMPT_CONFIRM: &str = "prompt.confirm";
}

/// Which part of the interface answers an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// The session: sending, cancelling, leaving.
    App,
    /// The line being edited.
    Editor,
    /// The prompt blocking the turn.
    Prompt,
}

/// One action, its scope, and the keys it answers to by default.
#[derive(Debug, Clone, Copy)]
pub struct ActionSpec {
    /// The id a file names and a warning quotes.
    pub id: &'static str,
    /// Where it is answered.
    pub scope: Scope,
    /// The keys it answers to when no file overrides them.
    pub default_keys: &'static [&'static str],
}

impl ActionSpec {
    /// An action with its default keys, for the table below.
    const fn new(id: &'static str, scope: Scope, default_keys: &'static [&'static str]) -> Self {
        Self {
            id,
            scope,
            default_keys,
        }
    }
}

/// Every action, in the order that settles a conflict.
///
/// This order is also declaration order in the file: the first action listed
/// here keeps a key two actions claim.
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec::new(ids::APP_SUBMIT, Scope::App, &["enter"]),
    ActionSpec::new(ids::APP_CANCEL, Scope::App, &["ctrl+c"]),
    ActionSpec::new(ids::APP_INTERRUPT, Scope::App, &["esc"]),
    // `ctrl+d` follows the draft at this key alone: the app deletes forward
    // while the draft has text and only leaves from an empty one. A file that
    // moves the action to another key gets the plain leave back there.
    ActionSpec::new(ids::APP_QUIT, Scope::App, &["ctrl+d"]),
    // The transcript's own keys: `up` and `down` stay with the draft and its
    // history, so the view moves on their shifted forms and on the page keys.
    ActionSpec::new(ids::APP_SCROLL_PAGE_UP, Scope::App, &["pageup"]),
    ActionSpec::new(ids::APP_SCROLL_PAGE_DOWN, Scope::App, &["pagedown"]),
    ActionSpec::new(ids::APP_SCROLL_UP, Scope::App, &["shift+up"]),
    ActionSpec::new(ids::APP_SCROLL_DOWN, Scope::App, &["shift+down"]),
    ActionSpec::new(ids::APP_SCROLL_BOTTOM, Scope::App, &["ctrl+end"]),
    // The key a cut tool preview names: reading the rest of one call's output
    // is the one thing the transcript cannot do with the scroll keys alone.
    ActionSpec::new(ids::APP_EXPAND_TOOL, Scope::App, &["ctrl+o"]),
    // The prose-only view: the record of finished calls goes, the work in
    // progress and the approvals it needs stay. The letter is free in the
    // shipped map, so no action loses a key to it.
    ActionSpec::new(ids::APP_TOGGLE_TOOL_ACTIVITY, Scope::App, &["alt+t"]),
    // `shift+tab` walks the effort ladder upwards. `tab` alone still completes
    // the draft: an exact binding is looked up before an unshifted one, and the
    // shift on the event stops `tab` from answering it.
    ActionSpec::new(ids::APP_CYCLE_EFFORT, Scope::App, &["shift+tab"]),
    // The model list, one keystroke away from the draft being typed.
    ActionSpec::new(ids::APP_PICK_MODEL, Scope::App, &["alt+p"]),
    // Copying is on a key because it is asked for while reading: `alt+shift+c`
    // and `alt+shift+l` are the keys the reference client puts here, and their
    // shifted characters arrive lower-cased, so the bindings are `alt+c` and
    // `alt+l`. A terminal that does not report option as meta sends a character
    // instead, which is why `/copy` reaches the same three targets.
    ActionSpec::new(ids::APP_COPY_CODE, Scope::App, &["alt+c"]),
    ActionSpec::new(ids::APP_COPY_LAST, Scope::App, &["alt+l"]),
    ActionSpec::new(ids::APP_COPY_DRAFT, Scope::App, &["alt+d"]),
    // The draft is one message in the editor; a line of it is the unit a user
    // copies out of a long one, so it has its own key rather than a mode.
    ActionSpec::new(ids::APP_COPY_LINE, Scope::App, &["alt+k"]),
    // Two steps through a shortlist of the user's own, forward and backward.
    // `ctrl+p` is free of the editor's own keys, and `ctrl+alt+p` is the same
    // letter with one modifier more, which every terminal reports apart from
    // `ctrl+p`; either can be rebound in `keybindings.toml`.
    ActionSpec::new(ids::APP_CYCLE_MODEL, Scope::App, &["ctrl+p"]),
    ActionSpec::new(ids::APP_CYCLE_MODEL_BACK, Scope::App, &["ctrl+alt+p"]),
    // The one key that changes nothing: it throws away the frame Ratatui kept,
    // so a screen another program wrote over is drawn again in full. It answers
    // before every surface and takes no key from one, because only the frames
    // are discarded, not the state they show. `ctrl+l` is the key a shell uses
    // for the same repair.
    ActionSpec::new(ids::APP_REPAINT, Scope::App, &["ctrl+l"]),
    // Reasoning display for this session: the draft keeps every letter, so the
    // toggle has to be a control key.
    ActionSpec::new(ids::APP_TOGGLE_THINKING, Scope::App, &["ctrl+t"]),
    // On-device voice input. `ctrl+space` is free in the shipped map; a
    // terminal that does not report it leaves `/voice` as the entry point, and
    // the key can be rebound in `keybindings.toml`.
    ActionSpec::new(ids::APP_VOICE, Scope::App, &["ctrl+space"]),
    ActionSpec::new(ids::EDITOR_BACKSPACE, Scope::Editor, &["backspace"]),
    ActionSpec::new(ids::EDITOR_DELETE, Scope::Editor, &["delete"]),
    ActionSpec::new(ids::EDITOR_LEFT, Scope::Editor, &["left", "ctrl+b"]),
    ActionSpec::new(ids::EDITOR_RIGHT, Scope::Editor, &["right", "ctrl+f"]),
    ActionSpec::new(ids::EDITOR_HOME, Scope::Editor, &["home", "ctrl+a"]),
    ActionSpec::new(ids::EDITOR_END, Scope::Editor, &["end", "ctrl+e"]),
    ActionSpec::new(ids::EDITOR_WORD_LEFT, Scope::Editor, &["alt+left"]),
    ActionSpec::new(ids::EDITOR_WORD_RIGHT, Scope::Editor, &["alt+right"]),
    // `up` and `down` move between the lines of a draft first; the app hands
    // them to the history only at the first and last line.
    ActionSpec::new(ids::EDITOR_HISTORY_PREV, Scope::Editor, &["up"]),
    ActionSpec::new(ids::EDITOR_HISTORY_NEXT, Scope::Editor, &["down"]),
    ActionSpec::new(
        ids::EDITOR_NEWLINE,
        Scope::Editor,
        &["shift+enter", "alt+enter", "ctrl+j"],
    ),
    ActionSpec::new(ids::EDITOR_KILL_TO_START, Scope::Editor, &["ctrl+u"]),
    ActionSpec::new(ids::EDITOR_KILL_TO_END, Scope::Editor, &["ctrl+k"]),
    // Two spellings, both of them the word the cursor just left: `ctrl+w` is
    // the one the editor grew with, `alt+backspace` is the one the terminals
    // and the other clients send for the same intent.
    ActionSpec::new(ids::EDITOR_DELETE_WORD, Scope::Editor, &["ctrl+w", "alt+backspace"]),
    ActionSpec::new(ids::EDITOR_COMPLETE, Scope::Editor, &["tab"]),
    // The one key that puts a ghost suggestion into the draft. It is a
    // modifier on the arrow that already moves by a character, so it cannot be
    // reached by a slip while typing.
    ActionSpec::new(ids::EDITOR_ACCEPT_SUGGESTION, Scope::Editor, &["ctrl+right"]),
    ActionSpec::new(ids::EDITOR_HISTORY_SEARCH, Scope::Editor, &["ctrl+r"]),
    // The draft leaves the client for the length of one edit. `ctrl+g` is
    // unclaimed by the rest of the map, so the key adds a surface without
    // taking one away.
    ActionSpec::new(ids::EDITOR_EXTERNAL, Scope::Editor, &["ctrl+g"]),
    ActionSpec::new(ids::PROMPT_ALLOW, Scope::Prompt, &["y"]),
    ActionSpec::new(ids::PROMPT_ALLOW_ALWAYS, Scope::Prompt, &["a"]),
    ActionSpec::new(ids::PROMPT_DENY, Scope::Prompt, &["n"]),
    ActionSpec::new(ids::PROMPT_DISMISS, Scope::Prompt, &["esc"]),
    ActionSpec::new(ids::PROMPT_CONFIRM, Scope::Prompt, &["enter"]),
];

/// One key and the action it answers, in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    key: Key,
    id: &'static str,
    scope: Scope,
}

/// The key map in force.
///
/// Kept as an ordered list rather than a hash map so resolution is
/// deterministic, a lookup allocates nothing, and the declaration order that
/// settles a conflict is the order the entries are stored in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    entries: Vec<Entry>,
}

impl Bindings {
    /// Build the map from the defaults and one table of overrides.
    ///
    /// A table entry replaces every default key of that action; an empty array
    /// leaves the action with no key at all. Returns the map and the warnings
    /// the file earned, which the caller shows in the transcript.
    pub fn from_table(table: &Table) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut overrides: BTreeMap<&'static str, Vec<Key>> = BTreeMap::new();
        for (id, value) in table {
            let Some(spec) = ACTIONS.iter().find(|spec| spec.id == id.as_str()) else {
                warnings.push(format!("unknown action `{id}`; ignored"));
                continue;
            };
            let Value::Array(items) = value else {
                warnings.push(format!(
                    "`{id}` must be an array of key names, not {}; ignored",
                    value.type_str()
                ));
                continue;
            };
            let mut keys = Vec::with_capacity(items.len());
            for item in items {
                let Value::String(text) = item else {
                    warnings.push(format!("`{id}` contains {}, not a key name; ignored", item.type_str()));
                    continue;
                };
                match Key::parse(text) {
                    Ok(key) => keys.push(key),
                    Err(reason) => {
                        warnings.push(format!("cannot use `{text}` for `{id}`: {reason}; ignored"));
                    }
                }
            }
            overrides.insert(spec.id, keys);
        }
        let (bindings, conflicts) = Self::build(&overrides);
        warnings.extend(conflicts);
        (bindings, warnings)
    }

    /// The action a keystroke means in one scope.
    pub fn resolve(&self, scope: Scope, key: &Key) -> Option<&'static str> {
        self.find(|entry| entry.scope == scope, key)
    }

    /// The action a keystroke means while typing: app and editor actions,
    /// which share the input line.
    pub fn resolve_editing(&self, key: &Key) -> Option<&'static str> {
        self.find(|entry| entry.scope != Scope::Prompt, key)
    }

    /// The first key an action answers to now.
    ///
    /// The one a hint or a marker names when an action may carry several: the
    /// first is the one the declaration order settled on, and a frame that only
    /// wants to name it must not build the whole list.
    pub fn first_key(&self, id: &str) -> Option<Key> {
        self.entries.iter().find(|entry| entry.id == id).map(|entry| entry.key)
    }

    /// The keys an action answers to now.
    pub fn keys_for(&self, id: &str) -> Vec<Key> {
        self.entries
            .iter()
            .filter(|entry| entry.id == id)
            .map(|entry| entry.key)
            .collect()
    }

    /// Exact match first, then a binding whose modifiers the event extends.
    fn find(&self, in_scope: impl Fn(&Entry) -> bool, key: &Key) -> Option<&'static str> {
        if let Some(entry) = self.entries.iter().find(|entry| in_scope(entry) && entry.key == *key) {
            return Some(entry.id);
        }
        self.entries
            .iter()
            .find(|entry| in_scope(entry) && extends(entry.key, key))
            .map(|entry| entry.id)
    }

    /// Lay the defaults and the overrides out as one ordered list, keeping the
    /// first action that claims a key and reporting the ones it displaced.
    fn build(overrides: &BTreeMap<&'static str, Vec<Key>>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut entries = Vec::new();
        let mut taken: BTreeMap<(Scope, Key), &'static str> = BTreeMap::new();
        for spec in ACTIONS {
            let keys = match overrides.get(spec.id) {
                Some(keys) => keys.clone(),
                None => {
                    let mut keys = Vec::with_capacity(spec.default_keys.len());
                    for name in spec.default_keys {
                        match Key::parse(name) {
                            Ok(key) => keys.push(key),
                            Err(reason) => {
                                warnings.push(format!("the default key for `{}` is unusable: {reason}", spec.id))
                            }
                        }
                    }
                    keys
                }
            };
            for key in keys {
                match taken.get(&(spec.scope, key)) {
                    Some(first) if *first != spec.id => {
                        warnings.push(format!(
                            "`{key}` is bound to both `{first}` and `{}`; keeping `{first}`",
                            spec.id
                        ));
                    }
                    Some(_) => {}
                    None => {
                        taken.insert((spec.scope, key), spec.id);
                        entries.push(Entry {
                            key,
                            id: spec.id,
                            scope: spec.scope,
                        });
                    }
                }
            }
        }
        (Self { entries }, warnings)
    }
}

/// Whether `binding` answers `event` when neither names the same key exactly.
///
/// A terminal reports `alt+left` for a plain arrow the interface has always
/// moved on, so control and alt are inferred. Shift is not: it arrives as its
/// own key, and `shift+up` scrolls the transcript while `up` stays with the
/// draft, so a binding answers a shifted event only when it named shift itself.
fn extends(binding: Key, event: &Key) -> bool {
    binding.covers(event) && (!event.shift || binding.shift)
}

impl Default for Bindings {
    /// The keys the interface ships with.
    fn default() -> Self {
        Self::build(&BTreeMap::new()).0
    }
}

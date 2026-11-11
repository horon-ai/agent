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
    /// Leave.
    pub const APP_QUIT: &str = "app.quit";
    /// Delete the character before the cursor.
    pub const EDITOR_BACKSPACE: &str = "editor.backspace";
    /// Delete the character at the cursor.
    pub const EDITOR_DELETE: &str = "editor.delete";
    /// Move the cursor one character left.
    pub const EDITOR_LEFT: &str = "editor.left";
    /// Move the cursor one character right.
    pub const EDITOR_RIGHT: &str = "editor.right";
    /// Move the cursor to the start of the line.
    pub const EDITOR_HOME: &str = "editor.home";
    /// Move the cursor to the end of the line.
    pub const EDITOR_END: &str = "editor.end";
    /// Complete the slash command being typed.
    pub const EDITOR_COMPLETE: &str = "editor.complete";
    /// Approve a parked tool call once.
    pub const PROMPT_ALLOW: &str = "prompt.allow";
    /// Approve a parked tool call and every later call like it.
    pub const PROMPT_ALLOW_ALWAYS: &str = "prompt.allow_always";
    /// Deny a parked tool call.
    pub const PROMPT_DENY: &str = "prompt.deny";
    /// Ignore the prompt; the turn stays parked.
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
    ActionSpec::new(ids::APP_QUIT, Scope::App, &["ctrl+d"]),
    ActionSpec::new(ids::EDITOR_BACKSPACE, Scope::Editor, &["backspace"]),
    ActionSpec::new(ids::EDITOR_DELETE, Scope::Editor, &["delete"]),
    ActionSpec::new(ids::EDITOR_LEFT, Scope::Editor, &["left"]),
    ActionSpec::new(ids::EDITOR_RIGHT, Scope::Editor, &["right"]),
    ActionSpec::new(ids::EDITOR_HOME, Scope::Editor, &["home"]),
    ActionSpec::new(ids::EDITOR_END, Scope::Editor, &["end"]),
    ActionSpec::new(ids::EDITOR_COMPLETE, Scope::Editor, &["tab"]),
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
            .find(|entry| in_scope(entry) && entry.key.covers(key))
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

impl Default for Bindings {
    /// The keys the interface ships with.
    fn default() -> Self {
        Self::build(&BTreeMap::new()).0
    }
}

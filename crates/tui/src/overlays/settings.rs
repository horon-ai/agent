//! The `/settings` screen, generated from the settings registry.
//!
//! No setting is listed by hand here. Rows come from
//! [`crate::settings::registry::REGISTRY`], grouped by the `ui.tab` and
//! `ui.group` each declaration carries, so a key added there appears on the
//! next start with the widget its [`SettingKind`] implies. Values and their
//! provenance are read from a [`Settings`] snapshot the caller refreshes, and
//! a confirmed change leaves as a [`SettingChange`] for the event loop to write
//! through [`crate::settings::Reloader::save_global`]. The overlay never writes
//! a file itself, which keeps the write and the re-apply in one place.
//!
//! One tab is not built from the registry: `Daemon` lists the values the
//! daemon reports through `get_effective_settings`, which this client can only
//! read. Its rows carry no widget, `Enter` does nothing there, and `r` asks for
//! a fresh read, so a screen that is mostly editable stays honest about the
//! part that is not.
//!
//! `/` opens a search line that narrows the rows to the ones whose label, key
//! or description contains what was typed, across every tab: the header moves
//! to the tab of the row the selection lands on. The query is screen state
//! only, so typing one never writes a settings file, and `esc` clears it before
//! it closes the screen. A query in force covers the registry's rows and takes
//! the screen off the daemon tab, whose rows the client does not name.

use std::borrow::Cow;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::settings::{self, DaemonRow, DaemonSettings, Provenance, SettingKind, SettingSpec, SettingValue, Settings};
use crate::theme::{Symbols, Theme};

/// The id whose values are theme files rather than an enum.
const THEME_ID: &str = "theme";

/// The tab listing the daemon's own values, which this client only reads.
pub const DAEMON_TAB: &str = "Daemon";

/// Width of the label column on the daemon tab.
///
/// The separator after it is printed whatever the label's length: a label
/// wider than the column must not run into the value, or a row would read as
/// one word ("Stream idle timeout (seconds)240").
const DAEMON_LABEL_WIDTH: usize = 28;

/// A confirmed change the event loop must write and re-apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChange {
    /// The registry id the change applies to.
    pub id: &'static str,
    /// The value the user chose.
    pub value: SettingValue,
}

/// One row of the overlay: a declared setting and the value in force.
#[derive(Debug, Clone)]
pub struct Row {
    /// The registry id, which is also the settings-file path.
    pub id: &'static str,
    /// The declaration the row's widget comes from.
    pub spec: &'static SettingSpec,
    /// The value in force when the snapshot was taken.
    pub value: SettingValue,
    /// The layer that supplied the value.
    pub provenance: Provenance,
}

impl Row {
    /// Where this row's value came from, as one line.
    pub fn source(&self) -> String {
        self.provenance.label()
    }

    /// The value as it appears on the row.
    pub fn text(&self) -> String {
        self.value_cow().into_owned()
    }

    fn value_cow(&self) -> Cow<'_, str> {
        match &self.value {
            SettingValue::Bool(value) => Cow::Borrowed(if *value { "true" } else { "false" }),
            SettingValue::Str(value) => Cow::Borrowed(value.as_str()),
            SettingValue::Number(value) => Cow::Owned(value.to_string()),
            SettingValue::List(items) => match items.as_slice() {
                [] => Cow::Borrowed(""),
                [single] => Cow::Borrowed(single.as_str()),
                _ => Cow::Owned(items.join(", ")),
            },
        }
    }

    fn source_cow(&self) -> Cow<'static, str> {
        match &self.provenance {
            Provenance::Default => Cow::Borrowed("  default"),
            Provenance::Flag => Cow::Borrowed("  command line"),
            _ => Cow::Owned(format!("  {}", self.provenance)),
        }
    }
}

/// What one keystroke asked the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The overlay consumed the key.
    Handled,
    /// Esc in the browse mode: close the overlay.
    Closed,
    /// A widget was confirmed; write and re-apply this value.
    Change(SettingChange),
    /// `r` on the daemon tab: ask the harness for the values again.
    Reload,
}

/// The editing state of the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// Moving between rows and tabs.
    Browse,
    /// Typing the search line, which narrows the rows and edits no value.
    Search,
    /// Choosing from a closed list.
    Pick { choices: Vec<String>, index: usize },
    /// Editing one line of text.
    Edit { buffer: String, error: Option<String> },
}

/// The `/settings` overlay.
pub struct SettingsOverlay {
    tabs: Vec<&'static str>,
    tab: usize,
    row: usize,
    rows: Vec<Row>,
    tab_rows: Vec<usize>,
    mode: Mode,
    /// What the search line holds. Empty means no filter is applied, so the
    /// rows are the selected tab's.
    query: String,
    /// The daemon values, as last reported, and why there are none.
    daemon: DaemonSettings,
    /// The selected row of the daemon tab, which is not a registry row.
    daemon_row: usize,
    themes_dir: PathBuf,
    theme_choices: Vec<String>,
}

impl SettingsOverlay {
    /// An overlay listing the built-in themes plus the `*.json` files in
    /// `themes_dir`, initialized with default settings.
    pub fn new(themes_dir: PathBuf) -> Self {
        Self::with_settings(themes_dir, &Settings::defaults())
    }

    /// An overlay seeded directly from `settings`, reading `themes_dir` once.
    pub fn with_settings(themes_dir: PathBuf, settings: &Settings) -> Self {
        let mut overlay = Self {
            tabs: tab_names(),
            tab: 0,
            row: 0,
            rows: Vec::with_capacity(settings::registry::REGISTRY.len()),
            tab_rows: Vec::with_capacity(settings::registry::REGISTRY.len()),
            mode: Mode::Browse,
            query: String::new(),
            daemon: DaemonSettings::default(),
            daemon_row: 0,
            themes_dir,
            theme_choices: Vec::new(),
        };
        overlay.refresh(settings);
        overlay
    }

    /// Replace the snapshot with the settings in force.
    ///
    /// The selection and an open editor survive, so a reload while the overlay
    /// is open does not move the row the user is looking at.
    pub fn refresh(&mut self, settings: &Settings) {
        self.rows.clear();
        self.rows.extend(settings::registry::REGISTRY.iter().map(|spec| Row {
            id: spec.id,
            spec,
            value: match settings.get(spec.id) {
                Some(entry) => entry.value.clone(),
                None => spec.default_value(),
            },
            provenance: settings.provenance(spec.id).cloned().unwrap_or(Provenance::Default),
        }));
        self.theme_choices = theme_choices(&self.themes_dir);
        self.rebuild_tab();
    }

    /// Replace the daemon snapshot, keeping the selection where it was.
    pub fn set_daemon(&mut self, daemon: DaemonSettings) {
        self.daemon = daemon;
        self.daemon_row = 0;
        if self.on_daemon_tab() && self.query.is_empty() {
            self.rebuild_tab();
        }
    }

    /// The tabs: the registry's own, then the read-only daemon tab.
    pub fn tabs(&self) -> &[&'static str] {
        &self.tabs
    }

    /// Whether the selected tab is the daemon's own.
    pub fn on_daemon_tab(&self) -> bool {
        self.tab_name() == Some(DAEMON_TAB)
    }

    /// The daemon rows, which are drawn on their own tab.
    pub fn daemon(&self) -> &DaemonSettings {
        &self.daemon
    }

    /// The selected tab's name.
    pub fn tab_name(&self) -> Option<&'static str> {
        self.tabs.get(self.tab).copied()
    }

    /// The selected tab's rows, in registry order.
    ///
    /// Empty on the daemon tab, where the rows come from [`Self::daemon`] and
    /// none of them is a value this client can change. A search lists the
    /// registry's matches instead, so a row can come from a tab that is not
    /// the one last selected.
    pub fn rows(&self) -> Vec<&Row> {
        self.tab_rows.iter().filter_map(|index| self.rows.get(*index)).collect()
    }

    /// The daemon row the selection is on, when the daemon tab is open.
    pub fn selected_daemon(&self) -> Option<&DaemonRow> {
        if !self.on_daemon_tab() {
            return None;
        }
        self.daemon.rows().get(self.daemon_row)
    }

    /// The row the selection is on.
    pub fn selected(&self) -> Option<&Row> {
        self.tab_rows.get(self.row).and_then(|index| self.rows.get(*index))
    }

    /// The value shown for `id`, whichever tab it belongs to.
    pub fn value(&self, id: &str) -> Option<&SettingValue> {
        self.rows.iter().find(|row| row.id == id).map(|row| &row.value)
    }

    /// Where the value of `id` came from, whichever tab it belongs to.
    pub fn provenance(&self, id: &str) -> Option<&Provenance> {
        self.rows.iter().find(|row| row.id == id).map(|row| &row.provenance)
    }

    /// Whether an editor or pick list is open.
    pub fn is_editing(&self) -> bool {
        matches!(self.mode, Mode::Pick { .. } | Mode::Edit { .. })
    }

    /// The inline error from a rejected edit, if there is one.
    pub fn error(&self) -> Option<&str> {
        match &self.mode {
            Mode::Edit { error, .. } => error.as_deref(),
            Mode::Browse | Mode::Search | Mode::Pick { .. } => None,
        }
    }

    /// The choices the open pick list offers.
    pub fn choices(&self) -> Option<&[String]> {
        match &self.mode {
            Mode::Pick { choices, .. } => Some(choices),
            Mode::Browse | Mode::Search | Mode::Edit { .. } => None,
        }
    }

    /// The chosen index of the open pick list.
    pub fn pick_index(&self) -> Option<usize> {
        match &self.mode {
            Mode::Pick { index, .. } => Some(*index),
            Mode::Browse | Mode::Search | Mode::Edit { .. } => None,
        }
    }

    /// Select a tab by name.
    pub fn select_tab(&mut self, name: &str) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| *tab == name) else {
            return false;
        };
        if self.tab != index || !self.query.is_empty() {
            self.tab = index;
            self.row = 0;
            self.mode = Mode::Browse;
            // Naming a tab asks for that tab's rows, so a filter in force is
            // dropped rather than left drawing another tab's matches.
            self.query.clear();
            self.rebuild_tab();
        }
        true
    }

    /// Select a row by id, switching tabs when the id lives on another one.
    pub fn select_id(&mut self, id: &str) -> bool {
        let Some(index) = self.rows.iter().position(|row| row.id == id) else {
            return false;
        };
        // The row may be hidden by a filter, and a caller naming it wants it
        // selected, so the filter goes first.
        if !self.query.is_empty() {
            self.clear_query();
        }
        let tab = self.rows[index].spec.ui.tab;
        if self.select_tab(tab) || self.tab_name() == Some(tab) {
            self.row = self
                .tab_rows
                .iter()
                .position(|candidate| *candidate == index)
                .unwrap_or(0);
            return true;
        }
        false
    }

    /// Translate one keystroke.
    ///
    /// Every key is consumed: the editor under the overlay must not receive a
    /// letter meant for a value or for the search line.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => return self.escape(),
            KeyCode::Enter => return self.confirm(),
            _ => {}
        }
        if matches!(self.mode, Mode::Search) {
            match key.code {
                KeyCode::Backspace => {
                    self.query.pop();
                    self.restart_search();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.push(character);
                    self.restart_search();
                }
                KeyCode::Up => self.step(-1),
                KeyCode::Down => self.step(1),
                _ => {}
            }
            return Outcome::Handled;
        }
        if let Mode::Edit { buffer, error } = &mut self.mode {
            match key.code {
                KeyCode::Backspace => {
                    buffer.pop();
                    *error = None;
                    return Outcome::Handled;
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buffer.push(character);
                    *error = None;
                    return Outcome::Handled;
                }
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {}
                _ => return Outcome::Handled,
            }
        }
        match key.code {
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            // `tab` moves forward through the header and `shift+tab` back,
            // which is the order the tabs are drawn in: the two arrow keys keep
            // the same meaning.
            KeyCode::Left | KeyCode::BackTab => self.left(),
            KeyCode::Right | KeyCode::Tab => self.right(),
            // `/` opens the search line, and only from the row list: an open
            // pick list keeps the key, so a query never throws away a choice.
            // No other printable key starts one, so a stray character on the
            // list does nothing instead of changing what the screen shows.
            KeyCode::Char('/')
                if !key.modifiers.contains(KeyModifiers::CONTROL) && matches!(self.mode, Mode::Browse) =>
            {
                self.mode = Mode::Search;
            }
            // `r` asks for the daemon's report again, and only on its own tab:
            // every other tab edits a local file, which `r` would have nothing
            // to reread. A filter takes the screen off that tab, so it also
            // takes `r` with it.
            KeyCode::Char('r')
                if self.on_daemon_tab()
                    && self.query.is_empty()
                    && matches!(self.mode, Mode::Browse)
                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                return Outcome::Reload;
            }
            _ => {}
        }
        Outcome::Handled
    }

    /// Leave the open widget, then the filter, and only then close the screen.
    ///
    /// A value being edited is abandoned first, so an `esc` that changes the
    /// screen never throws away what the row still shows. A filter is cleared
    /// next: closing the screen is the last thing `esc` does, which keeps a
    /// half-typed query from taking the screen with it.
    fn escape(&mut self) -> Outcome {
        match self.mode {
            Mode::Pick { .. } | Mode::Edit { .. } => {
                self.mode = Mode::Browse;
                Outcome::Handled
            }
            Mode::Search => {
                self.mode = Mode::Browse;
                self.clear_query();
                Outcome::Handled
            }
            Mode::Browse => {
                if self.query.is_empty() {
                    return Outcome::Closed;
                }
                self.clear_query();
                Outcome::Handled
            }
        }
    }

    /// Left: move inside a pick list, otherwise to the previous tab.
    fn left(&mut self) {
        if !self.pick_step(-1) && matches!(self.mode, Mode::Browse) {
            self.switch_tab(-1);
        }
    }

    /// Right: move inside a pick list, otherwise to the next tab.
    fn right(&mut self) {
        if !self.pick_step(1) && matches!(self.mode, Mode::Browse) {
            self.switch_tab(1);
        }
    }

    /// Move the open pick list's selection; false when no list is open.
    fn pick_step(&mut self, delta: i32) -> bool {
        let Mode::Pick { choices, index } = &mut self.mode else {
            return false;
        };
        let last = choices.len().saturating_sub(1);
        *index = if delta < 0 {
            index.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            (*index + delta as usize).min(last)
        };
        true
    }

    /// Move the selection within the rows or the open pick list.
    ///
    /// The daemon tab has no registry row, so its selection is the daemon
    /// report's: moving it here keeps the two from sharing an index that means
    /// different things on different tabs.
    fn step(&mut self, delta: i32) {
        if self.on_daemon_tab() && self.query.is_empty() && matches!(self.mode, Mode::Browse) {
            let length = self.daemon.rows().len();
            self.daemon_row = stepped(self.daemon_row, length, delta);
            return;
        }
        let (length, position) = match &mut self.mode {
            Mode::Browse | Mode::Search => (self.tab_rows.len(), &mut self.row),
            Mode::Pick { choices, index } => (choices.len(), index),
            Mode::Edit { .. } => return,
        };
        if length == 0 {
            *position = 0;
            return;
        }
        *position = stepped(*position, length, delta);
        self.follow_selection();
    }

    /// Move to the previous or next tab, wrapping around.
    ///
    /// A filter already decides which rows are on screen and which tab the
    /// header names, so the tab keys do nothing until it is cleared.
    fn switch_tab(&mut self, delta: i32) {
        if self.tabs.is_empty() || !self.query.is_empty() {
            return;
        }
        let count = self.tabs.len();
        self.tab = if delta < 0 {
            (self.tab + count - 1) % count
        } else {
            (self.tab + 1) % count
        };
        self.row = 0;
        self.mode = Mode::Browse;
        self.rebuild_tab();
    }

    /// A new query reads from the top of its own result.
    fn restart_search(&mut self) {
        self.row = 0;
        self.rebuild_tab();
    }

    /// Drop the filter and show the selected tab's rows again.
    fn clear_query(&mut self) {
        if self.query.is_empty() {
            return;
        }
        self.query.clear();
        self.row = 0;
        self.rebuild_tab();
    }

    /// Rebuild the visible row indices: the selected tab's, or every match
    /// while a query is in force.
    ///
    /// The daemon tab holds no registry row: its length is the server's report,
    /// so the selection is clamped to that instead and every registry row is
    /// hidden while it is open. A query outranks it, because the filter
    /// searches the registry's rows and none of the daemon's.
    fn rebuild_tab(&mut self) {
        if self.on_daemon_tab() && self.query.is_empty() {
            self.tab_rows.clear();
            self.row = 0;
            self.daemon_row = match self.daemon.rows().len() {
                0 => 0,
                length => self.daemon_row.min(length - 1),
            };
            return;
        }
        self.tab_rows.clear();
        if self.query.is_empty() {
            let tab = self.tab_name();
            self.tab_rows.extend(
                self.rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| Some(row.spec.ui.tab) == tab)
                    .map(|(index, _)| index),
            );
        } else {
            // Best hit first, so the selection a query lands on is the row it
            // describes best rather than the first one that happens to share
            // its letters. Equal scores keep registry order, which is why a
            // one-letter query still reads top to bottom.
            let needle = self.query.to_lowercase();
            let mut ranked: Vec<(i32, usize)> = Vec::with_capacity(self.rows.len());
            for (index, row) in self.rows.iter().enumerate() {
                if let Some(score) = match_score(&needle, row) {
                    ranked.push((score, index));
                }
            }
            ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
            self.tab_rows.extend(ranked.into_iter().map(|(_, index)| index));
        }
        self.row = match self.tab_rows.len() {
            0 => 0,
            length => self.row.min(length - 1),
        };
        self.follow_selection();
    }

    /// Name the tab the selected match lives on, so the header says where the
    /// row a filter landed on came from.
    fn follow_selection(&mut self) {
        if self.query.is_empty() {
            return;
        }
        let Some(tab) = self.selected().map(|row| row.spec.ui.tab) else {
            return;
        };
        if let Some(index) = self.tabs.iter().position(|candidate| *candidate == tab) {
            self.tab = index;
        }
    }

    /// Act on the selected row: open its widget, or settle it.
    ///
    /// Nothing on the daemon tab opens a widget, because the client cannot
    /// write a daemon value: `Enter` there is read as "nothing to do" rather
    /// than reported as a failure.
    fn confirm(&mut self) -> Outcome {
        // Enter in the search line hands the keys back to the rows without
        // touching the setting the filter landed on: a search types a query,
        // it does not change a value.
        if matches!(self.mode, Mode::Search) {
            self.mode = Mode::Browse;
            return Outcome::Handled;
        }
        if self.on_daemon_tab() && self.query.is_empty() {
            return Outcome::Handled;
        }
        let Some((id, spec, current, text)) = self
            .selected()
            .map(|row| (row.id, row.spec, row.value.clone(), row.text()))
        else {
            return Outcome::Handled;
        };
        match self.mode.clone() {
            Mode::Browse => match spec.kind {
                SettingKind::Bool => {
                    let value = SettingValue::Bool(!matches!(current, SettingValue::Bool(true)));
                    self.settle(id, spec, value)
                }
                SettingKind::Enum(values) => {
                    let choices: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
                    self.mode = Mode::Pick {
                        index: choice_index(&choices, &current),
                        choices,
                    };
                    Outcome::Handled
                }
                SettingKind::Text if id == THEME_ID => {
                    let choices = self.theme_choices.clone();
                    let index = choice_index(&choices, &current);
                    self.mode = Mode::Pick { index, choices };
                    Outcome::Handled
                }
                SettingKind::Text | SettingKind::Number | SettingKind::Array => {
                    self.mode = Mode::Edit {
                        buffer: text,
                        error: None,
                    };
                    Outcome::Handled
                }
            },
            Mode::Pick { choices, index } => match choices.get(index).cloned() {
                Some(choice) => self.settle(id, spec, SettingValue::Str(choice)),
                None => {
                    self.mode = Mode::Browse;
                    Outcome::Handled
                }
            },
            Mode::Edit { buffer, .. } => match edit_value(spec, &buffer) {
                Some(value) => self.settle(id, spec, value),
                None => {
                    self.mode = Mode::Edit {
                        buffer,
                        error: Some(edit_error(spec)),
                    };
                    Outcome::Handled
                }
            },
            // Handled above, before a row is read at all.
            Mode::Search => Outcome::Handled,
        }
    }

    /// Close the widget and report a value, once the declaration accepts it.
    fn settle(&mut self, id: &'static str, spec: &'static SettingSpec, value: SettingValue) -> Outcome {
        if !spec.accepts(&value) {
            self.mode = Mode::Edit {
                buffer: raw_text(&value),
                error: Some(format!("{} needs {}", spec.ui.label, spec.kind.label())),
            };
            return Outcome::Handled;
        }
        self.mode = Mode::Browse;
        Outcome::Change(SettingChange { id, value })
    }
}

/// One step within a list of `length` rows, clamped at both ends.
fn stepped(position: usize, length: usize, delta: i32) -> usize {
    if length == 0 {
        return 0;
    }
    let last = length - 1;
    if delta < 0 {
        position.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        (position + delta as usize).min(last)
    }
}

/// How well a lowercased query matches this row, or nothing when it does not.
///
/// A row is searched by its visible label, its key and its description, each as
/// a case-insensitive substring: the three fields spell out what a reader would
/// type, so `wheel` finds `Wheel scrolls`, `context` finds
/// `statusline.context_window`, and a word only a key or a description carries
/// finds its row on a tab that is not open. The best field decides the rank, so
/// the row a query lands on is the one whose name it matches before the ones it
/// only shares a key or a sentence with.
fn contains_case_insensitive(haystack: &str, lower_needle: &str) -> bool {
    if lower_needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && lower_needle.is_ascii() {
        let needle = lower_needle.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle));
    }
    haystack.to_lowercase().contains(lower_needle)
}

fn match_score(needle: &str, row: &Row) -> Option<i32> {
    if contains_case_insensitive(row.spec.ui.label, needle) {
        Some(3)
    } else if contains_case_insensitive(row.id, needle) {
        Some(2)
    } else if contains_case_insensitive(row.spec.ui.description, needle) {
        Some(1)
    } else {
        None
    }
}

/// The tabs in registry order, each named once, then the daemon tab.
///
/// The daemon tab is last and is not a registry tab: its rows come from the
/// server, so nothing in the registry can add or remove it.
fn tab_names() -> Vec<&'static str> {
    let mut tabs: Vec<&'static str> = Vec::new();
    for spec in settings::registry::REGISTRY {
        if !tabs.contains(&spec.ui.tab) {
            tabs.push(spec.ui.tab);
        }
    }
    tabs.push(DAEMON_TAB);
    tabs
}

/// The position of the current value inside a choice list.
fn choice_index(choices: &[String], value: &SettingValue) -> usize {
    match value {
        SettingValue::Str(current) => choices.iter().position(|choice| choice == current).unwrap_or(0),
        SettingValue::Bool(_) | SettingValue::Number(_) | SettingValue::List(_) => 0,
    }
}

/// How a value is written when it is not being edited.
fn raw_text(value: &SettingValue) -> String {
    match value {
        SettingValue::Bool(value) => value.to_string(),
        SettingValue::Str(value) => value.clone(),
        SettingValue::Number(value) => value.to_string(),
        SettingValue::List(items) => items.join(", "),
    }
}

/// Turn an editor line back into the value its declaration promises, or nothing
/// when the text is not that kind.
fn edit_value(spec: &SettingSpec, buffer: &str) -> Option<SettingValue> {
    match spec.kind {
        SettingKind::Bool | SettingKind::Enum(_) => None,
        SettingKind::Text => Some(SettingValue::Str(buffer.to_string())),
        SettingKind::Number => match buffer.trim().parse::<i64>() {
            Ok(number) => Some(SettingValue::Number(number)),
            Err(_) => None,
        },
        SettingKind::Array => Some(SettingValue::List(
            buffer
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect(),
        )),
    }
}

/// What an editor says when its line is not the declared kind.
fn edit_error(spec: &SettingSpec) -> String {
    if matches!(spec.kind, SettingKind::Number) {
        format!("{} needs a whole number", spec.ui.label)
    } else {
        format!("{} needs {}", spec.ui.label, spec.kind.label())
    }
}

/// The theme names the pick list offers: `dark` and `light`, then the user
/// files in `dir`, sorted.
fn theme_choices(dir: &std::path::Path) -> Vec<String> {
    let mut choices: Vec<String> = ["dark", "light"].iter().map(|name| (*name).to_string()).collect();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return choices;
    };
    let mut user: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem != "dark" && stem != "light" && !user.iter().any(|existing| existing == stem) {
            user.push(stem.to_string());
        }
    }
    user.sort();
    choices.append(&mut user);
    choices
}

/// Narrowest pane the panel is drawn in; below it the overlay says where it is.
pub const MIN_WIDTH: u16 = 12;
/// Shortest pane the panel is drawn in.
pub const MIN_HEIGHT: u16 = 5;

/// Where the panel is drawn inside `area`.
///
/// It is narrower than the pane so the screen around it still shows where the
/// client is, and it stops two columns short of the pane's edges so its border
/// is never written into the terminal's last cell, which some terminals drop.
pub fn panel(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(76);
    let height = area.height.saturating_sub(2);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The daemon tab's rows, in the order the server reported them.
///
/// A row prints the label the server sent, its value, and where the value came
/// from. None of the three is computed here: the client draws what it was told,
/// which is what keeps this tab honest about values it cannot set.
fn daemon_lines(overlay: &SettingsOverlay, theme: &Theme) -> Vec<Line<'static>> {
    if overlay.daemon().rows().is_empty() {
        // Every state without rows says which one it is: "not read yet",
        // "this daemon cannot say", and "it said nothing" read differently,
        // and a blank table would look like the third when it may be the first.
        return vec![Line::from(Span::styled(
            format!(" {}", overlay.daemon().notice()),
            Style::default().fg(theme.core.muted),
        ))];
    }
    overlay
        .daemon()
        .rows()
        .iter()
        .enumerate()
        .map(|(position, row)| {
            let chosen = position == overlay.daemon_row;
            let base = if chosen {
                Style::default()
                    .fg(theme.core.text)
                    .bg(theme.prompt.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.core.text)
            };
            let origin = if chosen {
                Style::default()
                    .fg(theme.core.text)
                    .bg(theme.prompt.selected_bg)
                    .add_modifier(Modifier::ITALIC)
            } else {
                Style::default().fg(theme.core.muted)
            };
            Line::from(vec![
                Span::styled(format!("  {:<width$} ", row.label, width = DAEMON_LABEL_WIDTH), base),
                Span::styled(row.value.clone(), base),
                Span::styled(format!("  {}", row.source_label()), origin),
            ])
        })
        .collect()
}

/// The daemon tab's footer: what the row is, and the two keys it answers to.
fn draw_daemon_footer(frame: &mut Frame<'_>, area: Rect, overlay: &SettingsOverlay, theme: &Theme) {
    let description = match overlay.daemon() {
        DaemonSettings::Unavailable(reason) | DaemonSettings::Failed(reason) => reason.clone(),
        DaemonSettings::Unknown | DaemonSettings::Loaded(_) => overlay
            .selected_daemon()
            .map(|row| format!("{} — read-only, from the daemon", row.key))
            .unwrap_or_else(|| overlay.daemon().notice().to_string()),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(" {description}"),
                Style::default().fg(theme.core.text),
            )),
            Line::from(Span::styled(
                " r reload  up/down move  tab switch  esc close",
                Style::default().fg(theme.core.muted),
            )),
        ]),
        area,
    );
}

/// Draw the overlay over `area`, returning where an open editor wants the
/// cursor.
pub fn draw(
    frame: &mut Frame<'_>,
    area: Rect,
    overlay: &SettingsOverlay,
    theme: &Theme,
    symbols: Symbols,
) -> Option<(u16, u16)> {
    // The overlay is modal, so the conversation behind it is not being read.
    // A transcript character left beside the panel reads as part of the screen,
    // so the whole pane is cleared and the panel is drawn on the blank area.
    frame.render_widget(Clear, area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        // No room for a border and one row. The overlay still owns every key,
        // so it says it is there instead of disappearing and swallowing them.
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " settings hidden: window too small, esc closes",
                Style::default().fg(theme.core.muted),
            ))),
            area,
        );
        return None;
    }
    let panel = panel(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.core.border))
        .title(Span::styled(
            format!(" {} Settings ", symbols.bullet),
            Style::default().fg(theme.core.accent),
        ));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width == 0 || inner.height < 3 {
        return None;
    }

    let [tabs_area, body_area, footer_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(2)]).areas(inner);

    let searching = !overlay.query.is_empty();
    let mut tabs: Vec<Span<'_>> = Vec::with_capacity(overlay.tabs.len() * 4 + if searching { 2 } else { 0 });
    for (index, tab) in overlay.tabs.iter().enumerate() {
        let style = if index == overlay.tab {
            Style::default()
                .fg(theme.core.accent)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().fg(theme.core.muted)
        };
        tabs.push(Span::styled(" ", style));
        tabs.push(Span::styled(*tab, style));
        tabs.push(Span::styled(" ", style));
        tabs.push(Span::raw(" "));
    }
    // A filter outlives the search line, and it decides which rows are on
    // screen, so the header keeps it in view: otherwise a short list with the
    // matching tab highlighted would read as the whole tab.
    if searching {
        let style = Style::default().fg(theme.core.text).add_modifier(Modifier::BOLD);
        tabs.push(Span::styled("/", style));
        tabs.push(Span::styled(overlay.query.as_str(), style));
    }
    frame.render_widget(Line::from(tabs), tabs_area);

    let visible = body_area.height as usize;
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(overlay.tab_rows.len().saturating_add(8));
    let mut selected_line = 0usize;
    if overlay.on_daemon_tab() && !searching {
        // The daemon tab is a read-only table, so it draws its own rows and
        // footer and returns before the registry path can draw a widget over
        // a value no client can change. A filter outranks it, since the query
        // names registry rows and leaves this tab with nothing to show.
        let lines = daemon_lines(overlay, theme);
        let offset = match visible {
            0 => 0,
            _ if overlay.daemon_row < visible => 0,
            _ => (overlay.daemon_row + 1 - visible) as u16,
        };
        frame.render_widget(Paragraph::new(lines).scroll((offset, 0)), body_area);
        draw_daemon_footer(frame, footer_area, overlay, theme);
        return None;
    }
    const SPACES_20: &str = "                    ";
    let mut group: Option<(&'static str, &'static str)> = None;
    for (position, row) in overlay
        .tab_rows
        .iter()
        .filter_map(|&index| overlay.rows.get(index))
        .enumerate()
    {
        // A filter puts rows from every tab in one list, so the header names
        // the tab as well: one `Content` heading under `Transcript` and another
        // under `Editor` would read as the same group twice.
        if group != Some((row.spec.ui.tab, row.spec.ui.group)) {
            let group_style = Style::default().fg(theme.core.muted).add_modifier(Modifier::BOLD);
            let line = if searching {
                Line::from(Span::styled(
                    format!(" {} · {}", row.spec.ui.tab, row.spec.ui.group),
                    group_style,
                ))
            } else {
                Line::from(vec![
                    Span::styled(" ", group_style),
                    Span::styled(row.spec.ui.group, group_style),
                ])
            };
            lines.push(line);
            group = Some((row.spec.ui.tab, row.spec.ui.group));
        }
        let chosen = position == overlay.row;
        if chosen {
            selected_line = lines.len();
        }
        let base = if chosen {
            Style::default()
                .fg(theme.core.text)
                .bg(theme.prompt.selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.core.text)
        };
        let origin = if chosen {
            Style::default()
                .fg(theme.core.text)
                .bg(theme.prompt.selected_bg)
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default().fg(theme.core.muted)
        };
        let label = row.spec.ui.label;
        let value = Span::styled(row.value_cow(), base);
        let source = Span::styled(row.source_cow(), origin);
        let spans = if label.is_ascii() && label.len() <= 20 {
            vec![
                Span::styled("  ", base),
                Span::styled(label, base),
                Span::styled(&SPACES_20[..20 - label.len()], base),
                value,
                source,
            ]
        } else {
            vec![Span::styled(format!("  {label:<20}"), base), value, source]
        };
        lines.push(Line::from(spans));
    }
    if lines.is_empty() && searching {
        // A filter with no hit is a result, not a blank panel: the screen says
        // the query kept nothing so the row area reads as empty on purpose.
        lines.push(Line::from(Span::styled(
            format!(" nothing matches \"{}\"", overlay.query),
            Style::default().fg(theme.core.muted),
        )));
    }
    let offset = if visible == 0 || selected_line < visible {
        0
    } else {
        (selected_line + 1 - visible) as u16
    };
    frame.render_widget(Paragraph::new(lines).scroll((offset, 0)), body_area);

    let mut cursor = None;
    match &overlay.mode {
        Mode::Browse => {
            let description = overlay
                .selected()
                .map(|row| row.spec.ui.description)
                .unwrap_or(if searching {
                    "no setting matches that search"
                } else {
                    "no settings are declared"
                });
            // A filter that is still in force says so, so a short list reads as
            // the query's result rather than as the whole tab, and `esc` says
            // what it does next: clear the filter, not close the screen.
            let hint = if searching {
                " / edit filter  Enter change  up/down move  esc clears filter"
            } else {
                " / search  Enter change  up/down move  tab switch  esc close"
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(description, Style::default().fg(theme.core.text)),
                    ]),
                    Line::from(Span::styled(hint, Style::default().fg(theme.core.muted))),
                ]),
                footer_area,
            );
        }
        Mode::Search => {
            let description = overlay
                .selected()
                .map(|row| row.spec.ui.description)
                .unwrap_or("no setting matches that search");
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(description, Style::default().fg(theme.core.text)),
                    ]),
                    Line::from(vec![
                        Span::styled(" /", Style::default().fg(theme.core.text)),
                        Span::styled(overlay.query.as_str(), Style::default().fg(theme.core.text)),
                        Span::styled("  enter keep filter  esc clear", Style::default().fg(theme.core.muted)),
                    ]),
                ]),
                footer_area,
            );
            // The query is typed on the footer's second line, after the ` /`,
            // so the cursor follows the characters already typed.
            let query_width = unicode_width::UnicodeWidthStr::width(overlay.query.as_str());
            let x = footer_area
                .x
                .saturating_add(2)
                .saturating_add(u16::try_from(query_width).unwrap_or(u16::MAX));
            cursor = Some((x, footer_area.y.saturating_add(1)));
        }
        Mode::Pick { choices, index } => {
            let mut spans = vec![Span::styled(" choose ", Style::default().fg(theme.core.muted))];
            for (position, choice) in choices.iter().enumerate() {
                let style = if position == *index {
                    Style::default()
                        .fg(theme.core.text)
                        .bg(theme.prompt.selected_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.core.muted)
                };
                spans.push(Span::styled(format!(" {choice} "), style));
            }
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(spans),
                    Line::from(Span::styled(
                        " enter confirm  esc back",
                        Style::default().fg(theme.core.muted),
                    )),
                ]),
                footer_area,
            );
        }
        Mode::Edit { buffer, error } => {
            let (hint, style) = match error {
                Some(error) => (error.as_str(), Style::default().fg(theme.core.error)),
                None => (" enter save  esc cancel", Style::default().fg(theme.core.muted)),
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(buffer.as_str(), Style::default().fg(theme.core.text)),
                    ]),
                    Line::from(vec![Span::raw(" "), Span::styled(hint, style)]),
                ]),
                footer_area,
            );
            let x = footer_area
                .x
                .saturating_add(1)
                .saturating_add(u16::try_from(buffer.chars().count()).unwrap_or(u16::MAX));
            cursor = Some((x, footer_area.y));
        }
    }
    cursor
}

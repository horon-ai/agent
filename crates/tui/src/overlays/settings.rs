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

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::settings::{self, Provenance, SettingKind, SettingSpec, SettingValue, Settings};
use crate::theme::{Symbols, Theme};

/// The id whose values are theme files rather than an enum.
const THEME_ID: &str = "theme";

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
        match &self.value {
            SettingValue::Bool(value) => value.to_string(),
            SettingValue::Str(value) => value.clone(),
            SettingValue::Number(value) => value.to_string(),
            SettingValue::List(items) => items.join(", "),
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
}

/// The editing state of the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// Moving between rows and tabs.
    Browse,
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
    themes_dir: PathBuf,
    theme_choices: Vec<String>,
}

impl SettingsOverlay {
    /// An overlay listing the built-in themes plus the `*.json` files in
    /// `themes_dir`.
    pub fn new(themes_dir: PathBuf) -> Self {
        let theme_choices = theme_choices(&themes_dir);
        let mut overlay = Self {
            tabs: tab_names(),
            tab: 0,
            row: 0,
            rows: Vec::new(),
            tab_rows: Vec::new(),
            mode: Mode::Browse,
            themes_dir,
            theme_choices,
        };
        overlay.refresh(&Settings::defaults());
        overlay
    }

    /// Replace the snapshot with the settings in force.
    ///
    /// The selection and an open editor survive, so a reload while the overlay
    /// is open does not move the row the user is looking at.
    pub fn refresh(&mut self, settings: &Settings) {
        self.rows = settings::registry::REGISTRY
            .iter()
            .map(|spec| Row {
                id: spec.id,
                spec,
                value: match settings.get(spec.id) {
                    Some(entry) => entry.value.clone(),
                    None => spec.default_value(),
                },
                provenance: settings.provenance(spec.id).cloned().unwrap_or(Provenance::Default),
            })
            .collect();
        self.theme_choices = theme_choices(&self.themes_dir);
        self.rebuild_tab();
    }

    /// The tabs, in registry order.
    pub fn tabs(&self) -> &[&'static str] {
        &self.tabs
    }

    /// The selected tab's name.
    pub fn tab_name(&self) -> Option<&'static str> {
        self.tabs.get(self.tab).copied()
    }

    /// The selected tab's rows, in registry order.
    pub fn rows(&self) -> Vec<&Row> {
        self.tab_rows.iter().filter_map(|index| self.rows.get(*index)).collect()
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
        !matches!(self.mode, Mode::Browse)
    }

    /// The inline error from a rejected edit, if there is one.
    pub fn error(&self) -> Option<&str> {
        match &self.mode {
            Mode::Edit { error, .. } => error.as_deref(),
            Mode::Browse | Mode::Pick { .. } => None,
        }
    }

    /// The choices the open pick list offers.
    pub fn choices(&self) -> Option<&[String]> {
        match &self.mode {
            Mode::Pick { choices, .. } => Some(choices),
            Mode::Browse | Mode::Edit { .. } => None,
        }
    }

    /// The chosen index of the open pick list.
    pub fn pick_index(&self) -> Option<usize> {
        match &self.mode {
            Mode::Pick { index, .. } => Some(*index),
            Mode::Browse | Mode::Edit { .. } => None,
        }
    }

    /// Select a tab by name.
    pub fn select_tab(&mut self, name: &str) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| *tab == name) else {
            return false;
        };
        if self.tab != index {
            self.tab = index;
            self.row = 0;
            self.mode = Mode::Browse;
            self.rebuild_tab();
        }
        true
    }

    /// Select a row by id, switching tabs when the id lives on another one.
    pub fn select_id(&mut self, id: &str) -> bool {
        let Some(index) = self.rows.iter().position(|row| row.id == id) else {
            return false;
        };
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
    /// letter meant for a value.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => return self.escape(),
            KeyCode::Enter => return self.confirm(),
            _ => {}
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
            KeyCode::Left | KeyCode::Tab => self.left(),
            KeyCode::Right | KeyCode::BackTab => self.right(),
            _ => {}
        }
        Outcome::Handled
    }

    /// Leave the open widget, or close the overlay when nothing is open.
    fn escape(&mut self) -> Outcome {
        if matches!(self.mode, Mode::Browse) {
            return Outcome::Closed;
        }
        self.mode = Mode::Browse;
        Outcome::Handled
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
    fn step(&mut self, delta: i32) {
        let (length, position) = match &mut self.mode {
            Mode::Browse => (self.tab_rows.len(), &mut self.row),
            Mode::Pick { choices, index } => (choices.len(), index),
            Mode::Edit { .. } => return,
        };
        if length == 0 {
            *position = 0;
            return;
        }
        let last = length - 1;
        *position = if delta < 0 {
            position.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            (*position + delta as usize).min(last)
        };
    }

    /// Move to the previous or next tab, wrapping around.
    fn switch_tab(&mut self, delta: i32) {
        if self.tabs.is_empty() {
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

    /// Rebuild the selected tab's row indices.
    fn rebuild_tab(&mut self) {
        let tab = self.tab_name();
        self.tab_rows = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| Some(row.spec.ui.tab) == tab)
            .map(|(index, _)| index)
            .collect();
        self.row = match self.tab_rows.len() {
            0 => 0,
            length => self.row.min(length - 1),
        };
    }

    /// Act on the selected row: open its widget, or settle it.
    fn confirm(&mut self) -> Outcome {
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

/// The tabs in registry order, each named once.
fn tab_names() -> Vec<&'static str> {
    let mut tabs: Vec<&'static str> = Vec::new();
    for spec in settings::registry::REGISTRY {
        if !tabs.contains(&spec.ui.tab) {
            tabs.push(spec.ui.tab);
        }
    }
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
        if !choices.contains(&stem.to_string()) && !user.contains(&stem.to_string()) {
            user.push(stem.to_string());
        }
    }
    user.sort();
    choices.append(&mut user);
    choices
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
    if area.width < 12 || area.height < 5 {
        return None;
    }
    let width = area.width.saturating_sub(4).min(76);
    let height = area.height.saturating_sub(2);
    let panel = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, panel);
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

    let mut tabs: Vec<Span<'static>> = Vec::new();
    for (index, tab) in overlay.tabs.iter().enumerate() {
        let style = if index == overlay.tab {
            Style::default()
                .fg(theme.core.accent)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().fg(theme.core.muted)
        };
        tabs.push(Span::styled(format!(" {tab} "), style));
        tabs.push(Span::raw(" "));
    }
    frame.render_widget(Line::from(tabs), tabs_area);

    let visible = body_area.height as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = 0usize;
    let mut group = "";
    for (position, row) in overlay.rows().into_iter().enumerate() {
        if row.spec.ui.group != group {
            group = row.spec.ui.group;
            lines.push(Line::from(Span::styled(
                format!(" {group}"),
                Style::default().fg(theme.core.muted).add_modifier(Modifier::BOLD),
            )));
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
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<20}", row.spec.ui.label), base),
            Span::styled(row.text(), base),
            Span::styled(format!("  {}", row.source()), origin),
        ]));
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
                .unwrap_or("no settings are declared");
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!(" {description}"),
                        Style::default().fg(theme.core.text),
                    )),
                    Line::from(Span::styled(
                        " Enter change  up/down move  tab switch  esc close",
                        Style::default().fg(theme.core.muted),
                    )),
                ]),
                footer_area,
            );
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
                Some(error) => (error.clone(), Style::default().fg(theme.core.error)),
                None => (
                    " enter save  esc cancel".to_string(),
                    Style::default().fg(theme.core.muted),
                ),
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(format!(" {buffer}"), Style::default().fg(theme.core.text))),
                    Line::from(Span::styled(format!(" {hint}"), style)),
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

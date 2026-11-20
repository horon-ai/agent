//! `/help` and the slash popup: the two places the client lists what it can do.
//!
//! Both listings are built here rather than where they are drawn, so the
//! columns come out one width, the popup stays inside the pane, and what the
//! interface says can be tested without a terminal.
//!
//! Every key in the listing comes from the map in force. A listing written as
//! text would keep naming `esc` after the user rebound the action to `ctrl+x`,
//! which is worse than saying nothing: the one place that claims to explain the
//! keys would be wrong.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::commands::{Entry, Group, Registry};
use crate::keys::{ACTIONS, Bindings, ids};
use crate::theme::{Symbols, Theme};

/// Rows the popup may show at once.
///
/// A full list of commands would cover the conversation it is being typed
/// over; eight rows is one screenful of choices next to the draft.
pub const POPUP_ROWS: usize = 8;

/// Spaces between the two columns of a listing.
const GAP: usize = 2;

/// What `/help` says when no command file was found.
const NO_COMMAND_FILES: &str = "no command files; add .horon/commands/<name>.md to a project or \
     $HORON_HOME/commands/<name>.md for every project";

/// The action ids the interface answers to, with the line that explains each.
///
/// The rows are generated from [`ACTIONS`] in its own order, so this table only
/// adds the words; an action missing here still gets a row, without a
/// description.
const ACTION_DESCRIPTIONS: &[(&str, &str)] = &[
    (ids::APP_SUBMIT, "send the typed message"),
    (
        ids::APP_CANCEL,
        "cancel the running turn, or leave when nothing is running",
    ),
    (
        ids::APP_INTERRUPT,
        "interrupt the running turn, or send queued messages into it",
    ),
    (ids::APP_QUIT, "leave"),
    (ids::APP_SCROLL_PAGE_UP, "hold the transcript one page higher"),
    (
        ids::APP_SCROLL_PAGE_DOWN,
        "move the transcript one page back towards the end",
    ),
    (ids::APP_SCROLL_UP, "hold the transcript one row higher"),
    (ids::APP_SCROLL_DOWN, "move the transcript one row back towards the end"),
    (ids::APP_SCROLL_BOTTOM, "follow the end of the conversation again"),
    (
        ids::APP_EXPAND_TOOL,
        "show the whole output of the newest cut tool call, or hide it again",
    ),
    (
        ids::APP_CYCLE_EFFORT,
        "step to the next reasoning effort, wrapping at the last one",
    ),
    (ids::APP_PICK_MODEL, "pick the model this session uses"),
    (ids::APP_COPY_DRAFT, "copy the draft to the terminal clipboard"),
    (
        ids::APP_COPY_LINE,
        "copy the line under the cursor to the terminal clipboard",
    ),
    (ids::APP_COPY_LAST, "copy the last answer to the terminal clipboard"),
    (
        ids::APP_COPY_CODE,
        "copy the newest code block to the terminal clipboard",
    ),
    (ids::EDITOR_BACKSPACE, "delete the character before the cursor"),
    (ids::EDITOR_DELETE, "delete the character at the cursor"),
    (ids::EDITOR_LEFT, "move the cursor one character left"),
    (ids::EDITOR_RIGHT, "move the cursor one character right"),
    (ids::EDITOR_HOME, "move the cursor to the start of its line"),
    (ids::EDITOR_END, "move the cursor to the end of its line"),
    (ids::EDITOR_WORD_LEFT, "move to the start of the word before the cursor"),
    (ids::EDITOR_WORD_RIGHT, "move past the word after the cursor"),
    (
        ids::EDITOR_HISTORY_PREV,
        "step back through earlier drafts, then into the lines of the draft",
    ),
    (
        ids::EDITOR_HISTORY_NEXT,
        "step forward again, back to the draft being typed",
    ),
    (ids::EDITOR_NEWLINE, "start a new line without sending the draft"),
    (
        ids::EDITOR_KILL_TO_START,
        "delete from the start of the line to the cursor",
    ),
    (ids::EDITOR_KILL_TO_END, "delete from the cursor to the end of the line"),
    (ids::EDITOR_DELETE_WORD, "delete the word before the cursor"),
    (
        ids::EDITOR_COMPLETE,
        "fill the popup's selected command, or complete the prefix",
    ),
    (ids::EDITOR_HISTORY_SEARCH, "search the drafts sent before this one"),
    (ids::PROMPT_ALLOW, "approve the parked tool call once"),
    (
        ids::PROMPT_ALLOW_ALWAYS,
        "approve this call and every later call like it",
    ),
    (ids::PROMPT_DENY, "deny the parked tool call"),
    (ids::PROMPT_DISMISS, "deny the parked call; a question stays open"),
    (ids::PROMPT_CONFIRM, "confirm a multi-choice question"),
];

/// The line that explains one built-in UI command.
pub(crate) fn local_description(name: &str) -> &'static str {
    match name {
        "clear" => "clear the transcript",
        "copy" => "copy a recent answer or code block to the terminal clipboard, or list them",
        "effort" => "set the reasoning effort, or pick one from the ladder",
        "exit" => "leave, the same as /quit",
        "help" => "list the commands and the keys in force",
        "keys" => "list the keys in force and what each does",
        "model" => "pick a model, or switch with /model <name>",
        "new" => "start a new session in this directory",
        "quit" => "leave",
        "resume" => "replay a stored session, or pick one from the list",
        "settings" => "open the settings screen",
        "skills" => "list the skills the harness reports, and put one in the draft",
        "switch" => "move to another stored session, or pick one from the list",
        _ => "",
    }
}

/// The line shown when a typed name matches no command.
///
/// `/help` and the resolver share it, so a name that runs nothing reads the
/// same whether it was typed as a command or asked about.
pub(crate) fn unknown(name: &str) -> String {
    format!("unknown command `/{name}`; /help lists the commands")
}

/// `/help <command>`: one command's label, description and group.
///
/// `name` carries no leading slash; the caller strips it so a `/model` typed
/// after `/help` reads the same as `model`. A name nothing answers to gives
/// `None`, which the caller shows as [`unknown`] rather than as an empty help
/// block that would look like success.
pub(crate) fn detail(entries: &[Entry], name: &str) -> Option<String> {
    // The command `resolve` would answer, not the first row that happens to
    // carry the name: a command file outranks the harness's own commands, and a
    // built-in outranks both.
    let entry = entries
        .iter()
        .filter(|entry| entry.name == name)
        .min_by_key(|entry| match entry.group {
            Group::Builtin => 0,
            Group::File if !entry.shadowed => 1,
            Group::Harness => 2,
            Group::File => 3,
            Group::Skill => 4,
        })?;
    let description = if entry.description.is_empty() {
        entry.label()
    } else {
        format!("{}  {}", entry.label(), entry.description)
    };
    let mut lines = vec![description, format!("group: {}", header(entry.group))];
    let shadowed = entries.iter().any(|other| other.name == name && other.shadowed);
    if shadowed {
        lines.push(format!(
            "a command file named `{name}` is shadowed: the built-in runs, not the file"
        ));
    }
    Some(lines.join("\n"))
}

/// The line that explains one harness command.
///
/// The names come out of serde from `RuntimeCommand`, so a name this table does
/// not know still lists: it simply shows without a description.
pub(crate) fn harness_description(name: &str) -> &'static str {
    match name {
        "agents" => "list the agent models, and switch one for this session or every session",
        "auth" => "show the login status and the provider profiles",
        "autojudge" => "turn the automatic judge on or off",
        "autoreview" => "turn the automatic review on or off",
        "context" => "show what is filling the context window",
        "continue" => "resume every paused session",
        "fast" => "switch the provider's fast (priority) service tier",
        "info" => "show the session, provider and usage details",
        "initiatives" => "list the open initiatives",
        "judge" => "spawn a judge that checks the work against the request",
        "logout" => "drop the stored provider credentials",
        "memory" => "show the memory status; `memory extract` schedules an extraction",
        "overnight" => "run the overnight work queue",
        "plan" => "enter or leave plan mode",
        "productivity" => "generate the productivity report",
        "refresh_model_list" => "re-read the provider's model catalog",
        "remote" => "show or change the remote access settings",
        "resume_all" => "resume every paused session",
        "review" => "spawn a review of the working tree changes",
        "server_reload" => "upgrade the server in place, keeping its state",
        "subagent" => "spawn a subagent with the task given after the command",
        "subagent_model" => "show or set the model subagents run on",
        "swarm" => "show the swarm and its members",
        "transport" => "show or set the provider transport",
        _ => "",
    }
}

/// One row of a two-column listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The name or key column.
    pub left: String,
    /// What it does; empty when nothing says.
    pub right: String,
}

/// The `/help` text: commands by source, then the keys in force.
pub fn text(entries: &[Entry], bindings: &Bindings) -> String {
    let commands = command_rows(entries);
    let width = column(&commands);
    let mut lines: Vec<String> = Vec::with_capacity(commands.len() + 3);
    let has_files = entries.iter().any(|entry| entry.group == Group::File);
    let mut group = None;
    for (entry, row) in entries.iter().zip(commands.iter()) {
        if group != Some(entry.group) {
            // The notice about command files stands where the file section
            // would be, so a skills section after it still reads as its own.
            if !has_files && entry.group == Group::Skill {
                lines.push(NO_COMMAND_FILES.to_string());
            }
            group = Some(entry.group);
            lines.push(format!("{}:", header(entry.group)));
        }
        let mut line = aligned(row, width);
        if entry.shadowed {
            line.push_str("  (shadowed by the built-in)");
        }
        lines.push(line);
    }
    let has_skills = entries.iter().any(|entry| entry.group == Group::Skill);
    if !has_files && !has_skills {
        lines.push(NO_COMMAND_FILES.to_string());
    }
    let keys = key_text(bindings);
    lines.push(keys);
    lines.join("\n")
}

/// `/keys`: the header and rows of the listing's key section, and nothing else.
///
/// The action ids are the map's own names, not what a user types, so the
/// listing shows the keys in force and the words for what each does; an action
/// nobody bound still gets a row, marked unbound.
pub(crate) fn key_text(bindings: &Bindings) -> String {
    let rows = key_rows(bindings);
    let width = column(&rows);
    let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 1);
    lines.push("keys:".to_string());
    lines.extend(rows.iter().map(|row| aligned(row, width)));
    lines.join("\n")
}

/// Every command as one row, in the order the sections print them.
pub fn command_rows(entries: &[Entry]) -> Vec<Row> {
    entries
        .iter()
        .map(|entry| Row {
            left: entry.label(),
            right: entry.description.clone(),
        })
        .collect()
}

/// Every action as one row: the keys it answers to now, and what it does.
///
/// The order is [`ACTIONS`] order, which is also the order a conflict is settled
/// in, so the listing explains the map as it stands rather than a copy of it.
pub fn key_rows(bindings: &Bindings) -> Vec<Row> {
    ACTIONS
        .iter()
        .map(|spec| {
            let keys = bindings.keys_for(spec.id);
            let mut left = String::new();
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    left.push_str(", ");
                }
                left.push_str(&key.to_string());
            }
            if left.is_empty() {
                left.push_str("(unbound)");
            }
            Row {
                left,
                right: description_for(spec.id),
            }
        })
        .collect()
}

/// The words for one action id.
fn description_for(id: &str) -> String {
    match ACTION_DESCRIPTIONS.iter().find(|(action, _)| *action == id) {
        Some((_, text)) => (*text).to_string(),
        None => String::new(),
    }
}

/// The heading of one group of commands.
fn header(group: Group) -> &'static str {
    match group {
        Group::Builtin => "built-in commands",
        Group::Harness => "harness commands",
        Group::File => "command files",
        Group::Skill => "skills",
    }
}

/// The width the left column of a listing needs.
fn column(rows: &[Row]) -> usize {
    rows.iter()
        .map(|row| UnicodeWidthStr::width(row.left.as_str()))
        .max()
        .unwrap_or(0)
}

/// One row with its description in the column `width` wide.
fn aligned(row: &Row, width: usize) -> String {
    let mut line = String::with_capacity(width + row.right.len() + GAP + 2);
    line.push_str("  ");
    line.push_str(&row.left);
    line.push_str(&" ".repeat(width.saturating_sub(UnicodeWidthStr::width(row.left.as_str()))));
    if !row.right.is_empty() {
        line.push_str(&" ".repeat(GAP));
        line.push_str(&row.right);
    }
    line
}

/// The prefix a draft asks a popup for, if it asks for one.
///
/// Only the first word completes: once the draft has a space the argument
/// position is the user's to fill, and `//text` is the escape for a message that
/// starts with a slash rather than a command.
pub fn prefix(draft: &str) -> Option<&str> {
    let rest = draft.trim_start().strip_prefix('/')?;
    if rest.starts_with('/') || rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

/// The commands a draft offers, best match first.
///
/// A shadowed command file is left out: the built-in answers that name, so
/// offering the file would offer a line that never runs.
///
/// The rank puts the names a prefix still spells out first, in name order, so
/// the rows a typed word used to offer are the first rows it offers now; the
/// names the letters only resemble follow, best first.
pub fn popup_matches(entries: &[Entry], needle: &str) -> Vec<Entry> {
    let mut matches: Vec<(i32, Entry)> = entries
        .iter()
        .filter(|entry| !entry.shadowed)
        .filter_map(|entry| {
            let found = crate::fuzzy::find(needle, &entry.name)?;
            Some((found.score, entry.clone()))
        })
        .collect();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        right_score.cmp(left_score).then_with(|| left.name.cmp(&right.name))
    });
    // Equal names carry equal scores, so the duplicates a source can repeat sit
    // next to each other once the sort is done.
    matches.dedup_by(|(_, left), (_, right)| left.name == right.name);
    matches.into_iter().map(|(_, entry)| entry).collect()
}

/// Drop the offer once the draft already names the command it would offer.
///
/// A list saying what the user has just finished typing is in the way: there is
/// nothing left to choose, and the next key is the one that sends. A name that
/// another match extends still leaves a choice (`resume` and `resume_all`), so
/// the list stays; a row the letters only resemble (`autoreview` for `review`)
/// is no choice at all and must not keep a finished name open.
fn settled(matches: Vec<Entry>, needle: &str) -> Vec<Entry> {
    let names = matches.iter().any(|entry| entry.name == needle);
    let extended = matches
        .iter()
        .any(|entry| entry.name != needle && entry.name.starts_with(needle));
    if names && !extended { Vec::new() } else { matches }
}

/// The rows the popup shows, and the first of them.
///
/// The selection stays on screen as it moves, and a list shorter than the popup
/// shows every row it has.
pub fn window(len: usize, selected: usize, rows: usize) -> (usize, usize) {
    let rows = rows.max(1);
    if len <= rows {
        return (0, len);
    }
    let selected = selected.min(len - 1);
    let first = (selected + 1).saturating_sub(rows);
    (first, rows)
}

/// What a popup row is drawn with.
///
/// The matcher and the glyph preset travel together because both come from the
/// same draft and the same settings, and a row cannot underline the right
/// characters without the needle that ranked it.
#[derive(Debug, Clone, Copy)]
pub struct Look<'a> {
    /// The needle the rows were ranked for; the characters it matched in a name
    /// are underlined.
    pub needle: &'a str,
    /// The glyphs in force, so the marker follows the chosen preset.
    pub symbols: Symbols,
}

/// The popup's rows as drawn: one line per command, cut to `width` cells.
///
/// Each line is padded to the full width so the row clears whatever the frame
/// left behind it. The chosen row starts with the prompt's marker, so the row
/// the keys act on is the one that looks chosen, and the characters the needle
/// matched in the name are underlined on top of the row style.
pub fn popup_lines(
    entries: &[Entry],
    selected: usize,
    first: usize,
    count: usize,
    width: usize,
    look: Look<'_>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let visible: Vec<&Entry> = entries.iter().skip(first).take(count).collect();
    let labels: Vec<String> = visible.iter().map(|entry| entry.label()).collect();
    let label_width = labels
        .iter()
        .map(|label| UnicodeWidthStr::width(label.as_str()))
        .max()
        .unwrap_or(0);
    // The marker and the space after it, which every row spends so the names
    // line up whether or not the row is chosen.
    let lead = usize::from(look.symbols.prompt_marker_width()) + 1;
    visible
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let chosen = first + offset == selected;
            let mut body = String::with_capacity(lead + labels[offset].len() + entry.description.len());
            if chosen {
                body.push_str(look.symbols.prompt_marker);
            } else {
                body.push_str(&" ".repeat(lead - 1));
            }
            body.push(' ');
            body.push_str(&labels[offset]);
            body.push_str(
                &" ".repeat(label_width.saturating_sub(UnicodeWidthStr::width(labels[offset].as_str())) + GAP),
            );
            body.push_str(&entry.description);
            // The label starts with `/`; the name the matcher saw follows it.
            let emphasise: Vec<usize> = match crate::fuzzy::find(look.needle, entry.name.as_str()) {
                Some(found) => found.positions.iter().map(|position| position + lead + 1).collect(),
                None => Vec::new(),
            };
            let style = if chosen {
                Style::default()
                    .fg(theme.prompt.prompt_border)
                    .bg(theme.prompt.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.prompt.prompt_text)
            };
            let mark = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            Line::from(marked_fit(&body, &emphasise, width, style, mark))
        })
        .collect()
}

/// `text` cut or padded to exactly `width` cells, with `emphasised` underlined.
///
/// A character that would cross the edge starts no row: the cell stays a space,
/// so a wide glyph is never split in half. Runs of like characters share one
/// span, so a row without an emphasis stays a single span.
fn marked_fit(text: &str, emphasised: &[usize], width: usize, style: Style, mark: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_marked = false;
    let mut used = 0;
    for (index, character) in text.chars().enumerate() {
        let cell = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + cell > width {
            break;
        }
        used += cell;
        let marked = emphasised.binary_search(&index).is_ok();
        if !run.is_empty() && marked != run_marked {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_marked { mark } else { style },
            ));
        }
        run_marked = marked;
        run.push(character);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_marked { mark } else { style }));
    }
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    spans
}

/// The slash popup: the commands the draft offers, and the row chosen.
///
/// The matches are kept with the draft they were computed for, so a frame that
/// draws the popup costs nothing, and a key that reads the length cannot read
/// the list of another draft.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlashPopup {
    /// The draft the matches belong to.
    draft: String,
    /// Fingerprint of that draft, so a caller can skip rebuilding it.
    synced: Option<u64>,
    matches: Vec<Entry>,
    selected: usize,
    dismissed: bool,
}

impl SlashPopup {
    /// A popup with nothing to offer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bring the matches in line with the draft, if it changed.
    ///
    /// A new draft starts the selection over and brings back a popup `esc`
    /// closed: the dismissal belonged to the text that was there.
    pub fn sync(&mut self, registry: &Registry, draft: &str) {
        // The length is counted here, once per changed draft, never in the
        // per-frame check.
        let fingerprint = crate::input::fingerprint(draft.chars().count(), draft.chars());
        if self.draft == draft {
            self.synced = Some(fingerprint);
            return;
        }
        self.draft.clear();
        self.draft.push_str(draft);
        self.synced = Some(fingerprint);
        self.selected = 0;
        self.dismissed = false;
        self.matches = match prefix(draft) {
            Some(prefix) => settled(popup_matches(&registry.entries(), prefix), prefix),
            None => Vec::new(),
        };
    }

    /// Whether the matches already belong to the draft with this fingerprint.
    ///
    /// The caller reads it before building the draft: a key that changed
    /// nothing used to build the whole message a second time to find out.
    pub fn is_synced(&self, fingerprint: u64) -> bool {
        self.synced == Some(fingerprint)
    }

    /// Bring the popup in line with a draft that cannot open it.
    ///
    /// A message being written, or a line holding more than one, is not a
    /// command: the list closes without the draft being built to prove it.
    pub fn clear_for(&mut self, fingerprint: u64) {
        self.synced = Some(fingerprint);
        if self.matches.is_empty() && self.draft.is_empty() {
            return;
        }
        self.draft.clear();
        self.matches.clear();
        self.selected = 0;
        self.dismissed = false;
    }

    /// Whether the popup is on screen.
    pub fn is_open(&self) -> bool {
        !self.matches.is_empty() && !self.dismissed
    }

    /// Close the popup without touching the draft.
    ///
    /// Used where something else takes the keys, and by `esc`. The next edit
    /// brings it back.
    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    /// Every command the draft offers, whether or not it is on screen.
    pub fn matches(&self) -> &[Entry] {
        &self.matches
    }

    /// How many commands the draft offers.
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Whether the draft offers nothing.
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// The row the keys move.
    pub fn selected(&self) -> usize {
        self.matches.get(self.selected).map_or(0, |_| self.selected)
    }

    /// Move the selection, stopping at both ends.
    pub fn move_by(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        self.selected = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize).min(last)
        };
    }

    /// The name `editor.complete` fills in.
    pub fn selected_name(&self) -> Option<&str> {
        self.matches.get(self.selected).map(|entry| entry.name.as_str())
    }

    /// The needle the matches were ranked for, when the draft asks for a list.
    ///
    /// The rows are drawn from it: the characters it matched are the ones the
    /// frame underlines.
    pub fn needle(&self) -> Option<&str> {
        prefix(&self.draft)
    }

    /// The rows the popup may show: the first one, and how many.
    pub fn visible(&self) -> (usize, usize) {
        window(self.matches.len(), self.selected, POPUP_ROWS)
    }
}

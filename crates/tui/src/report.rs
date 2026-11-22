//! A runtime command's answer, drawn as labelled rows instead of JSON.
//!
//! The harness answers a slash command with `data` and a `message`. For most
//! commands the message is that same payload serialized as JSON: it is useful to
//! a program and unreadable on a terminal, where the keys repeat the field names
//! and the punctuation takes more room than the values. This module turns the
//! payload into the shape an interface shows: scalars first, then nested
//! sections, with long notes last and secret-shaped fields hidden.
//!
//! Nothing here decides whether a command was worth drawing. It reads the answer
//! it was given and refuses one that is prose, so a command with a report of its
//! own — `/skills`, `/plan`, `/remote pair` — keeps the words it wrote.

use horon_agent_sdk::RuntimeCommand;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::item_lines::Look;
use crate::wrap::Indented;

/// Columns one nesting level adds before its rows.
const LEVEL_INDENT: usize = 2;
/// Spaces between a label and its value.
const GAP: usize = 2;
/// The value column a label must leave before the alignment is dropped.
const MIN_VALUE_COLUMNS: usize = 8;
/// The marker a hidden value is drawn with.
const HIDDEN: &str = "— hidden";
/// The value an absent one is drawn with.
const ABSENT: &str = "—";

/// How a value is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    /// A word, a number or a name.
    Plain,
    /// A switch that is on.
    Positive,
    /// A switch that is off, or nothing to show.
    Muted,
}

/// One row of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// A section introduced by the payload's own key.
    Heading {
        depth: usize,
        label: String,
        count: Option<usize>,
    },
    /// One element of a list, named by its own `name` or by its position.
    Item { depth: usize, label: String },
    /// One key and its value.
    Field {
        depth: usize,
        label: String,
        value: String,
        tone: Tone,
    },
    /// A sentence the payload carries beside its fields.
    Note { depth: usize, text: String },
}

/// A runtime command's structured answer, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    title: String,
    rows: Vec<Row>,
}

impl Report {
    /// Read an answer, or `None` when the command wrote prose instead of a dump.
    ///
    /// `data` is what the daemon meant as structure and is preferred; the
    /// message is only its rendering. A message with no JSON in it belongs to a
    /// command that reported in words, and the caller shows it unchanged.
    pub fn from_answer(command: RuntimeCommand, data: &Value, message: &str) -> Option<Self> {
        let dumped = json_in(message)?;
        let payload = match data {
            Value::Object(fields) if !fields.is_empty() => data,
            Value::Array(items) if !items.is_empty() => data,
            _ => &dumped,
        };
        let mut report = Self {
            title: title(command).to_string(),
            rows: Vec::new(),
        };
        match payload {
            Value::Object(fields) => report.push_object(fields, 0, None),
            Value::Array(items) => report.push_array("entries", items, 0),
            _ => return None,
        }
        Some(report)
    }

    /// The title line, over the rows.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// One key's fields, then its sections, then the notes it carries.
    fn push_object(&mut self, fields: &serde_json::Map<String, Value>, depth: usize, skip: Option<&str>) {
        let mut sections = Vec::new();
        for (key, value) in fields {
            if skip == Some(key.as_str()) || is_note(key) && value.is_string() {
                continue;
            }
            // A list is never joined into a scalar row: it may be long, it may
            // hide secrets, and its elements may have rows of their own.
            if is_section(value) || value.is_array() {
                sections.push((key, value));
            } else {
                self.push_scalar(key, value, depth);
            }
        }
        for (key, value) in sections {
            self.push_value(key, value, depth);
        }
        // A note is a sentence about the fields above it, not a field beside
        // them: it reads last, where a reader looks after the numbers.
        for (key, value) in fields {
            if skip == Some(key.as_str()) {
                continue;
            }
            if is_note(key)
                && let Some(text) = value.as_str().map(str::trim).filter(|text| !text.is_empty())
            {
                self.rows.push(Row::Note {
                    depth,
                    text: text.to_string(),
                });
            }
        }
    }

    /// One key whose value has parts of its own.
    fn push_value(&mut self, key: &str, value: &Value, depth: usize) {
        match value {
            Value::Object(fields) if !fields.is_empty() => {
                self.rows.push(Row::Heading {
                    depth,
                    label: label_of(key),
                    count: None,
                });
                self.push_object(fields, depth + 1, None);
            }
            Value::Array(items) => self.push_array(key, items, depth),
            _ => self.push_scalar(key, value, depth),
        }
    }

    /// One list: joined when its elements are words, listed when they have parts.
    fn push_array(&mut self, key: &str, items: &[Value], depth: usize) {
        let simple = items.iter().all(|item| !item.is_object() && !item.is_array());
        if simple {
            if items.is_empty() {
                self.field(key, ABSENT.to_string(), depth, Tone::Muted);
                return;
            }
            if sensitive(key) {
                self.field(key, HIDDEN.to_string(), depth, Tone::Muted);
                return;
            }
            let words: Vec<String> = items.iter().map(scalar_text).collect();
            self.field(key, words.join(", "), depth, Tone::Plain);
            return;
        }
        self.rows.push(Row::Heading {
            depth,
            label: label_of(key),
            count: Some(items.len()),
        });
        for (index, item) in items.iter().enumerate() {
            match item {
                Value::Object(fields) => {
                    let (label, named) = match item_name(fields) {
                        Some((key, name)) => (name, Some(key)),
                        None => (format!("#{}", index + 1), None),
                    };
                    self.rows.push(Row::Item {
                        depth: depth + 1,
                        label,
                    });
                    // The row already says the name; repeating it as a field
                    // under itself costs a line and adds nothing.
                    self.push_object(fields, depth + 2, named);
                }
                Value::Array(nested) => self.push_array(&format!("#{}", index + 1), nested, depth + 1),
                other => {
                    let value = scalar_text(other);
                    self.field(&format!("#{}", index + 1), value, depth + 1, Tone::Plain);
                }
            }
        }
    }

    /// One key and one value, with a secret-shaped string hidden.
    fn push_scalar(&mut self, key: &str, value: &Value, depth: usize) {
        match value {
            Value::Bool(true) => self.field(key, "yes".to_string(), depth, Tone::Positive),
            Value::Bool(false) => self.field(key, "no".to_string(), depth, Tone::Muted),
            Value::Null => self.field(key, ABSENT.to_string(), depth, Tone::Muted),
            Value::String(text) if text.trim().is_empty() => self.field(key, ABSENT.to_string(), depth, Tone::Muted),
            Value::String(_) if sensitive(key) => self.field(key, HIDDEN.to_string(), depth, Tone::Muted),
            other => self.field(key, scalar_text(other), depth, Tone::Plain),
        }
    }

    /// Add one field row.
    fn field(&mut self, key: &str, value: String, depth: usize, tone: Tone) {
        self.rows.push(Row::Field {
            depth,
            label: label_of(key),
            value,
            tone,
        });
    }
}

/// The lines the report is drawn with.
pub(crate) fn lines(report: &Report, look: &Look<'_>) -> Vec<Indented> {
    let accent = Style::default().fg(look.theme.core.accent).add_modifier(Modifier::BOLD);
    let strong = Style::default().fg(look.theme.core.text).add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(look.theme.core.muted);
    let plain = Style::default().fg(look.theme.core.text);
    let width = usize::from(look.width);

    // One width per nesting level, so a block's labels share a column without
    // a section's long key pushing an unrelated block's aside. A label that
    // would leave no room for the value is padded with one space instead.
    let mut widths: Vec<usize> = Vec::new();
    for row in &report.rows {
        if let Row::Field { depth, label, .. } = row {
            if widths.len() <= *depth {
                widths.resize(*depth + 1, 0);
            }
            let columns = label.chars().count();
            widths[*depth] = widths[*depth].max(columns);
        }
    }
    for (depth, label_width) in widths.iter_mut().enumerate() {
        let room = width
            .saturating_sub(depth * LEVEL_INDENT + GAP + MIN_VALUE_COLUMNS)
            .max(MIN_VALUE_COLUMNS);
        *label_width = (*label_width).min(room);
    }

    let mut lines: Vec<Indented> = Vec::with_capacity(report.rows.len() + 1);
    lines.push(Indented::plain(Line::from(Span::styled(report.title.clone(), accent))));
    for row in &report.rows {
        match row {
            Row::Heading { depth, label, count } => {
                let pad = depth * LEVEL_INDENT;
                let mut spans = vec![Span::raw(" ".repeat(pad)), Span::styled(label.clone(), strong)];
                if let Some(count) = count {
                    spans.push(Span::styled(format!(" · {count}"), muted));
                }
                lines.push(Indented::new(Line::from(spans), pad));
            }
            Row::Item { depth, label } => {
                let pad = depth * LEVEL_INDENT;
                let line = Line::from(vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(format!("{} ", look.symbols.bullet), accent),
                    Span::styled(label.clone(), strong),
                ]);
                lines.push(Indented::new(line, pad + look.symbols.bullet.chars().count() + 1));
            }
            Row::Field {
                depth,
                label,
                value,
                tone,
            } => {
                let pad = depth * LEVEL_INDENT;
                let label_width = widths.get(*depth).copied().unwrap_or(0);
                let columns = label.chars().count();
                let gap = " ".repeat(label_width.saturating_sub(columns) + GAP);
                let value_style = match tone {
                    Tone::Plain => plain,
                    Tone::Positive => Style::default().fg(look.theme.core.success),
                    Tone::Muted => muted,
                };
                let value_column = pad + label_width + GAP;
                let line = Line::from(vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(format!("{label}{gap}"), muted),
                    Span::styled(value.clone(), value_style),
                ]);
                lines.push(Indented::new(line, value_column));
            }
            Row::Note { depth, text } => {
                let pad = depth * LEVEL_INDENT;
                let style = muted.add_modifier(Modifier::ITALIC);
                lines.push(Indented::new(Line::from(Span::styled(text.clone(), style)), pad));
            }
        }
    }
    lines
}

/// Whether a value is drawn as a section with rows under it.
fn is_section(value: &Value) -> bool {
    match value {
        Value::Object(fields) => !fields.is_empty(),
        Value::Array(items) => items.iter().any(is_section),
        _ => false,
    }
}

/// Whether a key carries a sentence to read after the fields.
fn is_note(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "note" | "notes" | "message" | "description" | "detail" | "hint" | "usage"
    )
}

/// Whether a key names something that must not be printed.
///
/// A variable's *name* is not its value, so an `_env` key stays visible: it is
/// the one thing a reader uses to find where the secret actually lives.
fn sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    if key.ends_with("_env") || key.ends_with("_env_var") {
        return false;
    }
    key == "token"
        || key == "api_key"
        || key == "password"
        || key == "secret"
        || key == "authorization"
        || key.ends_with("_token")
        || key.ends_with("_key")
        || key.ends_with("_secret")
        || key.ends_with("_password")
        || key.contains("secret_key")
}

/// A key as words: `openai_compatible_credentials` reads as three of them.
fn label_of(key: &str) -> String {
    key.replace('_', " ")
}

/// A scalar's text, as one value.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The name a list element carries on its own, when it has one.
fn item_name(fields: &serde_json::Map<String, Value>) -> Option<(&'static str, String)> {
    const KEYS: &[&str] = &[
        "name", "profile", "id", "label", "role", "model", "title", "kind", "path", "key",
    ];
    for key in KEYS {
        if let Some(name) = fields.get(*key).and_then(Value::as_str) {
            let name = name.trim();
            if !name.is_empty() {
                return Some((key, name.to_string()));
            }
        }
    }
    None
}

/// The JSON a message is, when the message is a dump rather than prose.
///
/// A command without a report of its own answers with the payload itself, and
/// the recovery commands wrap it in a fenced block under a heading.
fn json_in(message: &str) -> Option<Value> {
    let trimmed = message.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && (value.is_object() || value.is_array())
    {
        return Some(value);
    }
    let open = trimmed.find("```json")?;
    let rest = &trimmed[open + "```json".len()..];
    let close = rest.find("```")?;
    let value: Value = match serde_json::from_str(rest[..close].trim()) {
        Ok(value) => value,
        // A fence that does not hold JSON is prose, and the caller prints it.
        Err(_) => return None,
    };
    (value.is_object() || value.is_array()).then_some(value)
}

/// The heading a command's report wears.
fn title(command: RuntimeCommand) -> &'static str {
    match command {
        RuntimeCommand::Skills => "skills",
        RuntimeCommand::Context => "context",
        RuntimeCommand::Info => "session info",
        RuntimeCommand::Memory => "memory",
        RuntimeCommand::Swarm => "swarm",
        RuntimeCommand::Agents => "agent models",
        RuntimeCommand::ModelPreset => "model presets",
        RuntimeCommand::SubagentModel => "subagent model",
        RuntimeCommand::Trim => "tool result trim",
        RuntimeCommand::Autoreview => "automatic review",
        RuntimeCommand::Autojudge => "automatic judge",
        RuntimeCommand::Transport => "provider transport",
        RuntimeCommand::Fast => "service tier",
        RuntimeCommand::Overnight => "overnight",
        RuntimeCommand::Productivity => "productivity report",
        RuntimeCommand::Initiatives => "initiatives",
        RuntimeCommand::RefreshModelList => "model catalog",
        RuntimeCommand::Continue | RuntimeCommand::ResumeAll => "sessions resumed",
        RuntimeCommand::Remote => "remote access",
        RuntimeCommand::ServerReload => "server reload",
        RuntimeCommand::Review => "review",
        RuntimeCommand::Judge => "judge",
        RuntimeCommand::Subagent => "subagent",
        RuntimeCommand::Auth => "runtime authentication",
        RuntimeCommand::Logout => "logout",
        RuntimeCommand::Plan => "plan mode",
        RuntimeCommand::Goal => "session goal",
        RuntimeCommand::Advisor => "advisor",
        RuntimeCommand::Dirs => "workspace directories",
        RuntimeCommand::Mcp => "MCP servers",
        RuntimeCommand::PackageCommands => "package commands",
        RuntimeCommand::PackageCommand => "package command",
    }
}

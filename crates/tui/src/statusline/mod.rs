//! The status line, assembled from named segments.
//!
//! The line the client draws at the bottom is a list of segments: the model,
//! the session, the turn's state, the working directory, token counts, the
//! context percentage, the time and a pending-decision marker. A preset names
//! the segments for each side, and `statusline.preset = "custom"` replaces that
//! with the explicit `statusline.left` and `statusline.right` arrays.
//!
//! Two rules keep a narrow terminal readable: a segment with no data draws
//! nothing at all, and when the line does not fit the least important segment
//! is dropped first. A misspelled segment is reported once and skipped instead
//! of hiding the rest of the line.

use chrono::NaiveTime;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::settings::Settings;
use crate::theme::{Symbols, Theme};

#[cfg(test)]
mod tests;

/// The segments a status line can show, in catalogue order.
pub const SEGMENT_IDS: &[&str] = &[
    "model",
    "session",
    "status",
    "path",
    "tokens",
    "context_pct",
    "time",
    "pending",
];

/// One segment of the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentId {
    /// `provider/model`.
    Model,
    /// The session id, shortened.
    Session,
    /// What the turn is doing.
    Status,
    /// The working directory.
    Path,
    /// Input and output tokens of the last request.
    Tokens,
    /// Share of the context window used.
    ContextPct,
    /// Local wall-clock time.
    Time,
    /// A parked approval or question.
    Pending,
}

impl SegmentId {
    /// The segment a settings value names, or `None` for an unknown id.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "model" => Some(Self::Model),
            "session" => Some(Self::Session),
            "status" => Some(Self::Status),
            "path" => Some(Self::Path),
            "tokens" => Some(Self::Tokens),
            "context_pct" => Some(Self::ContextPct),
            "time" => Some(Self::Time),
            "pending" => Some(Self::Pending),
            _ => None,
        }
    }

    /// The id a settings file uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Session => "session",
            Self::Status => "status",
            Self::Path => "path",
            Self::Tokens => "tokens",
            Self::ContextPct => "context_pct",
            Self::Time => "time",
            Self::Pending => "pending",
        }
    }

    /// How hard this segment is to lose. The lowest priority is dropped first
    /// when the line does not fit.
    pub fn priority(self) -> u8 {
        match self {
            Self::Time => 2,
            Self::Session => 3,
            Self::Tokens => 4,
            Self::ContextPct => 5,
            Self::Path => 6,
            Self::Pending => 7,
            Self::Status => 8,
            Self::Model => 9,
        }
    }
}

/// The token counts one request reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    /// Prompt tokens.
    pub input: u64,
    /// Completion tokens.
    pub output: u64,
    /// Prompt tokens served from the provider's cache.
    pub cache_read: u64,
    /// Prompt tokens written to the provider's cache.
    pub cache_creation: u64,
}

impl TokenTotals {
    /// Tokens the request occupied in the context window.
    pub fn used(self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }
}

/// What the segments read, taken as a snapshot so the renderer stays a plain
/// function.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusData<'a> {
    /// Provider reported by the daemon.
    pub provider: Option<&'a str>,
    /// Model reported by the daemon.
    pub model: Option<&'a str>,
    /// Session the client is attached to.
    pub session_id: Option<&'a str>,
    /// Whether a turn is in flight.
    pub pending_turn: bool,
    /// Latest session status word, e.g. `idle` or `generating`.
    pub session_status: Option<&'a str>,
    /// A parked approval or question, already worded for the line.
    pub prompt: Option<&'a str>,
    /// Working directory of the client.
    pub cwd: Option<&'a str>,
    /// The user's home directory, shortened to `~` in the path segment.
    pub home: Option<&'a str>,
    /// Latest token totals.
    pub tokens: Option<TokenTotals>,
    /// Local time, from the clock.
    pub now: Option<NaiveTime>,
}

/// A named arrangement of segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// The full line: state, model and directory on the left, counts and the
    /// clock on the right.
    Default,
    /// The model on the left, the state on the right.
    Minimal,
    /// Whatever `statusline.left` and `statusline.right` name.
    Custom,
}

impl Preset {
    /// The preset a settings value names.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "default" => Some(Self::Default),
            "minimal" => Some(Self::Minimal),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    /// The name the settings registry uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Minimal => "minimal",
            Self::Custom => "custom",
        }
    }

    /// The segments this preset puts on each side.
    pub fn segments(self) -> (Vec<SegmentId>, Vec<SegmentId>) {
        match self {
            Self::Default => (
                vec![SegmentId::Status, SegmentId::Model, SegmentId::Path],
                vec![SegmentId::Tokens, SegmentId::ContextPct, SegmentId::Time],
            ),
            Self::Minimal => (vec![SegmentId::Model], vec![SegmentId::Status]),
            Self::Custom => (Vec::new(), Vec::new()),
        }
    }
}

/// How long the path segment may get before it is shortened from the left.
pub const PATH_MAX_DEFAULT: i64 = 40;

/// The resolved status-line layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The preset that produced the segments.
    pub preset: Preset,
    /// Segments drawn from the left, in order.
    pub left: Vec<SegmentId>,
    /// Segments drawn from the right, in order.
    pub right: Vec<SegmentId>,
    /// Longest path the `path` segment shows; `0` keeps it whole.
    pub path_max: usize,
    /// Context window the `context_pct` segment divides by; `0` hides it.
    pub context_window: u64,
}

impl Default for Layout {
    fn default() -> Self {
        let preset = Preset::Default;
        let (left, right) = preset.segments();
        Self {
            preset,
            left,
            right,
            path_max: usize::try_from(PATH_MAX_DEFAULT).unwrap_or(40),
            context_window: 0,
        }
    }
}

impl Layout {
    /// Build the layout from the settings in force.
    ///
    /// The returned sentences name unknown segment ids; they are warnings for
    /// the transcript, and the segment is skipped so the rest still draws.
    pub fn from_settings(settings: &Settings) -> (Self, Vec<String>) {
        let preset = match settings.text("statusline.preset") {
            Some(value) => Preset::parse(value).unwrap_or(Preset::Default),
            None => Preset::Default,
        };
        let mut warnings = Vec::new();
        let (left, right) = match preset {
            Preset::Custom => (
                named_segments(settings, "statusline.left", &mut warnings),
                named_segments(settings, "statusline.right", &mut warnings),
            ),
            named => named.segments(),
        };
        let path_max = match settings.number("statusline.path_max") {
            Some(value) if value >= 0 => usize::try_from(value).unwrap_or(0),
            Some(_) | None => usize::try_from(PATH_MAX_DEFAULT).unwrap_or(40),
        };
        let context_window = match settings.number("statusline.context_window") {
            Some(value) if value > 0 => u64::try_from(value).unwrap_or(0),
            Some(_) | None => 0,
        };
        (
            Self {
                preset,
                left,
                right,
                path_max,
                context_window,
            },
            warnings,
        )
    }
}

/// Parse one segment array, warning about ids the catalogue does not have.
fn named_segments(settings: &Settings, key: &str, warnings: &mut Vec<String>) -> Vec<SegmentId> {
    let values = match settings.list(key) {
        Some(values) => values,
        None => return Vec::new(),
    };
    let mut segments = Vec::new();
    for value in values {
        match SegmentId::parse(value) {
            Some(id) => {
                if !segments.contains(&id) {
                    segments.push(id);
                }
            }
            None => warnings.push(format!("unknown segment \"{value}\" in {key}; skipped")),
        }
    }
    segments
}

/// The local wall-clock time, read once per frame by the renderer.
pub fn local_now() -> NaiveTime {
    chrono::Local::now().time()
}

/// One visible segment: its id, its text and its colour.
#[derive(Debug, Clone)]
struct Entry {
    id: SegmentId,
    text: String,
    style: Style,
}

/// Draw the status line into `width` columns.
///
/// The left segments are followed by a right-aligned group. When the two do
/// not fit, segments are dropped in ascending priority order until they do; a
/// zero-width area draws nothing.
pub fn render(data: &StatusData<'_>, layout: &Layout, theme: &Theme, symbols: Symbols, width: u16) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }
    let mut left = collect(&layout.left, data, layout, theme);
    let mut right = collect(&layout.right, data, layout, theme);
    let available = usize::from(width);
    let separator = Span::styled(
        format!(" {} ", symbols.separator),
        Style::default().fg(theme.core.border),
    );
    loop {
        let used = side_width(&left, &separator)
            .saturating_add(side_width(&right, &separator))
            .saturating_add(gap_width(&left, &right));
        if used <= available {
            break;
        }
        // The last segment stays even when it is wider than the pane: a
        // clipped model is more useful than an empty bar.
        if left.len() + right.len() <= 1 {
            break;
        }
        if !drop_lowest(&mut left, &mut right) {
            break;
        }
    }
    let mut spans = Vec::new();
    push_side(&mut spans, &left, &separator);
    let gap = available
        .saturating_sub(side_width(&left, &separator))
        .saturating_sub(side_width(&right, &separator));
    if gap > 0 && !left.is_empty() && !right.is_empty() {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    push_side(&mut spans, &right, &separator);
    Line::from(spans)
}

/// The visible segments of one side, in order, without empty entries.
fn collect(ids: &[SegmentId], data: &StatusData<'_>, layout: &Layout, theme: &Theme) -> Vec<Entry> {
    let mut entries = Vec::new();
    for id in ids {
        let text = segment_text(*id, data, layout);
        if text.is_empty() {
            continue;
        }
        entries.push(Entry {
            id: *id,
            text,
            style: segment_style(*id, data, theme),
        });
    }
    entries
}

/// The text of one segment, or the empty string when it has no data.
fn segment_text(id: SegmentId, data: &StatusData<'_>, layout: &Layout) -> String {
    match id {
        SegmentId::Model => match (data.provider, data.model) {
            (Some(provider), Some(model)) => format!("{provider}/{model}"),
            (None, Some(model)) => model.to_string(),
            (Some(provider), None) => provider.to_string(),
            (None, None) => String::new(),
        },
        SegmentId::Session => match data.session_id {
            Some(id) => short_id(id),
            None => String::new(),
        },
        SegmentId::Status => status_word(data).to_string(),
        SegmentId::Path => match data.cwd {
            Some(cwd) => shorten_path(cwd, data.home, layout.path_max),
            None => String::new(),
        },
        SegmentId::Tokens => match data.tokens {
            Some(tokens) => format!("↑{} ↓{}", compact(tokens.input), compact(tokens.output)),
            None => String::new(),
        },
        SegmentId::ContextPct => context_text(data, layout.context_window),
        SegmentId::Time => match data.now {
            Some(now) => now.format("%H:%M").to_string(),
            None => String::new(),
        },
        SegmentId::Pending => match data.prompt {
            Some(prompt) => prompt.to_string(),
            None => String::new(),
        },
    }
}

/// What the turn is doing, in the vocabulary the status segment uses.
fn status_word<'a>(data: &StatusData<'a>) -> &'a str {
    if data.prompt.is_some() {
        return "requires_action";
    }
    if data.pending_turn {
        return "running";
    }
    match data.session_status {
        Some("generating") => "running",
        Some(status) if !status.is_empty() => status,
        Some(_) | None => "idle",
    }
}

/// The colour of one segment.
fn segment_style(id: SegmentId, data: &StatusData<'_>, theme: &Theme) -> Style {
    match id {
        SegmentId::Model => Style::default().fg(theme.status.status_accent),
        SegmentId::Status | SegmentId::Pending => {
            if data.prompt.is_some() {
                Style::default().fg(theme.core.warning)
            } else if data.pending_turn {
                Style::default().fg(theme.core.warning)
            } else {
                Style::default().fg(theme.core.success)
            }
        }
        SegmentId::Session | SegmentId::Path | SegmentId::Tokens | SegmentId::ContextPct | SegmentId::Time => {
            Style::default().fg(theme.status.status_text)
        }
    }
}

/// A session id shortened to its leading characters.
fn short_id(id: &str) -> String {
    const SHORT: usize = 8;
    let count = id.chars().count();
    if count <= SHORT {
        return id.to_string();
    }
    id.chars().take(SHORT).collect()
}

/// A path with the home directory replaced by `~` and a length limit applied
/// from the left, so the leaf stays visible.
fn shorten_path(path: &str, home: Option<&str>, max: usize) -> String {
    let mut text = match home {
        Some(home) if !home.is_empty() => {
            if path == home {
                "~".to_string()
            } else {
                match path.strip_prefix(home) {
                    Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => {
                        format!("~{rest}")
                    }
                    Some(_) | None => path.to_string(),
                }
            }
        }
        Some(_) | None => path.to_string(),
    };
    if max == 0 {
        return text;
    }
    let count = text.chars().count();
    if count > max {
        let tail: String = text.chars().skip(count - max.saturating_sub(1)).collect();
        text = format!("…{tail}");
    }
    text
}

/// A token count in a compact form, e.g. `12.3k`.
fn compact(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

/// The context percentage, or nothing when the window size or the totals are
/// unknown.
fn context_text(data: &StatusData<'_>, window: u64) -> String {
    if window == 0 {
        return String::new();
    }
    let tokens = match data.tokens {
        Some(tokens) => tokens,
        None => return String::new(),
    };
    let percent = tokens.used().saturating_mul(100) / window;
    format!("{}%", percent.min(100))
}

/// Width of one side's spans, separators included.
fn side_width(entries: &[Entry], separator: &Span<'static>) -> usize {
    let mut width = 0;
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            width = width.saturating_add(separator.width());
        }
        width = width.saturating_add(Span::raw(entry.text.clone()).width());
    }
    width
}

/// One space between the two sides, when both have something to draw.
fn gap_width(left: &[Entry], right: &[Entry]) -> usize {
    usize::from(!left.is_empty() && !right.is_empty())
}

/// Append one side's text with separators between entries but not around it.
fn push_side(spans: &mut Vec<Span<'static>>, entries: &[Entry], separator: &Span<'static>) {
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            spans.push(separator.clone());
        }
        spans.push(Span::styled(entry.text.clone(), entry.style));
    }
}

/// Remove the lowest-priority visible segment. Returns whether one was removed.
fn drop_lowest(left: &mut Vec<Entry>, right: &mut Vec<Entry>) -> bool {
    let left_low = left.iter().enumerate().min_by_key(|(_, entry)| entry.id.priority());
    let right_low = right.iter().enumerate().min_by_key(|(_, entry)| entry.id.priority());
    match (left_low, right_low) {
        (Some((left_index, left_entry)), Some((right_index, right_entry))) => {
            if right_entry.id.priority() < left_entry.id.priority() {
                right.remove(right_index);
            } else {
                left.remove(left_index);
            }
            true
        }
        (Some((index, _)), None) => {
            left.remove(index);
            true
        }
        (None, Some((index, _))) => {
            right.remove(index);
            true
        }
        (None, None) => false,
    }
}

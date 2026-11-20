//! The declared settings.
//!
//! One row per key, and nothing else: the registry is the single place a key,
//! its type, its default, its environment variable and its UI labels appear.
//! The loader rejects unknown keys against this table, so a typo in a settings
//! file produces a warning naming the key instead of a value that silently
//! never applies.

use super::SettingValue;

/// A setting's value shape.
///
/// [`SettingKind::Enum`] is a string restricted to the listed values; the list
/// is also what a settings overlay draws as choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    Text,
    Enum(&'static [&'static str]),
    Number,
    Array,
}

impl SettingKind {
    /// How a warning names this shape.
    pub fn label(self) -> String {
        match self {
            Self::Bool => "a boolean".to_string(),
            Self::Text => "a string".to_string(),
            Self::Enum(values) => format!("one of {}", values.join(", ")),
            Self::Number => "a number".to_string(),
            Self::Array => "an array of strings".to_string(),
        }
    }
}

/// Labels a settings overlay uses to group and explain a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiMeta {
    /// Top-level tab, for example `Appearance`.
    pub tab: &'static str,
    /// Group inside the tab.
    pub group: &'static str,
    /// Human-readable name.
    pub label: &'static str,
    /// One sentence explaining what the key changes.
    pub description: &'static str,
}

/// A default value that can live in a `static` table.
#[derive(Debug, Clone, Copy)]
pub enum DefaultValue {
    Bool(bool),
    Text(&'static str),
    Number(i64),
    List(&'static [&'static str]),
}

impl DefaultValue {
    /// The value the lowest layer starts with.
    pub fn value(self) -> SettingValue {
        match self {
            Self::Bool(value) => SettingValue::Bool(value),
            Self::Text(value) => SettingValue::Str(value.to_string()),
            Self::Number(value) => SettingValue::Number(value),
            Self::List(items) => SettingValue::List(items.iter().map(|item| (*item).to_string()).collect()),
        }
    }
}

/// One declared setting.
#[derive(Debug, Clone, Copy)]
pub struct SettingSpec {
    /// Dot path, for example `statusline.preset`.
    pub id: &'static str,
    /// Accepted shape.
    pub kind: SettingKind,
    /// Environment variable that overrides every file.
    pub env: &'static str,
    /// Value used when no layer sets the key.
    pub default: DefaultValue,
    /// Labels for the settings overlay.
    pub ui: UiMeta,
}

impl SettingSpec {
    /// The default value for this key.
    pub fn default_value(&self) -> SettingValue {
        self.default.value()
    }

    /// Whether `value` has the declared shape.
    ///
    /// An enum checks membership, not just the string type: a value outside the
    /// list would otherwise reach a consumer that only handles the listed ones.
    pub fn accepts(&self, value: &SettingValue) -> bool {
        match (self.kind, value) {
            (SettingKind::Bool, SettingValue::Bool(_)) => true,
            (SettingKind::Text, SettingValue::Str(_)) => true,
            (SettingKind::Enum(values), SettingValue::Str(value)) => values.contains(&value.as_str()),
            (SettingKind::Number, SettingValue::Number(_)) => true,
            (SettingKind::Array, SettingValue::List(_)) => true,
            _ => false,
        }
    }
}

const SYMBOLS: &[&str] = &["unicode", "nerd", "ascii"];
const STATUSLINE_PRESETS: &[&str] = &["default", "minimal", "custom"];
const STATUSLINE_SEPARATORS: &[&str] = &["preset", "dot", "bar"];
const STATUSLINE_BACKGROUNDS: &[&str] = &["theme", "terminal"];

/// Every setting the client knows.
///
/// Kept small on purpose: the keys here are the ones the client needs now or in
/// the next few pieces of work (theme, symbols, status line, transcript), and a
/// key that nothing reads yet still resolves and reports where its value came
/// from, which is what the settings overlay draws.
pub const REGISTRY: &[SettingSpec] = &[
    SettingSpec {
        id: "theme",
        kind: SettingKind::Text,
        env: "HORON_TUI_THEME",
        default: DefaultValue::Text("dark"),
        ui: UiMeta {
            tab: "Appearance",
            group: "Theme",
            label: "Theme",
            description: "Colour theme: dark, light, or a file in $HORON_HOME/themes.",
        },
    },
    SettingSpec {
        id: "symbols",
        kind: SettingKind::Enum(SYMBOLS),
        env: "HORON_TUI_SYMBOLS",
        default: DefaultValue::Text("unicode"),
        ui: UiMeta {
            tab: "Appearance",
            group: "Glyphs",
            label: "Symbol set",
            description: "Unicode, Nerd Font or plain ASCII glyphs.",
        },
    },
    SettingSpec {
        id: "motion.preset",
        kind: SettingKind::Enum(super::motion::MODES),
        env: "HORON_TUI_MOTION_PRESET",
        default: DefaultValue::Text("full"),
        ui: UiMeta {
            tab: "Appearance",
            group: "Motion",
            label: "Motion",
            description: "Full animates the spinner, reduced freezes it, static draws no spinner and adds no frames.",
        },
    },
    SettingSpec {
        id: "motion.spinner_interval",
        kind: SettingKind::Number,
        env: "HORON_TUI_MOTION_SPINNER_INTERVAL",
        default: DefaultValue::Number(super::motion::DEFAULT_SPINNER_INTERVAL),
        ui: UiMeta {
            tab: "Appearance",
            group: "Motion",
            label: "Spinner interval",
            description: "Milliseconds one spinner frame stays, 40 to 2000.",
        },
    },
    SettingSpec {
        id: "motion.max_fps",
        kind: SettingKind::Number,
        env: "HORON_TUI_MOTION_MAX_FPS",
        default: DefaultValue::Number(super::motion::DEFAULT_MAX_FPS),
        ui: UiMeta {
            tab: "Appearance",
            group: "Motion",
            label: "Max FPS",
            description: "Most frames a second a streaming turn draws, 1 to 240.",
        },
    },
    SettingSpec {
        id: "title.state",
        kind: SettingKind::Bool,
        env: "HORON_TUI_TITLE_STATE",
        default: DefaultValue::Bool(false),
        ui: UiMeta {
            tab: "Appearance",
            group: "Window",
            label: "Title state",
            description: "Append running, waiting or idle to the window title; off keeps the session name alone.",
        },
    },
    SettingSpec {
        id: "statusline.preset",
        kind: SettingKind::Enum(STATUSLINE_PRESETS),
        env: "HORON_TUI_STATUSLINE_PRESET",
        default: DefaultValue::Text("default"),
        ui: UiMeta {
            tab: "Status line",
            group: "Layout",
            label: "Preset",
            description: "Named arrangement of the status line segments.",
        },
    },
    SettingSpec {
        id: "statusline.left",
        kind: SettingKind::Array,
        env: "HORON_TUI_STATUSLINE_LEFT",
        default: DefaultValue::List(&["model"]),
        ui: UiMeta {
            tab: "Status line",
            group: "Layout",
            label: "Left segments",
            description: "Segments drawn from the left, in order.",
        },
    },
    SettingSpec {
        id: "statusline.right",
        kind: SettingKind::Array,
        env: "HORON_TUI_STATUSLINE_RIGHT",
        default: DefaultValue::List(&["status"]),
        ui: UiMeta {
            tab: "Status line",
            group: "Layout",
            label: "Right segments",
            description: "Segments drawn from the right, in order.",
        },
    },
    SettingSpec {
        id: "statusline.path_max",
        kind: SettingKind::Number,
        env: "HORON_TUI_STATUSLINE_PATH_MAX",
        default: DefaultValue::Number(40),
        ui: UiMeta {
            tab: "Status line",
            group: "Segments",
            label: "Path length",
            description: "Longest directory the path segment shows before it is shortened; 0 keeps it whole.",
        },
    },
    SettingSpec {
        id: "statusline.context_window",
        kind: SettingKind::Number,
        env: "HORON_TUI_STATUSLINE_CONTEXT_WINDOW",
        default: DefaultValue::Number(0),
        ui: UiMeta {
            tab: "Status line",
            group: "Segments",
            label: "Context window",
            description: "Tokens the model can hold, used for the context percentage; 0 hides the segment.",
        },
    },
    SettingSpec {
        id: "statusline.separator",
        kind: SettingKind::Enum(STATUSLINE_SEPARATORS),
        env: "HORON_TUI_STATUSLINE_SEPARATOR",
        default: DefaultValue::Text("preset"),
        ui: UiMeta {
            tab: "Status line",
            group: "Style",
            label: "Separator",
            description: "Mark between the parts: the glyph set's own (`preset`), a dot, or a bar.",
        },
    },
    SettingSpec {
        id: "statusline.background",
        kind: SettingKind::Enum(STATUSLINE_BACKGROUNDS),
        env: "HORON_TUI_STATUSLINE_BACKGROUND",
        default: DefaultValue::Text("theme"),
        ui: UiMeta {
            tab: "Status line",
            group: "Style",
            label: "Background",
            description: "The theme's bottom-line colour, or the terminal's own background unpainted.",
        },
    },
    SettingSpec {
        id: "editor.submit_on_enter",
        kind: SettingKind::Bool,
        env: "HORON_TUI_EDITOR_SUBMIT_ON_ENTER",
        default: DefaultValue::Bool(true),
        ui: UiMeta {
            tab: "Editor",
            group: "Keys",
            label: "Enter sends",
            description: "Send the typed message with Enter; off makes Enter insert a newline.",
        },
    },
    SettingSpec {
        id: "editor.suggestions",
        kind: SettingKind::Bool,
        env: "HORON_TUI_EDITOR_SUGGESTIONS",
        default: DefaultValue::Bool(false),
        ui: UiMeta {
            tab: "Editor",
            group: "Suggestions",
            label: "Local suggestions",
            description: "Offer a ghost completion of the draft from drafts sent before this one; nothing leaves the client.",
        },
    },
    SettingSpec {
        id: "transcript.tool_output_lines",
        kind: SettingKind::Number,
        env: "HORON_TUI_TRANSCRIPT_TOOL_OUTPUT_LINES",
        default: DefaultValue::Number(3),
        ui: UiMeta {
            tab: "Transcript",
            group: "Content",
            label: "Tool output lines",
            description: "Rows of a finished tool call's output shown under its line; 0 hides the preview.",
        },
    },
    SettingSpec {
        id: "transcript.mouse_scroll",
        kind: SettingKind::Bool,
        env: "HORON_TUI_TRANSCRIPT_MOUSE_SCROLL",
        default: DefaultValue::Bool(true),
        ui: UiMeta {
            tab: "Transcript",
            group: "Keys",
            label: "Wheel scrolls",
            description: "Give the mouse wheel to the transcript; off keeps the terminal's own text selection.",
        },
    },
    SettingSpec {
        id: "transcript.show_reasoning",
        kind: SettingKind::Bool,
        env: "HORON_TUI_TRANSCRIPT_SHOW_REASONING",
        default: DefaultValue::Bool(false),
        ui: UiMeta {
            tab: "Transcript",
            group: "Content",
            label: "Show thinking",
            description: "Keep the model's reasoning and thinking blocks in the transcript; off hides them as they stream.",
        },
    },
    SettingSpec {
        id: "transcript.markdown",
        kind: SettingKind::Bool,
        env: "HORON_TUI_TRANSCRIPT_MARKDOWN",
        default: DefaultValue::Bool(true),
        ui: UiMeta {
            tab: "Transcript",
            group: "Content",
            label: "Markdown",
            description: "Render the model's Markdown; off draws its text with the markers as typed.",
        },
    },
    SettingSpec {
        id: "transcript.visual_blocks",
        kind: SettingKind::Bool,
        env: "HORON_TUI_TRANSCRIPT_VISUAL_BLOCKS",
        default: DefaultValue::Bool(true),
        ui: UiMeta {
            tab: "Transcript",
            group: "Content",
            label: "Visual blocks",
            description: "Draw a diagram fence's one-line flow; off draws the fence as typed.",
        },
    },
];

/// The spec for `id`, or `None` when the key is not declared.
pub fn spec(id: &str) -> Option<&'static SettingSpec> {
    REGISTRY.iter().find(|setting| setting.id == id)
}

/// Every declared key, in registry order.
pub fn ids() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|setting| setting.id)
}

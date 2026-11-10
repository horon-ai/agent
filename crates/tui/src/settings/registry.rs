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
        id: "transcript.show_reasoning",
        kind: SettingKind::Bool,
        env: "HORON_TUI_TRANSCRIPT_SHOW_REASONING",
        default: DefaultValue::Bool(false),
        ui: UiMeta {
            tab: "Transcript",
            group: "Content",
            label: "Show reasoning",
            description: "Keep the model's reasoning lines in the transcript.",
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

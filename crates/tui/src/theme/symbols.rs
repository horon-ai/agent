//! Glyph presets for the few marks the interface draws.
//!
//! A theme may name a preset, and the `symbols` setting overrides it, so a
//! terminal without a Nerd Font can still choose plain ASCII. Every mark is a
//! `&'static str` because the set is fixed at compile time.

/// A named glyph set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolPreset {
    /// The default: single-column Unicode marks.
    Unicode,
    /// Private-use Nerd Font glyphs.
    Nerd,
    /// Plain ASCII, for terminals without Unicode.
    Ascii,
}

impl SymbolPreset {
    /// Parse a settings or theme value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unicode" => Some(Self::Unicode),
            "nerd" => Some(Self::Nerd),
            "ascii" => Some(Self::Ascii),
            _ => None,
        }
    }

    /// The name the settings registry and theme files use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unicode => "unicode",
            Self::Nerd => "nerd",
            Self::Ascii => "ascii",
        }
    }

    /// The marks of this preset.
    pub fn symbols(self) -> Symbols {
        match self {
            Self::Unicode => Symbols {
                bullet: "•",
                tool_running: "◐",
                tool_done: "✓",
                tool_error: "✕",
                prompt_marker: "❯",
                separator: "·",
            },
            Self::Nerd => Symbols {
                bullet: "\u{f111}",
                tool_running: "\u{f110}",
                tool_done: "\u{f00c}",
                tool_error: "\u{f00d}",
                prompt_marker: "\u{f054}",
                separator: "\u{e0b1}",
            },
            Self::Ascii => Symbols {
                bullet: "*",
                tool_running: "~",
                tool_done: "+",
                tool_error: "x",
                prompt_marker: ">",
                separator: "|",
            },
        }
    }
}

/// The glyphs one preset draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Symbols {
    /// A transcript bullet for notices.
    pub bullet: &'static str,
    /// A tool call that is still running.
    pub tool_running: &'static str,
    /// A tool call that finished without an error.
    pub tool_done: &'static str,
    /// A tool call that failed.
    pub tool_error: &'static str,
    /// The mark before the input line.
    pub prompt_marker: &'static str,
    /// Joins the parts of the status line.
    pub separator: &'static str,
}

impl Default for Symbols {
    fn default() -> Self {
        SymbolPreset::Unicode.symbols()
    }
}

impl Symbols {
    /// Columns the prompt marker occupies before the input text.
    pub fn prompt_marker_width(self) -> u16 {
        u16::try_from(self.prompt_marker.chars().count()).unwrap_or(1)
    }
}

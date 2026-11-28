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
                more_below: "↓",
                separator: "·",
                quote: "│",
                rule: "─",
                bar_full: "█",
                bar_empty: "░",
                spinner: &["⠋", "⠙", "⠹", "⠸"],
                border: Border::ROUNDED,
            },
            Self::Nerd => Symbols {
                bullet: "\u{f111}",
                tool_running: "\u{f110}",
                tool_done: "\u{f00c}",
                tool_error: "\u{f00d}",
                prompt_marker: "\u{f054}",
                more_below: "\u{f063}",
                separator: "\u{e0b1}",
                quote: "\u{2502}",
                rule: "\u{2500}",
                bar_full: "\u{2588}",
                bar_empty: "\u{2591}",
                spinner: &["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}"],
                border: Border::ROUNDED,
            },
            Self::Ascii => Symbols {
                bullet: "*",
                tool_running: "~",
                tool_done: "+",
                tool_error: "x",
                prompt_marker: ">",
                more_below: "v",
                separator: "|",
                quote: "|",
                rule: "-",
                bar_full: "#",
                bar_empty: "-",
                spinner: &["-", "\\", "|", "/"],
                border: Border::ASCII,
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
    /// The mark before the count of rows hidden below a held transcript view.
    pub more_below: &'static str,
    /// Joins the parts of the status line.
    pub separator: &'static str,
    /// The bar before a block quote, repeated once per level of nesting.
    pub quote: &'static str,
    /// The mark a horizontal rule is drawn with, repeated across the pane.
    pub rule: &'static str,
    /// A filled cell of a proportional bar, as the `/context` report draws.
    pub bar_full: &'static str,
    /// An empty cell of a proportional bar.
    pub bar_empty: &'static str,
    /// The frames of the spinner beside a running turn, in the order they turn.
    ///
    /// The first frame is what a `reduced` preset freezes on, so it carries the
    /// same reading as the running glyph: something is still happening. The
    /// frames are their own rotation rather than `tool_running` repeated: a
    /// tool row waiting on a decision and a turn in flight are two different
    /// things, and the same mark would read as one.
    pub spinner: &'static [&'static str],
    /// The box a tool call's card is drawn in.
    pub border: Border,
}

/// The pieces of a card's box, each one column wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Border {
    /// The corner that opens the title row.
    pub top_left: &'static str,
    /// The corner that closes the title row.
    pub top_right: &'static str,
    /// The corner that opens the closing row.
    pub bottom_left: &'static str,
    /// The corner that closes the closing row.
    pub bottom_right: &'static str,
    /// The line the top, the bottom and a divider are drawn with.
    pub horizontal: &'static str,
    /// The side of every row between the top and the bottom.
    pub vertical: &'static str,
    /// Where a divider meets the left side.
    pub tee_left: &'static str,
    /// Where a divider meets the right side.
    pub tee_right: &'static str,
}

impl Border {
    /// Box-drawing characters with rounded corners.
    pub const ROUNDED: Self = Self {
        top_left: "╭",
        top_right: "╮",
        bottom_left: "╰",
        bottom_right: "╯",
        horizontal: "─",
        vertical: "│",
        tee_left: "├",
        tee_right: "┤",
    };

    /// Plain ASCII, for terminals without box-drawing characters.
    pub const ASCII: Self = Self {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        horizontal: "-",
        vertical: "|",
        tee_left: "+",
        tee_right: "+",
    };
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

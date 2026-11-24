//! The opt-in palette additions and removals are drawn in.
//!
//! A change is text first: a unified-diff row keeps the `+` or `-` the tool
//! wrote, and a call's outcome keeps the tick or the cross of the symbol preset.
//! Colour is the second signal, and the theme's own `success` and `error` pair
//! is not readable to every user: red and green sit on the same axis for the
//! two most common colour-vision differences, so one row of each reads as one
//! colour at two brightnesses.
//!
//! A row is read as a change only inside a block that carries a diff marker
//! ([`looks_like_diff`]), because a leading sign on its own is ordinary text: a
//! long directory listing gives every file a `-`, and a tool's prose may hold a
//! Markdown bullet. The diff's file and hunk headers are the diff's own
//! description of the file rather than a change to it, and keep the output's
//! ordinary colour ([`change_of`]).
//!
//! [`ChangePalette::Accessible`] repaints the positive and negative tokens in a
//! teal and a red-orange. Measured in OKLCH, the two hues are 153.5 degrees
//! apart in `dark` and 164.5 in `light`, which keeps them apart under
//! deuteranopia, protanopia and tritanopia as well as for full colour vision,
//! and each is at least 34 degrees from the amber the client already uses for a
//! warning. The contrast of both colours against the surfaces the built-in
//! themes paint is pinned by a test in `theme/tests.rs`, so the numbers in the
//! constants below are checked rather than claimed.
//!
//! A theme says which appearance it was built for through its own text colour:
//! light text is a theme for a dark background, dark text is a theme for a
//! light one. That is the only signal available, because a theme does not name
//! the terminal's background, and it is enough to choose between the bright
//! pair and the deep pair.
//!
//! Nothing on disk changes. The palette is applied to the copy of the theme the
//! renderer reads, so turning the setting off draws the selected theme exactly
//! as its file wrote it, down to the last token.

use ratatui::style::{Color, Style};

use crate::settings::Settings;

use super::schema::Theme;

/// The setting that picks a palette: `theme` or `accessible`.
const SETTING: &str = "appearance.changes";

/// Added colour on a dark surface, teal.
///
/// Measured on `dark`'s own surfaces: 9.34:1 on `status_bg`, 7.34:1 on
/// `selected_bg`, 12.61:1 on a black terminal background.
const DARK_ADDED: Color = Color::Rgb(0x73, 0xda, 0xca);

/// Removed colour on a dark surface, red-orange.
///
/// Measured on `dark`'s own surfaces: 6.79:1 on `status_bg`, 5.33:1 on
/// `selected_bg`, 9.17:1 on a black terminal background.
const DARK_REMOVED: Color = Color::Rgb(0xff, 0x8a, 0x7a);

/// Added colour on a light surface, deep teal.
///
/// Measured on `light`'s own surfaces: 5.32:1 on `status_bg`, 5.13:1 on
/// `selected_bg`, 6.46:1 on a white terminal background.
const LIGHT_ADDED: Color = Color::Rgb(0x11, 0x69, 0x69);

/// Removed colour on a light surface, deep red-orange.
///
/// Measured on `light`'s own surfaces: 5.30:1 on `status_bg`, 5.12:1 on
/// `selected_bg`, 6.45:1 on a white terminal background.
const LIGHT_REMOVED: Color = Color::Rgb(0xb0, 0x2e, 0x1f);

/// The mid-grey text luminance that separates the two appearances.
///
/// Both built-in themes sit far from it (`dark` text is 0.56, `light` text is
/// 0.01), so only a deliberately mid-grey text colour is a judgement call, and
/// such a theme is treated as a light one: the deep pair is the safer mistake,
/// because a colour too dark to read is obvious and one too pale to tell apart
/// is not.
const MID_TEXT: f64 = 0.2;

/// Which pair of colours additions and removals are drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangePalette {
    /// The theme's own `success` and `error` colours.
    Theme,
    /// A pair that stays distinguishable under colour-vision differences.
    Accessible,
}

impl ChangePalette {
    /// The palette a settings value names, or `None` for an unknown word.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "theme" => Some(Self::Theme),
            "accessible" => Some(Self::Accessible),
            _ => None,
        }
    }

    /// The word the settings file uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Accessible => "accessible",
        }
    }

    /// The palette the settings in force select.
    ///
    /// The loader rejects a value outside the declared set, so an unreadable
    /// word here means no layer set the key and the theme keeps its own pair.
    pub fn from_settings(settings: &Settings) -> Self {
        settings.text(SETTING).and_then(Self::parse).unwrap_or(Self::Theme)
    }

    /// The colours this palette draws additions and removals in.
    ///
    /// [`Self::Theme`] answers the theme's own pair unchanged, which is what
    /// makes the setting reversible: the same call restores what the file says.
    pub fn colors(self, theme: &Theme) -> (Color, Color) {
        match self {
            Self::Theme => (theme.core.success, theme.core.error),
            Self::Accessible if for_light_background(theme) => (LIGHT_ADDED, LIGHT_REMOVED),
            Self::Accessible => (DARK_ADDED, DARK_REMOVED),
        }
    }
}

/// One row of a tool's output, as the client reads a change in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A line the diff adds.
    Added,
    /// A line the diff removes.
    Removed,
}

/// The change a unified-diff row carries, if it carries one.
///
/// A file header (`--- a/f`, `+++ b/f`), a hunk header (`@@ -1,2 +1,3 @@`), a
/// `diff --git` or `index` line and a `\ No newline at end of file` note are
/// the diff's own description of the file rather than a change to it, so they
/// answer `None` and keep the output's ordinary colour. A line of content after
/// them starts with exactly one sign, and that sign is the marker a reader who
/// cannot see the colour follows, which is why the row itself is never rewritten.
pub fn change_of(row: &str) -> Option<Change> {
    // Three signs are the header's, and a header carries the path after a
    // space or a tab; no content line starts that way.
    if row.starts_with("+++") || row.starts_with("---") {
        return None;
    }
    match row.as_bytes().first() {
        Some(b'+') => Some(Change::Added),
        Some(b'-') => Some(Change::Removed),
        _ => None,
    }
}

/// Whether a block of a tool's output carries a unified diff.
///
/// A leading sign alone does not make a row a change: `ls -l` prints every
/// ordinary file with a leading `-`, and a tool's own prose may hold a Markdown
/// bullet. The block is read as a diff only when it carries one of the diff's
/// own markers, which `git diff`, an `edit` call's `File diff:` block and a bare
/// hunk header all do.
pub fn looks_like_diff(rows: &[String]) -> bool {
    rows.iter().any(|row| diff_marker(row))
}

/// [`looks_like_diff`] for a kept output that is still the text it came from.
///
/// The pane holds a cut call's whole output as one borrowed string, so its rows
/// are read without being built first.
pub fn looks_like_diff_text(text: &str) -> bool {
    text.lines().any(diff_marker)
}

/// Whether one row is the diff's own description of a block.
fn diff_marker(row: &str) -> bool {
    row.starts_with("diff --git") || row.starts_with("```diff") || row.starts_with("@@")
}

/// Whether a theme was built for a light background.
///
/// The theme does not name its background, so this reads the one colour every
/// theme sets for a purpose that cannot be inverted: its text. Dark text is
/// drawn on a light background, and light text on a dark one. A text colour the
/// terminal owns says nothing at all, and is read as a dark background.
fn for_light_background(theme: &Theme) -> bool {
    relative_luminance(theme.core.text).is_some_and(|luminance| luminance <= MID_TEXT)
}

/// The theme as the palette draws it.
///
/// The positive and negative tokens move together, so a change reads the same
/// in the transcript, in a report's yes and in the status line's state word.
/// Every other token is copied untouched.
pub fn apply(theme: &Theme, palette: ChangePalette) -> Theme {
    let mut drawn = theme.clone();
    drawn.changes = palette;
    let (added, removed) = palette.colors(theme);
    drawn.core.success = added;
    drawn.transcript.tool_done = added;
    drawn.core.error = removed;
    drawn.transcript.tool_error = removed;
    drawn
}

/// How the rows of one block of a call's output are coloured.
///
/// Chosen once per call, then asked per row: the pane builds only the rows it
/// shows, so the rows it does show still have to be told apart one at a time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowStyle {
    /// Every row in one style, which is the whole block when the theme's own
    /// change pair is in force.
    Plain(Style),
    /// A change row in its own colour, every other row in `plain`.
    Changes {
        /// The style of a context row, a file header or a hunk header.
        plain: Style,
        /// The style of a line the diff adds.
        added: Style,
        /// The style of a line the diff removes.
        removed: Style,
    },
}

impl RowStyle {
    /// The style `row` is drawn in.
    ///
    /// `row` is asked as it reads, so the decision costs one prefix comparison
    /// for a plain block and allocates nothing either way.
    pub fn of(self, row: &str) -> Style {
        match self {
            Self::Plain(style) => style,
            Self::Changes { plain, added, removed } => match change_of(row) {
                Some(Change::Added) => added,
                Some(Change::Removed) => removed,
                None => plain,
            },
        }
    }

    /// The style of a row that is not a change: the block's own style.
    ///
    /// A count row the client adds itself (`+3 lines`) starts with the same
    /// sign an added line does, so the rows the client builds are drawn through
    /// here rather than through [`Self::of`].
    pub fn plain(self) -> Style {
        match self {
            Self::Plain(style) => style,
            Self::Changes { plain, .. } => plain,
        }
    }

    /// How a call's output is coloured under `theme`.
    ///
    /// `diff` answers whether the block carries a unified diff, and is asked
    /// only where the answer can matter: a block that is not a diff keeps one
    /// style even with the accessible pair on, because a sign at the start of a
    /// row means a change only inside a diff. The palette is read once here,
    /// not once per row.
    pub fn for_theme(theme: &Theme, plain: Style, diff: impl FnOnce() -> bool) -> Self {
        if theme.changes == ChangePalette::Theme || !diff() {
            return Self::Plain(plain);
        }
        Self::Changes {
            plain,
            added: Style::default().fg(theme.core.success),
            removed: Style::default().fg(theme.core.error),
        }
    }
}

/// WCAG relative luminance of a theme colour, or `None` for a colour the
/// terminal owns.
///
/// A theme may leave text at the terminal's default (`Color::Reset`) or name a
/// palette index, and neither says anything about the background. The palette
/// then falls back to its dark pair, which is what `Theme::default` assumes and
/// what the built-in `dark` theme is for.
fn relative_luminance(color: Color) -> Option<f64> {
    let Color::Rgb(red, green, blue) = color else {
        return None;
    };
    let channel = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue))
}

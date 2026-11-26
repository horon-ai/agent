//! What the draft carries besides its text: a chosen skill and pasted images.
//!
//! Both are blocks rather than characters of the draft. A skill chosen from the
//! slash popup is drawn as `✦ name` before the text, and an image pasted from
//! the clipboard or dropped as a file path is drawn as `📷 #N` there, with a
//! thumbnail card above the status line. `backspace` at the start of the draft
//! takes the last block off.
//!
//! The message itself stays text, so the queue, a refusal and a retry keep
//! working on strings: a send writes `/skill:<name>` and one `[Image #N]` marker
//! per image into the line, and the bridge attaches the image every marker
//! names from [`Attachments::images_for`]. The images stay in the store after
//! the draft lets them go, because a queued or refused message still names them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Columns of image inside one thumbnail card.
const THUMB_COLS: u32 = 20;
/// Rows of image inside one thumbnail card; each row is two pixels.
const THUMB_ROWS: u32 = 4;
/// Cells one card takes: the image and a side on each edge.
const CARD_WIDTH: usize = THUMB_COLS as usize + 2;
/// Blank cells between two cards.
const CARD_GAP: usize = 2;
/// Rows the card panel takes: the image and a border above and below it.
pub const PANEL_ROWS: u16 = THUMB_ROWS as u16 + 2;
/// The longest side an image is sent with; a larger one is scaled down first.
const MAX_SIDE: u32 = 2000;
/// The largest encoded image sent as it was pasted; a larger one is re-encoded.
const MAX_BYTES: usize = 3_500_000;
/// Extensions a pasted path is read as an image for.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg"];
/// The colour of a skill block.
const SKILL_COLOR: Color = Color::Rgb(198, 120, 221);
/// The colour of an image block's number.
const IMAGE_COLOR: Color = Color::Rgb(126, 211, 151);
/// The card borders, one per image number in turn.
const CARD_COLORS: [Color; 4] = [
    Color::Rgb(97, 175, 239),
    Color::Rgb(198, 120, 221),
    Color::Rgb(126, 211, 151),
    Color::Rgb(229, 192, 123),
];

/// Why an image could not be attached, phrased for the warning line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageError(String);

impl From<String> for ImageError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for ImageError {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One pasted image: what is sent and what the card draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The media type the provider reads the data as.
    media_type: String,
    /// The encoded image, base64.
    data: String,
    /// The pasted image's own size, drawn under its card.
    width: u32,
    height: u32,
    /// The card's picture: `cols` by `pixel_rows` colours, row by row.
    cols: u32,
    pixel_rows: u32,
    pixels: Vec<[u8; 3]>,
}

impl Image {
    /// Decode an image, keep a small picture of it, and make it sendable.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ImageError> {
        let format = image::guess_format(&bytes).map_err(|error| format!("not an image: {error}"))?;
        let decoded = image::load_from_memory(&bytes).map_err(|error| format!("cannot read the image: {error}"))?;
        let (width, height) = (decoded.width(), decoded.height());
        if width == 0 || height == 0 {
            return Err("the image is empty".into());
        }
        let passes = matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg)
            && bytes.len() <= MAX_BYTES
            && width.max(height) <= MAX_SIDE;
        let (media_type, encoded) = if passes {
            let media_type = if format == image::ImageFormat::Png {
                "image/png"
            } else {
                "image/jpeg"
            };
            (media_type.to_string(), bytes)
        } else {
            // Formats a provider may refuse, and pictures larger than it takes,
            // are sent as a PNG no wider or taller than the cap.
            let scaled = if width.max(height) > MAX_SIDE {
                decoded.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle)
            } else {
                decoded.clone()
            };
            let mut out = Vec::new();
            scaled
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .map_err(|error| format!("cannot encode the image: {error}"))?;
            ("image/png".to_string(), out)
        };
        // A cell is about twice as tall as it is wide, and each cell holds two
        // pixels stacked, so the picture's pixels come out roughly square.
        let fit = f64::min(
            f64::from(THUMB_COLS) / f64::from(width),
            f64::from(THUMB_ROWS * 2) / f64::from(height),
        );
        let cols = scaled_side(width, fit, THUMB_COLS);
        let pixel_rows = scaled_side(height, fit, THUMB_ROWS * 2);
        let small = decoded
            .resize_exact(cols, pixel_rows, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let pixels = small.pixels().map(|pixel| pixel.0).collect();
        Ok(Self {
            media_type,
            data: base64::engine::general_purpose::STANDARD.encode(encoded),
            width,
            height,
            cols,
            pixel_rows,
            pixels,
        })
    }

    /// The colour at a pixel of the card's picture, if the picture covers it.
    fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.cols || y >= self.pixel_rows {
            return None;
        }
        let [r, g, b] = self.pixels[(y * self.cols + x) as usize];
        Some(Color::Rgb(r, g, b))
    }
}

/// One side of a picture scaled by `fit`, at least one pixel and at most `cap`.
fn scaled_side(side: u32, fit: f64, cap: u32) -> u32 {
    let scaled = (f64::from(side) * fit).round();
    if scaled < 1.0 {
        1
    } else if scaled >= f64::from(cap) {
        cap
    } else {
        // Rounded and inside `1..cap`, so the conversion is exact.
        scaled as u32
    }
}

/// The blocks the draft carries, and every image it has carried.
#[derive(Debug, Clone, Default)]
pub struct Attachments {
    /// The skill the draft invokes.
    skill: Option<String>,
    /// The images the draft carries, in the order they were added.
    draft: Vec<u32>,
    /// Every image pasted in this client, by number.
    store: BTreeMap<u32, Image>,
    /// The number the last image took.
    last: u32,
}

impl Attachments {
    /// Whether the draft carries a block.
    pub fn is_empty(&self) -> bool {
        self.skill.is_none() && self.draft.is_empty()
    }

    /// The skill the draft invokes.
    pub fn skill(&self) -> Option<&str> {
        self.skill.as_deref()
    }

    /// Make the draft invoke `name`.
    pub fn set_skill(&mut self, name: &str) {
        self.skill = Some(name.to_string());
    }

    /// Add an image to the draft and answer its number.
    pub fn add_image(&mut self, image: Image) -> u32 {
        self.last += 1;
        self.store.insert(self.last, image);
        self.draft.push(self.last);
        self.last
    }

    /// The image numbers the draft carries.
    pub fn draft_images(&self) -> &[u32] {
        &self.draft
    }

    /// Take the last block off the draft: the newest image, then the skill.
    pub fn pop(&mut self) -> bool {
        if self.draft.pop().is_some() {
            return true;
        }
        self.skill.take().is_some()
    }

    /// Drop every block from the draft; the images stay in the store.
    pub fn clear(&mut self) {
        self.skill = None;
        self.draft.clear();
    }

    /// The line to send for `text`, the blocks written into it, and the draft
    /// emptied of them.
    ///
    /// Each image becomes an `[Image #N]` marker ahead of the text, and the
    /// skill becomes the `/skill:<name>` address the line starts with.
    pub fn compose(&mut self, text: &str) -> String {
        let mut body: Vec<String> = self.draft.iter().map(|id| format!("[Image #{id}]")).collect();
        let typed = text.trim();
        if !typed.is_empty() {
            body.push(typed.to_string());
        }
        let body = body.join(" ");
        let line = match &self.skill {
            Some(skill) if body.is_empty() => format!("/skill:{skill}"),
            Some(skill) => format!("/skill:{skill} {body}"),
            None => body,
        };
        self.clear();
        line
    }

    /// The images the markers in `text` name, as `(media type, base64)`, each
    /// once and in the order they are named.
    pub fn images_for(&self, text: &str) -> Vec<(String, String)> {
        let mut named: Vec<u32> = Vec::new();
        let mut rest = text;
        while let Some(at) = rest.find("[Image #") {
            rest = &rest[at + "[Image #".len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if rest[digits.len()..].starts_with(']')
                && let Ok(id) = digits.parse::<u32>()
                && !named.contains(&id)
            {
                named.push(id);
            }
        }
        named
            .iter()
            .filter_map(|id| self.store.get(id))
            .map(|image| (image.media_type.clone(), image.data.clone()))
            .collect()
    }

    /// The blocks drawn before the draft's first row, a space after each.
    pub fn chip_spans(&self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        if let Some(skill) = &self.skill {
            let style = Style::default().fg(SKILL_COLOR).add_modifier(Modifier::BOLD);
            spans.push(Span::styled(format!("✦ {skill}"), style));
            spans.push(Span::raw(" "));
        }
        for id in &self.draft {
            spans.push(Span::raw("📷 "));
            spans.push(Span::styled(
                format!("#{id}"),
                Style::default().fg(IMAGE_COLOR).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
        }
        spans
    }

    /// The cells [`Self::chip_spans`] take.
    pub fn chip_cells(&self) -> u16 {
        let cells: usize = self.chip_spans().iter().map(|span| span.content.width()).sum();
        u16::try_from(cells).unwrap_or(u16::MAX)
    }

    /// Rows the thumbnail panel takes: none without images.
    pub fn panel_rows(&self) -> u16 {
        if self.draft.is_empty() { 0 } else { PANEL_ROWS }
    }

    /// The thumbnail cards of the draft's images, side by side, as many as fit
    /// `width` cells.
    pub fn panel_lines(&self, width: u16) -> Vec<Line<'static>> {
        let fits = (usize::from(width) + CARD_GAP) / (CARD_WIDTH + CARD_GAP);
        let shown: Vec<(u32, &Image)> = self
            .draft
            .iter()
            .filter_map(|id| self.store.get(id).map(|image| (*id, image)))
            .take(fits)
            .collect();
        let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new(); usize::from(PANEL_ROWS)];
        for (index, (id, image)) in shown.iter().enumerate() {
            let color = CARD_COLORS[(*id as usize).saturating_sub(1) % CARD_COLORS.len()];
            let edge = Style::default().fg(color);
            if index > 0 {
                for line in &mut lines {
                    line.push(Span::raw(" ".repeat(CARD_GAP)));
                }
            }
            lines[0].extend(rule("╭", "╮", &format!("📷 #{id}"), edge));
            let size = format!("{}x{}", image.width, image.height);
            lines[usize::from(PANEL_ROWS) - 1].extend(rule("╰", "╯", &size, edge));
            let left = (THUMB_COLS - image.cols) / 2;
            let top = (THUMB_ROWS * 2 - image.pixel_rows) / 4;
            for row in 0..THUMB_ROWS {
                let line = &mut lines[row as usize + 1];
                line.push(Span::styled("│", edge));
                for column in 0..THUMB_COLS {
                    let x = column.wrapping_sub(left);
                    let y = (row.wrapping_sub(top)).wrapping_mul(2);
                    let upper = image.pixel(x, y);
                    let lower = image.pixel(x, y.wrapping_add(1));
                    line.push(match (upper, lower) {
                        (Some(upper), Some(lower)) => Span::styled("▀", Style::default().fg(upper).bg(lower)),
                        (Some(upper), None) => Span::styled("▀", Style::default().fg(upper)),
                        (None, Some(lower)) => Span::styled("▄", Style::default().fg(lower)),
                        (None, None) => Span::raw(" "),
                    });
                }
                line.push(Span::styled("│", edge));
            }
        }
        lines.into_iter().map(Line::from).collect()
    }
}

/// A card's top or bottom border with `label` in its middle.
fn rule(open: &'static str, close: &'static str, label: &str, edge: Style) -> Vec<Span<'static>> {
    let inner = CARD_WIDTH - 2;
    let label_cells = label.width() + 2;
    let fill = inner.saturating_sub(label_cells);
    let before = fill / 2;
    vec![
        Span::styled(format!("{open}{}", "─".repeat(before)), edge),
        Span::styled(format!(" {label} "), edge.add_modifier(Modifier::BOLD)),
        Span::styled(format!("{}{close}", "─".repeat(fill - before)), edge),
    ]
}

/// The image file a paste names, when the pasted text is one path to one.
///
/// A file dropped onto a terminal arrives as its path: quoted, escaped with
/// backslashes, or as a `file://` URL depending on the terminal.
pub fn image_path(text: &str) -> Option<PathBuf> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return None;
    }
    let unquoted = trimmed
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .or_else(|| trimmed.strip_prefix('"').and_then(|inner| inner.strip_suffix('"')))
        .unwrap_or(trimmed);
    let unquoted = unquoted.strip_prefix("file://").unwrap_or(unquoted);
    let mut path = String::with_capacity(unquoted.len());
    let mut characters = unquoted.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => path.extend(characters.next()),
            _ => path.push(character),
        }
    }
    let path = PathBuf::from(path.replace("%20", " "));
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    (IMAGE_EXTENSIONS.contains(&extension.as_str()) && path.is_file()).then_some(path)
}

/// Read an image file for the draft.
pub fn read_image_file(path: &Path) -> Result<Image, ImageError> {
    let bytes = std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Image::from_bytes(bytes)
}

/// The image on the system clipboard, as PNG bytes.
///
/// The terminal passes text through a paste and never an image, so the client
/// asks the platform's clipboard tool directly.
pub fn clipboard_image() -> Result<Vec<u8>, ImageError> {
    // Chosen at run time rather than by `cfg`: the client keeps no
    // platform-specific lines, and each branch only starts a program.
    match std::env::consts::OS {
        "macos" => {
            let output = std::process::Command::new("osascript")
                .args(["-e", "get the clipboard as «class PNGf»"])
                .output()
                .map_err(|error| format!("cannot run osascript: {error}"))?;
            if !output.status.success() {
                return Err("there is no image on the clipboard".into());
            }
            let text = String::from_utf8_lossy(&output.stdout);
            let hex = text
                .split_once("«data PNGf")
                .and_then(|(_, rest)| rest.split_once('»'))
                .map(|(hex, _)| hex)
                .ok_or(ImageError::from("there is no image on the clipboard"))?;
            decode_hex(hex.trim())
        }
        "windows" => Err("pasting an image from the clipboard is not supported here; drop the file instead".into()),
        _ => {
            for (program, arguments) in [
                ("wl-paste", &["--no-newline", "--type", "image/png"][..]),
                (
                    "xclip",
                    &["-selection", "clipboard", "-target", "image/png", "-out"][..],
                ),
            ] {
                if let Ok(output) = std::process::Command::new(program).args(arguments).output()
                    && output.status.success()
                    && !output.stdout.is_empty()
                {
                    return Ok(output.stdout);
                }
            }
            Err("there is no image on the clipboard (wl-paste or xclip reads it)".into())
        }
    }
}

/// Bytes from the hexadecimal text `osascript` prints for clipboard data.
fn decode_hex(hex: &str) -> Result<Vec<u8>, ImageError> {
    if !hex.len().is_multiple_of(2) {
        return Err("the clipboard image is malformed".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).map_err(|_| "the clipboard image is malformed".into()))
        .collect()
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod tests;

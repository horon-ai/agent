//! The theme file schema and its validation.
//!
//! A theme is one JSON object with a required `name`, a required `colors`
//! table and optional `vars` and `symbols` tables. Validation runs before any
//! colour is used, and every failure names the theme, the group and the token,
//! because a theme with one missing token would otherwise render as a silently
//! uncoloured interface.
//!
//! A colour value is one of:
//!
//! - `"#RRGGBB"`, an exact RGB colour,
//! - an integer `0` to `255`, a 256-colour palette index,
//! - the name of an entry in `vars`,
//! - `""`, the terminal's own default colour.

use std::collections::BTreeMap;

use ratatui::style::Color;
use serde_json::Value;

use super::symbols::SymbolPreset;

/// The dark theme shipped with the binary.
pub const BUILTIN_DARK: &str = include_str!("../../themes/dark.json");
/// The light theme shipped with the binary.
pub const BUILTIN_LIGHT: &str = include_str!("../../themes/light.json");

/// The built-in source for `name`, if one has that name.
///
/// Built-ins win over a user file of the same name, so this is the first
/// lookup step.
pub fn builtin_source(name: &str) -> Option<&'static str> {
    match name {
        "dark" => Some(BUILTIN_DARK),
        "light" => Some(BUILTIN_LIGHT),
        _ => None,
    }
}

/// The `core` group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreColors {
    /// Selection and attention colour.
    pub accent: Color,
    /// Lines and frames.
    pub border: Color,
    /// Secondary text.
    pub muted: Color,
    /// Primary text.
    pub text: Color,
    /// A positive outcome.
    pub success: Color,
    /// Something worth noticing.
    pub warning: Color,
    /// A failure.
    pub error: Color,
}

impl Default for CoreColors {
    fn default() -> Self {
        Self {
            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            border: Color::Rgb(0x3b, 0x42, 0x61),
            muted: Color::Rgb(0x56, 0x5f, 0x89),
            text: Color::Rgb(0xc0, 0xca, 0xf5),
            success: Color::Rgb(0x9e, 0xce, 0x6a),
            warning: Color::Rgb(0xe0, 0xaf, 0x68),
            error: Color::Rgb(0xf7, 0x76, 0x8e),
        }
    }
}

/// The `transcript` group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptColors {
    /// A message the user sent.
    pub user_text: Color,
    /// Model output.
    pub assistant_text: Color,
    /// The name of a tool call.
    pub tool_title: Color,
    /// A tool call that is still running.
    pub tool_running: Color,
    /// A tool call that finished without an error.
    pub tool_done: Color,
    /// A tool call that failed.
    pub tool_error: Color,
    /// Session-level information.
    pub notice: Color,
}

impl Default for TranscriptColors {
    fn default() -> Self {
        Self {
            user_text: Color::Rgb(0x7d, 0xcf, 0xff),
            assistant_text: Color::Rgb(0xc0, 0xca, 0xf5),
            tool_title: Color::Rgb(0xbb, 0x9a, 0xf7),
            tool_running: Color::Rgb(0xe0, 0xaf, 0x68),
            tool_done: Color::Rgb(0x9e, 0xce, 0x6a),
            tool_error: Color::Rgb(0xf7, 0x76, 0x8e),
            notice: Color::Rgb(0x56, 0x5f, 0x89),
        }
    }
}

/// The `prompt` group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptColors {
    /// The frame of a prompt blocking the turn.
    pub prompt_border: Color,
    /// The prompt's text.
    pub prompt_text: Color,
    /// Background of the selected or headline prompt row.
    pub selected_bg: Color,
}

impl Default for PromptColors {
    fn default() -> Self {
        Self {
            prompt_border: Color::Rgb(0x7a, 0xa2, 0xf7),
            prompt_text: Color::Rgb(0xc0, 0xca, 0xf5),
            selected_bg: Color::Rgb(0x28, 0x34, 0x57),
        }
    }
}

/// The `status` group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusColors {
    /// Background of the bottom line.
    pub status_bg: Color,
    /// Foreground of the bottom line.
    pub status_text: Color,
    /// Colour of the attached-to part.
    pub status_accent: Color,
}

impl Default for StatusColors {
    fn default() -> Self {
        Self {
            status_bg: Color::Rgb(0x1f, 0x23, 0x35),
            status_text: Color::Rgb(0xc0, 0xca, 0xf5),
            status_accent: Color::Rgb(0x7a, 0xa2, 0xf7),
        }
    }
}

/// A validated theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// The theme's own name, which may differ from the file name.
    pub name: String,
    /// The `core` group.
    pub core: CoreColors,
    /// The `transcript` group.
    pub transcript: TranscriptColors,
    /// The `prompt` group.
    pub prompt: PromptColors,
    /// The `status` group.
    pub status: StatusColors,
    /// The preset this theme asks for, if any.
    pub symbols: Option<SymbolPreset>,
}

impl Default for Theme {
    /// The dark palette, used before the first theme file is resolved.
    fn default() -> Self {
        Self {
            name: "dark".to_string(),
            core: CoreColors::default(),
            transcript: TranscriptColors::default(),
            prompt: PromptColors::default(),
            status: StatusColors::default(),
            symbols: Some(SymbolPreset::Unicode),
        }
    }
}

/// Parse and validate one theme document.
///
/// `origin` names the file the text came from and appears in every error.
pub fn parse(text: &str, origin: &str) -> Result<Theme, String> {
    let root: Value = serde_json::from_str(text).map_err(|error| format!("{origin}: {error}"))?;
    let object = root
        .as_object()
        .ok_or_else(|| format!("{origin}: the theme must be a JSON object"))?;
    let name = object.get("name").and_then(Value::as_str).unwrap_or("");
    if name.trim().is_empty() {
        return Err(format!("{origin}: missing required string `name`"));
    }
    let error = |message: &str| format!("theme `{name}`: {message}");

    let mut vars: BTreeMap<&str, Color> = BTreeMap::new();
    if let Some(value) = object.get("vars") {
        let table = value.as_object().ok_or_else(|| error("`vars` must be an object"))?;
        for (key, value) in table {
            let color = concrete_color(value).map_err(|problem| error(&format!("vars.{key}: {problem}")))?;
            vars.insert(key.as_str(), color);
        }
    }

    let colors = object
        .get("colors")
        .ok_or_else(|| error("missing the `colors` object"))?
        .as_object()
        .ok_or_else(|| error("`colors` must be an object"))?;
    let mut resolved: BTreeMap<&str, Color> = BTreeMap::new();
    for (token, value) in colors {
        let color = resolve_color(value, &vars).map_err(|problem| error(&format!("colors.{token}: {problem}")))?;
        resolved.insert(token.as_str(), color);
    }

    let token = |group: &str, token: &str| {
        resolved
            .get(token)
            .copied()
            .ok_or_else(|| error(&format!("group `{group}` is missing token `{token}`")))
    };

    let core = CoreColors {
        accent: token("core", "accent")?,
        border: token("core", "border")?,
        muted: token("core", "muted")?,
        text: token("core", "text")?,
        success: token("core", "success")?,
        warning: token("core", "warning")?,
        error: token("core", "error")?,
    };
    let transcript = TranscriptColors {
        user_text: token("transcript", "user_text")?,
        assistant_text: token("transcript", "assistant_text")?,
        tool_title: token("transcript", "tool_title")?,
        tool_running: token("transcript", "tool_running")?,
        tool_done: token("transcript", "tool_done")?,
        tool_error: token("transcript", "tool_error")?,
        notice: token("transcript", "notice")?,
    };
    let prompt = PromptColors {
        prompt_border: token("prompt", "prompt_border")?,
        prompt_text: token("prompt", "prompt_text")?,
        selected_bg: token("prompt", "selected_bg")?,
    };
    let status = StatusColors {
        status_bg: token("status", "status_bg")?,
        status_text: token("status", "status_text")?,
        status_accent: token("status", "status_accent")?,
    };

    let symbols = match object.get("symbols") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let table = value
                .as_object()
                .ok_or_else(|| error("group `symbols` must be an object"))?;
            match table.get("preset") {
                None | Some(Value::Null) => None,
                Some(Value::String(preset)) => Some(
                    SymbolPreset::parse(preset)
                        .ok_or_else(|| error(&format!("group `symbols` has unknown preset `{preset}`")))?,
                ),
                Some(_) => return Err(error("group `symbols`: `preset` must be a string")),
            }
        }
    };

    Ok(Theme {
        name: name.to_string(),
        core,
        transcript,
        prompt,
        status,
        symbols,
    })
}

/// A `vars` value, which must name a concrete colour.
fn concrete_color(value: &Value) -> Result<Color, String> {
    match value {
        Value::String(text) if text.is_empty() => Ok(Color::Reset),
        Value::String(text) if text.starts_with('#') => parse_hex(text),
        Value::String(text) => Err(format!(
            "`{text}` is not a colour; a vars entry cannot name another var"
        )),
        Value::Number(number) => parse_index(number),
        _ => Err("expected \"#RRGGBB\", a 0-255 index or \"\"".to_string()),
    }
}

/// A `colors` value, which may also name a `vars` entry.
fn resolve_color(value: &Value, vars: &BTreeMap<&str, Color>) -> Result<Color, String> {
    match value {
        Value::String(text) if text.is_empty() => Ok(Color::Reset),
        Value::String(text) if text.starts_with('#') => parse_hex(text),
        Value::String(text) => vars
            .get(text.as_str())
            .copied()
            .ok_or_else(|| format!("`{text}` is not a declared var")),
        Value::Number(number) => parse_index(number),
        _ => Err("expected \"#RRGGBB\", a 0-255 index, a var name or \"\"".to_string()),
    }
}

/// `"#RRGGBB"` to `Color::Rgb`.
fn parse_hex(text: &str) -> Result<Color, String> {
    let digits = &text[1..];
    if digits.len() != 6 || !digits.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(format!("`{text}` is not a #RRGGBB colour"));
    }
    let value =
        u32::from_str_radix(digits, 16).map_err(|problem| format!("`{text}` is not a #RRGGBB colour: {problem}"))?;
    Ok(Color::Rgb(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ))
}

/// A 0-255 palette index to `Color::Indexed`.
fn parse_index(number: &serde_json::Number) -> Result<Color, String> {
    let index = number
        .as_u64()
        .ok_or_else(|| format!("`{number}` is not a non-negative index"))?;
    let index = u8::try_from(index).map_err(|_| format!("`{index}` is outside 0-255"))?;
    Ok(Color::Indexed(index))
}

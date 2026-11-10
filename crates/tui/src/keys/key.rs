//! One canonical spelling for a keystroke.
//!
//! The default table, `keybindings.toml` and the terminal events all name keys
//! differently: crossterm reports `KeyCode::Char('J')` with a shift modifier for
//! a capital, and a file may say `Shift+Ctrl+Return`. [`Key`] is the single
//! spelling they meet on, so two ways of naming one keystroke cannot become two
//! bindings.
//!
//! Normalisation is total for the keys the interface can act on and refuses
//! everything else with a sentence: an unknown name in the file becomes a
//! warning rather than a silently dead binding.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A key with its modifiers in canonical form.
///
/// A character key never carries the shift modifier: the terminal delivers the
/// shifted character itself, so `A` and `shift+a` are the same key spelled two
/// ways. Every other key keeps shift, which is what makes `shift+enter` a
/// binding distinct from `enter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    /// The key itself.
    pub code: Code,
    /// Control was held.
    pub ctrl: bool,
    /// Alt (Option on macOS) was held.
    pub alt: bool,
    /// Shift was held on a key that does not carry the shift in its character.
    pub shift: bool,
}

/// The key itself, without modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Code {
    /// A printable character, lower-cased.
    Char(char),
    /// Enter, also spelled `return`.
    Enter,
    /// Escape, also spelled `esc`.
    Esc,
    /// Tab; shift+tab is [`Code::Tab`] with `shift` set.
    Tab,
    /// Backspace.
    Backspace,
    /// The forward delete key.
    Delete,
    /// Arrow keys.
    Left,
    /// Arrow keys.
    Right,
    /// Arrow keys.
    Up,
    /// Arrow keys.
    Down,
    /// Start of line.
    Home,
    /// End of line.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// A function key, 1 through 12.
    Function(u8),
}

impl Key {
    /// Read a key name from the configuration file.
    ///
    /// Modifiers come first in any order and are written `ctrl`, `alt` and
    /// `shift`; they are reordered to the canonical `ctrl+alt+shift+key`.
    /// Matching is case-insensitive throughout, so `Control+J` and `ctrl+j`
    /// are one key.
    pub fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("an empty key name".to_string());
        }
        let parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
        let Some((name, modifiers)) = parts.split_last() else {
            return Err(format!("cannot read the key `{text}`"));
        };
        if name.is_empty() {
            return Err(format!("the key `{text}` ends with a modifier"));
        }
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => {
                    if ctrl {
                        return Err(format!("`{text}` names ctrl twice"));
                    }
                    ctrl = true;
                }
                "alt" | "option" | "opt" => {
                    if alt {
                        return Err(format!("`{text}` names alt twice"));
                    }
                    alt = true;
                }
                "shift" => {
                    if shift {
                        return Err(format!("`{text}` names shift twice"));
                    }
                    shift = true;
                }
                other => return Err(format!("unknown modifier `{other}` in `{text}`")),
            }
        }
        let code = code_from_name(name)?;
        // A character key carries its shift in the character itself, so the
        // modifier adds nothing and would split one binding into two.
        let shift = shift && !matches!(code, Code::Char(_));
        Ok(Self { code, ctrl, alt, shift })
    }

    /// The key a terminal reported, in the same canonical form.
    ///
    /// Returns `None` for a key the interface has no name for, such as the
    /// insert key or a media key; such a keystroke is ignored exactly as an
    /// unbound one is.
    pub fn from_event(event: KeyEvent) -> Option<Self> {
        let code = match event.code {
            KeyCode::Char(character) => Code::Char(character.to_ascii_lowercase()),
            KeyCode::Enter => Code::Enter,
            KeyCode::Esc => Code::Esc,
            KeyCode::Tab => Code::Tab,
            KeyCode::BackTab => Code::Tab,
            KeyCode::Backspace => Code::Backspace,
            KeyCode::Delete => Code::Delete,
            KeyCode::Left => Code::Left,
            KeyCode::Right => Code::Right,
            KeyCode::Up => Code::Up,
            KeyCode::Down => Code::Down,
            KeyCode::Home => Code::Home,
            KeyCode::End => Code::End,
            KeyCode::PageUp => Code::PageUp,
            KeyCode::PageDown => Code::PageDown,
            KeyCode::F(number) if (1..=12).contains(&number) => Code::Function(number),
            _ => return None,
        };
        let shift = event.modifiers.contains(KeyModifiers::SHIFT) && !matches!(code, Code::Char(_));
        Some(Self {
            code,
            ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
            alt: event.modifiers.contains(KeyModifiers::ALT),
            shift,
        })
    }

    /// Whether this binding also answers an event that holds extra modifiers.
    ///
    /// A terminal reports alt+left for the same arrow the interface has always
    /// moved on, and no binding names alt+left, so a binding answers any event
    /// with the same key and at least its modifiers. An exact match is checked
    /// first, so `shift+enter` still beats a plain `enter` binding.
    pub fn covers(&self, event: &Key) -> bool {
        self.code == event.code
            && (!self.ctrl || event.ctrl)
            && (!self.alt || event.alt)
            && (!self.shift || event.shift)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            formatter.write_str("ctrl+")?;
        }
        if self.alt {
            formatter.write_str("alt+")?;
        }
        if self.shift {
            formatter.write_str("shift+")?;
        }
        formatter.write_str(&self.code.name())
    }
}

impl Code {
    /// The name this key has in the configuration file.
    pub fn name(self) -> String {
        match self {
            Self::Char(' ') => "space".to_string(),
            Self::Char(character) => character.to_string(),
            Self::Enter => "enter".to_string(),
            Self::Esc => "esc".to_string(),
            Self::Tab => "tab".to_string(),
            Self::Backspace => "backspace".to_string(),
            Self::Delete => "delete".to_string(),
            Self::Left => "left".to_string(),
            Self::Right => "right".to_string(),
            Self::Up => "up".to_string(),
            Self::Down => "down".to_string(),
            Self::Home => "home".to_string(),
            Self::End => "end".to_string(),
            Self::PageUp => "pageup".to_string(),
            Self::PageDown => "pagedown".to_string(),
            Self::Function(number) => format!("f{number}"),
        }
    }
}

/// Resolve the key part of a name, ignoring the modifiers.
fn code_from_name(name: &str) -> Result<Code, String> {
    let lowered = name.to_ascii_lowercase();
    let code = match lowered.as_str() {
        "enter" | "return" => Code::Enter,
        "esc" | "escape" => Code::Esc,
        "tab" => Code::Tab,
        "backspace" | "bs" => Code::Backspace,
        "delete" | "del" => Code::Delete,
        "left" => Code::Left,
        "right" => Code::Right,
        "up" => Code::Up,
        "down" => Code::Down,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" | "pgup" => Code::PageUp,
        "pagedown" | "pgdn" => Code::PageDown,
        "space" => Code::Char(' '),
        _ => {
            if let Some(number) = function_number(&lowered) {
                return Ok(Code::Function(number));
            }
            let mut characters = lowered.chars();
            match (characters.next(), characters.next()) {
                (Some(character), None) => Code::Char(character),
                _ => return Err(format!("unknown key `{name}`")),
            }
        }
    };
    Ok(code)
}

/// The number in `f1` through `f12`, and nothing for any other name.
fn function_number(name: &str) -> Option<u8> {
    let rest = name.strip_prefix('f')?;
    match rest.parse::<u8>() {
        Ok(number) if (1..=12).contains(&number) => Some(number),
        _ => None,
    }
}

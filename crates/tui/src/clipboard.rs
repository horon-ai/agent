//! The terminal's clipboard, reached by the OSC 52 escape.
//!
//! A terminal client cannot call a clipboard API: the clipboard belongs to the
//! terminal emulator, and the one channel into it is an escape sequence. So the
//! client writes `ESC ] 52 ; c ; <base64> BEL` to its own standard output, and
//! the terminal that recognised the sequence puts the decoded text on its
//! clipboard. A terminal that does not recognise it ignores the sequence, which
//! is why copying is only ever an addition: the text is on screen either way and
//! the terminal's own selection still works.
//!
//! Two properties make the sequence safe to write into a live terminal:
//!
//! * the payload is base64 and nothing else, so no byte of the copied text can
//!   appear in the sequence as a control character. A model's answer holding an
//!   `ESC` is copied as the bytes `\`, `x`, `1`, `b` are to a shell, not as an
//!   escape the terminal would act on;
//! * the whole sequence is one write, capped at [`MAX_BYTES`] of text, so a
//!   half-written sequence cannot leave the terminal in a state where the next
//!   keystroke is read as part of it.
//!
//! Control characters in the copied text are dropped rather than encoded: a
//! clipboard carrying an `ESC` pastes that `ESC` into the next program, and the
//! user asked to copy text, not for the terminal's parser to be driven. Newlines
//! and tabs survive, because a multi-line draft is text.
//!
//! Remote terminals and multiplexers are the reason [`support`] exists. `tmux`
//! and `screen` only pass OSC 52 through when configured for it, and a terminal
//! reached over `ssh` may drop the sequence too, so the client asks only where
//! the terminal has declared it can take it and otherwise says so and leaves the
//! text on screen. `HORON_TUI_OSC52=0` turns the emission off entirely, and
//! `HORON_TUI_OSC52=1` sends it regardless of that gate for the one case the
//! environment cannot show: a multiplexer or a session that was configured to
//! forward it.

use std::io::{self, Write};

/// The most text one sequence may carry, in bytes of UTF-8.
///
/// Well under the smallest payload a common terminal accepts (xterm's
/// `maxStringLength` sits in the thousands, kitty's default clipboard limit is
/// far above this) and small enough that the sequence fits one terminal buffer.
pub const MAX_BYTES: usize = 8 * 1024;

/// The environment variable that turns copying through the terminal off.
pub const ENV: &str = "HORON_TUI_OSC52";

/// The start of the sequence: OSC 52, the `c` (clipboard) selection.
const OPEN: &str = "\x1b]52;c;";

/// The end of the sequence, BEL.
const CLOSE: char = '\u{7}';

/// Why the terminal will not be asked to take the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The user turned the emission off.
    Disabled,
    /// The terminal, or the multiplexer in front of it, does not take OSC 52.
    Unsupported(&'static str),
}

impl Refusal {
    /// One sentence naming the reason, for the line the transcript keeps.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Disabled => "copying through the terminal is off (HORON_TUI_OSC52)",
            Self::Unsupported(reason) => reason,
        }
    }
}

/// Whether this terminal can be asked to hold the text.
///
/// The answer comes from the environment rather than from a terminal query,
/// because a query needs a reply on the input stream that would then have to be
/// told apart from the user's keys. A terminal that is not named here is treated
/// as unable: writing the sequence to it is harmless, but promising a copy that
/// never arrives is not, so an unlisted terminal gets the sentence and the
/// selectable text instead. `HORON_TUI_OSC52=1` is the way past that, for the
/// one case this module cannot see: a multiplexer that was configured to
/// forward the sequence, or a terminal reached through `ssh` that passes it on.
pub fn support(env: impl Fn(&str) -> Option<String>) -> Result<(), Refusal> {
    match env(ENV) {
        Some(value) if is_off(&value) => return Err(Refusal::Disabled),
        // The user knows something about their terminal this gate cannot read.
        Some(value) if is_on(&value) => return Ok(()),
        _ => {}
    }
    // A multiplexer owns the screen in front of the terminal and only forwards
    // OSC 52 when it was told to, so the program behind it says nothing about
    // whether the sequence arrives.
    if env("TMUX").is_some_and(|value| !value.is_empty()) {
        return Err(Refusal::Unsupported(
            "tmux forwards no OSC 52 unless its clipboard passthrough is on",
        ));
    }
    if env("STY").is_some_and(|value| !value.is_empty()) {
        return Err(Refusal::Unsupported("screen forwards no OSC 52"));
    }
    let program = variable(&env, "TERM_PROGRAM").to_ascii_lowercase();
    if !program.is_empty() {
        return match program.as_str() {
            "iterm.app" | "wezterm" | "vscode" | "ghostty" | "kitty" | "alacritty" | "rio" => Ok(()),
            _ => Err(Refusal::Unsupported("this terminal has not declared OSC 52 support")),
        };
    }
    // `TERM_PROGRAM` is a macOS convention; other terminals declare themselves
    // in `TERM` alone.
    let term = variable(&env, "TERM").to_ascii_lowercase();
    const DECLARING: &[&str] = &[
        "xterm-kitty",
        "xterm-ghostty",
        "alacritty",
        "wezterm",
        "foot",
        "contour",
        "rio",
    ];
    if DECLARING.iter().any(|name| term.starts_with(name)) {
        return Ok(());
    }
    Err(Refusal::Unsupported("this terminal has not declared OSC 52 support"))
}

/// One variable's value, or an empty string when it is not set.
///
/// A terminal sets only some of the names this module reads, so an unset one is
/// the ordinary case rather than a failure to report.
fn variable(env: &impl Fn(&str) -> Option<String>, name: &str) -> String {
    env(name).unwrap_or_else(String::new)
}

/// The environment the running client reads the terminal's declarations from.
///
/// A variable that is not set is the ordinary case here: most terminals set two
/// or three of the names [`support`] reads, and an absent one means "did not
/// declare", not "could not be read". A value that is not UTF-8 is read through
/// `OsString::to_string_lossy`, because no terminal name this gate matches
/// contains a byte that would need replacing.
pub fn environment() -> impl Fn(&str) -> Option<String> {
    |name| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

/// Whether an environment value asks for the emission to stop.
///
/// Only an explicit no counts: a value the client does not understand keeps the
/// default, so a typo in a shell profile does not silently disable the feature.
fn is_off(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no")
}

/// Whether an environment value asks for the emission regardless of the gate.
///
/// Only an explicit yes counts, for the same reason as [`is_off`]: a typo must
/// not change what the client does with the clipboard.
fn is_on(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes")
}

/// One copy's text after the limits are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The text the sequence carries: control characters removed, cut to
    /// [`MAX_BYTES`].
    pub text: String,
    /// Bytes of the text before anything was removed.
    pub total: usize,
    /// Control characters left out, newlines and tabs excluded.
    pub dropped: usize,
    /// Whether the cut left part of the text behind.
    pub truncated: bool,
}

/// Remove what a clipboard must not carry and cut the text to the cap.
pub fn prepare(text: &str) -> Prepared {
    let total = text.len();
    let mut kept = String::with_capacity(total.min(MAX_BYTES));
    let mut dropped = 0;
    let mut truncated = false;
    for character in text.chars() {
        // A clipboard holding an `ESC` pastes that `ESC` into whatever reads
        // the clipboard next, so every control character goes except the two a
        // draft's own text is made of.
        if character.is_control() && !matches!(character, '\n' | '\t') {
            dropped += 1;
            continue;
        }
        if kept.len() + character.len_utf8() > MAX_BYTES {
            truncated = true;
            break;
        }
        kept.push(character);
    }
    Prepared {
        text: kept,
        total,
        dropped,
        truncated,
    }
}

/// The sequence that puts `prepared` on the terminal's clipboard.
///
/// `None` when there is nothing to carry: an empty payload still asks the
/// terminal to clear its clipboard, and a copy the user asked for must not
/// silently empty what was there.
pub fn sequence(prepared: &Prepared) -> Option<String> {
    if prepared.text.is_empty() {
        return None;
    }
    let mut encoded = String::with_capacity(OPEN.len() + prepared.text.len().div_ceil(3) * 4 + 1);
    encoded.push_str(OPEN);
    base64(prepared.text.as_bytes(), &mut encoded);
    encoded.push(CLOSE);
    Some(encoded)
}

/// Write one sequence in a single call and flush it.
///
/// One write is the point: the terminal reads the escape as a unit, so a
/// sequence split by an interrupted write is never half-visible.
pub fn write(out: &mut impl Write, sequence: &str) -> io::Result<()> {
    out.write_all(sequence.as_bytes())?;
    out.flush()
}

/// The standard base64 alphabet, with `=` padding.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Append `bytes` to `out` as standard base64.
///
/// Written out rather than pulled in as a dependency: the client encodes at most
/// one buffer per copy, and a crate for this much arithmetic would be a crate to
/// keep updated for no other use.
fn base64(bytes: &[u8], out: &mut String) {
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map(u32::from).unwrap_or(0);
        let third = chunk.get(2).copied().map(u32::from).unwrap_or(0);
        let triple = (first << 16) | (second << 8) | third;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode standard base64, for the tests that read the sequence back.
    fn decode(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buffer = 0u32;
        let mut bits = 0;
        for byte in text.bytes() {
            if byte == b'=' {
                break;
            }
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                other => panic!("not base64: {other}"),
            };
            buffer = (buffer << 6) | u32::from(value);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buffer >> bits) as u8);
            }
        }
        out
    }

    /// The payload of a sequence, which is everything between the header and BEL.
    fn payload(sequence: &str) -> &str {
        sequence
            .strip_prefix(OPEN)
            .and_then(|rest| rest.strip_suffix(CLOSE))
            .expect("a sequence holds the header and one BEL")
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn base64_matches_the_known_vectors() {
        for (text, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            let mut out = String::new();
            base64(text.as_bytes(), &mut out);
            assert_eq!(out, encoded, "base64 of {text:?}");
        }
    }

    #[test]
    fn a_sequence_is_one_control_sequence_around_a_base64_payload() {
        let sequence = sequence(&prepare("hello\nworld")).expect("text carries a sequence");
        // One escape, one BEL, and nothing between them but the header and the
        // alphabet: a control character in the payload would let a copied text
        // drive the terminal that receives it.
        assert_eq!(sequence.matches('\u{1b}').count(), 1, "one escape: {sequence:?}");
        assert_eq!(sequence.matches(CLOSE).count(), 1, "one BEL: {sequence:?}");
        let payload = payload(&sequence);
        assert!(
            payload
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')),
            "payload is not base64: {payload}"
        );
        assert_eq!(decode(payload), b"hello\nworld");
    }

    #[test]
    fn control_characters_are_dropped_before_the_carry() {
        let prepared = prepare("a\u{1b}]0;title\u{7}b\tc\nd");
        assert_eq!(prepared.text, "a]0;titleb\tc\nd");
        assert_eq!(prepared.dropped, 3, "escape, BEL and nothing else");
        let sequence = sequence(&prepared).expect("text carries a sequence");
        assert_eq!(sequence.matches('\u{1b}').count(), 1, "model text added an escape");
        assert_eq!(decode(payload(&sequence)), prepared.text.as_bytes());
    }

    #[test]
    fn text_over_the_cap_is_cut_at_a_character_boundary() {
        // Three bytes per character, so a byte-at-a-time cut would split one.
        let text = "한".repeat(MAX_BYTES);
        let prepared = prepare(&text);
        assert!(prepared.truncated);
        assert_eq!(prepared.total, text.len());
        assert_eq!(prepared.text.len(), MAX_BYTES - (MAX_BYTES % 3));
        assert!(prepared.text.chars().all(|character| character == '한'));
        let sequence = sequence(&prepared).expect("a cut text still carries");
        assert_eq!(decode(payload(&sequence)), prepared.text.as_bytes());
    }

    #[test]
    fn an_empty_copy_asks_for_no_sequence() {
        assert_eq!(prepare("").text, "");
        assert_eq!(sequence(&prepare("")), None);
        // Whitespace is text, and a copy of it is the user's choice.
        assert!(sequence(&prepare(" ")).is_some());
    }

    #[test]
    fn a_declared_terminal_is_asked_and_the_rest_is_left_alone() {
        assert_eq!(support(env(&[("TERM_PROGRAM", "iTerm.app")])), Ok(()));
        assert_eq!(support(env(&[("TERM", "xterm-kitty")])), Ok(()));
        assert!(support(env(&[("TERM_PROGRAM", "Apple_Terminal")])).is_err());
        assert!(support(env(&[])).is_err());
    }

    #[test]
    fn a_multiplexer_is_not_promised_a_copy() {
        // iTerm would take the sequence, but tmux in front of it will not pass
        // it on without being told to.
        assert!(matches!(
            support(env(&[("TERM_PROGRAM", "iTerm.app"), ("TMUX", "/tmp/tmux-501/default,1,0")])),
            Err(Refusal::Unsupported(reason)) if reason.contains("tmux")
        ));
        assert!(support(env(&[("TERM", "xterm-kitty"), ("STY", "1234.pts-0")])).is_err());
    }

    #[test]
    fn the_environment_variable_turns_the_emission_off() {
        assert_eq!(
            support(env(&[("TERM_PROGRAM", "iTerm.app"), (ENV, "0")])),
            Err(Refusal::Disabled)
        );
        // The same variable is the way past the gate for a terminal it cannot
        // see: a multiplexer told to forward, or an `ssh` session that passes
        // the sequence on.
        assert_eq!(
            support(env(&[("TERM_PROGRAM", "Apple_Terminal"), (ENV, "yes")])),
            Ok(())
        );
        assert_eq!(
            support(env(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TMUX", "/tmp/tmux-501,1,0"),
                (ENV, "1")
            ])),
            Ok(())
        );
        // A value the client does not understand is not an override either.
        assert!(support(env(&[("TERM_PROGRAM", "Apple_Terminal"), (ENV, "maybe")])).is_err());
    }
}

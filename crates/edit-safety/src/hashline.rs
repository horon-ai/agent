//! The hashline codec: a short content anchor per line and the rendered line
//! format that carries it.
//!
//! A tag is the first two bytes of the line's SHA-256, written as four
//! lowercase hex characters. Sixteen bits is not a cryptographic commitment; it
//! is a cheap way for the model to name a line it read, and the caller must
//! still reject an anchor whose tag no longer matches.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Two-byte line anchor rendered as four lowercase hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tag([u8; 2]);

impl std::fmt::Display for Tag {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:02x}{:02x}", self.0[0], self.0[1])
    }
}

/// Tag the exact bytes of one line.
///
/// The caller decides what counts as the line: this function hashes the slice
/// it is given and never strips a terminator.
pub fn tag_for(line_bytes: &[u8]) -> Tag {
    let digest = Sha256::digest(line_bytes);
    Tag([digest[0], digest[1]])
}

/// Render one anchored line as `{n:>5}#{tag}:{text}`.
pub fn format_line(n: usize, tag: Tag, text: &str) -> String {
    format!("{n:>5}#{tag}:{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_for_is_deterministic_and_four_lowercase_hex_characters() {
        assert_eq!(tag_for(b"").to_string(), "e3b0");
        assert_eq!(tag_for(b"hello").to_string(), "2cf2");
        assert_eq!(tag_for(b"hello"), tag_for(b"hello"));

        let rendered = tag_for(b"let value = 1;").to_string();
        assert_eq!(rendered.len(), 4);
        assert!(
            rendered
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn format_line_uses_a_five_wide_number_and_a_colon() {
        assert_eq!(format_line(7, tag_for(b"hello"), "hi"), "    7#2cf2:hi");
        assert_eq!(format_line(12345, tag_for(b"hello"), ""), "12345#2cf2:");
        assert_eq!(format_line(1, tag_for(b""), "first"), "    1#e3b0:first");
    }

    #[test]
    fn tag_covers_the_bytes_it_is_given_and_adds_no_terminator() {
        assert_ne!(tag_for(b"no newline"), tag_for(b"no newline\n"));

        let tag = tag_for(b"no newline");
        assert_eq!(format_line(1, tag, "no newline"), format!("    1#{tag}:no newline"));
    }
}

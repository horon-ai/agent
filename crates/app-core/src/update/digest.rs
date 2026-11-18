//! File identity for the updater's rollback check.
//!
//! A rollback compares bytes on disk with the copy it took before activation, so both sides must
//! be hashed the same way. The digest is streamed: a large binary is never held in memory.
//!
//! Reinstalling a version that is already installed hashes the freshly staged agent binary and the
//! installed one back to back to prove they are identical, so the per-file cost is paid twice on
//! ~100 MB of executable. `ring` is used because it selects the platform's SHA-256 instructions at
//! run time (ARMv8 SHA-2 extensions, x86 SHA-NI) and falls back to portable code elsewhere, while
//! `sha2` without its `asm` feature is a scalar Rust implementation on AArch64. The files hashed,
//! the moment they are hashed, and the comparison performed are unchanged.

use ring::digest::{Context, SHA256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Lowercase hex sha256 of one file, streamed so a large binary is not held in memory.
pub(crate) fn file_sha256(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut context = Context::new(&SHA256);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    Ok(hex_lower(context.finish().as_ref()))
}

/// Lowercase hex, one character per nibble, without allocating per byte.
fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn hashes_known_content_as_lowercase_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload");
        std::fs::write(&path, b"abc").unwrap();

        assert_eq!(
            file_sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hashes_content_larger_than_the_read_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload");
        std::fs::write(&path, vec![b'a'; 200_000]).unwrap();

        assert_eq!(
            file_sha256(&path).unwrap(),
            "2287d207f24a941ff3b56c04c8a25ad56b63e3023207b3bb5b4ac0c9869d74be"
        );
    }

    #[test]
    fn missing_path_is_a_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent");

        let error = file_sha256(&path).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::NotFound);
    }
}

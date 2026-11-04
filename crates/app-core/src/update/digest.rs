//! File identity for the updater's rollback check.
//!
//! A rollback compares bytes on disk with the copy it took before activation, so both sides must
//! be hashed the same way. The digest is streamed: a large binary is never held in memory.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Lowercase hex sha256 of one file, streamed so a large binary is not held in memory.
pub(crate) fn file_sha256(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
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
    fn missing_path_is_a_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent");

        let error = file_sha256(&path).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::NotFound);
    }
}

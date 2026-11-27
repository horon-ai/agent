//! The session id a profile's `session_header` carries, in UUID form.
//!
//! Session-aware gateways name a conversation by a UUID, as Claude Code and omp
//! do (`01a21584-b837-73c5-8501-6b2ed6edc0a4`). A horon session id
//! (`session_<name>_<unix-ms>_<hex>`) is mapped onto a UUIDv7: the creation
//! time it carries fills the 48-bit timestamp, and SHA-256 of the whole id fills
//! the rest. The mapping is a pure function of the id, so a session resumed
//! after a daemon restart keeps naming the same upstream conversation, and two
//! sessions never share one.

use sha2::{Digest, Sha256};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// The UUID a session header carries for `session_id`.
///
/// An id that is already a UUID is sent as it is.
pub(super) fn session_uuid(session_id: &str) -> String {
    if is_uuid(session_id) {
        return session_id.to_ascii_lowercase();
    }
    let digest = Sha256::digest(session_id.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    if let Some(created_ms) = created_ms(session_id) {
        bytes[..6].copy_from_slice(&created_ms.to_be_bytes()[2..]);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let mut uuid = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            uuid.push('-');
        }
        uuid.push(HEX_DIGITS[usize::from(byte >> 4)] as char);
        uuid.push(HEX_DIGITS[usize::from(byte & 0x0f)] as char);
    }
    uuid
}

/// Creation time a `session_<name>_<unix-ms>_<hex>` id carries, within 48 bits.
fn created_ms(session_id: &str) -> Option<u64> {
    let rest = session_id.strip_prefix("session_")?;
    let (head, _random) = rest.rsplit_once('_')?;
    let (_name, millis) = head.rsplit_once('_')?;
    // An id whose time field is not a 48-bit number keeps the digest there.
    match millis.parse::<u64>() {
        Ok(millis) if millis < 1 << 48 => Some(millis),
        Ok(_) | Err(_) => None,
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

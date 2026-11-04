//! Strict, bounded RFC 4648 transport encoding without a new workspace dependency.
use crate::{StoreError, StoreErrorCode};

pub(crate) fn encode(bytes: &[u8], url: bool) -> String {
    let alphabet = if url {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(char::from(alphabet[usize::from(a >> 2)]));
        out.push(char::from(alphabet[usize::from((a & 3) << 4 | b >> 4)]));
        if chunk.len() > 1 {
            out.push(char::from(alphabet[usize::from((b & 15) << 2 | c >> 6)]));
        } else if !url {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(alphabet[usize::from(c & 63)]));
        } else if !url {
            out.push('=');
        }
    }
    out
}

pub(crate) fn decode(text: &str, url: bool, max: usize) -> Result<Vec<u8>, StoreError> {
    let invalid = || StoreError::invalid_request(StoreErrorCode::InvalidRecord, "invalid base64");
    if text.len() > max.saturating_add(2) / 3 * 4 {
        return Err(invalid());
    }
    let raw = if url { text } else { text.trim_end_matches('=') };
    let mut out = Vec::new();
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in raw.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' if url => 62,
            b'_' if url => 63,
            b'+' if !url => 62,
            b'/' if !url => 63,
            _ => return Err(invalid()),
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((accumulator >> bits) & 255).map_err(|_| invalid())?);
            accumulator &= (1 << bits) - 1;
        }
    }
    if out.len() > max || encode(&out, url) != text {
        return Err(invalid());
    }
    Ok(out)
}

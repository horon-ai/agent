//! LSP base-protocol framing: `Content-Length` headers, then a JSON body.
//!
//! A malformed or oversized frame ends the connection instead of being
//! tolerated: a server that cannot frame its output has nothing useful to say,
//! and reading an unbounded header would let it exhaust memory first.

use std::io::{Error, ErrorKind};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// Largest header block this client accepts.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Largest message body this client accepts.
pub const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

/// Wrap one JSON body in a base-protocol frame.
pub fn encode(body: &[u8]) -> Vec<u8> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(body);
    frame
}

/// Read one framed message body.
///
/// Returns `Ok(None)` when the peer closed the stream between messages, which
/// is how a language server that exited normally reads.
pub async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut header_bytes = 0usize;
    loop {
        let Some(line) = read_line_limited(reader, MAX_HEADER_BYTES).await? else {
            return Ok(None);
        };
        if line.is_empty() {
            break;
        }
        header_bytes += line.len() + 2;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(malformed("header block exceeds 16 KiB"));
        }
        let header = String::from_utf8_lossy(&line);
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| malformed("invalid Content-Length"))?;
            content_length = Some(parsed);
        }
    }

    let Some(length) = content_length else {
        return Err(malformed("message has no Content-Length header"));
    };
    if length > MAX_BODY_BYTES {
        return Err(malformed("message body exceeds 256 MiB"));
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

/// Read one `\n`-terminated line without letting a header line grow past `max`.
async fn read_line_limited<R: AsyncBufRead + Unpin>(reader: &mut R, max: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() > max {
                return Err(malformed("header block exceeds 16 KiB"));
            }
            return Ok(Some(line));
        }
        let consumed = available.len();
        line.extend_from_slice(available);
        reader.consume(consumed);
        if line.len() > max {
            return Err(malformed("header block exceeds 16 KiB"));
        }
    }
}

fn malformed(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod tests;

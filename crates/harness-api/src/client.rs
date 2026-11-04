//! NDJSON framing helpers and a minimal blocking client.

use crate::{API_VERSION_MAJOR, ApiEvent, ApiRequest, ClientFrame, ServerFrame};
use std::io::{self, BufRead, Read, Write};

/// Largest frame this reader accepts, in bytes.
///
/// The bridge refuses an inbound frame above 64 MiB (`MAX_FRAME_BYTES` in
/// `harness-api-server`), a ceiling picked because a 32 MiB aggregate PDF
/// budget expands to ~43 MiB of base64 plus metadata. Reading without a bound
/// is the same defect on this side: `read_line` grows its buffer until it finds
/// a newline, so a server, proxy, or stuck peer that never sends one makes the
/// client allocate forever. The value matches the bridge's, so a frame the
/// bridge calls legitimate is never refused here, and it is not raised above
/// it, because no legitimate frame is larger.
pub const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// Errors from reading or writing a frame.
#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Json(serde_json::Error),
    /// The stream closed cleanly.
    Eof,
    /// One frame grew past [`MAX_FRAME_BYTES`]. The oversized bytes were
    /// consumed up to the cap, so the stream is left inside that frame and the
    /// connection cannot be reused.
    FrameTooLarge {
        limit: u64,
    },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "harness API I/O error: {e}"),
            Self::Json(e) => write!(f, "harness API JSON error: {e}"),
            Self::Eof => write!(f, "harness API stream closed"),
            Self::FrameTooLarge { limit } => write!(f, "harness API frame exceeds {limit} byte limit"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// Write one frame as a single NDJSON line and flush.
pub fn write_frame<W: Write, T: serde::Serialize>(writer: &mut W, frame: &T) -> Result<(), FrameError> {
    let mut line = serde_json::to_string(frame)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    Ok(())
}

/// Read one frame from a buffered reader. Skips blank lines.
///
/// A frame is bounded by [`MAX_FRAME_BYTES`]: the reader takes at most that many
/// bytes per frame, and a full buffer with no newline is refused as
/// [`FrameError::FrameTooLarge`] rather than buffered further. The `String` is
/// local to this call, so a large frame's capacity is released when the frame
/// is returned instead of being pinned for the life of the connection.
pub fn read_frame<R: BufRead, T: serde::de::DeserializeOwned>(reader: &mut R) -> Result<T, FrameError> {
    let mut line = String::new();
    loop {
        line.clear();
        // `take` bounds how far one frame can grow. It is rebuilt per frame
        // because a `Take` left in place would stop reading after the first.
        let read = (&mut *reader).take(MAX_FRAME_BYTES).read_line(&mut line)?;
        if read == 0 {
            return Err(FrameError::Eof);
        }
        // A full buffer with no terminator means the frame exceeded the cap (or
        // is exactly at it and unterminated); either way it cannot be trusted.
        if read as u64 >= MAX_FRAME_BYTES && !line.ends_with('\n') {
            return Err(FrameError::FrameTooLarge { limit: MAX_FRAME_BYTES });
        }
        if line.trim().is_empty() {
            continue;
        }
        return Ok(serde_json::from_str(line.trim())?);
    }
}

/// Minimal blocking harness client over any read/write pair.
///
/// Handles id assignment and the version handshake. Transport-agnostic:
/// pass a Unix socket, TCP stream, or an in-memory pipe for tests.
pub struct HarnessClient<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    next_id: u64,
}

impl<R: BufRead, W: Write> HarnessClient<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
        }
    }

    /// Perform the version handshake. Must be called first.
    pub fn hello(&mut self, client_name: &str) -> Result<ServerFrame, FrameError> {
        self.send(ApiRequest::Hello {
            min_version: API_VERSION_MAJOR,
            max_version: API_VERSION_MAJOR,
            client: client_name.to_string(),
            supports_user_response: false,
            supports_ask_requests: false,
        })?;
        self.recv()
    }

    /// Send a request, returning the assigned frame id.
    pub fn send(&mut self, request: ApiRequest) -> Result<u64, FrameError> {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(&mut self.writer, &ClientFrame::new(id, request))?;
        Ok(id)
    }

    /// Receive the next server frame, skipping unknown event kinds.
    pub fn recv(&mut self) -> Result<ServerFrame, FrameError> {
        loop {
            let frame: ServerFrame = read_frame(&mut self.reader)?;
            if matches!(frame.event, ApiEvent::Unknown) {
                continue;
            }
            return Ok(frame);
        }
    }
}

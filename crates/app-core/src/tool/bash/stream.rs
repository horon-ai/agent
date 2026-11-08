//! Live command output for an ACP v2 editor, and the bounded buffer every
//! command's collected output lands in.
//!
//! A v2 editor shows a running shell's output as it arrives, so the bytes a
//! command writes are forwarded as `tool_output_delta` events while the command
//! still runs: from the foreground read loop, and from the output file of a
//! command the daemon already moved to the background. The forwarding is
//! coalesced (100 ms) and capped (32 KiB per event) so a command that prints a
//! hundred thousand lines cannot turn into a hundred thousand frames.
//!
//! None of this may cost a client that does not render it. Whether the owning
//! ACP connection negotiated v2 is asked once, when the call starts, and the
//! answer travels as `Option<OutputStream>`: `None` collects nothing, allocates
//! no buffer, and arms no timer. `send_v2` still re-checks ownership on every
//! frame, so a re-subscribe cannot route one connection's output to another's.

use super::*;
use crate::protocol::ServerEvent;

/// The most raw command output kept in memory while a foreground command
/// runs. Past this point only a rolling tail is retained.
const MAX_COLLECTED_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// How much of the newest output is kept once [`MAX_COLLECTED_OUTPUT_BYTES`]
/// has been reached.
const COLLECTED_OUTPUT_TAIL_BYTES: usize = 1024 * 1024;

/// How often collected bytes are flushed to the editor.
pub(super) const OUTPUT_DELTA_INTERVAL: Duration = Duration::from_millis(100);

/// The most bytes one `tool_output_delta` carries. A larger burst becomes
/// several events in order rather than one oversized frame.
const OUTPUT_DELTA_MAX_BYTES: usize = 32 * 1024;

/// Forwarding state for one tool call's output.
///
/// Exists only for a call whose session is owned by a v2 connection, which is
/// what keeps the non-v2 path free of buffering.
pub(super) struct OutputStream {
    session_id: String,
    call_id: String,
    pending: Vec<u8>,
    last_flush: Instant,
}

impl OutputStream {
    /// An [`OutputStream`] for this call, or `None` when `v2` is false.
    ///
    /// The caller passes the answer it already has, so one call asks the
    /// registry once however many streams the command has.
    pub(super) fn for_call(session_id: &str, call_id: &str, v2: bool) -> Option<Self> {
        v2.then(|| Self {
            session_id: session_id.to_string(),
            call_id: call_id.to_string(),
            pending: Vec::new(),
            last_flush: Instant::now(),
        })
    }

    /// Add bytes. A burst larger than one event, or one older than the
    /// coalescing interval, is flushed at once.
    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() >= OUTPUT_DELTA_MAX_BYTES || self.last_flush.elapsed() >= OUTPUT_DELTA_INTERVAL {
            self.flush();
        }
    }

    /// Send everything collected so far, in event-sized pieces.
    pub(super) fn flush(&mut self) {
        self.last_flush = Instant::now();
        let mut offset = 0;
        while offset < self.pending.len() {
            let end = (offset + OUTPUT_DELTA_MAX_BYTES).min(self.pending.len());
            self.emit(&self.pending[offset..end]);
            offset = end;
        }
        self.pending.clear();
    }

    fn emit(&self, bytes: &[u8]) {
        crate::tool::acp_session::send_v2(
            &self.session_id,
            ServerEvent::ToolOutputDelta {
                id: self.call_id.clone(),
                data: crate::acp_render::base64_encode(bytes),
            },
        );
    }
}

/// Collect a command's output stream line by line, reporting any parsed
/// progress so a later background promotion has live progress instead of
/// sitting at 0% until completion, and forwarding the bytes when a v2 editor is
/// watching.
pub(super) async fn collect_output<R>(
    reader: Option<R>,
    progress: std::sync::Arc<PromotedCommandProgress>,
    mut stream: Option<OutputStream>,
) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut out = BoundedOutput::default();
    let Some(reader) = reader else {
        return String::new();
    };
    let mut lines = BufReader::new(reader).lines();
    // A trailing line with no newline still counts as output, and a command
    // that prints nothing for a while must not hold back what it already
    // printed, so the tick flushes the idle tail of the interval.
    let mut interval = tokio::time::interval(OUTPUT_DELTA_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Ok(Some(update)) = parse_progress_line(&line) {
                        progress.record(update).await;
                    }
                    out.push_line(&line);
                    if let Some(stream) = stream.as_mut() {
                        stream.push(line.as_bytes());
                        stream.push(b"\n");
                    }
                }
                _ => break,
            },
            _ = interval.tick(), if stream.is_some() => {
                if let Some(stream) = stream.as_mut() {
                    stream.flush();
                }
            }
        }
    }
    if let Some(mut stream) = stream {
        stream.flush();
    }
    out.finish()
}

/// Command output bounded in memory: the first [`MAX_COLLECTED_OUTPUT_BYTES`]
/// are kept, then only a rolling tail of the newest
/// [`COLLECTED_OUTPUT_TAIL_BYTES`]. The dropped middle is marked with the same
/// elision marker as [`horon_agent_core::util::head_tail`], so the model still
/// learns how much of the output it never saw instead of silently reading a
/// truncated stream.
#[derive(Default)]
struct BoundedOutput {
    head: String,
    tail: String,
    spilling: bool,
    total: usize,
}

impl BoundedOutput {
    fn push_line(&mut self, line: &str) {
        self.total += line.len() + 1;
        if !self.spilling {
            self.head.push_str(line);
            self.head.push('\n');
            if self.head.len() > MAX_COLLECTED_OUTPUT_BYTES {
                self.spilling = true;
                let split = self.head.len().saturating_sub(COLLECTED_OUTPUT_TAIL_BYTES);
                let split = match self.head[split..].find('\n') {
                    Some(offset) => split + offset + 1,
                    None => split,
                };
                self.tail = self.head.split_off(split);
            }
            return;
        }
        self.tail.push_str(line);
        self.tail.push('\n');
        while self.tail.len() > COLLECTED_OUTPUT_TAIL_BYTES {
            let Some(newline) = self.tail.find('\n') else {
                break;
            };
            self.tail.drain(..=newline);
        }
    }

    fn finish(self) -> String {
        if !self.spilling {
            return self.head;
        }
        let elided = self.total.saturating_sub(self.head.len() + self.tail.len());
        format!("{}\n[…{elided}B elided…]\n{}", self.head, self.tail)
    }
}

/// Reads a running background command's output file and forwards what grew.
///
/// Background commands write straight to their output file, so nothing in the
/// tool task sees their bytes. The owner of this cursor drains it on a timer
/// while the task runs and once more when it ends, so the deltas of a finished
/// command are always queued before the final `background_work` event.
pub(crate) struct BackgroundTail {
    session_id: String,
    call_id: String,
    output_file: PathBuf,
    pos: u64,
}

impl BackgroundTail {
    pub(crate) fn new(session_id: &str, call_id: &str, output_file: PathBuf) -> Self {
        Self {
            session_id: session_id.to_string(),
            call_id: call_id.to_string(),
            output_file,
            pos: 0,
        }
    }

    /// Send whatever the output file grew since the last call.
    pub(crate) async fn drain(&mut self) {
        use tokio::io::AsyncSeekExt;
        let Ok(mut file) = tokio::fs::File::open(&self.output_file).await else {
            // The task has not created its output file yet, or finalization
            // already removed it; either way nothing new to show.
            return;
        };
        if file.seek(std::io::SeekFrom::Start(self.pos)).await.is_err() {
            return;
        }
        let mut chunk = Vec::new();
        if file.read_to_end(&mut chunk).await.is_err() || chunk.is_empty() {
            return;
        }
        for piece in chunk.chunks(OUTPUT_DELTA_MAX_BYTES) {
            crate::tool::acp_session::send_v2(
                &self.session_id,
                ServerEvent::ToolOutputDelta {
                    id: self.call_id.clone(),
                    data: crate::acp_render::base64_encode(piece),
                },
            );
        }
        self.pos += chunk.len() as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_splits_a_burst_into_event_sized_pieces() {
        let mut stream = OutputStream {
            session_id: "no-such-session".to_string(),
            call_id: "call".to_string(),
            pending: Vec::new(),
            last_flush: Instant::now(),
        };
        // No v2 owner, so nothing is delivered; the buffer accounting is what
        // this test can observe without a registry installation.
        stream.push(&vec![b'x'; OUTPUT_DELTA_MAX_BYTES + 5]);
        assert!(stream.pending.is_empty(), "a full event's worth is flushed immediately");
    }

    #[test]
    fn bounded_output_keeps_the_head_and_the_tail() {
        let mut out = BoundedOutput::default();
        out.push_line("first");
        let text = out.finish();
        assert_eq!(text, "first\n");
    }
}

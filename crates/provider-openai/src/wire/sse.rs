//! One SSE stream shared by every wire.
//!
//! The transport work is identical for all four protocols: decode chunks into
//! text without splitting a character, frame `data:` payloads, hand each payload
//! to the wire's parser, and close the stream the same way at end of body. Only
//! [`WireParser`] differs, so the framing lives here once and each wire supplies
//! a parser.

use crate::chat::transport::redact_secret;
use crate::stream::SseFrameBuffer;
use bytes::Bytes;
use futures::Stream;
use horon_agent_core::util::Utf8StreamDecoder;
use horon_agent_message_types::StreamEvent;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Translates one wire's `data:` payloads into shared [`StreamEvent`]s.
///
/// A parser sees each payload once and may push any number of events. It also
/// sees the end of the body, which is where a protocol that has not yet seen its
/// terminal marker reports a truncated response instead of an empty one.
pub trait WireParser: Send + Unpin + 'static {
    /// One SSE `data:` payload; push zero or more events.
    fn parse(&mut self, data: &str, out: &mut VecDeque<StreamEvent>);

    /// EOF: push closing events, or a truncation [`StreamEvent::Error`] when the
    /// protocol's end marker never arrived.
    fn finish(&mut self, out: &mut VecDeque<StreamEvent>);
}

/// An SSE body translated by `P` into shared stream events.
pub struct WireSseStream<P: WireParser> {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    framer: SseFrameBuffer,
    /// Carries incomplete multi-byte UTF-8 sequences across chunk boundaries.
    utf8: Utf8StreamDecoder,
    pending: VecDeque<StreamEvent>,
    parser: P,
    drained: bool,
    /// Names the wire in a transport error, e.g. `OpenAI Responses`.
    label: &'static str,
    /// Credential to keep out of any diagnostic produced while parsing.
    api_key: Option<Arc<str>>,
}

impl<P: WireParser> WireSseStream<P> {
    /// `api_key` is redacted from every error this stream reports.
    pub fn new(
        inner: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
        parser: P,
        label: &'static str,
        api_key: Option<Arc<str>>,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            framer: SseFrameBuffer::new(),
            utf8: Utf8StreamDecoder::new(),
            pending: VecDeque::new(),
            parser,
            drained: false,
            label,
            api_key,
        }
    }

    /// Redact the credential from an error message this stream is about to emit.
    ///
    /// Every parser builds its own messages, so the one place that knows the key
    /// scrubs them all on the way out rather than trusting each parser to.
    fn scrub(&self, event: StreamEvent) -> StreamEvent {
        match event {
            StreamEvent::Error {
                message,
                retry_after_secs,
            } => StreamEvent::Error {
                message: redact_secret(message, self.api_key.as_deref()),
                retry_after_secs,
            },
            other => other,
        }
    }

    /// The next queued event, with any error already scrubbed.
    fn pop_pending(&mut self) -> Option<StreamEvent> {
        self.pending.pop_front().map(|event| self.scrub(event))
    }

    /// Parse every complete frame already buffered, queuing its events.
    fn drain_frames(&mut self) {
        while let Some(data) = self.framer.take_next() {
            self.parser.parse(&data, &mut self.pending);
        }
    }
}

impl<P: WireParser> Stream for WireSseStream<P> {
    type Item = anyhow::Result<StreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.pop_pending() {
                return Poll::Ready(Some(Ok(event)));
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let text = this.utf8.decode(&bytes);
                    this.framer.push_str(&text);
                    this.drain_frames();
                }
                Poll::Ready(Some(Err(error))) => {
                    let message =
                        redact_secret(format!("{} stream error: {error}", this.label), this.api_key.as_deref());
                    return Poll::Ready(Some(Err(anyhow::anyhow!(message))));
                }
                Poll::Ready(None) => {
                    if this.drained {
                        return Poll::Ready(None);
                    }
                    this.drained = true;
                    let tail = this.utf8.flush();
                    if !tail.is_empty() {
                        this.framer.push_str(&tail);
                    }
                    // A trailing frame without its blank line is parsed here. Its
                    // events must be queued behind everything already pending and
                    // before the closing events: returning early would skip the
                    // `finish` step below on the next poll, which then only sees
                    // `drained` and ends the stream silently.
                    if let Some(data) = this.framer.flush() {
                        this.parser.parse(&data, &mut this.pending);
                    }
                    this.parser.finish(&mut this.pending);
                    if let Some(event) = this.pop_pending() {
                        return Poll::Ready(Some(Ok(event)));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WireParser, WireSseStream};
    use crate::chat::transport::REDACTED;
    use crate::test_support::{DEADLINE, TEST_KEY};
    use bytes::Bytes;
    use futures::StreamExt;
    use horon_agent_message_types::StreamEvent;
    use std::collections::VecDeque;
    use std::sync::Arc;

    /// Records every payload and, unless it saw the end marker, a `TextDelta`.
    struct Recorder {
        seen: Vec<String>,
        closed: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                seen: Vec::new(),
                closed: false,
            }
        }
    }

    impl WireParser for Recorder {
        fn parse(&mut self, data: &str, out: &mut VecDeque<StreamEvent>) {
            self.seen.push(data.to_string());
            if data == "[DONE]" {
                self.closed = true;
                out.push_back(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                });
                return;
            }
            out.push_back(StreamEvent::TextDelta(data.to_string()));
        }

        fn finish(&mut self, out: &mut VecDeque<StreamEvent>) {
            if !self.closed {
                out.push_back(StreamEvent::Error {
                    message: format!("truncated at {TEST_KEY}"),
                    retry_after_secs: None,
                });
            }
        }
    }

    type Chunks = Vec<Result<Bytes, reqwest::Error>>;

    fn stream_from(chunks: Chunks, api_key: Option<&str>) -> WireSseStream<Recorder> {
        WireSseStream::new(
            futures::stream::iter(chunks),
            Recorder::new(),
            "Test wire",
            api_key.map(Arc::from),
        )
    }

    fn ok(text: &str) -> Chunks {
        vec![Ok(Bytes::from(text.to_string()))]
    }

    async fn drain(stream: &mut WireSseStream<Recorder>) -> Vec<anyhow::Result<StreamEvent>> {
        let mut events = Vec::new();
        loop {
            let next = tokio::time::timeout(DEADLINE, stream.next())
                .await
                .expect("an event within the deadline");
            match next {
                Some(event) => events.push(event),
                None => return events,
            }
        }
    }

    fn text_of(event: &anyhow::Result<StreamEvent>) -> String {
        match event {
            Ok(StreamEvent::TextDelta(text)) => text.clone(),
            other => panic!("expected a text delta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_frames_arrive_in_order() {
        let mut stream = stream_from(ok("data: one\n\ndata: two\n\ndata: [DONE]\n\n"), None);
        let events = drain(&mut stream).await;
        assert_eq!(events.len(), 3);
        assert_eq!(text_of(&events[0]), "one");
        assert_eq!(text_of(&events[1]), "two");
        assert!(matches!(events[2], Ok(StreamEvent::MessageEnd { .. })));
    }

    #[tokio::test]
    async fn a_frame_split_across_chunks_is_framed_once() {
        let chunks: Chunks = vec![
            Ok(Bytes::from_static(b"data: he")),
            Ok(Bytes::from_static(b"llo\n")),
            Ok(Bytes::from_static(b"\ndata: [DONE]\n\n")),
        ];
        let mut stream = stream_from(chunks, None);
        let events = drain(&mut stream).await;
        assert_eq!(text_of(&events[0]), "hello");
        assert!(matches!(events[1], Ok(StreamEvent::MessageEnd { .. })));
    }

    #[tokio::test]
    async fn a_multibyte_character_split_across_chunks_survives() {
        // The bytes of "안녕" are cut between the second and third byte of the
        // first character, which only a decoder that carries the remainder can
        // reassemble.
        let body = "data: 안녕\n\ndata: [DONE]\n\n".as_bytes().to_vec();
        let cut = body.iter().position(|byte| *byte >= 0x80).expect("non-ASCII byte") + 1;
        let chunks: Chunks = vec![
            Ok(Bytes::from(body[..cut].to_vec())),
            Ok(Bytes::from(body[cut..].to_vec())),
        ];
        let mut stream = stream_from(chunks, None);
        let events = drain(&mut stream).await;
        assert_eq!(text_of(&events[0]), "안녕");
    }

    #[tokio::test]
    async fn a_crlf_frame_is_framed_like_an_lf_one() {
        let mut stream = stream_from(ok("data: one\r\n\r\ndata: [DONE]\r\n\r\n"), None);
        let events = drain(&mut stream).await;
        assert_eq!(text_of(&events[0]), "one");
        assert!(matches!(events[1], Ok(StreamEvent::MessageEnd { .. })));
    }

    #[tokio::test]
    async fn a_trailing_frame_without_its_blank_line_is_still_parsed_before_the_close() {
        // EOF with a half-written frame: the payload must be emitted, then the
        // parser's closing events, in that order. Returning the payload early
        // from the EOF branch would drop the close entirely.
        let mut stream = stream_from(ok("data: one\n\ndata: [DONE]"), None);
        let events = drain(&mut stream).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(text_of(&events[0]), "one");
        assert!(matches!(events[1], Ok(StreamEvent::MessageEnd { .. })));
    }

    #[tokio::test]
    async fn a_stream_that_never_saw_its_end_marker_reports_an_error() {
        let mut stream = stream_from(ok("data: one\n\n"), None);
        let events = drain(&mut stream).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[1], Ok(StreamEvent::Error { .. })));
    }

    #[tokio::test]
    async fn the_parser_never_sees_an_empty_payload() {
        // A comment line and a keepalive blank line frame nothing.
        let mut stream = stream_from(ok(": keepalive\n\ndata: one\n\ndata: [DONE]\n\n"), None);
        let events = drain(&mut stream).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(text_of(&events[0]), "one");
    }

    #[tokio::test]
    async fn an_error_a_parser_emits_is_scrubbed_of_the_credential() {
        let mut stream = stream_from(ok("data: one\n\n"), Some(TEST_KEY));
        let events = drain(&mut stream).await;
        let Ok(StreamEvent::Error { message, .. }) = &events[1] else {
            panic!("expected an error event, got {:?}", events[1]);
        };
        assert!(message.contains(REDACTED), "{message}");
        assert!(!message.contains(TEST_KEY), "{message}");
    }

    #[tokio::test]
    async fn a_transport_error_is_reported_with_the_wire_label() {
        let failure = reqwest::Client::new()
            .get(format!("http://127.0.0.1:1/?k={TEST_KEY}"))
            .send()
            .await
            .expect_err("connecting to port 1 is refused");
        let mut stream = stream_from(vec![Err(failure)], Some(TEST_KEY));
        let event = tokio::time::timeout(DEADLINE, stream.next())
            .await
            .expect("an event within the deadline")
            .expect("the stream yields the transport failure");
        let error = event.expect_err("the transport failure is an error");
        let message = error.to_string();
        assert!(message.starts_with("Test wire stream error: "), "{message}");
        assert!(message.contains(REDACTED), "{message}");
        assert!(!message.contains(TEST_KEY), "{message}");
    }

    #[tokio::test]
    async fn the_stream_ends_after_its_closing_events() {
        let mut stream = stream_from(ok("data: one\n\ndata: [DONE]\n\n"), None);
        let events = drain(&mut stream).await;
        assert_eq!(events.len(), 2);
        // A second drain finds nothing: `drained` keeps the close from repeating.
        let again = drain(&mut stream).await;
        assert!(again.is_empty(), "{again:?}");
    }
}

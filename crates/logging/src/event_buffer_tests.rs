//! The kept buffers of a structured line: reuse, release, and the rules a
//! field goes through on its way into the line.

use super::*;
use std::cell::Cell as Flag;

/// Sets its flag when it is printed.
struct Spy<'a>(&'a Flag<bool>);

impl Display for Spy<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.set(true);
        f.write_str("sk-secret")
    }
}

/// Prints part of its text, then reports an error.
struct Failing;

impl Display for Failing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("partial")?;
        Err(fmt::Error)
    }
}

/// Assembles a line of its own while it is printed.
struct Nested;

impl Display for Nested {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::format_structured_event_as(false, "inner", [("k", "v")]))
    }
}

#[test]
fn buffers_are_kept_between_events() {
    let mut buffers = EventBuffers::new();
    buffers.compose(false, "first", [("b", "two"), ("a", "one")]);
    assert_eq!(buffers.message(), "EVENT event=first a=one b=two");
    let (text, message) = (buffers.text.as_ptr(), buffers.message.as_ptr());
    let fields = buffers.fields.capacity();

    buffers.compose(false, "next", [("c", "3")]);
    assert_eq!(buffers.message(), "EVENT event=next c=3");
    assert_eq!(buffers.text.as_ptr(), text);
    assert_eq!(buffers.message.as_ptr(), message);
    assert_eq!(buffers.fields.capacity(), fields);
}

#[test]
fn a_buffer_that_grew_large_is_released() {
    let long = "v".repeat(MAX_KEPT_BYTES + 1);
    let line = with_event_buffers(|buffers| {
        buffers.compose(false, "big", [("value", long.as_str())]);
        buffers.message().to_string()
    });
    assert_eq!(
        line,
        format!("EVENT event=big value={}...", "v".repeat(MAX_LOG_VALUE_BYTES))
    );

    let (text, message) = with_event_buffers(|buffers| (buffers.text.capacity(), buffers.message.capacity()));
    assert_eq!(text, 0, "the text buffer that held the large value is gone");
    assert!(message > 0, "the short line buffer is kept");
}

#[test]
fn the_last_of_two_fields_with_one_key_is_written() {
    let mut buffers = EventBuffers::new();
    buffers.compose(false, "e", [("a", "1"), ("b", "x"), ("a", "2")]);
    assert_eq!(buffers.message(), "EVENT event=e a=2 b=x");

    buffers.compose(true, "e", [("a", "1"), ("b", "x"), ("a", "2")]);
    assert_eq!(buffers.message(), r#"EVENT_JSON {"a":"2","b":"x","event":"e"}"#);

    // Two keys that are the same once sanitized are one key.
    buffers.compose(false, "e", [("a b", "1"), ("a_b", "2")]);
    assert_eq!(buffers.message(), "EVENT event=e a_b=2");
}

#[test]
fn a_credential_field_is_redacted_without_being_printed() {
    let printed = Flag::new(false);
    let mut buffers = EventBuffers::new();
    buffers.compose(false, "login", [("api_key", Spy(&printed))]);
    assert_eq!(buffers.message(), "EVENT event=login api_key=<redacted>");
    assert!(!printed.get());
    assert!(!buffers.text.contains("sk-secret"));

    buffers.compose(false, "login", [("name", Spy(&printed))]);
    assert_eq!(buffers.message(), "EVENT event=login name=sk-secret");
    assert!(printed.get());
}

#[test]
fn a_value_with_a_line_break_a_query_or_extra_length_is_sanitized() {
    let over = "v".repeat(MAX_LOG_VALUE_BYTES + 1);
    let mut buffers = EventBuffers::new();
    buffers.compose(
        false,
        "a\nb",
        [
            ("note", "one\ntwo\tthree"),
            ("over", over.as_str()),
            ("url", "https://example.test/cb?code=secret"),
        ],
    );
    assert_eq!(
        buffers.message(),
        format!(
            "EVENT event=\"a b\" note=\"one two three\" over={}... url=https://example.test/cb?<redacted>",
            "v".repeat(MAX_LOG_VALUE_BYTES)
        )
    );
}

#[test]
fn a_value_that_fails_to_print_is_marked() {
    let mut buffers = EventBuffers::new();
    buffers.compose(false, "e", [("broken", Failing)]);
    assert_eq!(buffers.message(), "EVENT event=e broken=<unprintable>");
}

#[test]
fn a_line_assembled_while_a_value_prints_leaves_the_outer_line_whole() {
    let line = crate::format_structured_event_as(false, "outer", [("inner", Nested)]);
    assert_eq!(line, r#"EVENT event=outer inner="EVENT event=inner k=v""#);
    // The buffers are back in their slot and assemble the next line.
    let line = crate::format_structured_event_as(false, "after", [("k", "v")]);
    assert_eq!(line, "EVENT event=after k=v");
}

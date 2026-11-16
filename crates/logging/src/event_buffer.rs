//! Kept buffers for assembling one structured line.
//!
//! The keys and the rendered values of an event go back to back into one text
//! buffer, and the line is assembled from spans of it. A thread keeps its
//! buffers between events, so the usual event allocates nothing: no `String`
//! per value, no list of pairs and no line of its own.

use std::borrow::Cow;
use std::cell::Cell;
use std::fmt::{self, Display, Write};

use super::{MAX_LOG_VALUE_BYTES, is_credential_key, push_log_field_value, sanitize_log_key, sanitize_log_value};

/// A buffer that grew past this is released after the event instead of kept,
/// so one large value does not pin its size to the thread.
pub(crate) const MAX_KEPT_BYTES: usize = 8 * 1024;
const MAX_KEPT_FIELDS: usize = 256;

/// What is written for a value whose `Display` reports an error.
const UNPRINTABLE: &str = "<unprintable>";

/// One field inside [`EventBuffers::text`]: the sanitized key, then the value
/// as it will be written.
#[derive(Clone, Copy)]
struct FieldSpan {
    key_start: usize,
    value_start: usize,
    value_end: usize,
}

impl FieldSpan {
    fn key<'t>(&self, text: &'t str) -> &'t str {
        &text[self.key_start..self.value_start]
    }

    fn value<'t>(&self, text: &'t str) -> &'t str {
        &text[self.value_start..self.value_end]
    }
}

pub(crate) struct EventBuffers {
    text: String,
    fields: Vec<FieldSpan>,
    message: String,
}

thread_local! {
    static EVENT_BUFFERS: Cell<Option<EventBuffers>> = const { Cell::new(None) };
}

/// Run `f` with this thread's buffers. They are taken out of the slot while in
/// use, so an event logged from inside a value's `Display` gets its own.
pub(crate) fn with_event_buffers<R>(f: impl FnOnce(&mut EventBuffers) -> R) -> R {
    let mut buffers = match EVENT_BUFFERS.try_with(Cell::take) {
        Ok(Some(kept)) => kept,
        // Nothing kept yet, or the thread is shutting down and has no slot left.
        Ok(None) | Err(_) => EventBuffers::new(),
    };
    let result = f(&mut buffers);
    buffers.release_oversized();
    keep(buffers);
    result
}

fn keep(buffers: EventBuffers) {
    if EVENT_BUFFERS.try_with(|slot| slot.set(Some(buffers))).is_err() {
        // The thread is shutting down: the buffers went with the closure.
    }
}

/// Whether a value is written as it is: within the limit, with no line break
/// or tab to replace and no `?` that could start a URL query.
fn is_plain_log_value(value: &str) -> bool {
    value.len() <= MAX_LOG_VALUE_BYTES && !value.bytes().any(|byte| matches!(byte, b'\n' | b'\r' | b'\t' | b'?'))
}

fn render(text: &mut String, value: &dyn Display) -> fmt::Result {
    write!(text, "{value}")
}

impl EventBuffers {
    const fn new() -> Self {
        Self {
            text: String::new(),
            fields: Vec::new(),
            message: String::new(),
        }
    }

    /// The line assembled by the last [`Self::compose`].
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    #[cfg(test)]
    pub(crate) fn take_message(&mut self) -> String {
        std::mem::take(&mut self.message)
    }

    /// Assemble one structured line, as JSON or as `key=value` text.
    ///
    /// Fields are written in key order, and of two fields that share a key the
    /// later one is written.
    pub(crate) fn compose<K, V, I>(&mut self, json: bool, event: &str, fields: I)
    where
        K: AsRef<str>,
        V: Display,
        I: IntoIterator<Item = (K, V)>,
    {
        self.text.clear();
        self.fields.clear();
        self.message.clear();
        for (key, value) in fields {
            self.push_field(key.as_ref(), &value);
        }
        self.finish(json, event);
    }

    fn push_field(&mut self, raw_key: &str, value: &dyn Display) {
        let key_start = self.text.len();
        self.text.push_str(&sanitize_log_key(raw_key));
        let value_start = self.text.len();
        if is_credential_key(raw_key) {
            self.text.push_str("<redacted>");
        } else {
            if render(&mut self.text, value).is_err() {
                self.text.truncate(value_start);
                self.text.push_str(UNPRINTABLE);
            }
            if !is_plain_log_value(&self.text[value_start..]) {
                let sanitized = sanitize_log_value(&self.text[value_start..]);
                self.text.truncate(value_start);
                self.text.push_str(&sanitized);
            }
        }
        self.fields.push(FieldSpan {
            key_start,
            value_start,
            value_end: self.text.len(),
        });
    }

    fn finish(&mut self, json: bool, event: &str) {
        let Self { text, fields, message } = self;
        let text = text.as_str();
        // Key order, and fields that share a key in the order they were given:
        // `key_start` grows with every field, so it breaks the tie the way a
        // stable sort would, without the scratch space of one.
        fields.sort_unstable_by(|left, right| {
            left.key(text)
                .cmp(right.key(text))
                .then(left.key_start.cmp(&right.key_start))
        });
        let event = if is_plain_log_value(event) {
            Cow::Borrowed(event)
        } else {
            Cow::Owned(sanitize_log_value(event))
        };

        if json {
            let mut object = serde_json::Map::new();
            object.insert("event".to_string(), serde_json::Value::String(event.into_owned()));
            for span in fields.iter() {
                object.insert(
                    span.key(text).to_string(),
                    serde_json::Value::String(span.value(text).to_string()),
                );
            }
            message.push_str("EVENT_JSON ");
            message.push_str(&serde_json::Value::Object(object).to_string());
            return;
        }

        message.push_str("EVENT event=");
        push_log_field_value(message, &event);
        for (index, span) in fields.iter().enumerate() {
            if fields
                .get(index + 1)
                .is_some_and(|next| next.key(text) == span.key(text))
            {
                continue;
            }
            message.push(' ');
            message.push_str(span.key(text));
            message.push('=');
            push_log_field_value(message, span.value(text));
        }
    }

    fn release_oversized(&mut self) {
        if self.text.capacity() > MAX_KEPT_BYTES {
            self.text = String::new();
        }
        if self.message.capacity() > MAX_KEPT_BYTES {
            self.message = String::new();
        }
        if self.fields.capacity() > MAX_KEPT_FIELDS {
            self.fields = Vec::new();
        }
    }
}

#[cfg(test)]
#[path = "event_buffer_tests.rs"]
mod tests;

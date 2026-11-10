//! The conversation as it is drawn: user messages, streamed assistant text,
//! and one line per tool call.
//!
//! The transcript owns the streaming buffer rather than the app, because the
//! rules about it (a delta for a new message closes the previous one, an empty
//! replacement retracts text) are the same everywhere and are easy to get wrong
//! at the call site.

/// What a tool call line says about its call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    /// Started, no outcome yet.
    Running,
    /// Finished without an error.
    Done,
    /// Finished with an error the model will see.
    Failed(String),
}

impl ToolState {
    /// The short word shown after the tool name.
    pub fn label(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed(_) => "failed",
        }
    }
}

/// One line of conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A message the user sent.
    User(String),
    /// A completed assistant message.
    Assistant(String),
    /// A tool call, addressed by its call id while it runs.
    Tool { name: String, state: ToolState },
    /// Session-level information, not model output.
    Notice(String),
    /// Something the user needs to see and act on.
    Warning(String),
}

/// The streaming message being assembled.
#[derive(Debug, Default)]
struct Streaming {
    message_id: Option<String>,
    text: String,
    open: bool,
}

/// The transcript buffer.
#[derive(Debug, Default)]
pub struct Transcript {
    items: Vec<Item>,
    streaming: Streaming,
}

impl Transcript {
    /// An empty transcript.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every item in display order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Record a user message, closing any partly streamed reply first.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::User(text.into()));
    }

    /// Record an informational line.
    pub fn push_notice(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::Notice(text.into()));
    }

    /// Record a warning line.
    pub fn push_warning(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::Warning(text.into()));
    }

    /// Append streamed assistant text.
    ///
    /// A delta that carries a different message id than the open message starts
    /// a new one, which is how one turn's several messages stay separate
    /// without waiting for `TextDone`.
    pub fn text_delta(&mut self, message_id: Option<&str>, text: &str) {
        if self.streaming.open && self.streaming.message_id.as_deref() != message_id {
            self.finish_stream();
        }
        self.streaming.open = true;
        self.streaming.message_id = message_id.map(str::to_string);
        self.streaming.text.push_str(text);
    }

    /// Replace the open message's text.
    ///
    /// An empty replacement retracts it, including one that was already
    /// completed: providers retry, and the client must not keep text the model
    /// withdrew.
    pub fn text_replace(&mut self, message_id: Option<&str>, text: &str) {
        if self.streaming.open && self.streaming.message_id.as_deref() != message_id {
            self.finish_stream();
        }
        if text.is_empty() {
            let retracts_open = self.streaming.open && self.streaming.message_id.as_deref() == message_id;
            if retracts_open {
                self.streaming = Streaming::default();
                return;
            }
            if let Some(index) = self.last_assistant_index() {
                self.items.remove(index);
            }
            return;
        }
        self.streaming.open = true;
        self.streaming.message_id = message_id.map(str::to_string);
        self.streaming.text.clear();
        self.streaming.text.push_str(text);
    }

    /// A message ended; the next delta starts a new one.
    pub fn text_done(&mut self) {
        self.finish_stream();
    }

    /// A tool call started.
    pub fn tool_start(&mut self, name: &str) {
        self.finish_stream();
        self.items.push(Item::Tool {
            name: name.to_string(),
            state: ToolState::Running,
        });
    }

    /// A tool call finished, with the error text when it failed.
    pub fn tool_done(&mut self, name: &str, error: Option<&str>) {
        let state = match error {
            Some(message) if !message.is_empty() => ToolState::Failed(message.to_string()),
            _ => ToolState::Done,
        };
        if let Some(Item::Tool { state: slot, .. }) = self
            .items
            .iter_mut()
            .rev()
            .find(|item| matches!(item, Item::Tool { name: held, .. } if held == name))
        {
            *slot = state;
            return;
        }
        self.items.push(Item::Tool {
            name: name.to_string(),
            state,
        });
    }

    /// Commit the open streaming message, dropping an empty one.
    pub fn finish_stream(&mut self) {
        if !self.streaming.open {
            return;
        }
        let streaming = std::mem::take(&mut self.streaming);
        if !streaming.text.trim().is_empty() {
            self.items.push(Item::Assistant(streaming.text));
        }
    }

    fn last_assistant_index(&self) -> Option<usize> {
        self.items.iter().rposition(|item| matches!(item, Item::Assistant(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_accumulate_and_a_new_message_id_splits() {
        let mut transcript = Transcript::new();
        transcript.text_delta(Some("m1"), "one ");
        transcript.text_delta(Some("m1"), "two");
        transcript.text_delta(Some("m2"), "three");
        transcript.finish_stream();
        assert_eq!(
            transcript.items(),
            &[
                Item::Assistant("one two".to_string()),
                Item::Assistant("three".to_string())
            ]
        );
    }

    #[test]
    fn empty_message_is_not_published() {
        let mut transcript = Transcript::new();
        transcript.text_delta(Some("m1"), "draft");
        transcript.text_replace(Some("m1"), "");
        transcript.finish_stream();
        assert!(transcript.items().is_empty());
    }

    #[test]
    fn tool_done_updates_the_running_line() {
        let mut transcript = Transcript::new();
        transcript.tool_start("bash");
        transcript.tool_done("bash", None);
        assert_eq!(
            transcript.items(),
            &[Item::Tool {
                name: "bash".to_string(),
                state: ToolState::Done
            }]
        );
    }

    #[test]
    fn tool_done_without_a_start_still_renders() {
        let mut transcript = Transcript::new();
        transcript.tool_done("read", Some("no such file"));
        assert_eq!(
            transcript.items(),
            &[Item::Tool {
                name: "read".to_string(),
                state: ToolState::Failed("no such file".to_string())
            }]
        );
    }
}

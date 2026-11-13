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
    /// A thought the model published before its answer, drawn apart from it.
    Reasoning(String),
}

/// The streaming message being assembled.
#[derive(Debug, Default)]
struct Streaming {
    message_id: Option<String>,
    text: String,
    open: bool,
}

/// The reasoning being assembled, kept apart from the answer.
///
/// A provider streams reasoning and assistant text on the same message id, in
/// order, interleaved with tool calls. Accumulating both into one buffer would
/// splice a thought into the middle of the answer it precedes, so reasoning has
/// its own slot and is committed on its own boundary.
#[derive(Debug, Default)]
struct Reasoned {
    text: String,
    open: bool,
}

/// The transcript buffer.
#[derive(Debug, Default)]
pub struct Transcript {
    items: Vec<Item>,
    streaming: Streaming,
    reasoned: Reasoned,
    /// Whether reasoning is kept. Off by default, as
    /// `transcript.show_reasoning` resolves; the option is read at the event,
    /// so flipping it affects the next delta rather than only the next turn.
    show_reasoning: bool,
}

impl Transcript {
    /// An empty transcript.
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep, or stop keeping, the model's reasoning.
    ///
    /// Turning it off drops reasoning already being assembled, because a
    /// setting the user just switched off must not leave half a thought on
    /// screen; turning it on keeps what the answer has shown so far.
    pub fn set_show_reasoning(&mut self, show: bool) {
        self.show_reasoning = show;
        if !show {
            self.reasoned = Reasoned::default();
        }
    }

    /// Whether reasoning is being kept.
    pub fn show_reasoning(&self) -> bool {
        self.show_reasoning
    }

    /// Every item in display order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The assistant text still streaming, if any.
    ///
    /// `None` once the message is committed by [`finish_stream`](Self::finish_stream),
    /// retracted by [`text_replace`](Self::text_replace), or when the open
    /// message holds only whitespace, which `finish_stream` would drop rather
    /// than publish. The renderer draws this as its newest entry, so a message
    /// appears while it streams instead of all at once when it ends.
    pub fn open_text(&self) -> Option<&str> {
        if !self.streaming.open || self.streaming.text.trim().is_empty() {
            return None;
        }
        Some(&self.streaming.text)
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
        self.finish_reasoning();
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

    /// Reasoning text arrived for the message being assembled.
    ///
    /// Dropped when the setting is off, and dropped rather than buffered: a
    /// client that keeps it anyway pays for tokens it will never draw.
    pub fn reasoning_delta(&mut self, text: &str) {
        if !self.show_reasoning {
            return;
        }
        // Accumulate. Reasoning commits at a boundary (`reasoning_done`, a tool
        // call, or the first answer delta), never on the next delta: a provider
        // streams a thought as many deltas, and committing each one would leave
        // one item per chunk instead of one per thought.
        self.reasoned.text.push_str(text);
        self.reasoned.open = true;
    }

    /// Reasoning ended for the current step.
    pub fn reasoning_done(&mut self) {
        self.finish_reasoning();
    }

    /// Reasoning still streaming, for the renderer to draw dimmed.
    pub fn open_reasoning(&self) -> Option<&str> {
        if !self.reasoned.open || self.reasoned.text.trim().is_empty() {
            return None;
        }
        Some(&self.reasoned.text)
    }

    /// Commit the open reasoning, dropping an empty one.
    fn finish_reasoning(&mut self) {
        if !self.reasoned.open {
            return;
        }
        let reasoned = std::mem::take(&mut self.reasoned);
        if !reasoned.text.trim().is_empty() {
            self.items.push(Item::Reasoning(reasoned.text));
        }
    }

    /// A tool call started.
    pub fn tool_start(&mut self, name: &str) {
        self.finish_reasoning();
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
        self.finish_reasoning();
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
    fn open_text_reports_the_message_until_it_is_committed() {
        let mut transcript = Transcript::new();
        assert_eq!(transcript.open_text(), None, "nothing is open yet");

        transcript.text_delta(Some("m1"), "partial");
        assert_eq!(transcript.open_text(), Some("partial"));

        transcript.text_delta(Some("m1"), " more");
        assert_eq!(transcript.open_text(), Some("partial more"));

        // The committed item is the same text, so the renderer must stop
        // drawing the open one or the reply would appear twice.
        transcript.finish_stream();
        assert_eq!(transcript.open_text(), None);
        assert_eq!(transcript.items(), &[Item::Assistant("partial more".to_string())]);
    }

    #[test]
    fn open_text_hides_blank_and_retracted_messages() {
        let mut transcript = Transcript::new();
        transcript.text_delta(Some("m1"), "  \n\t");
        assert_eq!(transcript.open_text(), None, "whitespace is never published");

        // Deltas append, so the whitespace that was hidden on its own is now
        // part of the message; drawing anything but the raw buffer would make
        // the open row differ from the item `finish_stream` commits.
        transcript.text_delta(Some("m1"), "draft");
        assert_eq!(transcript.open_text(), Some("  \n\tdraft"));
        transcript.text_replace(Some("m1"), "");
        assert_eq!(transcript.open_text(), None, "a retraction must not be drawn");
    }

    #[test]
    fn reasoning_is_dropped_while_the_setting_is_off() {
        let mut transcript = Transcript::new();
        transcript.reasoning_delta("thinking about it");
        assert_eq!(
            transcript.open_reasoning(),
            None,
            "off means the client keeps nothing to draw"
        );
        transcript.reasoning_done();
        assert!(
            !transcript.items().iter().any(|item| matches!(item, Item::Reasoning(_))),
            "and nothing reaches the transcript: {:?}",
            transcript.items()
        );
    }

    #[test]
    fn reasoning_is_kept_apart_from_the_answer_when_the_setting_is_on() {
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(true);
        transcript.reasoning_delta("first I look");
        transcript.reasoning_delta(" at the file");
        assert_eq!(transcript.open_reasoning(), Some("first I look at the file"));
        transcript.reasoning_done();
        assert_eq!(transcript.open_reasoning(), None, "a boundary commits it");

        transcript.text_delta(None, "here is the answer");
        assert_eq!(
            transcript.open_text(),
            Some("here is the answer"),
            "the reasoning never joins the answer"
        );
        transcript.text_done();

        let items = transcript.items();
        assert_eq!(
            items,
            &[
                Item::Reasoning("first I look at the file".to_string()),
                Item::Assistant("here is the answer".to_string()),
            ],
            "reasoning precedes its answer in display order"
        );
    }

    #[test]
    fn a_tool_call_commits_the_reasoning_that_preceded_it() {
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(true);
        transcript.reasoning_delta("I need to read the file");
        transcript.tool_start("read");
        assert_eq!(transcript.open_reasoning(), None);
        assert_eq!(
            transcript.items().first(),
            Some(&Item::Reasoning("I need to read the file".to_string())),
            "the thought belongs above the call it caused: {:?}",
            transcript.items()
        );
    }

    #[test]
    fn turning_reasoning_off_retracts_a_half_written_thought() {
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(true);
        transcript.reasoning_delta("half a thought");
        transcript.set_show_reasoning(false);
        assert_eq!(transcript.open_reasoning(), None);
        transcript.reasoning_done();
        assert!(
            !transcript.items().iter().any(|item| matches!(item, Item::Reasoning(_))),
            "nothing half-finished stays on screen: {:?}",
            transcript.items()
        );
    }

    #[test]
    fn blank_reasoning_is_never_published() {
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(true);
        transcript.reasoning_delta("   ");
        transcript.reasoning_done();
        assert!(
            transcript.items().is_empty(),
            "whitespace alone is not a thought: {:?}",
            transcript.items()
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

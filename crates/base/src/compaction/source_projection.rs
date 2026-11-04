//! Source-bound projection of the messages one compaction job may summarize.
//!
//! Coverage starts at the previous compaction boundary, ends on a tool-pair
//! boundary, and never swallows the newest user instruction. The excluded
//! remainder stays in the live tail in its original order. Long tool results
//! keep their head, first error line, and tail inside the character budget, and
//! every rendered message carries the id of the event it came from. Images are
//! labelled as not read. Originals are never mutated here.

use super::summary::SummaryError;
use crate::message::{ContentBlock, Message, Role};
use horon_agent_compaction_core::{message_token_count, safe_compaction_cutoff, truncate_str_boundary};

/// Per-message floor for rendered content.
const MIN_MESSAGE_CHARS: usize = 160;

/// Width reserved for the fixed-size omission marker inside one tool result.
const OMISSION_MARKER_CHARS: usize = 40;

/// Cap on the preserved error line.
const MAX_ERROR_LINE_CHARS: usize = 400;

const ERROR_MARKERS: &[&str] = &[
    "error",
    "failed",
    "failure",
    "panic",
    "exception",
    "traceback",
    "fatal",
    "exit code",
];

/// One original message plus the identity of the event it came from.
///
/// `event_id` and `content_hash` are `None` only when the caller has no event
/// store at hand, such as a transfer handoff built from bare messages. Such a
/// projection is not source-bound and produces no digest.
#[derive(Clone, Debug)]
pub struct SourceMessage {
    pub event_id: Option<String>,
    pub content_hash: Option<String>,
    pub message: Message,
}

impl SourceMessage {
    pub fn unattributed(message: Message) -> Self {
        Self {
            event_id: None,
            content_hash: None,
            message,
        }
    }
}

/// Ordered source range for one compaction job.
#[derive(Clone, Debug, Default)]
pub struct SourceProjection {
    messages: Vec<SourceMessage>,
}

/// Half-open index range `[start, end)` over a [`SourceProjection`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoveredRange {
    start: usize,
    end: usize,
}

impl CoveredRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn start(&self) -> usize {
        self.start
    }

    pub fn end(&self) -> usize {
        self.end
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SourceProjection {
    pub fn from_messages(messages: &[Message]) -> Self {
        Self {
            messages: messages.iter().cloned().map(SourceMessage::unattributed).collect(),
        }
    }

    pub fn from_source_messages(messages: Vec<SourceMessage>) -> Self {
        Self { messages }
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn messages(&self) -> &[SourceMessage] {
        &self.messages
    }

    pub fn message_at(&self, index: usize) -> Option<&SourceMessage> {
        self.messages.get(index)
    }

    fn raw_messages(&self) -> Vec<Message> {
        self.messages.iter().map(|source| source.message.clone()).collect()
    }

    /// Index of the newest user instruction.
    ///
    /// A newer revision of the same logical message replaces the older one in
    /// the event projection, so the last user message *is* the newest revision
    /// of the user's request. Nothing may summarize it away.
    pub fn newest_user_index(&self) -> Option<usize> {
        self.messages
            .iter()
            .rposition(|source| source.message.role == Role::User)
    }

    /// Event ids of the covered messages in original order, or `None` when any
    /// covered message has no event identity.
    pub fn covered_ids(&self, range: CoveredRange) -> Option<Vec<String>> {
        let mut ids = Vec::with_capacity(range.len());
        for source in self.messages.get(range.start..range.end)? {
            ids.push(source.event_id.clone()?);
        }
        Some(ids)
    }

    pub fn covered_through_id(&self, range: CoveredRange) -> Option<String> {
        self.messages.get(range.end.checked_sub(1)?)?.event_id.clone()
    }

    pub fn first_kept_id(&self, range: CoveredRange) -> Option<String> {
        self.messages.get(range.end)?.event_id.clone()
    }

    /// Digest of the covered prefix from the same canonical parts the event
    /// store hashes: `{event_id, content_hash}` per event, preceded by the prior
    /// compaction when this summary is incremental.
    ///
    /// `None` means the projection is not source-bound; the caller must then let
    /// the store derive the boundary from the covered message count instead of
    /// recording a digest it cannot reproduce.
    pub fn source_prefix_digest(&self, range: CoveredRange, prior: PriorBoundary<'_>) -> Option<String> {
        let mut parts: Vec<serde_json::Value> = Vec::new();
        if let PriorBoundary::Compacted { event_id, content_hash } = prior {
            parts.push(serde_json::json!({
                "prior_compaction_id": event_id,
                "content_hash": content_hash,
            }));
        }
        for source in self.messages.get(range.start..range.end)? {
            parts.push(serde_json::json!({
                "event_id": source.event_id.as_deref()?,
                "content_hash": source.content_hash.as_deref()?,
            }));
        }
        horon_agent_session_store::canonical_digest_of_values(&parts).ok()
    }

    /// Tokens the excluded tail would occupy, measured like the recent-tail
    /// allowance.
    pub fn tail_tokens(&self, range: CoveredRange) -> usize {
        self.messages
            .get(range.end..)
            .unwrap_or_default()
            .iter()
            .map(|source| message_token_count(&source.message))
            .sum()
    }
}

/// How this summary relates to the compaction it builds on.
#[derive(Clone, Copy, Debug)]
pub enum PriorBoundary<'a> {
    None,
    Compacted {
        event_id: &'a str,
        content_hash: Option<&'a str>,
    },
}

/// Plan the coverage for a candidate end index.
///
/// The end is clamped so the newest user instruction stays verbatim, then moved
/// back to a boundary that never leaves a kept tool result without its tool
/// call. Covering nothing is refused instead of reported as a boundary.
pub fn plan_covered_range(
    projection: &SourceProjection,
    candidate_end: usize,
    keep_newest_user: bool,
) -> Result<CoveredRange, SummaryError> {
    let mut end = candidate_end.min(projection.len());
    if keep_newest_user
        && let Some(newest_user) = projection.newest_user_index()
        && end > newest_user
    {
        end = newest_user;
    }
    let messages = projection.raw_messages();
    let end = safe_compaction_cutoff(&messages, end);
    if end == 0 {
        return Err(SummaryError::NothingToSummarize);
    }
    Ok(CoveredRange::new(0, end))
}

/// Halve a planned coverage, ending on a tool-pair boundary.
///
/// `None` means halving cannot make progress; that is what stops the overflow
/// loop on a prefix that is already a single tool pair.
pub fn halve_covered_range(
    projection: &SourceProjection,
    range: CoveredRange,
    keep_newest_user: bool,
) -> Option<CoveredRange> {
    if range.len() <= 1 {
        return None;
    }
    let candidate = range.start + range.len() / 2;
    let messages = projection.raw_messages();
    let mut end = safe_compaction_cutoff(&messages, candidate).min(range.end);
    if keep_newest_user
        && let Some(newest_user) = projection.newest_user_index()
        && end > newest_user
    {
        end = newest_user;
    }
    if end <= range.start || end >= range.end {
        return None;
    }
    Some(CoveredRange::new(range.start, end))
}

/// Render the covered range for the provider inside `budget_chars`.
///
/// Every message is prefixed with its source event id (or `?` when the caller
/// has none), so a summary can cite where a fact came from.
pub fn render_source_range(projection: &SourceProjection, range: CoveredRange, budget_chars: usize) -> String {
    let covered = match projection.messages.get(range.start..range.end) {
        Some(covered) if !covered.is_empty() => covered,
        _ => return String::new(),
    };

    let label_chars: usize = covered
        .iter()
        .map(|source| event_label(source).len() + role_label(&source.message.role).len() + 4)
        .sum();
    let content_budget = budget_chars.saturating_sub(label_chars);
    let per_message = (content_budget / covered.len()).max(MIN_MESSAGE_CHARS);

    let mut rendered = String::new();
    for source in covered {
        rendered.push_str(&event_label(source));
        rendered.push_str(role_label(&source.message.role));
        rendered.push_str(": ");
        render_message(&source.message, per_message, &mut rendered);
        rendered.push('\n');
    }
    rendered
}

fn event_label(source: &SourceMessage) -> String {
    match source.event_id.as_deref() {
        Some(id) => format!("[event {id}] "),
        None => "[event ?] ".to_string(),
    }
}

fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn render_message(message: &Message, allowance: usize, out: &mut String) {
    for block in &message.content {
        match block {
            ContentBlock::Text { text, .. } => {
                out.push_str(&render_bounded_text(text, allowance));
                out.push(' ');
            }
            ContentBlock::ToolUse { name, input, .. } => {
                let input_text = input.to_string();
                let rendered_input = truncate_str_boundary(&input_text, allowance / 2);
                out.push_str(&format!("[tool {name} {rendered_input}] "));
            }
            ContentBlock::ToolResult { content, .. } => {
                out.push_str("[result] ");
                out.push_str(&render_tool_result(content, allowance));
                out.push(' ');
            }
            ContentBlock::Image { .. } => {
                out.push_str("[image attachment — content not read] ");
            }
            ContentBlock::ProviderNative { .. } => {
                out.push_str("[provider-native item — content not read] ");
            }
            ContentBlock::OpenAICompaction { .. } => {
                out.push_str("[openai native compaction state] ");
            }
            ContentBlock::Reasoning { .. }
            | ContentBlock::ReasoningTrace { .. }
            | ContentBlock::AnthropicThinking { .. }
            | ContentBlock::OpenAIReasoning { .. }
            | ContentBlock::ToolReference { .. } => {}
        }
    }
}

/// Render text within `allowance`, keeping the head and the tail.
pub fn render_bounded_text(text: &str, allowance: usize) -> String {
    if text.len() <= allowance {
        return text.to_string();
    }
    render_head_tail(text, allowance, None)
}

/// Render a tool result within `allowance`, keeping the head, the first error
/// line, and the tail, with an explicit marker for what was dropped.
pub fn render_tool_result(content: &str, allowance: usize) -> String {
    if content.len() <= allowance {
        return content.to_string();
    }
    let error_line = first_error_line(content);
    render_head_tail(content, allowance, error_line.as_deref())
}

fn render_head_tail(value: &str, allowance: usize, error_line: Option<&str>) -> String {
    let error_marker = error_line.map(|line| {
        let line = truncate_str_boundary(line, MAX_ERROR_LINE_CHARS);
        format!("[error] {line}\n")
    });
    let error_len = error_marker.as_ref().map(String::len).unwrap_or(0);

    if value.len() <= error_len + OMISSION_MARKER_CHARS + 8 {
        return String::new();
    }

    let body_allowance = allowance.saturating_sub(error_len + OMISSION_MARKER_CHARS);
    let head_allowance = body_allowance * 3 / 5;
    let tail_allowance = body_allowance - head_allowance;
    let head = truncate_str_boundary(value, head_allowance);
    let tail = suffix_str_boundary(value, tail_allowance);
    let omitted = value.len().saturating_sub(head.len()).saturating_sub(tail.len());
    let marker = format_omission_marker(omitted);

    let mut out = String::with_capacity(value.len().min(allowance) + error_len);
    out.push_str(head);
    if let Some(error_marker) = error_marker {
        out.push('\n');
        out.push_str(&error_marker);
    }
    out.push_str(&marker);
    out.push_str(tail);
    out
}

fn format_omission_marker(omitted: usize) -> String {
    let digits = format!("{omitted:>12}");
    format!("\n[... {digits} chars omitted ...]\n")
}

/// The first line of `value` that looks like an error, trimmed.
pub fn first_error_line(value: &str) -> Option<String> {
    value.lines().find_map(|line| {
        let lowered = line.to_ascii_lowercase();
        ERROR_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker))
            .then(|| line.trim().to_string())
    })
}

/// The last `max_bytes` of `value`, cut on a character boundary.
pub fn suffix_str_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len() - max_bytes;
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

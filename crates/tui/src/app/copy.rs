//! Copying the draft, the line under the cursor, and what the model answered.
//!
//! The app decides *what* a copy carries and leaves the terminal alone; writing
//! the sequence is [`crate::loop_io::flush_copy`]'s job, where the real standard
//! output lives. That split is what keeps these choices testable: the tests read
//! [`App::take_copy`] and the transcript, never a terminal.
//!
//! A copy is always explicit. Nothing here runs on a timer or as a side effect
//! of a turn ending, so a model's answer cannot quietly take the clipboard the
//! user filled for something else.

use std::borrow::Cow;

use super::{Action, App};
use crate::overlays::picker::{Kind, Picker, Row};
use crate::transcript::{Item, Transcript};

/// The title the list is drawn with.
const TITLE: &str = "Copy to the terminal clipboard";

/// What the list says when the draft and every answer it reaches are empty.
const EMPTY: &str = "nothing to copy yet";

/// How many answers back the list and its keys reach.
///
/// A conversation is long, and the list is opened to copy the thing just read;
/// four answers cover that and keep the preview pane from carrying the whole
/// session around.
const ANSWERS: usize = 4;

/// Rows the list may hold, so a message with many code blocks stays a list.
const MAX_ROWS: usize = 16;

/// Lines of one choice the preview pane shows.
const PREVIEW_LINES: usize = 6;

/// One copy the app has asked for, waiting for the loop to write it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyRequest {
    /// What the transcript calls it, e.g. "the draft".
    pub(crate) label: String,
    /// The text the sequence carries.
    pub(crate) text: String,
}

impl App {
    /// Take the copy waiting to be written, if one is.
    pub(crate) fn take_copy(&mut self) -> Option<CopyRequest> {
        self.pending_copy.take()
    }

    /// Answer `/copy`: the list when nothing is named, one target otherwise.
    pub(crate) fn copy_command(&mut self, arguments: &str) -> Option<Action> {
        match arguments.split_whitespace().next().unwrap_or("") {
            "" => self.open_copy_picker(),
            "last" | "answer" => self.copy_last_answer(),
            "code" | "block" => self.copy_newest_code(),
            // The editor holds the draft until Enter takes it, and Enter is how
            // this line gets here: a `/copy draft` would copy the command line
            // that replaced the draft. The key is the explicit copy action for
            // the draft, and the sentence names it as the map in force spells it.
            "draft" => self.warn(self.draft_hint(crate::keys::ids::APP_COPY_DRAFT, "the draft")),
            "line" | "current" => {
                self.warn(self.draft_hint(crate::keys::ids::APP_COPY_LINE, "the line under the cursor"))
            }
            other => self.warn(format!(
                "copy: unknown choice `{other}`; try /copy, /copy last or /copy code"
            )),
        }
        None
    }

    /// The sentence a `/copy draft` or `/copy line` earns: the key that does it.
    ///
    /// The editor holds the draft until Enter takes it, and Enter is how this
    /// command got here, so the command line replaced the draft it was meant to
    /// copy. The sentence names the key from the map in force rather than the
    /// shipped one, so a rebound copy key is the key it tells the user to press.
    fn draft_hint(&self, action: &str, what: &str) -> String {
        match self.bindings.first_key(action) {
            Some(key) => {
                format!("copy: the command line replaced the draft, so {key} copies {what}")
            }
            None => format!("copy: the command line replaced the draft, and the copy key for {what} is unbound"),
        }
    }

    /// Copy the whole draft, line breaks and all.
    pub(crate) fn copy_draft(&mut self) {
        let text = self.input.text();
        self.request_copy("the draft", text);
    }

    /// Copy the line the cursor is on.
    pub(crate) fn copy_line(&mut self) {
        let text = self.input.current_line();
        self.request_copy("the line under the cursor", text);
    }

    /// Copy the newest answer, streamed or finished.
    pub(crate) fn copy_last_answer(&mut self) {
        self.copy_answer(0, None);
    }

    /// Copy the last code block of the newest answer.
    ///
    /// The last block is the one the model just wrote, which is what a reader
    /// reaching for this key wants; the list copies any other block.
    pub(crate) fn copy_newest_code(&mut self) {
        let Some(answer) = self.answer_at(0) else {
            self.warn("copy: there is no answer to copy from yet");
            return;
        };
        let mut last = None;
        scan_code_blocks(answer, |span| {
            last = Some((span.body_start, span.body_end, span.raw_bytes));
            true
        });
        match last {
            Some((start, end, cap)) => {
                let text = materialize_block(answer, start, end, cap);
                self.request_copy("the newest code block", text);
            }
            None => self.warn(format!("copy: {} has no code block", answer_name(0))),
        }
    }

    /// Open the list of answers and code blocks this session can copy.
    ///
    /// The draft is not a row: a slash command only reaches the client after
    /// Enter took the draft, so the list always opens with an empty editor. The
    /// draft's own copy is the key, which needs no list to name one thing.
    pub(crate) fn open_copy_picker(&mut self) {
        let rows = build_copy_rows(&self.transcript);
        if rows.is_empty() {
            self.warn("copy: nothing to copy yet; the answers are empty");
            return;
        }
        self.picker = Some(Picker::new(Kind::Copies, TITLE, rows, EMPTY));
    }

    /// Copy the choice a row of the list carries.
    ///
    /// The row's value names the source rather than holding the text, so the
    /// preview the list drew and the text that reaches the clipboard cannot be
    /// two different messages: a value that no longer resolves copies nothing
    /// rather than something else.
    pub(crate) fn copy_selection(&mut self, value: &str) {
        if let Some(rest) = value.strip_prefix("answer:") {
            if let Ok(back) = rest.parse::<usize>() {
                self.copy_answer(back, None);
            }
            return;
        }
        if let Some(rest) = value.strip_prefix("code:")
            && let Some((back, index)) = rest.split_once(':')
            && let (Ok(back), Ok(index)) = (back.parse::<usize>(), index.parse::<usize>())
        {
            self.copy_answer(back, Some(index));
        }
    }

    /// Answer a copy key, reporting whether the action was one of them.
    ///
    /// The key map decides which keys reach here; this is the one place the
    /// copy actions become the targets `/copy` names, so a key and its command
    /// alias cannot drift apart.
    pub(crate) fn copy_action(&mut self, action: &str) -> bool {
        match action {
            crate::keys::ids::APP_COPY_DRAFT => self.copy_draft(),
            crate::keys::ids::APP_COPY_LINE => self.copy_line(),
            crate::keys::ids::APP_COPY_LAST => self.copy_last_answer(),
            crate::keys::ids::APP_COPY_CODE => self.copy_newest_code(),
            _ => return false,
        }
        true
    }

    /// Queue one copy, or say why there is nothing to queue.
    fn request_copy(&mut self, label: impl Into<String>, text: String) {
        let label = label.into();
        if text.trim().is_empty() {
            self.warn(format!("copy: {label} holds nothing to copy"));
            return;
        }
        self.pending_copy = Some(CopyRequest { label, text });
    }

    /// Copy one answer back, or one of its code blocks.
    fn copy_answer(&mut self, back: usize, block: Option<usize>) {
        let Some(answer) = self.answer_at(back) else {
            self.warn("copy: there is no answer to copy yet");
            return;
        };
        let name = answer_name(back);
        match block {
            None => {
                let owned = answer.to_string();
                self.request_copy(name, owned);
            }
            Some(index) => {
                let mut seen = 0;
                let mut target = None;
                scan_code_blocks(answer, |span| {
                    if seen == index {
                        target = Some((span.body_start, span.body_end, span.raw_bytes));
                        return false;
                    }
                    seen += 1;
                    true
                });
                match target {
                    Some((start, end, cap)) => {
                        let text = materialize_block(answer, start, end, cap);
                        self.request_copy(format!("code block {} of {name}", index + 1), text);
                    }
                    None => self.warn(format!("copy: {name} has no code block {}", index + 1)),
                }
            }
        }
    }

    /// The `back`-th newest answer, counting a streamed one as the newest.
    fn answer_at(&self, back: usize) -> Option<&str> {
        answer_in_transcript(&self.transcript, back)
    }
}

fn answer_in_transcript(transcript: &Transcript, back: usize) -> Option<&str> {
    let mut seen = 0;
    if let Some(text) = transcript.open_text() {
        if back == 0 {
            return Some(text);
        }
        seen = 1;
    }
    for item in transcript.items().iter().rev() {
        if let Item::Assistant(text) = item {
            if seen == back {
                return Some(text.as_str());
            }
            seen += 1;
        }
    }
    None
}

fn build_copy_rows(transcript: &Transcript) -> Vec<Row> {
    let mut rows = Vec::new();
    for back in 0..ANSWERS {
        if rows.len() >= MAX_ROWS {
            break;
        }
        let Some(answer) = answer_in_transcript(transcript, back) else {
            break;
        };
        let name = answer_name(back);
        let mut index = 0;
        scan_code_blocks(answer, |span| {
            if rows.len() >= MAX_ROWS {
                return false;
            }
            let language = if span.language.is_empty() {
                "code"
            } else {
                span.language
            };
            let detail = format!("{language} · {} lines", span.line_count);
            let label = format!("code block {} of {name}", index + 1);
            let value = format!("code:{back}:{index}");
            rows.push(
                Row::new(label, value)
                    .with_detail(detail)
                    .with_preview(preview_lines(span.preview[..span.preview_len].iter().copied())),
            );
            index += 1;
            rows.len() < MAX_ROWS
        });
        if rows.len() < MAX_ROWS {
            rows.push(row(name.as_ref(), &format!("answer:{back}"), answer));
        }
    }
    rows
}

/// What the transcript calls the `back`-th newest answer.
fn answer_name(back: usize) -> Cow<'static, str> {
    match back {
        0 => Cow::Borrowed("the last answer"),
        1 => Cow::Borrowed("the answer before it"),
        other => Cow::Owned(format!("answer {other} back")),
    }
}

/// One row of the list: the name, the value, and the head of the text.
fn row(label: &str, value: &str, text: &str) -> Row {
    Row::new(label.to_string(), value.to_string())
        .with_detail(detail(text))
        .with_preview(preview(text))
}

/// The short second column: how much text the choice carries.
fn detail(text: &str) -> String {
    let lines = text.lines().count();
    let characters = if text.is_ascii() {
        text.len()
    } else {
        text.chars().count()
    };
    if lines > 1 {
        format!("{lines} lines · {characters} characters")
    } else {
        format!("{characters} characters")
    }
}

/// The first rows of a choice, with control characters left out.
///
/// The preview is drawn by the same pane that draws everything else, so a
/// character that would drive the terminal is dropped here rather than carried
/// into a row: a model's answer is not a trusted drawing instruction.
fn preview(text: &str) -> Vec<String> {
    preview_lines(text.lines())
}

fn preview_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<String> {
    lines
        .take(PREVIEW_LINES)
        .map(|line| {
            if line.is_ascii() && line.bytes().all(|byte| byte == b'\t' || (byte >= 0x20 && byte != 0x7f)) {
                line.to_string()
            } else {
                line.chars()
                    .filter(|character| !character.is_control() || *character == '\t')
                    .collect()
            }
        })
        .collect()
}

struct BlockSpan<'a> {
    language: &'a str,
    body_start: usize,
    body_end: usize,
    line_count: usize,
    raw_bytes: usize,
    preview: [&'a str; PREVIEW_LINES],
    preview_len: usize,
}

struct OpenFence<'a> {
    marker: &'static str,
    language: &'a str,
    body_start: usize,
    raw_count: usize,
    raw_bytes: usize,
    preview: [&'a str; PREVIEW_LINES],
    preview_len: usize,
}

fn materialize_block(text: &str, body_start: usize, body_end: usize, exact_bytes: usize) -> String {
    let mut out = String::with_capacity(exact_bytes);
    let mut first = true;
    for line in text[body_start..body_end].lines() {
        if !first {
            out.push('\n');
        }
        first = false;
        out.push_str(line.trim_end());
    }
    out
}

/// Visit every closed fenced block of `text`, in the order the answer wrote them.
///
/// The rules are `markdown`'s: an opening fence starts a line with three
/// backticks or three tildes, and only a line of at least as many of the same
/// character closes it. A fence with no end is dropped, because its code is
/// still arriving: copying half a block would be worse than saying there is
/// nothing there yet.
fn scan_code_blocks<'a>(text: &'a str, mut visit: impl FnMut(BlockSpan<'a>) -> bool) {
    let mut offset = 0usize;
    let mut open: Option<OpenFence<'a>> = None;
    for chunk in text.split_inclusive('\n') {
        let chunk_start = offset;
        offset += chunk.len();
        let line = chunk
            .strip_suffix("\r\n")
            .or_else(|| chunk.strip_suffix('\n'))
            .unwrap_or(chunk);
        let trimmed = line.trim_end();
        if let Some(state) = open.as_mut() {
            if closes_fence(trimmed, state.marker) {
                let state = open.take().expect("open fence");
                let span = BlockSpan {
                    language: state.language,
                    body_start: state.body_start,
                    body_end: chunk_start,
                    line_count: state.raw_count,
                    raw_bytes: state.raw_bytes + state.raw_count.saturating_sub(1),
                    preview: state.preview,
                    preview_len: state.preview_len,
                };
                if !visit(span) {
                    return;
                }
            } else {
                state.raw_count += 1;
                state.raw_bytes += trimmed.len();
                if state.preview_len < PREVIEW_LINES {
                    state.preview[state.preview_len] = trimmed;
                    state.preview_len += 1;
                }
            }
            continue;
        }
        if let Some((marker, info)) = fence_info(trimmed.trim_start()) {
            let language = info.split_whitespace().next().unwrap_or("");
            open = Some(OpenFence {
                marker,
                language,
                body_start: offset,
                raw_count: 0,
                raw_bytes: 0,
                preview: [""; PREVIEW_LINES],
                preview_len: 0,
            });
        }
    }
}

/// The fence marker this line opens a block with, and its info string.
fn fence_info(line: &str) -> Option<(&'static str, &str)> {
    for marker in ["```", "~~~"] {
        if let Some(info) = line.strip_prefix(marker) {
            return Some((marker, info.trim()));
        }
    }
    None
}

/// Whether this line ends a block opened with `marker`.
fn closes_fence(line: &str, marker: &str) -> bool {
    let byte = marker.as_bytes()[0];
    let trimmed = line.trim();
    trimmed.len() >= marker.len() && trimmed.bytes().all(|found| found == byte)
}

//! Measure and draw the visible rows of the transcript.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::{app::App, item_lines, notes, scroll, tool_expand, transcript::Item, wrap};

/// The transcript pane: the rows on screen, and the follow-bottom indicator.
///
/// Lines are wrapped here rather than by the widget: a logical line that wraps
/// over the pane width occupies more than one row, so trimming logical lines
/// first would push the newest rows off the bottom of the pane.
///
/// Items are walked from the newest one so the work is bounded by the pane: a
/// turn in flight redraws as its text arrives, and wrapping the whole
/// conversation on every one of those frames would cost more as the session
/// grows.
///
/// A message that is still streaming is drawn first as the newest entry, so
/// text appears while it arrives instead of all at once when the turn ends.
/// It is not also in `items`, and `finish_stream` moves it there exactly once.
///
/// A view held above the end is a different walk: the content is measured once,
/// the rows below it are skipped by whole blocks, and only the block holding the
/// bottom edge of the view is wrapped again. That is what keeps a frame's cost at
/// the visible rows plus the scroll offset rather than the whole conversation.
pub(crate) fn transcript_pane(app: &mut App, width: u16, height: u16) -> Pane {
    // Noticed before anything is drawn: a delta that lands between two frames is
    // what the indicator calls new output, whether or not it added a row.
    let mark = scroll::Mark::new(
        app.transcript.items().len(),
        app.transcript.open_text().map_or(0, str::len),
        app.transcript.open_reasoning().map_or(0, str::len),
    );
    app.scroll.observe(mark);
    // A note hangs under the message its key names, and the keys only matter to
    // a session that has a note: only then are they brought up to date, once,
    // for every walk this frame makes.
    if app
        .session_id
        .as_deref()
        .is_some_and(|session| app.notes.count(session) > 0)
    {
        app.note_keys.refresh(&app.transcript);
    }
    let view = usize::from(height.max(1));
    // The indicator spends the pane's bottom row while it is drawn, so the
    // content is measured against the rows it may actually fill.
    let held_before = app.scroll.offset() > 0;
    let measure_rows = if held_before {
        view.saturating_sub(1).max(1)
    } else {
        view
    };
    if held_before {
        measure_transcript(app, width, measure_rows);
    } else {
        // A frame at the end measures nothing, so it must say so: the next held
        // frame takes its height baseline fresh rather than trusting the last
        // scroll's.
        app.scroll.following_frame();
    }
    let offset = app.scroll.offset();
    let indicator = offset > 0 && view > 1;
    let capacity = if indicator { view - 1 } else { view };
    let rows = if offset > 0 {
        held_rows(app, width, offset, capacity)
    } else {
        follow_rows(app, width, capacity)
    };
    let indicator = if offset > 0 && view > 1 {
        Some(indicator_line(app, offset))
    } else {
        None
    };
    Pane { rows, indicator }
}

/// The rows the pane shows, and the line that says it is not at the end.
pub(crate) struct Pane {
    pub(crate) rows: Vec<Line<'static>>,
    pub(crate) indicator: Option<Line<'static>>,
}

/// One block of the conversation, in the order the pane draws them.
enum Block<'a> {
    Item(&'a Item),
    Reasoning(&'a str),
    Answer(&'a str),
}

/// How many blocks the pane draws: the committed items, then the open buffers.
///
/// Reasoning being assembled sits above the answer it precedes, the same order
/// the model produced it in, and both sit below everything already committed.
fn block_count(items: usize, reasoning: Option<&str>, answer: Option<&str>) -> usize {
    items
        .saturating_add(usize::from(reasoning.is_some()))
        .saturating_add(usize::from(answer.is_some()))
}

/// One block by its place in the pane's order.
///
/// The committed items are read where they already are, so walking the
/// conversation as the pane draws it costs no per-frame copy of the item list;
/// the two open buffers follow them as the newest blocks.
fn block_at<'a>(
    items: &'a [Item],
    reasoning: Option<&'a str>,
    answer: Option<&'a str>,
    index: usize,
) -> Option<Block<'a>> {
    if let Some(item) = items.get(index) {
        return Some(Block::Item(item));
    }
    let mut slot = items.len();
    if let Some(text) = reasoning {
        if index == slot {
            return Some(Block::Reasoning(text));
        }
        slot += 1;
    }
    if let Some(text) = answer
        && index == slot
    {
        return Some(Block::Answer(text));
    }
    None
}

/// One block's lines, ready to be asked for the rows the pane shows.
///
/// The wrapped height is what the scrollback measures, so a block means the same
/// thing here and in `measure_transcript`. `first` is the oldest block, which has
/// nothing above it to be separated from.
///
/// A block is not wrapped here: the pane asks its source for the rows it draws,
/// and a tool call drawing its whole output answers with only the rows near the
/// end instead of a row for every line it kept.
fn block_source<'a>(
    block: &Block<'_>,
    look: &item_lines::Look<'_>,
    expand: Option<&tool_expand::Frame<'a>>,
    first: bool,
    frame: &notes::Frame<'_>,
    index: usize,
) -> wrap::Source<'a> {
    let mut source = match block {
        Block::Item(item) => item_lines::source(item, look, expand),
        Block::Reasoning(text) => wrap::Source::built(item_lines::reasoning(text, look)),
        Block::Answer(text) => wrap::Source::built(item_lines::answer(text, look)),
    };
    // The reader's note hangs under the message it describes, separated by one
    // blank row so it reads as a mark beside the conversation rather than as
    // more of it. It is part of the block, so the height this source reports
    // already includes it and a held view keeps its place.
    if let Some(text) = frame.at(index) {
        source.append(wrap::Indented::plain(Line::default()));
        source.append(item_lines::note(text, look));
    }
    // A turn starts one blank row below the previous one, so a new message is
    // not read as more of the answer above it. It is a line of the block, so a
    // window that starts at the block's first row still counts it.
    if !first && matches!(block, Block::Item(Item::User(_))) {
        source.prepend(wrap::Indented::plain(Line::default()));
    }
    source
}

/// Count the conversation's rows so a held view knows how far it may move.
///
/// The measurement is kept between frames: only items the last one never saw are
/// wrapped, so a view held during a long turn does not re-wrap the conversation.
/// The block still streaming is counted every frame, because it is the one block
/// whose height changes while the view is held.
fn measure_transcript(app: &mut App, width: u16, view: usize) {
    let items = app.transcript.items().len();
    let App {
        scroll,
        transcript,
        theme,
        symbols,
        settings,
        bindings,
        notes: note_store,
        note_keys,
        session_id,
        ..
    } = app;
    let look = item_lines::Look {
        theme,
        symbols: *symbols,
        width,
        markdown: item_lines::markdown_enabled(settings),
        visual_blocks: item_lines::visual_blocks_enabled(settings),
    };
    let open = [
        open_rows(transcript.open_reasoning(), &look, true),
        open_rows(transcript.open_text(), &look, false),
    ];
    // An expanded call is one row taller per row of its whole output, so the
    // height measured here has to know about the expansion: a view held above
    // the end counts this height, and a stale one would leave the offset short.
    let expand = tool_expand::Frame::new(transcript.expand_state(), bindings);
    let frame = notes::Frame::from_parts(note_store, session_id.as_deref(), transcript, note_keys);
    scroll.measure(width, items, open, view, |index| match transcript.items().get(index) {
        // Counted, not built: the rows of a block that stays off screen are not
        // made to measure it.
        Some(item) => block_source(&Block::Item(item), &look, Some(&expand), index == 0, &frame, index).height(width),
        None => 0,
    });
}

/// Wrapped rows of one open buffer, as the pane would draw it.
fn open_rows(text: Option<&str>, look: &item_lines::Look<'_>, reasoning: bool) -> usize {
    match text {
        Some(text) if reasoning => wrap::Source::built(item_lines::reasoning(text, look)).height(look.width),
        Some(text) => wrap::Source::built(item_lines::answer(text, look)).height(look.width),
        None => 0,
    }
}

/// The newest rows, following the end of the conversation.
fn follow_rows(app: &App, width: u16, capacity: usize) -> Vec<Line<'static>> {
    let look = item_lines::Look::new(app, width);
    let expand = tool_expand::Frame::new(app.transcript.expand_state(), &app.bindings);
    let frame = notes::Frame::new(app);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let items = app.transcript.items();
    let reasoning = app.transcript.open_reasoning();
    let answer = app.transcript.open_text();
    let count = block_count(items.len(), reasoning, answer);
    for index in (0..count).rev() {
        if rows.len() >= capacity {
            break;
        }
        let Some(block) = block_at(items, reasoning, answer, index) else {
            continue;
        };
        // Only the rows this block contributes to the pane are built, and they
        // are built from the end: a block taller than the pane costs the pane's
        // rows, not its own.
        let source = block_source(&block, &look, Some(&expand), index == 0, &frame, index);
        let wanted = capacity - rows.len();
        let window = source.rows_from_end(width, wrap::FromEnd::new(0, wanted));
        rows.extend(window.into_iter().rev());
    }
    rows.reverse();
    placeholder(app, rows, width)
}

/// The rows a view held `offset` rows above the end shows.
///
/// Blocks below the view are skipped by their measured height, so the rows the
/// user scrolled past are not wrapped again. Only the block that holds the bottom
/// edge of the view is wrapped, and only for the rows above that edge.
fn held_rows(app: &App, width: u16, offset: usize, capacity: usize) -> Vec<Line<'static>> {
    let look = item_lines::Look::new(app, width);
    let expand = tool_expand::Frame::new(app.transcript.expand_state(), &app.bindings);
    let frame = notes::Frame::new(app);
    let items = app.transcript.items();
    let reasoning = app.transcript.open_reasoning();
    let answer = app.transcript.open_text();
    let count = block_count(items.len(), reasoning, answer);
    let measured = block_heights(app);
    // A measurement taken before the block list changed says nothing about this
    // one, so it is dropped rather than read against the wrong block.
    let heights = (measured.len() == count).then_some(&measured);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut below = offset;
    for index in (0..count).rev() {
        // Wholly below the view: its height is known, so it is skipped without
        // wrapping a single line of it.
        if let Some(height) = heights.and_then(|heights| heights.get(index))
            && below >= height
            && height > 0
        {
            below -= height;
            continue;
        }
        if rows.len() >= capacity {
            break;
        }
        let Some(block) = block_at(items, reasoning, answer, index) else {
            continue;
        };
        let source = block_source(&block, &look, Some(&expand), index == 0, &frame, index);
        let wanted = capacity - rows.len();
        // The rows below the view's bottom edge are counted and dropped; what
        // is built is the window above them that the pane fills with.
        let window = source.rows_from_end(width, wrap::FromEnd::new(below, wanted));
        rows.extend(window.into_iter().rev());
        below = 0;
    }
    rows.reverse();
    rows
}

/// The measured height of every block, in the order the pane draws them.
///
/// The committed items come from the scrollback's tally and are read where they
/// already are, so a held frame does not copy the whole list of counted heights
/// on every step; the two open buffers are blocks only while they are being
/// streamed, and their slots are read after the committed ones so the list stays
/// aligned with [`block_at`].
struct BlockHeights<'a> {
    /// Wrapped rows of each committed item, as the scrollback last counted them.
    items: &'a [usize],
    /// Wrapped rows of the open reasoning block and the open answer.
    open: [usize; 2],
    /// Whether the reasoning buffer is a block of its own.
    reasoning: bool,
    /// Whether the answer buffer is a block of its own.
    answer: bool,
}

impl BlockHeights<'_> {
    /// How many blocks have a measured height.
    fn len(&self) -> usize {
        self.items
            .len()
            .saturating_add(usize::from(self.reasoning))
            .saturating_add(usize::from(self.answer))
    }

    /// The height of the block at `index`, or `None` when this list does not
    /// describe it.
    fn get(&self, index: usize) -> Option<usize> {
        if let Some(height) = self.items.get(index) {
            return Some(*height);
        }
        let mut slot = self.items.len();
        if self.reasoning {
            if index == slot {
                return Some(self.open[0]);
            }
            slot += 1;
        }
        if self.answer && index == slot {
            return Some(self.open[1]);
        }
        None
    }
}

/// The measured height of every block the scrollback knows about.
fn block_heights(app: &App) -> BlockHeights<'_> {
    BlockHeights {
        items: app.scroll.heights().unwrap_or(&[]),
        open: app.scroll.open_heights(),
        reasoning: app.transcript.open_reasoning().is_some(),
        answer: app.transcript.open_text().is_some(),
    }
}

/// The line a held view keeps in the pane's bottom row.
fn indicator_line(app: &App, below: usize) -> Line<'static> {
    let counted = if below == 1 {
        "1 row".to_string()
    } else {
        format!("{below} rows")
    };
    let text = if app.scroll.has_fresh_output() {
        format!("{} {counted} below · new output", app.symbols.more_below)
    } else {
        format!("{} {counted} below", app.symbols.more_below)
    };
    Line::from(Span::styled(text, Style::default().fg(app.theme.core.muted)))
}

/// What an empty transcript says instead of nothing.
fn placeholder(app: &App, rows: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    // A conversation whose only rows are hidden tool calls still has content:
    // asking the reader to send a message would misread what is on screen.
    if !rows.is_empty() || !app.transcript.items().is_empty() {
        return rows;
    }
    wrap::wrap_line(
        &Line::from(Span::styled(
            "Send a message to start. /help lists the commands; /quit leaves.",
            Style::default().fg(app.theme.core.muted),
        )),
        usize::from(width),
    )
}

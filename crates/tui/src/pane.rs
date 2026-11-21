//! Measure and draw the visible rows of the transcript.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::{app::App, item_lines, scroll, tool_expand, transcript::Item, wrap};

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

/// The conversation as the pane draws it, oldest first.
///
/// Reasoning being assembled sits above the answer it precedes, the same order
/// the model produced it in, and both sit below everything already committed.
fn blocks(app: &App) -> Vec<Block<'_>> {
    let mut blocks: Vec<Block<'_>> = app.transcript.items().iter().map(Block::Item).collect();
    if let Some(text) = app.transcript.open_reasoning() {
        blocks.push(Block::Reasoning(text));
    }
    if let Some(text) = app.transcript.open_text() {
        blocks.push(Block::Answer(text));
    }
    blocks
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
) -> wrap::Source<'a> {
    let mut source = match block {
        Block::Item(item) => item_lines::source(item, look, expand),
        Block::Reasoning(text) => wrap::Source::built(item_lines::reasoning(text, look)),
        Block::Answer(text) => wrap::Source::built(item_lines::answer(text, look)),
    };
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
    scroll.measure(width, items, open, view, |index| match transcript.items().get(index) {
        // Counted, not built: the rows of a block that stays off screen are not
        // made to measure it.
        Some(item) => block_source(&Block::Item(item), &look, Some(&expand), index == 0).height(width),
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
    let mut rows: Vec<Line<'static>> = Vec::new();
    let blocks = blocks(app);
    for (index, block) in blocks.iter().enumerate().rev() {
        if rows.len() >= capacity {
            break;
        }
        // Only the rows this block contributes to the pane are built, and they
        // are built from the end: a block taller than the pane costs the pane's
        // rows, not its own.
        let source = block_source(block, &look, Some(&expand), index == 0);
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
    let blocks = blocks(app);
    let measured = block_heights(app);
    // A measurement taken before the block list changed says nothing about this
    // one, so it is dropped rather than read against the wrong block.
    let heights: &[usize] = if measured.len() == blocks.len() { &measured } else { &[] };
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut below = offset;
    for (index, block) in blocks.iter().enumerate().rev() {
        match heights.get(index) {
            // Wholly below the view: its height is known, so it is skipped
            // without wrapping a single line of it.
            Some(&height) if below >= height && height > 0 => {
                below -= height;
                continue;
            }
            _ => {}
        }
        if rows.len() >= capacity {
            break;
        }
        let source = block_source(block, &look, Some(&expand), index == 0);
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
/// The committed items come from the scrollback's tally; the two open buffers
/// are blocks only while they are being streamed, so their slots are added only
/// then, and the list stays aligned with [`blocks`].
fn block_heights(app: &App) -> Vec<usize> {
    let mut heights = match app.scroll.heights() {
        Some(items) => items.to_vec(),
        None => Vec::new(),
    };
    let open = app.scroll.open_heights();
    if app.transcript.open_reasoning().is_some() {
        heights.push(open[0]);
    }
    if app.transcript.open_text().is_some() {
        heights.push(open[1]);
    }
    heights
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

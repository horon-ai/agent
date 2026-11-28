//! Scrollback over the transcript's wrapped rows.
//!
//! The transcript pane follows new output: the newest rows are the ones on
//! screen. Holding the view away from the end keeps those rows still while more
//! arrives, and the pane says how many rows it left below. Every number here
//! counts rows rather than items, because a line wider than the pane occupies
//! more than one row once it is wrapped, and the row the user is reading is a
//! wrapped one.
//!
//! The device owns the arithmetic and the mouse mode; what the rows *are* is the
//! renderer's business. It is handed the wrapped height of each item so a held
//! view can be kept still without wrapping the conversation again: only items
//! the last measurement never saw are measured, and a width change is the one
//! case that recounts everything, because every row boundary moved with it.
//!
//! The offset is still clamped against the height the renderer found, so a
//! shorter transcript, `/clear`, or a narrower pane pulls a held view back into
//! range instead of leaving it pointing past the content.

use std::io::Write;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;

/// Rows one wheel event moves the view.
///
/// One, as a terminal's own scrollback moves: the terminal already sends one
/// event per row a trackpad travels and several per notch of a wheel (its
/// scroll multiplier), so a larger step here would multiply that again and
/// make the view jump where the hand glides.
pub const WHEEL_ROWS: usize = 1;

/// What the content held the last time the pane drew it.
///
/// Counting items and the characters of the two open buffers is enough to notice
/// that output arrived without rendering anything: a new row implies a new item,
/// a longer streamed message, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// Items committed to the transcript.
    pub items: usize,
    /// Characters of the answer still streaming.
    pub answer: usize,
    /// Characters of the reasoning still streaming.
    pub reasoning: usize,
}

impl Mark {
    /// The content as it stands now.
    pub fn new(items: usize, answer: usize, reasoning: usize) -> Self {
        Self {
            items,
            answer,
            reasoning,
        }
    }
}

/// The height of the content, counted once so a held view can stay still.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tally {
    /// The pane width every count was taken at.
    width: u16,
    /// Wrapped rows of each committed item, in display order.
    items: Vec<usize>,
    /// Wrapped rows of the open reasoning block and the open answer.
    open: [usize; 2],
}

/// Where the transcript view sits, and what arrived while it was held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scroll {
    /// Wrapped rows between the bottom of the view and the end of the content.
    offset: usize,
    /// Output arrived while the view was held away from the end.
    fresh: bool,
    /// The row heights the last measurement found.
    tally: Option<Tally>,
    /// The height the last measurement found, for spotting growth.
    height: Option<usize>,
    /// Whether the last frame drawn was a held one.
    ///
    /// A frame that follows the end does not measure the content, so the height
    /// it leaves behind says nothing about how much arrived since. The first
    /// held frame therefore takes its baseline fresh instead of reading the
    /// whole gap as output that landed below a view the user had already left.
    held_frame: bool,
    /// What the content held at the last frame.
    mark: Option<Mark>,
    /// A command asked for the newest item's first row at the top of the view,
    /// and the frame that can measure that item has not run yet.
    ///
    /// Kept here rather than worked out by the caller because only a frame knows
    /// how many rows the item wraps to, and the row count is what the offset is
    /// made of.
    hold_newest: bool,
    /// A block changed height without any output arriving, and the next held
    /// frame has to move the offset with it.
    ///
    /// A rebuild alone skips that move, because a width change recounts rows
    /// rather than adding them. A tool call opened or a tool line hidden is the
    /// other kind of change: the same content, wrapped differently, and the top
    /// row the reader is on must not slide.
    anchor: bool,
    /// The next frame must measure every block again.
    ///
    /// Kept apart from [`anchor`](Self::anchor) because the anchor lasts only
    /// until the next frame, while the recount can be needed even when that
    /// frame follows the end and clears the anchor: the block the caller changed
    /// has to be measured before a held frame reads the tally.
    recount: bool,
}

impl Scroll {
    /// A view following the end of the conversation.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrapped rows between the bottom of the view and the end of the content.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Whether the view sits at the end and follows new output.
    pub fn is_following(&self) -> bool {
        self.offset == 0
    }

    /// Whether output arrived since the view was held away from the end.
    pub fn has_fresh_output(&self) -> bool {
        self.fresh
    }

    /// The row heights of the last measurement, for a pane that has to find the
    /// rows a held view shows without wrapping the conversation again.
    ///
    /// `None` while a recount is pending: the heights describe content the caller
    /// has already changed, and a pane that matched them against the new items
    /// would place the view on the wrong row.
    pub fn heights(&self) -> Option<&[usize]> {
        if self.recount {
            return None;
        }
        self.tally.as_ref().map(|tally| tally.items.as_slice())
    }

    /// The wrapped height of the two open buffers, as last measured.
    pub fn open_heights(&self) -> [usize; 2] {
        if self.recount {
            return [0, 0];
        }
        self.tally.as_ref().map_or([0, 0], |tally| tally.open)
    }

    /// Forget the counted heights.
    ///
    /// Called when something other than the width changes how many rows a block
    /// takes, such as the Markdown renderer being switched off: the next held
    /// frame recounts the conversation rather than moving the view against
    /// heights that no longer describe what is drawn.
    ///
    /// A pending [`hold_newest`](Self::hold_newest) request survives: the rebuild
    /// counts the same newest item, so the frame that recounts is also the frame
    /// that can place it.
    pub fn invalidate(&mut self) {
        self.tally = None;
    }

    /// Recount the content after a block changed height on its own.
    ///
    /// A tool call opened or closed, or the tool lines were hidden: no output
    /// arrived, so the rows below the view did not grow by themselves, but some
    /// block wraps differently now. The next held frame recounts and moves the
    /// offset by the rows the changed blocks added at or below the view's top
    /// edge, which is what keeps the rows on screen where they were instead of
    /// jumping to the end of the conversation.
    pub fn reflow(&mut self) {
        // The tally is kept: the next frame compares it with what it measures
        // to learn how many rows the changed block added above or below the
        // view's top edge.
        self.recount = true;
        self.anchor = true;
    }

    /// Hold the view `rows` higher.
    ///
    /// The caller does not have to know how tall the content is: the next
    /// measurement clamps the offset against the rows it finds, which is also
    /// what makes a page past the first row stop at the first row.
    pub fn up(&mut self, rows: usize) {
        self.offset = self.offset.saturating_add(rows.max(1));
    }

    /// Move the view `rows` back towards the end.
    pub fn down(&mut self, rows: usize) {
        self.offset = self.offset.saturating_sub(rows.max(1));
        if self.offset == 0 {
            self.fresh = false;
            // The view is following again, so a request that never reached a
            // frame is spent: it would otherwise fire on a later item.
            self.hold_newest = false;
        }
    }

    /// Follow the end again.
    pub fn to_bottom(&mut self) {
        self.offset = 0;
        self.fresh = false;
        self.hold_newest = false;
    }

    /// Ask the next frame to put the newest item's first row at the top.
    ///
    /// A listing taller than the pane (`/keys`) is drawn from its end, which
    /// hides the rows a reader looks for first. How many rows the item wraps to
    /// is only known once a frame has measured it, so the request waits for one:
    /// the offset is taken from that measurement, and an item that fits the pane
    /// leaves the view following the end.
    pub fn hold_newest(&mut self) {
        self.hold_newest = true;
        // Only a held frame measures the content, so the request needs one.
        self.up(1);
    }

    /// Pull a held view back into the content.
    ///
    /// Called with the height the pane found and the rows it can show. A view
    /// the content no longer reaches above matches the end again, which is what
    /// keeps a resize from drawing rows that are not there.
    pub fn clamp(&mut self, height: usize, view: usize) {
        self.offset = self.offset.min(height.saturating_sub(view));
        if self.offset == 0 {
            self.fresh = false;
        }
    }

    /// Note that a frame followed the end of the conversation.
    ///
    /// Called for every frame drawn at the bottom. It is what makes the next
    /// held frame start a new baseline: the height measured during an earlier
    /// scroll is stale by however many rows arrived while the view followed.
    pub fn following_frame(&mut self) {
        self.held_frame = false;
        // Nothing measures a frame that follows the end, so a change that
        // asked for an anchored recount has nothing to move against; the next
        // held frame takes its height as a fresh baseline anyway.
        self.anchor = false;
    }

    /// Note what the content holds now.
    ///
    /// Only used for the indicator: a delta that adds characters without adding
    /// a row still arrived, and the held view should say so.
    pub fn observe(&mut self, mark: Mark) {
        if let Some(previous) = self.mark
            && self.offset > 0
            && previous != mark
        {
            self.fresh = true;
        }
        self.mark = Some(mark);
    }

    /// Bring the counted height of the content up to date, and return it.
    ///
    /// `rows_of` answers the wrapped height of one committed item. A held view
    /// is moved by however much the content changed at or below its top edge,
    /// which is what keeps the rows on screen from sliding while output arrives;
    /// a block above that edge moves the content without changing the rows the
    /// view shows, so it does not move the view. Everything already measured is
    /// reused except the newest item and the items the transcript changed in
    /// place (`changed_from` onwards), so the cost of a frame is those items,
    /// the items that are new, and the block still streaming — never the whole
    /// conversation.
    pub fn measure(
        &mut self,
        width: u16,
        items: usize,
        changed_from: Option<usize>,
        open: [usize; 2],
        view: usize,
        rows_of: impl FnMut(usize) -> usize,
    ) -> usize {
        let mut rows_of = rows_of;
        // The first held frame after the view followed takes the height as its
        // baseline: nothing measured it while the view was at the end, so every
        // row added since then would otherwise read as growth below the view.
        let baseline = !self.held_frame;
        // A change the caller made on purpose moves the offset against the
        // height it found, even though the recount below is a rebuild.
        let anchored = std::mem::take(&mut self.anchor);
        let recount = std::mem::take(&mut self.recount);
        let rebuilt = match &self.tally {
            Some(tally) => recount || tally.width != width || tally.items.len() > items,
            None => true,
        };
        // The comparison the anchor makes is against the tally the view was
        // drawn with, so that list is moved out rather than copied: the rebuild
        // below replaces it in the same call, and a clone would copy one row
        // count per item of the conversation on the frame a tool call opened.
        // Only `reflow` sets the anchor, and `reflow` always asks for the
        // rebuild, so an anchored frame is always a rebuilding one.
        let previous_tally = if rebuilt && anchored { self.tally.take() } else { None };
        if rebuilt {
            let mut heights = Vec::with_capacity(items);
            for index in 0..items {
                heights.push(rows_of(index));
            }
            self.tally = Some(Tally {
                width,
                items: heights,
                open,
            });
        }
        let mut above_top = 0i64;
        if !rebuilt && let Some(tally) = self.tally.as_mut() {
            // The newest item is the one that keeps changing: a tool call's
            // input and the output preview that replaces its running line both
            // land on it, so its height is measured again instead of trusted.
            // An older item the transcript changed in place (a result or a
            // preview that landed after newer items) is counted again from
            // there on, or the height falls short and the view cannot reach the
            // first messages.
            let counted = tally.items.len();
            let keep = items
                .saturating_sub(1)
                .min(changed_from.unwrap_or(usize::MAX))
                .min(counted);
            // A change above the view's top edge moves the content without
            // changing the rows on screen, so it is kept out of the offset.
            let top = self
                .height
                .map_or(0, |height| height.saturating_sub(self.offset).saturating_sub(view));
            let mut start = if keep + 1 < counted {
                tally.items[..keep].iter().sum::<usize>()
            } else {
                usize::MAX
            };
            for index in keep..counted {
                let old = tally.items[index];
                let new = rows_of(index);
                let end = start.saturating_add(old);
                if end <= top {
                    above_top = above_top.saturating_add(row_delta(old, new));
                }
                start = end;
                tally.items[index] = new;
            }
            for index in counted..items {
                let height = rows_of(index);
                tally.items.push(height);
            }
            tally.open = open;
        }
        let Some(height) = self
            .tally
            .as_ref()
            .map(|tally| tally.items.iter().sum::<usize>() + tally.open[0] + tally.open[1])
        else {
            return 0;
        };
        let previous = self.height.replace(height);
        // A rebuild recounts the same rows at a new width, which is not output
        // arriving: only a change the caller announced with `reflow` moves the
        // view then. A change there is measured from the view's top edge, so a
        // block above that edge cannot pull the view with it.
        if !baseline
            && let Some(previous_height) = previous
            && height != previous_height
            && self.offset > 0
        {
            let delta = if anchored {
                match (previous_tally.as_ref(), self.tally.as_ref()) {
                    (Some(old), Some(new)) if old.width == width && old.items.len() == new.items.len() => {
                        anchor_delta(old, new, previous_height, self.offset, view)
                    }
                    // A width or item-count change at the same time names no
                    // reliable row; like any resize, it leaves the offset
                    // where it was.
                    _ => 0,
                }
            } else if !rebuilt {
                i64::try_from(height).unwrap_or(i64::MAX)
                    - i64::try_from(previous_height).unwrap_or(i64::MAX)
                    - above_top
            } else {
                0
            };
            // Rows added below the view push the offset down with them, and
            // rows that went away pull it back: either way the rows on screen
            // stay where the user left them.
            if delta > 0 {
                self.offset = self.offset.saturating_add(usize::try_from(delta).unwrap_or(usize::MAX));
                // Only output arriving is worth the "new output" line; a press
                // the reader made is not.
                if !anchored {
                    self.fresh = true;
                }
            } else if delta < 0 {
                self.offset = self
                    .offset
                    .saturating_sub(usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX));
            }
        }
        // `view` here is the rows a held frame can fill, because the frame that
        // measures is held (the request armed one) and the pane spends its last
        // row on the "N rows below" line. The pane's first row is
        // `height - offset - view`, and the newest item's first row is
        // `height - below`, where `below` counts that item and the open
        // reasoning and answer under it. Setting the two equal gives
        // `offset = below - view`. When the item and its open rows fit the full
        // pane (`below <= view + 1`, which is the capacity of a frame that
        // follows the end) nothing has to be hidden, so the view stays at the
        // end and the whole item is on screen.
        if self.hold_newest
            && let Some(tally) = self.tally.as_ref()
        {
            self.hold_newest = false;
            let below = tally
                .items
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(tally.open[0])
                .saturating_add(tally.open[1]);
            self.offset = if below > view.saturating_add(1) {
                below - view
            } else {
                0
            };
            // A listing the user asked for is not output arriving: no "new
            // output" line, whichever way the view lands.
            self.fresh = false;
        }
        self.clamp(height, view);
        self.held_frame = true;
        height
    }
}

/// How many rows a recount added at or below the view's top edge.
///
/// `previous` is the tally the view was drawn against, `height` the content
/// height it had then, and `offset`/`view` the window it showed. A block above
/// the top edge may grow or shrink freely: it moves the rows on screen without
/// changing which rows they are, so it contributes nothing. The block holding
/// the top edge and every block below it do change what the bottom edge is, and
/// the offset moves by their own change. A block that sat entirely above the
/// top edge is skipped by its end row, so hiding or expanding it never drags
/// the reader away from what they were reading.
fn anchor_delta(previous: &Tally, now: &Tally, height: usize, offset: usize, view: usize) -> i64 {
    let top = height.saturating_sub(offset).saturating_sub(view);
    let mut start = 0usize;
    let mut delta = 0i64;
    let mut counting = false;
    for (index, old) in previous.items.iter().enumerate() {
        let end = start.saturating_add(*old);
        if !counting && start < top && end <= top {
            start = end;
            continue;
        }
        counting = true;
        let new = now.items.get(index).copied().unwrap_or(0);
        delta = delta.saturating_add(row_delta(*old, new));
        start = end;
    }
    for (index, old) in previous.open.iter().enumerate() {
        let end = start.saturating_add(*old);
        if !counting && start < top && end <= top {
            start = end;
            continue;
        }
        counting = true;
        let new = now.open.get(index).copied().unwrap_or(0);
        delta = delta.saturating_add(row_delta(*old, new));
        start = end;
    }
    delta
}

/// The signed rows one block gained or lost, for a row total.
fn row_delta(old: usize, new: usize) -> i64 {
    i64::try_from(new).unwrap_or(i64::MAX) - i64::try_from(old).unwrap_or(i64::MAX)
}

/// Ask the terminal to report the wheel and the other mouse events.
///
/// The mode takes the terminal's own text selection with it, which is why it
/// follows a setting rather than being always on. A terminal that does not
/// support it answers nothing, which is not a reason to refuse to start: the
/// pane still scrolls from the keyboard.
pub fn enable_mouse() -> std::io::Result<()> {
    let mut output = std::io::stdout();
    execute!(output, EnableMouseCapture).and_then(|()| output.flush())
}

/// Turn the mouse mode off, and say whether the terminal took the sequence.
///
/// Best effort, like the paste mode: it runs on the exit and panic paths, where
/// the terminal may already be gone and a second error would replace the one the
/// caller is trying to report.
pub fn disable_mouse() -> bool {
    let mut output = std::io::stdout();
    let written = execute!(output, DisableMouseCapture).is_ok();
    written && output.flush().is_ok()
}

/// Turn the mouse mode off before a panic unwinds, then let the installed hook
/// run.
///
/// Installed after the paste guard, so this hook chains the one that turns the
/// paste mode off, which in turn chains the one `try_init` installed to restore
/// the screen: a panic leaves neither mode on for the next program.
pub fn guard_panics() {
    let restoring = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        disable_mouse();
        restoring(info);
    }));
}

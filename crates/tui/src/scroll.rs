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

/// Rows one wheel notch moves the view.
pub const WHEEL_ROWS: usize = 3;

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
    pub fn heights(&self) -> Option<&[usize]> {
        self.tally.as_ref().map(|tally| tally.items.as_slice())
    }

    /// The wrapped height of the two open buffers, as last measured.
    pub fn open_heights(&self) -> [usize; 2] {
        self.tally.as_ref().map_or([0, 0], |tally| tally.open)
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
        }
    }

    /// Follow the end again.
    pub fn to_bottom(&mut self) {
        self.offset = 0;
        self.fresh = false;
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
    /// is moved by however much the content changed below it, which is what
    /// keeps the rows on screen from sliding while output arrives. Everything
    /// already measured is reused except the newest item, so the cost of a frame
    /// is that item, the items that are new, and the block still streaming —
    /// never the whole conversation.
    pub fn measure(
        &mut self,
        width: u16,
        items: usize,
        open: [usize; 2],
        view: usize,
        rows_of: impl FnMut(usize) -> usize,
    ) -> usize {
        let mut rows_of = rows_of;
        // The first held frame after the view followed takes the height as its
        // baseline: nothing measured it while the view was at the end, so every
        // row added since then would otherwise read as growth below the view.
        let baseline = !self.held_frame;
        let rebuilt = match &self.tally {
            Some(tally) => tally.width != width || tally.items.len() > items,
            None => true,
        };
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
        } else if let Some(tally) = self.tally.as_mut() {
            // The newest item is the one that keeps changing: a tool call's
            // input and the output preview that replaces its running line both
            // land on it, so its height is measured again instead of trusted.
            tally.items.truncate(items.saturating_sub(1));
            for index in tally.items.len()..items {
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
        // arriving: only a height that grew across two measurements of the same
        // width moves the offset.
        if !rebuilt
            && !baseline
            && let Some(previous) = previous
            && height != previous
            && self.offset > 0
        {
            // Rows that arrived below the view push the offset down with them,
            // and rows that went away pull it back: either way the rows on
            // screen stay where the user left them.
            if height > previous {
                self.offset = self.offset.saturating_add(height - previous);
                self.fresh = true;
            } else {
                self.offset = self.offset.saturating_sub(previous - height);
            }
        }
        self.clamp(height, view);
        self.held_frame = true;
        height
    }
}

/// Ask the terminal to report the wheel and the other mouse events.
///
/// The mode takes the terminal's own text selection with it, which is why it
/// follows a setting rather than being always on. A terminal that does not
/// support it answers nothing, which is not a reason to refuse to start: the
/// pane still scrolls from the keyboard.
pub fn enable_mouse() -> Result<(), String> {
    let mut output = std::io::stdout();
    execute!(output, EnableMouseCapture)
        .and_then(|()| output.flush())
        .map_err(|error| error.to_string())
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

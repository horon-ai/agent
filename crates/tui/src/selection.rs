//! Selecting conversation text with the mouse, and copying it on request.
//!
//! The client holds the terminal's mouse capture so the wheel scrolls the
//! transcript, and while it does the terminal's own drag selection never sees
//! the drag. The client therefore selects itself: a left-button drag marks a
//! range and the frame paints it reversed. Nothing is copied when the button
//! goes up; `ctrl+c` (or `cmd+c` where the terminal reports it) copies the
//! range and clears it.
//!
//! A point is kept in conversation rows, counted up from the last row, not in
//! screen cells: scrolling moves the rows under the pointer, and the range
//! stays on the text it covered. Dragging past the top or the bottom edge of
//! the pane scrolls it and keeps extending the range, and the copy reads rows
//! that have scrolled off screen as well as those on it.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;

/// One end of a selection: a column, and a conversation row counted from the
/// last one (0 is the newest row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub column: u16,
    pub distance: usize,
}

/// Which pane edge a drag has run past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Above,
    Below,
}

/// Where the conversation rows sat on the last frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Geometry {
    /// Screen row of the pane's first row.
    pub top: u16,
    /// Conversation rows drawn, from `top` down.
    pub rows: usize,
    /// Columns the rows were wrapped to.
    pub width: u16,
    /// Rows below the bottom one drawn (the scroll offset).
    pub offset: usize,
}

impl Geometry {
    /// The point under screen cell (`column`, `row`), and the edge the pointer
    /// has run past when it is outside the rows. A point past an edge is the
    /// nearest drawn row's end, so the range reaches the edge of what is shown.
    pub fn locate(&self, column: u16, row: u16) -> (Point, Option<Edge>) {
        let last = self.width.saturating_sub(1);
        if self.rows == 0 {
            return (
                Point {
                    column: 0,
                    distance: self.offset,
                },
                None,
            );
        }
        if row < self.top {
            let point = Point {
                column: 0,
                distance: self.offset + self.rows - 1,
            };
            return (point, Some(Edge::Above));
        }
        let index = usize::from(row - self.top);
        if index >= self.rows {
            let point = Point {
                column: last,
                distance: self.offset,
            };
            return (point, Some(Edge::Below));
        }
        let point = Point {
            column: column.min(last),
            distance: self.offset + self.rows - 1 - index,
        };
        (point, None)
    }

    /// The screen row conversation row `distance` is drawn on, if it is drawn.
    fn screen_row(&self, distance: usize) -> Option<u16> {
        let from_bottom = distance.checked_sub(self.offset)?;
        if from_bottom >= self.rows {
            return None;
        }
        let index = u16::try_from(self.rows - 1 - from_bottom).unwrap_or(u16::MAX);
        Some(self.top.saturating_add(index))
    }
}

/// One mouse selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Where the button went down.
    anchor: Point,
    /// Where the pointer is now.
    head: Point,
    /// The button is still held.
    dragging: bool,
    /// The pane edge the held pointer is past, which keeps scrolling.
    edge: Option<Edge>,
}

impl Selection {
    /// A selection that starts and ends at `point`.
    pub fn start(point: Point) -> Self {
        Self {
            anchor: point,
            head: point,
            dragging: true,
            edge: None,
        }
    }

    /// Follow the pointer while the button is held.
    pub fn extend(&mut self, point: Point, edge: Option<Edge>) {
        if self.dragging {
            self.head = point;
            self.edge = edge;
        }
    }

    /// Let the button go, and say whether the selection covers more than the
    /// cell it started on; a plain click selects nothing.
    pub fn finish(&mut self) -> bool {
        self.dragging = false;
        self.edge = None;
        !self.is_empty()
    }

    /// Whether anything beyond one cell is selected.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The edge a held drag is past, which the loop scrolls toward.
    pub fn scrolling(&self) -> Option<Edge> {
        self.edge.filter(|_| self.dragging)
    }

    /// The first and last point, in reading order: the older row first.
    fn ordered(&self) -> (Point, Point) {
        let (a, h) = (self.anchor, self.head);
        if (std::cmp::Reverse(a.distance), a.column) <= (std::cmp::Reverse(h.distance), h.column) {
            (a, h)
        } else {
            (h, a)
        }
    }

    /// The columns selected on conversation row `distance`, inclusive.
    fn columns(&self, distance: usize, width: u16) -> Option<(u16, u16)> {
        let (first, last) = self.ordered();
        if distance > first.distance || distance < last.distance {
            return None;
        }
        let from = if distance == first.distance { first.column } else { 0 };
        let to = if distance == last.distance {
            last.column
        } else {
            width.saturating_sub(1)
        };
        (from <= to).then_some((from, to))
    }

    /// Paint the rows of the selection that are on screen reversed.
    ///
    /// A held drag past an edge is pinned to the edge row first, so the range
    /// grows with the rows the scroll brings in.
    pub fn paint(&mut self, buffer: &mut Buffer, geometry: &Geometry) {
        if let Some(edge) = self.scrolling()
            && geometry.rows > 0
        {
            self.head = match edge {
                Edge::Above => Point {
                    column: 0,
                    distance: geometry.offset + geometry.rows - 1,
                },
                Edge::Below => Point {
                    column: geometry.width.saturating_sub(1),
                    distance: geometry.offset,
                },
            };
        }
        if self.is_empty() {
            return;
        }
        let area = buffer.area;
        let (first, last) = self.ordered();
        for distance in last.distance..=first.distance {
            let Some(row) = geometry.screen_row(distance) else {
                continue;
            };
            let Some((from, to)) = self.columns(distance, geometry.width) else {
                continue;
            };
            if row >= area.bottom() {
                continue;
            }
            let to = to.min(area.right().saturating_sub(1));
            for column in from..=to {
                buffer[(column, row)].modifier.insert(Modifier::REVERSED);
            }
        }
    }

    /// The selected text, from the conversation rows the range covers.
    ///
    /// `rows` are the rows ending at the range's newest row, oldest first, as
    /// the pane would draw them; there may be fewer when the range reaches the
    /// first row of the conversation.
    pub fn text(&self, rows: &[Line<'static>], width: u16) -> String {
        let (_, last) = self.ordered();
        let mut lines = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let distance = last.distance + (rows.len() - 1 - index);
            let Some((from, to)) = self.columns(distance, width) else {
                continue;
            };
            lines.push(slice_columns(row, from, to));
        }
        lines.join("\n")
    }
}

/// The characters of `row` in columns `from..=to`, padding trimmed.
fn slice_columns(row: &Line<'static>, from: u16, to: u16) -> String {
    let mut out = String::new();
    let mut column = 0usize;
    for span in &row.spans {
        for character in span.content.chars() {
            let width = UnicodeWidthChar::width(character).unwrap_or(0);
            if column >= usize::from(from) && column <= usize::from(to) {
                out.push(character);
            }
            column += width;
        }
    }
    out.trim_end().to_string()
}

impl crate::app::App {
    /// Scroll toward the edge a held drag is past, one row per pass of the
    /// loop. Answers whether the view moved, so a frame is drawn for it.
    pub(crate) fn drag_autoscroll(&mut self) -> bool {
        match self.selection.as_ref().and_then(Selection::scrolling) {
            Some(Edge::Above) => {
                self.scroll.up(1);
                true
            }
            Some(Edge::Below) if !self.scroll.is_following() => {
                self.scroll.down(1);
                true
            }
            _ => false,
        }
    }

    /// Copy the selection, including rows scrolled off screen, and clear it.
    /// Answers whether there was a selection to copy.
    pub(crate) fn copy_mouse_selection(&mut self) -> bool {
        let Some(selection) = self.selection.take().filter(|selection| !selection.is_empty()) else {
            return false;
        };
        let (first, last) = selection.ordered();
        let width = self.pane.width;
        let count = first.distance - last.distance + 1;
        let rows = crate::pane::rows_ending_at(self, width, last.distance, count);
        let text = selection.text(&rows, width);
        self.request_copy("the selection", text);
        true
    }
}

/// Paint the app's selection over the finished frame.
pub(crate) fn paint(frame: &mut ratatui::Frame<'_>, app: &mut crate::app::App) {
    let geometry = app.pane;
    if let Some(selection) = app.selection.as_mut() {
        selection.paint(frame.buffer_mut(), &geometry);
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;

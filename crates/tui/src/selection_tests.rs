use super::*;
use ratatui::layout::Rect;
use ratatui::text::Line;

/// Five conversation rows drawn from screen row 1, scrolled `offset` rows up.
fn geometry(offset: usize) -> Geometry {
    Geometry {
        top: 1,
        rows: 5,
        width: 12,
        offset,
    }
}

#[test]
fn a_range_stays_on_its_text_when_the_view_scrolls() {
    let mut selection = Selection::start(geometry(0).locate(2, 2).0);
    let (point, edge) = geometry(0).locate(4, 4);
    selection.extend(point, edge);
    assert!(selection.finish());

    // One row up: the same conversation rows are now one screen row lower.
    let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 7));
    selection.paint(&mut buffer, &geometry(1));
    assert!(buffer[(2, 3)].modifier.contains(Modifier::REVERSED));
    assert!(buffer[(4, 5)].modifier.contains(Modifier::REVERSED));
    assert!(!buffer[(2, 2)].modifier.contains(Modifier::REVERSED));
    assert!(!buffer[(5, 5)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn a_drag_past_an_edge_scrolls_and_keeps_extending() {
    let mut selection = Selection::start(geometry(0).locate(3, 5).0);
    let (point, edge) = geometry(0).locate(0, 0);
    assert_eq!(edge, Some(Edge::Above));
    selection.extend(point, edge);
    assert_eq!(selection.scrolling(), Some(Edge::Above));
    // The loop scrolled three rows; the head follows to the new top row.
    let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 7));
    selection.paint(&mut buffer, &geometry(3));
    let (first, last) = selection.ordered();
    assert_eq!(first.distance, 7, "the range reaches the row now at the top");
    assert_eq!(last.distance, 0);
    assert!(selection.finish());
    assert_eq!(selection.scrolling(), None);
}

#[test]
fn the_copy_reads_rows_off_screen_and_cuts_the_end_columns() {
    // Rows 2..=0 from the end, oldest first.
    let mut selection = Selection::start(Point { column: 6, distance: 2 });
    selection.extend(Point { column: 2, distance: 0 }, None);
    selection.finish();
    let rows = [
        Line::from("hello world"),
        Line::from("한글 second"),
        Line::from("third"),
    ];
    assert_eq!(selection.text(&rows, 12), "world\n한글 second\nthi");
}

#[test]
fn a_click_selects_nothing() {
    let mut selection = Selection::start(geometry(0).locate(1, 1).0);
    assert!(!selection.finish());
}

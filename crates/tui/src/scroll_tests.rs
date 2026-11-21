//! Scrollback: the offset arithmetic, and the rows a held view draws.
//!
//! The unit tests drive [`Scroll`] with made-up row heights, so the clamping and
//! growth rules are checked without a terminal. The rest draw into a
//! `TestBackend` and read the screen, because "the rows did not move" is a
//! statement about what was drawn.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use horon_agent_sdk::api::ApiEvent;
use ratatui::backend::TestBackend;

use super::*;
use crate::app::Action;
use crate::prompts::Prompt;
use crate::scroll::{self, Mark, Scroll};

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    App::new("test socket".to_string(), receiver)
}

/// An app whose transcript holds `count` one-row notices.
fn app_with_lines(count: usize) -> App {
    let mut app = app();
    for index in 0..count {
        app.transcript.push_notice(format!("line {index:04}"));
    }
    app
}

/// An app whose every line wraps once at 60 columns and not at 100.
fn app_with_wrapping_lines(count: usize) -> App {
    let mut app = app();
    for index in 0..count {
        app.transcript
            .push_notice(format!("line {index:04} {}", "w".repeat(90)));
    }
    app
}

fn draw_at(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

/// The first `count` rows of a drawn screen, where the transcript sits.
fn top_rows(screen: &str, count: usize) -> String {
    screen.lines().take(count).collect::<Vec<_>>().join("\n")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shifted(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

fn control(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn wheel(kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }
}

fn text_event(text: &str, message_id: &str) -> ApiEvent {
    ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: text.into(),
        message_id: Some(message_id.into()),
    }
}

#[test]
fn scroll_holds_the_view_and_gives_it_back() {
    let mut scroll = Scroll::new();
    assert!(scroll.is_following());
    scroll.up(10);
    assert_eq!(scroll.offset(), 10);
    scroll.down(4);
    assert_eq!(scroll.offset(), 6);
    // Past the end is the end, not a negative offset.
    scroll.down(60);
    assert!(scroll.is_following());
    scroll.up(3);
    scroll.to_bottom();
    assert!(scroll.is_following());
    // A zero step still moves: a binding that reached here asked for one row.
    scroll.up(0);
    assert_eq!(scroll.offset(), 1);
}

#[test]
fn scroll_clamps_against_the_rows_it_finds() {
    let mut scroll = Scroll::new();
    scroll.up(50);
    // Twelve rows in a ten-row pane: the view can only be two rows above the end.
    assert_eq!(scroll.measure(80, 12, [0, 0], 10, |_| 1), 12);
    assert_eq!(scroll.offset(), 2);

    // Content that fits the pane has nothing above it to show.
    let mut fits = Scroll::new();
    fits.up(4);
    assert_eq!(fits.measure(80, 5, [0, 0], 10, |_| 1), 5);
    assert!(fits.is_following());
}

#[test]
fn scroll_reuses_the_rows_it_already_counted() {
    let mut counted = 0usize;
    let mut scroll = Scroll::new();
    scroll.up(2);
    scroll.measure(80, 4, [0, 0], 3, |_| {
        counted += 1;
        2
    });
    assert_eq!(counted, 4);
    // The newest item is measured again, because a running tool call keeps
    // changing it; the three before it are not wrapped a second time.
    scroll.measure(80, 4, [0, 0], 3, |_| {
        counted += 1;
        2
    });
    assert_eq!(counted, 5, "a frame re-wrapped the conversation it had measured");
    scroll.measure(80, 5, [0, 0], 3, |_| {
        counted += 1;
        2
    });
    assert_eq!(
        counted, 6,
        "the item that arrived was measured, and the newest one again"
    );
    // A width change moves every row boundary, so everything is counted again.
    scroll.measure(40, 5, [0, 0], 3, |_| {
        counted += 1;
        2
    });
    assert_eq!(counted, 11);
}

#[test]
fn a_shrinking_block_below_the_view_pulls_the_offset_back() {
    let mut scroll = Scroll::new();
    scroll.up(2);
    // A running tool line is replaced by a shorter summary: two rows went away
    // below the view, so the offset follows them and the view stays put.
    assert_eq!(scroll.measure(80, 4, [0, 0], 3, |_| 2), 8);
    scroll.up(1);
    assert_eq!(
        scroll.measure(80, 4, [0, 0], 3, |index| if index == 3 { 0 } else { 2 }),
        6
    );
    // Three rows below the view, two of them gone: one row is still below, so
    // the rows on screen are the ones the user was reading.
    assert_eq!(scroll.offset(), 1);
}

#[test]
fn scroll_only_follows_growth_while_the_view_is_held() {
    let mut scroll = Scroll::new();
    scroll.measure(80, 2, [0, 0], 3, |_| 2);
    // Two more rows arrived while the view was at the end: it stays at the end.
    scroll.measure(80, 3, [0, 0], 3, |_| 2);
    assert!(scroll.is_following(), "growth moved a view that was at the end");

    scroll.up(1);
    // Two rows arrived below the held view: the offset follows them, so the rows
    // on screen stay where they were.
    assert_eq!(scroll.measure(80, 4, [0, 0], 3, |_| 2), 8);
    assert_eq!(scroll.offset(), 3);
    assert!(scroll.has_fresh_output());
}

#[test]
fn a_hold_request_starts_the_newest_item_at_the_top() {
    let mut scroll = Scroll::new();
    // Three items of one row, one row and ten rows; a held frame fills six of
    // the pane's seven. The newest item's first row is row two of twelve, and a
    // view that starts there has four rows below its bottom edge.
    scroll.hold_newest();
    assert_eq!(
        scroll.measure(80, 3, [0, 0], 6, |index| if index == 2 { 10 } else { 1 }),
        12
    );
    assert_eq!(scroll.offset(), 4);
    assert!(!scroll.is_following());
    assert!(
        !scroll.has_fresh_output(),
        "a listing the user asked for is not output arriving"
    );
}

#[test]
fn a_hold_request_for_a_list_that_fits_leaves_the_view_following() {
    let mut scroll = Scroll::new();
    // Five rows in the six a held frame fills: the whole item is on screen, so
    // nothing is held and the next item still arrives in view.
    scroll.hold_newest();
    assert_eq!(
        scroll.measure(80, 2, [0, 0], 6, |index| if index == 1 { 4 } else { 1 }),
        5
    );
    assert!(scroll.is_following());
    assert!(!scroll.has_fresh_output());
}

#[test]
fn a_hold_request_counts_the_open_rows_below_the_item() {
    let mut scroll = Scroll::new();
    // Four rows of listing with a three-row and a two-row open buffer under it:
    // nine rows from the item's first row, so the offset is nine minus six.
    // Counting the item alone would leave the view following.
    scroll.hold_newest();
    assert_eq!(
        scroll.measure(80, 2, [3, 2], 6, |index| if index == 1 { 4 } else { 1 }),
        10
    );
    assert_eq!(scroll.offset(), 3);
}

#[test]
fn a_hold_request_is_spent_by_the_frame_that_measures_it() {
    let mut scroll = Scroll::new();
    scroll.hold_newest();
    scroll.measure(80, 3, [0, 0], 6, |index| if index == 2 { 10 } else { 1 });
    assert_eq!(scroll.offset(), 4);
    // One more one-row line: a held view keeps the rows it shows, so the offset
    // grows by the row that landed. A request still pending would instead jump
    // to the new line's first row, which is the end of the content.
    assert_eq!(scroll.measure(80, 4, [0, 0], 6, |_| 1), 13);
    assert_eq!(scroll.offset(), 5);
}

#[test]
fn a_hold_request_recounts_rows_measured_at_another_width() {
    let mut scroll = Scroll::new();
    // A frame at eighty columns counts the conversation; the request's frame
    // draws at forty, where every row boundary moved, so it counts them again
    // instead of placing the view against the wider counts.
    scroll.up(1);
    scroll.measure(80, 2, [0, 0], 6, |index| if index == 1 { 4 } else { 1 });
    let mut counted = 0usize;
    scroll.hold_newest();
    let height = scroll.measure(40, 2, [0, 0], 6, |index| {
        counted += 1;
        if index == 1 { 12 } else { 2 }
    });
    assert_eq!(height, 14);
    assert_eq!(counted, 2, "the narrower frame reused rows counted at eighty columns");
    assert_eq!(scroll.offset(), 6);
}

#[test]
fn going_back_to_the_end_spends_a_pending_hold_request() {
    let mut scroll = Scroll::new();
    scroll.hold_newest();
    // ctrl+end before the frame: the request was for a view the user has left,
    // so the next held frame scrolls like any other.
    scroll.to_bottom();
    scroll.up(2);
    assert_eq!(scroll.measure(80, 10, [0, 0], 6, |_| 1), 10);
    assert_eq!(scroll.offset(), 2);
}

#[test]
fn scrolling_back_to_the_end_spends_a_pending_hold_request() {
    let mut scroll = Scroll::new();
    scroll.hold_newest();
    // The key that brings the view to the end also spends the request; a
    // pending one would otherwise fire on whatever the newest item is by then.
    scroll.down(10);
    assert!(scroll.is_following());
    assert_eq!(
        scroll.measure(80, 10, [0, 0], 6, |index| if index == 9 { 10 } else { 1 }),
        19
    );
    assert!(
        scroll.is_following(),
        "a request spent before its frame still held the view"
    );
}

#[test]
fn leaving_the_bottom_drops_the_stale_height() {
    let mut scroll = Scroll::new();
    // A held frame, then back to the end: the height that frame measured is the
    // only baseline there is, and it goes stale the moment the view follows.
    scroll.up(2);
    scroll.measure(80, 2, [0, 0], 3, |_| 2);
    scroll.to_bottom();
    scroll.following_frame();

    // Sixteen rows landed while the view followed, then a page key.
    scroll.up(9);
    assert_eq!(scroll.measure(80, 10, [0, 0], 3, |_| 2), 20);
    assert_eq!(
        scroll.offset(),
        9,
        "the offset took the rows that arrived while following"
    );
    assert!(
        !scroll.has_fresh_output(),
        "output that landed before the view was held is not new output"
    );
}

#[test]
fn one_row_from_the_bottom_after_growth_moves_one_row() {
    let mut scroll = Scroll::new();
    scroll.up(1);
    scroll.measure(80, 1, [0, 0], 3, |_| 1);
    scroll.to_bottom();
    scroll.following_frame();

    // Thirty rows arrived while the view followed; one row up must move one row.
    scroll.up(1);
    assert_eq!(scroll.measure(80, 30, [0, 0], 3, |_| 1), 30);
    assert_eq!(scroll.offset(), 1);
    assert!(!scroll.has_fresh_output());
}

#[test]
fn scroll_marks_output_that_adds_no_row() {
    let mut scroll = Scroll::new();
    scroll.observe(Mark::new(1, 0, 0));
    assert!(!scroll.has_fresh_output());
    scroll.up(2);
    scroll.observe(Mark::new(1, 4, 0));
    assert!(
        scroll.has_fresh_output(),
        "a delta that added characters is still output"
    );
    scroll.to_bottom();
    assert!(!scroll.has_fresh_output());
}

#[test]
fn page_up_shows_earlier_rows_and_counts_what_it_left() {
    let mut app = app_with_lines(200);
    let bottom = draw_at(&mut app, 40, 12);
    assert!(bottom.contains("line 0199"), "{bottom}");

    app.on_key(key(KeyCode::PageUp));
    let held = draw_at(&mut app, 40, 12);
    assert!(
        held.contains("line 0188"),
        "the page did not reach earlier rows:\n{held}"
    );
    assert!(
        !held.contains("line 0199"),
        "the newest row stayed on screen after a page up:\n{held}"
    );
    // Ten pane rows, one of them the indicator: a page is the nine left.
    assert!(held.contains("9 rows below"), "no follow indicator:\n{held}");

    app.on_key(key(KeyCode::PageDown));
    app.on_key(key(KeyCode::PageDown));
    let back = draw_at(&mut app, 40, 12);
    assert!(back.contains("line 0199"), "{back}");
    assert!(
        !back.contains("rows below"),
        "the indicator outlived the scroll:\n{back}"
    );
}

#[test]
fn page_up_past_the_first_row_stops_there() {
    let mut app = app_with_lines(40);
    draw_at(&mut app, 40, 12);
    for _ in 0..10 {
        app.on_key(key(KeyCode::PageUp));
    }
    let screen = draw_at(&mut app, 40, 12);
    assert!(screen.contains("line 0000"), "the first row left the pane:\n{screen}");
    assert!(screen.contains("rows below"), "{screen}");
    let offset = app.scroll.offset();
    draw_at(&mut app, 40, 12);
    assert_eq!(app.scroll.offset(), offset, "the clamp drifted between frames");
    assert_eq!(offset, 40 - 9, "the view did not stop at the first row");
}

#[test]
fn new_output_does_not_move_a_held_view() {
    let mut app = app_with_lines(60);
    draw_at(&mut app, 40, 12);
    app.on_key(key(KeyCode::PageUp));
    let before = draw_at(&mut app, 40, 12);
    assert!(!before.contains("new output"), "{before}");

    app.apply_event(&text_event(&"fresh output\n".repeat(20), "m1"));
    let after = draw_at(&mut app, 40, 12);
    assert_eq!(
        top_rows(&before, 8),
        top_rows(&after, 8),
        "the rows on screen moved when output arrived"
    );
    assert!(
        after.contains("new output"),
        "the indicator missed the arrival:\n{after}"
    );

    // Back at the end, the new rows are the ones on screen.
    app.on_key(control(KeyCode::End));
    let end = draw_at(&mut app, 40, 12);
    assert!(end.contains("fresh output"), "{end}");
    assert!(!end.contains("rows below"), "{end}");
}

#[test]
fn a_page_after_output_arrived_while_following_moves_one_page() {
    let mut app = app_with_lines(20);
    draw_at(&mut app, 40, 12);
    // Hold once and come back, so a height measured during a scroll exists.
    app.on_key(key(KeyCode::PageUp));
    draw_at(&mut app, 40, 12);
    app.on_key(control(KeyCode::End));
    draw_at(&mut app, 40, 12);
    // Forty rows arrive while the view follows the end, frame after frame.
    for index in 20..60 {
        app.transcript.push_notice(format!("line {index:04}"));
    }
    let bottom = draw_at(&mut app, 40, 12);
    assert!(bottom.contains("line 0059"), "{bottom}");

    // One page is nine rows: the pane is ten rows and the indicator keeps one.
    app.on_key(key(KeyCode::PageUp));
    let screen = draw_at(&mut app, 40, 12);
    assert_eq!(
        app.scroll.offset(),
        9,
        "the page overshot by the rows that arrived while the view followed"
    );
    assert!(screen.contains("line 0050"), "{screen}");
    assert!(screen.contains("9 rows below"), "{screen}");
    assert!(
        !screen.contains("new output"),
        "nothing arrived after the view was held:\n{screen}"
    );
}

#[test]
fn a_line_shift_after_output_arrived_while_following_moves_one_row() {
    let mut app = app_with_lines(20);
    draw_at(&mut app, 40, 12);
    app.on_key(shifted(KeyCode::Up));
    draw_at(&mut app, 40, 12);
    app.on_key(control(KeyCode::End));
    draw_at(&mut app, 40, 12);
    for index in 20..60 {
        app.transcript.push_notice(format!("line {index:04}"));
    }
    draw_at(&mut app, 40, 12);

    app.on_key(shifted(KeyCode::Up));
    let screen = draw_at(&mut app, 40, 12);
    assert_eq!(
        app.scroll.offset(),
        1,
        "shift+up took the rows that arrived while following"
    );
    assert!(screen.contains("1 row below"), "{screen}");
    assert!(!screen.contains("new output"), "{screen}");
}

#[test]
fn output_that_arrives_after_the_view_is_held_still_moves_the_offset() {
    let mut app = app_with_lines(60);
    draw_at(&mut app, 40, 12);
    app.on_key(key(KeyCode::PageUp));
    let held = draw_at(&mut app, 40, 12);
    assert!(held.contains("9 rows below"), "{held}");

    // Three rows arrive below the held view: the offset follows them, so the
    // same rows stay on screen, and the indicator calls them new.
    for index in 60..63 {
        app.transcript.push_notice(format!("line {index:04}"));
    }
    let after = draw_at(&mut app, 40, 12);
    assert_eq!(app.scroll.offset(), 12);
    assert!(after.contains("12 rows below · new output"), "{after}");
    assert_eq!(top_rows(&held, 8), top_rows(&after, 8));
}

#[test]
fn an_empty_transcript_ignores_the_scroll_keys() {
    let mut app = app();
    let before = draw_at(&mut app, 40, 12);
    assert!(before.contains("Send a message to start"), "{before}");
    app.on_key(key(KeyCode::PageUp));
    app.on_key(shifted(KeyCode::Up));
    app.on_key(control(KeyCode::End));
    let after = draw_at(&mut app, 40, 12);
    assert_eq!(before, after, "an empty transcript changed under a scroll key");
    assert!(app.scroll.is_following());
}

#[test]
fn a_transcript_shorter_than_the_pane_never_scrolls() {
    let mut app = app_with_lines(3);
    let before = draw_at(&mut app, 40, 12);
    app.on_key(key(KeyCode::PageUp));
    let after = draw_at(&mut app, 40, 12);
    assert_eq!(before, after);
    assert!(app.scroll.is_following());
}

#[test]
fn a_resize_pulls_a_held_view_into_the_content() {
    let mut app = app_with_wrapping_lines(120);
    draw_at(&mut app, 100, 30);
    for _ in 0..4 {
        app.on_key(key(KeyCode::PageUp));
    }
    let wide = draw_at(&mut app, 100, 30);
    assert!(app.scroll.offset() > 0, "{wide}");

    // Every row now wraps in two, so the content grew under the held view.
    let narrow = draw_at(&mut app, 60, 10);
    assert!(app.scroll.offset() > 0, "{narrow}");
    assert!(narrow.contains("rows below"), "{narrow}");

    // A pane taller than the whole conversation has nothing above it to hold.
    let tall = draw_at(&mut app, 60, 400);
    assert!(app.scroll.is_following(), "{tall}");
    assert!(!tall.contains("rows below"), "{tall}");
}

#[test]
fn shift_keys_scroll_without_touching_the_draft() {
    let mut app = app_with_lines(60);
    app.on_paste("first line\nsecond line");
    draw_at(&mut app, 40, 12);
    app.on_key(shifted(KeyCode::Up));
    assert_eq!(app.input.text(), "first line\nsecond line", "the draft moved");
    assert_eq!(app.scroll.offset(), 1);
    app.on_key(shifted(KeyCode::Down));
    assert!(app.scroll.is_following());
    // A plain arrow is still the draft's.
    app.on_key(key(KeyCode::Up));
    assert!(app.scroll.is_following(), "a plain up scrolled the transcript");
}

#[test]
fn the_scroll_keys_work_while_a_prompt_waits() {
    let mut app = app_with_lines(60);
    draw_at(&mut app, 40, 12);
    app.prompt = Some(Prompt::approval("r1".into(), "bash".into()));
    app.on_key(key(KeyCode::PageUp));
    assert!(app.prompt.is_some(), "a scroll key answered the prompt");
    assert!(app.scroll.offset() > 0, "the transcript refused to scroll");

    // The prompt keeps its own keys, and the scroll key left nothing behind.
    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(Action::Approval {
            request_id: "r1".into(),
            approved: true,
            always: false
        })
    );
    assert!(app.prompt.is_none());
}

#[test]
fn the_wheel_scrolls_only_when_the_setting_holds_the_capture() {
    let mut app = app_with_lines(60);
    draw_at(&mut app, 40, 12);
    app.on_mouse(wheel(MouseEventKind::ScrollUp));
    assert!(
        app.scroll.is_following(),
        "a wheel event scrolled a terminal that never captured the mouse"
    );

    app.mouse_scroll = true;
    app.on_mouse(wheel(MouseEventKind::ScrollUp));
    assert_eq!(app.scroll.offset(), scroll::WHEEL_ROWS);

    // The setting going off drops the events a captured terminal still sends.
    app.mouse_scroll = false;
    app.on_mouse(wheel(MouseEventKind::ScrollUp));
    assert_eq!(app.scroll.offset(), scroll::WHEEL_ROWS);
    app.on_mouse(wheel(MouseEventKind::ScrollDown));
    assert_eq!(app.scroll.offset(), scroll::WHEEL_ROWS);
}

#[test]
fn the_settings_screen_keeps_the_wheel_and_the_keys() {
    let mut app = app_with_lines(60);
    draw_at(&mut app, 40, 12);
    app.mouse_scroll = true;
    let _ = app.open_settings();
    app.on_mouse(wheel(MouseEventKind::ScrollUp));
    assert!(
        app.scroll.is_following(),
        "the wheel moved the pane under the settings screen"
    );
    app.on_key(key(KeyCode::PageUp));
    assert!(app.scroll.is_following(), "a scroll key left the settings screen");
}

#[test]
fn a_five_thousand_row_transcript_scrolls() {
    let mut app = app_with_lines(5_000);
    draw_at(&mut app, 40, 12);
    app.on_key(key(KeyCode::PageUp));
    let screen = draw_at(&mut app, 40, 12);
    assert!(!screen.contains("line 4999"), "{screen}");
    assert!(screen.contains("9 rows below"), "{screen}");
    // The top of the conversation is a page at a time away, not a jump.
    for _ in 0..3 {
        app.on_key(key(KeyCode::PageUp));
    }
    let deeper = draw_at(&mut app, 40, 12);
    assert!(deeper.contains("36 rows below"), "{deeper}");
}

#[test]
fn clear_while_held_returns_to_an_empty_pane() {
    let mut app = app_with_lines(60);
    draw_at(&mut app, 40, 12);
    app.on_key(key(KeyCode::PageUp));
    draw_at(&mut app, 40, 12);
    app.transcript = crate::transcript::Transcript::new();
    app.on_key(key(KeyCode::PageUp));
    let screen = draw_at(&mut app, 40, 12);
    assert!(
        app.scroll.is_following(),
        "an offset outlived the transcript it pointed at"
    );
    assert!(!screen.contains("rows below"), "{screen}");
    assert!(screen.contains("Send a message to start"), "{screen}");
}

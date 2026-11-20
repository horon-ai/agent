//! The multi-line editor, and the history behind `up` and `down`.
//!
//! These drive [`Input`] and [`History`] directly. What a keystroke ends up
//! doing is an edit and a cursor, and checking that here keeps the app tests
//! about keys and the render tests about rows on screen.

use crate::history::History;
use crate::input::{Input, Placement};

fn typed(text: &str) -> Input {
    let mut input = Input::new();
    input.insert_text(text);
    input
}

/// The one row a single-line draft draws, and the caret's cell in it.
fn row(input: &Input, width: u16) -> (String, u16) {
    let view = input.view(width, 8);
    assert_eq!(view.rows.len(), 1, "a single line draws one row");
    (view.rows[0].clone(), view.cursor_cell)
}

#[test]
fn typing_and_editing_follow_the_cursor() {
    let mut input = Input::new();
    for character in "helo".chars() {
        input.insert(character);
    }
    assert_eq!(input.text(), "helo");
    input.left();
    input.insert('l');
    assert_eq!(input.text(), "hello");
    assert_eq!(input.cursor(), 4);
    input.backspace();
    assert_eq!(input.text(), "helo");
    input.home();
    input.delete();
    assert_eq!(input.text(), "elo");
    input.end();
    assert_eq!(input.cursor(), 3);
}

#[test]
fn take_empties_the_editor() {
    let mut input = Input::new();
    input.insert('a');
    assert_eq!(input.take(), "a");
    assert!(input.is_empty());
    assert_eq!(input.cursor_column(), 0);
    assert_eq!(input.line_count(), 1, "an empty draft is still one line");
}

#[test]
fn edits_at_the_boundaries_do_nothing() {
    let mut input = Input::new();
    input.backspace();
    input.delete();
    input.left();
    input.right();
    input.kill_to_start();
    input.kill_to_end();
    input.delete_word();
    input.word_left();
    input.word_right();
    assert!(input.is_empty());
    assert_eq!(input.cursor(), 0);
}

#[test]
fn short_line_is_not_windowed() {
    let mut input = typed("hi");
    assert_eq!(row(&input, 20), ("hi".to_string(), 2));
    input.home();
    assert_eq!(row(&input, 20), ("hi".to_string(), 0));
}

#[test]
fn long_line_scrolls_so_the_cursor_stays_inside() {
    // Ten characters in a six-cell pane: the last five fit before the cursor,
    // which sits in the last cell.
    let input = typed("abcdefghij");
    assert_eq!(row(&input, 6), ("fghij".to_string(), 5));
    assert!(
        usize::from(row(&input, 6).1) < 6,
        "cursor escaped the input area: {:?}",
        row(&input, 6)
    );
}

#[test]
fn moving_left_scrolls_the_window_back() {
    let mut input = typed("abcdefghij");
    for _ in 0..5 {
        input.left();
    }
    // Cursor on 'f' (column 5 in the full line) with a six-cell pane keeps the
    // start visible and the caret at cell five.
    assert_eq!(row(&input, 6), ("abcdef".to_string(), 5));
    input.home();
    assert_eq!(row(&input, 6), ("abcdef".to_string(), 0));
}

#[test]
fn wide_characters_move_the_cursor_by_their_cells() {
    let input = typed("한글");
    assert_eq!(input.cursor_column(), 4);
    // Three cells hold one two-cell syllable plus the cursor.
    assert_eq!(row(&input, 3), ("글".to_string(), 2));
}

#[test]
fn zero_width_window_shows_nothing() {
    let input = typed("text");
    assert_eq!(row(&input, 0), (String::new(), 0));
}

#[test]
fn empty_draft_draws_one_empty_row() {
    let input = Input::new();
    let view = input.view(20, 8);
    assert_eq!(view.rows, vec![String::new()]);
    assert_eq!((view.cursor_row, view.cursor_cell), (0, 0));
}

#[test]
fn newline_splits_the_draft_into_rows() {
    let mut input = typed("first");
    input.newline();
    input.insert_text("second");
    assert_eq!(input.text(), "first\nsecond");
    assert_eq!(input.line_count(), 2);
    assert_eq!(input.line_of_cursor(), 1);
    let view = input.view(20, 8);
    assert_eq!(view.rows, vec!["first".to_string(), "second".to_string()]);
    assert_eq!((view.cursor_row, view.cursor_cell), (1, 6));
}

#[test]
fn the_pane_follows_the_caret_and_stops_at_the_cap() {
    let mut input = typed("one\ntwo\nthree\nfour\nfive\nsix");
    let view = input.view(20, 3);
    assert_eq!(
        view.rows,
        vec!["four".to_string(), "five".to_string(), "six".to_string()]
    );
    assert_eq!(view.cursor_row, 2, "the caret's line is the last one shown");
    // Walking back one line scrolls the window with the caret.
    assert!(input.up());
    let view = input.view(20, 3);
    assert_eq!(
        view.rows,
        vec!["three".to_string(), "four".to_string(), "five".to_string()]
    );
    assert_eq!(view.cursor_row, 2);
    // A draft shorter than the pane is drawn from its first line.
    input.home();
    for _ in 0..5 {
        input.up();
    }
    let view = input.view(20, 6);
    assert_eq!(view.rows.len(), 6);
    assert_eq!(view.cursor_row, 0);
}

#[test]
fn a_paste_keeps_its_line_breaks_and_is_one_edit() {
    let mut input = typed("x");
    input.insert_text("a\nb\nc");
    assert_eq!(input.text(), "xa\nb\nc");
    assert_eq!(input.line_count(), 3);
    assert_eq!(input.cursor(), 6, "the cursor lands after the pasted text");
    let view = input.view(10, 8);
    assert_eq!(view.rows, vec!["xa".to_string(), "b".to_string(), "c".to_string()]);
}

#[test]
fn crlf_and_a_lone_carriage_return_become_one_newline() {
    let mut input = Input::new();
    input.insert_text("a\r\nb\rc");
    assert_eq!(input.text(), "a\nb\nc");
    assert_eq!(input.line_count(), 3);
}

#[test]
fn control_characters_are_dropped_and_tabs_become_spaces() {
    let mut input = Input::new();
    input.insert_text("a\u{1b}[31mb\tc\u{7}");
    assert_eq!(input.text(), "a[31mb    c");
    assert_eq!(input.cursor_column(), 11);
    // A control character typed on its own is dropped too, so a stray escape
    // cannot reach the buffer and leave the caret on a cell nothing draws.
    input.insert('\u{1b}');
    assert_eq!(input.text(), "a[31mb    c");
}

#[test]
fn kill_to_start_and_kill_to_end_stay_on_their_line() {
    let mut input = typed("first\nsecond");
    input.kill_to_start();
    assert_eq!(input.text(), "first\n", "only the line's own text goes");
    assert_eq!(input.cursor(), 6);
    input.kill_to_end();
    assert_eq!(input.text(), "first\n", "an empty tail has nothing to cut");
}

#[test]
fn kill_to_end_on_an_empty_tail_joins_the_lines() {
    let mut input = typed("ab\ncd");
    input.home();
    input.up();
    input.end();
    input.kill_to_end();
    assert_eq!(input.text(), "abcd");
    assert_eq!(input.cursor(), 2);
    assert_eq!(input.line_count(), 1);
}

#[test]
fn delete_word_removes_one_word_at_a_time() {
    let mut input = typed("hello brave world");
    input.delete_word();
    assert_eq!(input.text(), "hello brave ");
    input.delete_word();
    assert_eq!(input.text(), "hello ");
    input.delete_word();
    assert_eq!(input.text(), "");
}

#[test]
fn word_motion_crosses_lines() {
    let mut input = typed("ab cd\nef");
    input.home();
    assert_eq!(input.cursor(), 6, "`home` is the start of the caret's line");
    input.word_left();
    assert_eq!(input.cursor(), 3, "the line break is a word boundary");
    input.word_left();
    assert_eq!(input.cursor(), 0, "and the word before it is one too");
    input.word_right();
    assert_eq!(input.cursor(), 3, "past the first word and its space");
    input.word_right();
    assert_eq!(input.cursor(), 6, "onto the next line's word");
}

#[test]
fn vertical_motion_keeps_the_cell_and_stops_at_the_edges() {
    let mut input = typed("abcdef\nxy\nabcdef");
    assert!(input.up());
    assert_eq!(input.cursor(), 9, "the shorter line takes the caret to its end");
    assert!(input.up());
    assert_eq!(input.cursor(), 2, "the cell is kept across lines");
    assert!(!input.up(), "the first line hands `up` to the caller");
    assert!(input.down());
    assert_eq!(input.cursor(), 9);
    assert!(input.down());
    assert_eq!(input.cursor(), 12);
    assert!(!input.down(), "the last line hands `down` to the caller");
}

#[test]
fn a_wide_character_never_straddles_the_edge() {
    // A two-cell syllable in a one-cell pane cannot be shown at all: the row
    // stays empty and the caret stays inside, rather than half a character
    // being drawn and the caret landing past the pane.
    let input = typed("한");
    let view = input.view(1, 8);
    assert_eq!(view.rows, vec![String::new()]);
    assert_eq!(view.cursor_cell, 0);
    // With room for the syllable and the caret, it is drawn whole.
    let view = input.view(3, 8);
    assert_eq!(view.rows, vec!["한".to_string()]);
    assert_eq!(view.cursor_cell, 2);
    // Every width keeps the caret inside the pane.
    let input = typed("a한글b");
    for width in 1..=8 {
        let view = input.view(width, 8);
        assert!(
            usize::from(view.cursor_cell) < usize::from(width),
            "the caret left a {width}-cell pane: {}",
            view.cursor_cell
        );
    }
}

#[test]
fn a_five_thousand_line_paste_stays_a_bounded_view() {
    let mut input = Input::new();
    let pasted: String = (0..5_000).map(|line| format!("line {line}\n")).collect();
    input.insert_text(&pasted);
    assert_eq!(input.line_count(), 5_001, "the trailing newline starts one more");
    let view = input.view(40, 8);
    assert_eq!(view.rows.len(), 8, "only the pane's rows are built");
    assert_eq!(view.cursor_row, 7);
    assert_eq!(view.rows[7], "");
    // The index still describes the buffer after a large insert.
    input.up();
    assert_eq!(input.line_of_cursor(), 4_999);
    let view = input.view(40, 8);
    assert_eq!(view.rows[7], "line 4999");
}

#[test]
fn set_text_replaces_every_line() {
    let mut input = typed("stale");
    input.set_text("new\nlines");
    assert_eq!(input.text(), "new\nlines");
    assert_eq!(input.cursor(), 9);
    assert_eq!(input.line_count(), 2);
    input.set_text("");
    assert!(input.is_empty());
    assert_eq!(input.line_count(), 1);
}

#[test]
fn a_combining_mark_is_one_cursor_stop() {
    let mut input = typed("e\u{301}x");
    assert_eq!(input.cursor_column(), 2);
    input.left();
    assert_eq!(input.cursor(), 2, "the stop before `x` is a character boundary");
    input.left();
    assert_eq!(input.cursor(), 0, "and the accented letter is one whole stop");
    input.right();
    assert_eq!(input.cursor(), 2);
    input.right();
    assert_eq!(input.cursor(), 3);
}

#[test]
fn deleting_takes_the_whole_cluster() {
    let mut input = typed("e\u{301}");
    input.backspace();
    assert!(input.is_empty(), "the letter and its mark go together");
    let mut input = typed("e\u{301}x");
    input.home();
    input.delete();
    assert_eq!(input.text(), "x");
}

#[test]
fn decomposed_hangul_is_one_stop() {
    // `한` as a leading consonant, a medial vowel and a trailing consonant.
    let jamo = "\u{1112}\u{1161}\u{11ab}";
    let mut input = typed(jamo);
    assert_eq!(input.cursor_column(), 2, "the terminal composes it into one syllable");
    input.left();
    assert_eq!(input.cursor(), 0);
    input.right();
    assert_eq!(input.cursor(), 3);
    input.backspace();
    assert!(input.is_empty(), "one press takes the composed syllable");
}

#[test]
fn a_click_places_the_caret_on_the_cell_it_covered() {
    let mut input = typed("한글\nab");
    let view = input.view(10, 8);
    assert_eq!(view.rows.len(), 2);
    let pane = Placement { x: 2, y: 5, lead: 2 };
    input.note_view(pane, &view);
    // The text starts at column 4, the pane's column 2 plus the two-cell
    // marker: cells 4 and 5 are `한`, cells 6 and 7 are `글`. A cell inside the
    // wide syllable answers with the boundary before it, the only stop the
    // cursor can occupy.
    assert!(input.place_caret(5, 5));
    assert_eq!(input.cursor(), 0);
    assert!(input.place_caret(7, 5));
    assert_eq!(input.cursor(), 1);
    assert!(input.place_caret(8, 5));
    assert_eq!(input.cursor(), 2, "past the last cell is the end of the line");
    // The second row is its own line, and a click on the marker before the
    // text starts that line.
    assert!(input.place_caret(4, 6));
    assert_eq!((input.line_of_cursor(), input.cursor()), (1, 3));
    assert!(input.place_caret(2, 6));
    assert_eq!(input.cursor(), 3);
}

#[test]
fn a_click_outside_the_drawn_rows_does_nothing() {
    let mut input = typed("text");
    let view = input.view(20, 8);
    let pane = Placement { x: 0, y: 4, lead: 2 };
    input.note_view(pane, &view);
    assert!(!input.place_caret(3, 3), "above the pane");
    assert!(!input.place_caret(3, 5), "below its one row");
}

#[test]
fn a_click_needs_the_frame_that_drew_the_text() {
    let mut input = typed("text");
    let view = input.view(20, 8);
    let pane = Placement { x: 0, y: 4, lead: 2 };
    input.note_view(pane, &view);
    input.insert('!');
    assert!(
        !input.place_caret(4, 4),
        "the edit has not been drawn, so its cells are unknown"
    );
    let view = input.view(20, 8);
    input.note_view(pane, &view);
    assert!(input.place_caret(4, 4));
    assert_eq!(input.cursor(), 2);
}

#[test]
fn a_narrow_pane_never_shows_or_hits_half_a_cluster() {
    // Three cells of text in a two-cell pane scroll under the caret: whole
    // clusters leave the row, so the mark is not drawn without its letter.
    let mut input = typed("e\u{301}fg");
    let view = input.view(2, 8);
    assert_eq!(view.rows, vec!["g".to_string()]);
    assert_eq!(view.cursor_cell, 1);
    let pane = Placement { x: 0, y: 0, lead: 1 };
    input.note_view(pane, &view);
    assert!(input.place_caret(1, 0));
    assert_eq!(input.cursor(), 3, "the row starts at `g`");
    assert!(input.place_caret(2, 0));
    assert_eq!(input.cursor(), 4, "and a click past it ends the line");
    // A wide syllable that does not fit is not drawn either.
    let input = typed("한");
    assert_eq!(input.view(1, 8).rows, vec![String::new()]);
}

#[test]
fn a_resize_keeps_the_caret_on_the_same_cluster() {
    let mut input = typed("한글");
    let narrow = input.view(3, 8);
    assert_eq!(narrow.rows, vec!["글".to_string()]);
    assert_eq!((narrow.cursor_row, narrow.cursor_cell), (0, 2));
    let pane = Placement { x: 0, y: 0, lead: 1 };
    input.note_view(pane, &narrow);
    // Cells 1 and 2 are `글` in the narrow pane.
    assert!(input.place_caret(2, 0));
    assert_eq!(input.cursor(), 1);
    // The same draft in a wider pane draws both syllables and answers a click
    // on `한` with the boundary before it.
    let wide = input.view(10, 8);
    assert_eq!(wide.rows, vec!["한글".to_string()]);
    assert_eq!(wide.cursor_cell, 2, "the caret moved between the syllables");
    input.note_view(pane, &wide);
    assert!(input.place_caret(1, 0));
    assert_eq!(input.cursor(), 0);
    assert!(input.place_caret(4, 0));
    assert_eq!(input.cursor(), 1);
}

#[test]
fn a_flag_is_one_cursor_stop() {
    // Two regional indicators draw one flag, and four pair into two flags.
    let mut input = typed("🇰🇷🇺🇸");
    assert_eq!(input.cursor_column(), 4, "each flag draws two cells");
    input.left();
    assert_eq!(input.cursor(), 2, "the second flag is one stop");
    input.left();
    assert_eq!(input.cursor(), 0);
    input.right();
    assert_eq!(input.cursor(), 2);
    input.backspace();
    assert_eq!(input.text(), "🇺🇸", "one press takes the flag before the cursor");
    // The delete key takes the whole flag rather than half of it.
    let mut input = typed("🇰🇷!");
    input.home();
    input.delete();
    assert_eq!(input.text(), "!");
}

#[test]
fn an_indic_conjunct_is_one_cursor_stop() {
    // `क्ष`: the consonant, the virama linker and the second consonant are one
    // cluster under the Indic conjunct rule (GB9c).
    let mut input = typed("क्ष");
    assert_eq!(input.cursor(), 3);
    input.left();
    assert_eq!(input.cursor(), 0);
    input.right();
    assert_eq!(input.cursor(), 3);
    input.backspace();
    assert!(input.is_empty());
    // A spacing mark also joins the letter before it (GB9a).
    let mut input = typed("किx");
    input.left();
    assert_eq!(input.cursor(), 2);
    input.left();
    assert_eq!(input.cursor(), 0, "the vowel sign is not a stop of its own");
}

#[test]
fn a_flag_and_decomposed_hangul_keep_their_boundaries() {
    let jamo = "\u{1112}\u{1161}\u{11ab}";
    let mut input = typed("🇰🇷");
    input.insert_text(jamo);
    assert_eq!(input.cursor(), 5);
    input.left();
    assert_eq!(input.cursor(), 2, "the composed syllable is one stop");
    input.left();
    assert_eq!(input.cursor(), 0, "and the flag before it is one");
    input.right();
    assert_eq!(input.cursor(), 2);
    input.backspace();
    assert_eq!(input.text(), jamo);
}

#[test]
fn vertical_motion_keeps_its_cell_over_wide_text() {
    let mut input = typed("한글\nabcd");
    assert!(input.up());
    assert_eq!(input.cursor(), 2, "the caret keeps cell four, after both syllables");
    assert!(input.down());
    assert_eq!(input.cursor(), 7);
    // Cell one is inside `한`, and the only stop near it is its start.
    let mut input = typed("한\nabc");
    input.home();
    input.right();
    assert_eq!(input.cursor(), 3, "between `a` and `b`");
    assert!(input.up());
    assert_eq!(input.cursor(), 0, "a cell inside the syllable answers with its start");
    assert!(input.down());
    assert_eq!(input.cursor(), 2, "the line below starts at cell zero");
}

#[test]
fn a_paste_keeps_combining_text_whole_across_line_breaks() {
    let mut input = Input::new();
    input.insert_text("한\r\ne\u{301}\r\n\u{1112}\u{1161}\u{11ab}");
    assert_eq!(input.text(), "한\ne\u{301}\n\u{1112}\u{1161}\u{11ab}");
    assert_eq!(input.line_count(), 3);
    assert_eq!(input.cursor_column(), 2);
    let view = input.view(20, 8);
    assert_eq!(view.rows, vec!["한", "e\u{301}", "\u{1112}\u{1161}\u{11ab}"]);
}

#[test]
fn history_walks_back_and_the_typing_comes_back() {
    let mut history = History::new();
    history.record("first");
    history.record("second");
    assert_eq!(history.previous("draft"), Some("second".to_string()));
    assert_eq!(history.previous("ignored"), Some("first".to_string()));
    assert_eq!(history.previous("ignored"), None, "the oldest entry is the end");
    assert_eq!(history.forward(), Some("second".to_string()));
    assert_eq!(history.forward(), Some("draft".to_string()), "the draft returns");
    assert_eq!(history.forward(), None, "and the walk is over");
}

#[test]
fn a_repeated_draft_is_remembered_once() {
    let mut history = History::new();
    history.record("same");
    history.record("same");
    assert_eq!(history.previous(""), Some("same".to_string()));
    assert_eq!(history.previous(""), None);
}

#[test]
fn recording_a_draft_ends_a_walk() {
    let mut history = History::new();
    history.record("first");
    assert_eq!(history.previous(""), Some("first".to_string()));
    history.record("second");
    assert_eq!(
        history.previous("typed"),
        Some("second".to_string()),
        "a fresh walk starts at the newest entry, not where the last one stopped"
    );
}

#[test]
fn the_oldest_drafts_fall_off_the_front() {
    let mut history = History::new();
    for number in 0..105 {
        history.record(&number.to_string());
    }
    let mut walked = Vec::new();
    while let Some(entry) = history.previous("") {
        walked.push(entry);
        if walked.len() > 200 {
            break;
        }
    }
    assert_eq!(walked.len(), 100, "the history keeps a bounded number of drafts");
    assert_eq!(walked.first().map(String::as_str), Some("104"));
    assert_eq!(walked.last().map(String::as_str), Some("5"));
}

#[test]
fn an_empty_draft_is_not_remembered() {
    let mut history = History::new();
    history.record("   ");
    assert_eq!(history.previous(""), None);
}

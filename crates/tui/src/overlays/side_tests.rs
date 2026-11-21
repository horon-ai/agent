//! The side-question screen: what it draws while it starts, answers, finishes
//! and fails, the two ways out, and scrolling a long answer.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::side::{Outcome, SideOverlay, draw};
use crate::theme::{Symbols, Theme};

/// Draw the overlay into a test terminal and return the screen as text.
fn screen(overlay: &mut SideOverlay, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw(frame, area, overlay, &Theme::default(), Symbols::default());
        })
        .expect("the overlay draws");
    format!("{}", terminal.backend())
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn a_starting_question_names_itself_and_waits() {
    let mut overlay = SideOverlay::starting(
        1,
        "what did we decide about the cache?".to_string(),
        Some("msg_12".to_string()),
    );
    let drawn = screen(&mut overlay, 100, 24);
    assert!(drawn.contains("Side question"), "the panel has no title:\n{drawn}");
    assert!(
        drawn.contains("what did we decide about the cache?"),
        "the question is not on screen:\n{drawn}"
    );
    assert!(
        drawn.contains("starting a read-only side session"),
        "the state is not said:\n{drawn}"
    );
    assert!(
        drawn.contains("esc returns to the conversation"),
        "the way out is not offered:\n{drawn}"
    );
    assert!(
        drawn.contains("from msg_12"),
        "the chosen boundary is not named:\n{drawn}"
    );
}

#[test]
fn a_streamed_answer_and_its_end_are_shown_with_the_child_id() {
    let mut overlay = SideOverlay::starting(1, "why is the build slow?".to_string(), None);
    overlay.started("session_child".to_string());
    overlay.stream("because the cache".to_string());
    let drawn = screen(&mut overlay, 100, 24);
    assert!(drawn.contains("because the cache"), "the stream is not drawn:\n{drawn}");
    assert!(drawn.contains("session_child"), "the child id is not offered:\n{drawn}");

    overlay.done("because the cache is cold".to_string());
    let drawn = screen(&mut overlay, 100, 24);
    assert!(drawn.contains("because the cache is cold"), "the stored answer is not drawn:\n{drawn}");
    assert!(
        drawn.contains("done; the answer is stored"),
        "the finished state is not said:\n{drawn}"
    );
}

#[test]
fn a_failure_is_drawn_instead_of_a_waiting_placeholder() {
    let mut overlay = SideOverlay::starting(1, "why?".to_string(), None);
    overlay.failed("the harness refused a side session".to_string());
    let drawn = screen(&mut overlay, 100, 24);
    assert!(
        drawn.contains("the harness refused a side session"),
        "the failure is not drawn:\n{drawn}"
    );
    assert!(
        !drawn.contains("waiting for the answer"),
        "a failed question still claims to be waiting:\n{drawn}"
    );
}

#[test]
fn escape_leaves_and_enter_opens_the_child_once_it_exists() {
    let mut overlay = SideOverlay::starting(1, "why?".to_string(), None);
    assert_eq!(overlay.on_key(key(KeyCode::Enter)), Outcome::Handled);
    assert_eq!(overlay.on_key(key(KeyCode::Esc)), Outcome::Closed);

    overlay.started("session_child".to_string());
    assert_eq!(
        overlay.on_key(key(KeyCode::Enter)),
        Outcome::Open("session_child".to_string())
    );
}

#[test]
fn a_long_answer_opens_at_its_end_and_scrolls_up_to_its_start() {
    let mut overlay = SideOverlay::starting(1, "list everything".to_string(), None);
    let answer: String = (1..=60).map(|line| format!("line {line:02}\n")).collect();
    overlay.done(answer);

    let drawn = screen(&mut overlay, 80, 16);
    assert!(
        drawn.contains("line 60"),
        "the end of the answer is not on screen first:\n{drawn}"
    );
    assert!(
        !drawn.contains("line 01"),
        "the whole answer fits, so this case proves nothing:\n{drawn}"
    );

    // The pages go to the start, so one press of each key must move the view.
    for _ in 0..12 {
        overlay.on_key(key(KeyCode::PageUp));
    }
    let drawn = screen(&mut overlay, 80, 16);
    assert!(
        drawn.contains("line 01"),
        "the start of the answer cannot be reached:\n{drawn}"
    );
}

#[test]
fn a_pane_too_small_says_the_screen_is_there_instead_of_hiding_it() {
    let mut overlay = SideOverlay::starting(1, "why?".to_string(), None);
    // Wide enough for the sentence, too short for the panel: the height is
    // what hides the screen, and the line that says so must fit.
    let drawn = screen(&mut overlay, 100, 4);
    assert!(
        drawn.contains("side question hidden"),
        "a tiny pane hid the modal screen and its keys:\n{drawn}"
    );
}

#[test]
fn notices_are_drawn_above_the_footer() {
    let mut overlay = SideOverlay::starting(1, "why?".to_string(), None);
    overlay.started("session_child".to_string());
    overlay.notice("denied a request to use read".to_string());
    let drawn = screen(&mut overlay, 100, 24);
    assert!(
        drawn.contains("denied a request to use read"),
        "the notice is not drawn:\n{drawn}"
    );
}

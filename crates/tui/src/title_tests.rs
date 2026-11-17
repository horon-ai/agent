//! The window title: which name it takes, what it drops, and the bytes it sends.

use crate::title::{self, POP, PUSH, Title};

#[test]
fn the_session_title_names_the_window() {
    assert_eq!(
        title::label(Some("Fix the parser"), Some("/tmp/htb/work")),
        "Fix the parser",
        "the session's own title lost to the directory"
    );
}

#[test]
fn a_session_without_a_title_is_named_after_its_directory() {
    assert_eq!(title::label(None, Some("/tmp/htb/work")), "work");
    assert_eq!(
        title::label(Some("   "), Some("/tmp/htb/work/")),
        "work",
        "a blank title is no title, and a trailing separator is not a component"
    );
    assert_eq!(title::label(None, Some("/")), "", "the root has no last component");
    assert_eq!(title::label(None, None), "", "nothing named the window");
}

#[test]
fn control_characters_cannot_break_out_of_the_title() {
    // A title that carries its own escape sequence would otherwise close the
    // one the client writes and start another.
    let hostile = "ok\u{1b}]0;evil\u{7}\nnext\u{9b}2J";
    let label = title::label(Some(hostile), None);
    assert_eq!(label, "ok]0;evilnext2J", "a control character survived: {label:?}");
    let sequence = title::set(&label);
    assert_eq!(
        sequence.matches('\u{1b}').count(),
        2,
        "the label added an escape of its own: {sequence:?}"
    );
    assert_eq!(
        sequence.matches('\u{7}').count(),
        2,
        "the label added a terminator of its own: {sequence:?}"
    );
    assert_eq!(
        title::label(None, Some("/tmp/wo\u{1b}rk")),
        "work",
        "a directory name carried a control character into the title"
    );
}

#[test]
fn a_long_label_is_cut_to_the_cap() {
    let long = "x".repeat(500);
    let label = title::label(Some(long.as_str()), None);
    assert_eq!(label.chars().count(), 80, "the label was not cut to the cap");
    let korean_text = "가".repeat(100);
    let korean = title::label(Some(korean_text.as_str()), None);
    assert_eq!(
        korean.chars().count(),
        80,
        "a multi-byte label was cut somewhere other than the character count"
    );
}

#[test]
fn the_title_sequence_sets_both_names() {
    assert_eq!(
        title::set("work"),
        "\u{1b}]0;horon · work\u{7}\u{1b}]2;horon · work\u{7}"
    );
    assert_eq!(
        title::set(""),
        "\u{1b}]0;horon\u{7}\u{1b}]2;horon\u{7}",
        "a window with no label still says what it is"
    );
}

#[test]
fn the_title_stack_is_the_xterm_one() {
    assert_eq!(PUSH, "\u{1b}[22;2t");
    assert_eq!(POP, "\u{1b}[23;2t");
}

#[test]
fn an_unchanged_label_is_not_written_again() {
    let mut title = Title::new(Vec::<u8>::new(), true);
    title.start("work").expect("a buffer accepts every write");
    let first = format!("{}{}", PUSH, title::set("work"));
    assert_eq!(
        title.written().as_slice(),
        first.as_bytes(),
        "the push or the first title is missing"
    );

    title.show("work").expect("a repeated label is not an error");
    assert_eq!(title.written().len(), first.len(), "the same label was written twice");

    title.show("elsewhere").expect("a new label is written");
    let second = format!("{}{}", first, title::set("elsewhere"));
    assert_eq!(
        title.written().as_slice(),
        second.as_bytes(),
        "a changed label did not follow the first one"
    );
}

#[test]
fn a_redirected_stdout_gets_no_bytes() {
    let mut title = Title::new(Vec::<u8>::new(), false);
    title.start("work").expect("a redirect takes nothing and fails nothing");
    title
        .show("elsewhere")
        .expect("a redirect takes nothing and fails nothing");
    assert!(
        title.written().is_empty(),
        "a file received escape bytes: {:?}",
        title.written()
    );
}

//! The window title: which name it takes, what it drops, the state it adds, and
//! the bytes it sends.

use crate::title::{self, POP, PUSH, RunState, Title};

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
    let mut title = Title::with_stack(Vec::<u8>::new(), true, true);
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
    let mut title = Title::with_stack(Vec::<u8>::new(), false, true);
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

/// The save/restore pair goes only to a terminal that keeps a title stack.
///
/// `CSI 22 ; 2 t` and `CSI 23 ; 2 t` are an extension. A terminal without it
/// ignores both, so writing the pair there changed nothing while the
/// documentation promised the shell's title came back — it did not, on conhost
/// and on any terminal that answered nothing (#433).
#[test]
fn title_stack_written_only_when_supported() {
    let xterm = title::TitleEnvironment {
        term: Some("xterm-256color".to_string()),
        ..title::TitleEnvironment::default()
    };
    assert!(
        xterm.keeps_title_stack(false),
        "an xterm-like terminal keeps the stack, and today's bytes must not change there"
    );
    assert!(xterm.keeps_title_stack(true), "a redirected xterm is still an xterm");

    let dumb = title::TitleEnvironment {
        term: Some("dumb".to_string()),
        ..title::TitleEnvironment::default()
    };
    assert!(!dumb.keeps_title_stack(false), "`dumb` says it has no title handling");

    let conhost = title::TitleEnvironment::default();
    assert!(
        !conhost.keeps_title_stack(true),
        "a bare Windows console keeps no stack"
    );
    // The same environment on a Unix host is an unknown but xterm-like
    // terminal, so the platform is what decides, not a guess.
    assert!(conhost.keeps_title_stack(false));
    assert!(
        title::TitleEnvironment {
            wt_session: Some("1f3c".to_string()),
            ..title::TitleEnvironment::default()
        }
        .keeps_title_stack(true),
        "Windows Terminal keeps the stack"
    );

    // The title itself is still written where the pair is not.
    let mut supported = Title::with_stack(Vec::<u8>::new(), true, true);
    supported.start("work").expect("a buffer accepts every write");
    assert_eq!(
        supported.written().as_slice(),
        format!("{}{}", PUSH, title::set("work")).as_bytes()
    );

    let mut unsupported = Title::with_stack(Vec::<u8>::new(), true, false);
    unsupported.start("work").expect("a buffer accepts every write");
    assert_eq!(
        unsupported.written().as_slice(),
        title::set("work").as_bytes(),
        "the save/restore pair must not be written where the terminal ignores it"
    );

    // Popping reports success without writing when there is no stack to pop.
    let mut nothing = Vec::<u8>::new();
    assert!(
        title::write_pop(&mut nothing, false),
        "nothing to restore is not a failure"
    );
    assert!(
        nothing.is_empty(),
        "a pop was written to a terminal without a title stack"
    );
    let mut sequence = Vec::<u8>::new();
    assert!(title::write_pop(&mut sequence, true));
    assert_eq!(sequence.as_slice(), POP.as_bytes());
}

#[test]
fn the_state_marker_is_a_fixed_word_after_the_label() {
    let label = |text: &str| text.to_string();
    assert_eq!(
        title::with_state(label("work"), None),
        "work",
        "with the setting off the window keeps the name it has today"
    );
    assert_eq!(
        title::with_state(label("work"), Some(RunState::Running)),
        "work · running"
    );
    assert_eq!(
        title::with_state(label("work"), Some(RunState::Waiting)),
        "work · waiting"
    );
    assert_eq!(title::with_state(label("work"), Some(RunState::Idle)), "work · idle");
    assert_eq!(
        title::with_state(label(""), Some(RunState::Idle)),
        "idle",
        "a window with no label still says what it is doing"
    );
}

#[test]
fn a_prompt_outranks_the_turn_and_a_generating_session_is_running() {
    assert_eq!(RunState::of(false, false, Some("idle")), RunState::Idle);
    assert_eq!(RunState::of(false, false, None), RunState::Idle);
    assert_eq!(RunState::of(false, true, None), RunState::Running);
    assert_eq!(
        RunState::of(false, false, Some("generating")),
        RunState::Running,
        "a turn in flight is running even before this client holds it"
    );
    assert_eq!(
        RunState::of(true, true, Some("generating")),
        RunState::Waiting,
        "a prompt stops the turn, so waiting outranks running"
    );
}

#[test]
fn a_state_change_is_the_only_thing_that_renames_the_window() {
    let mut title = Title::with_stack(Vec::<u8>::new(), true, true);
    let running = title::with_state("work".to_string(), Some(RunState::Running));
    title.start(&running).expect("a buffer accepts every write");
    let first = format!("{}{}", PUSH, title::set(&running));
    assert_eq!(title.written().as_slice(), first.as_bytes());

    // The frame after the one that wrote it carries the same state again.
    title.show(&running).expect("a repeated state writes nothing new");
    assert_eq!(
        title.written().len(),
        first.len(),
        "an unchanged state wrote the title again"
    );

    let waiting = title::with_state("work".to_string(), Some(RunState::Waiting));
    title.show(&waiting).expect("a changed state is written");
    let second = format!("{}{}", first, title::set(&waiting));
    assert_eq!(
        title.written().as_slice(),
        second.as_bytes(),
        "the new state did not reach the title, or something else did"
    );
}

#[test]
fn a_terminal_without_a_title_stack_gets_no_state_marker() {
    // The marker rides the same sequences as the name, so the terminal that is
    // left with the name alone is the one whose title handling is already not
    // trusted: no stack pair is written there either.
    assert!(
        Title::with_stack(Vec::<u8>::new(), true, true).state_supported(),
        "an xterm-like terminal can carry the state"
    );
    assert!(
        !Title::with_stack(Vec::<u8>::new(), true, false).state_supported(),
        "a terminal without a title stack keeps the name it always had"
    );
}

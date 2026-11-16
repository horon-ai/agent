//! Tests for the Markdown renderer, kept out of `markdown.rs` so the renderer
//! stays inside its size budget.
//!
//! These cover what can be wrong without a terminal: which marks are turned into
//! styles, which lines keep their source text, how many columns each line
//! reports for its continuation rows, and what happens to a message that is
//! still arriving — a fence with no end, an emphasis marker with no partner, a
//! line that is nothing but markers.

use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::item_lines;
use crate::markdown;
use crate::theme::{Symbols, Theme};
use crate::transcript::Item;
use crate::wrap::{self, Indented};

/// The text of every line, before wrapping.
fn texts(rows: &[Indented]) -> Vec<String> {
    rows.iter().map(|row| text_of(row.line())).collect()
}

/// The text of one line, spans joined.
fn text_of(line: &Line<'_>) -> String {
    line.spans.iter().map(|span| span.content.as_ref()).collect()
}

/// The renderer's output for `source` at `width`, in the default theme.
fn render(source: &str, width: u16) -> Vec<Indented> {
    markdown::render(source, &Theme::default(), Symbols::default(), width)
}

/// The renderer's output for `source`, wrapped to `width` as the pane would.
fn wrapped(source: &str, width: u16) -> Vec<String> {
    wrap::wrap_rows(render(source, width), width)
        .iter()
        .map(text_of)
        .collect()
}

#[test]
fn a_heading_loses_its_hashes_and_takes_the_accent() {
    let rows = render("## List", 40);
    assert_eq!(texts(&rows), vec!["List"]);
    let style = rows[0].line().spans[0].style;
    assert_eq!(style.fg, Some(Theme::default().core.accent));
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn bold_italic_and_inline_code_lose_their_markers() {
    let rows = render("some **bold**, *italic* and `code` text", 80);
    assert_eq!(texts(&rows), vec!["some bold, italic and code text"]);
    let spans = &rows[0].line().spans;
    let bold = spans
        .iter()
        .find(|span| span.content.as_ref() == "bold")
        .expect("bold span");
    assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    let italic = spans
        .iter()
        .find(|span| span.content.as_ref() == "italic")
        .expect("italic span");
    assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
    let code = spans
        .iter()
        .find(|span| span.content.as_ref() == "code")
        .expect("code span");
    assert_eq!(code.style.fg, Some(Theme::default().transcript.tool_done));
}

#[test]
fn an_unclosed_emphasis_marker_is_drawn_as_text() {
    // Half of a streamed `**bold**` is still half of what the model sent, and
    // the characters have to be on screen rather than dropped.
    let rows = render("and then **half of it", 80);
    assert_eq!(texts(&rows), vec!["and then **half of it"]);
    assert!(
        rows[0]
            .line()
            .spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::BOLD))
    );
}

#[test]
fn nested_emphasis_keeps_the_text_and_both_styles() {
    let rows = render("**bold with *inside* here**", 80);
    assert_eq!(texts(&rows), vec!["bold with inside here"]);
    let spans = &rows[0].line().spans;
    assert!(
        spans
            .iter()
            .all(|span| span.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(
        spans
            .iter()
            .any(|span| span.style.add_modifier.contains(Modifier::ITALIC))
    );
}

#[test]
fn mismatched_markers_keep_every_character() {
    let rows = render("**bold *italic***", 80);
    // The paired markers are the only characters that go; nothing else is lost.
    assert_eq!(texts(&rows), vec!["bold *italic*"]);
}

#[test]
fn an_unclosed_fence_draws_the_code_that_arrived() {
    let rows = render("```rust\nfn main() {", 40);
    assert_eq!(texts(&rows), vec!["  fn main() {"]);
    assert_eq!(rows[0].indent(), 2);
}

#[test]
fn a_closed_fence_keeps_its_code_literal() {
    // Marks inside a fence are code, not emphasis: a shell `**` glob stays.
    let rows = render("```sh\nrm -rf **/target\n```\nafter", 40);
    assert_eq!(texts(&rows), vec!["  rm -rf **/target", "after"]);
    assert!(
        rows[0]
            .line()
            .spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::BOLD))
    );
}

#[test]
fn a_fence_line_alone_adds_no_row() {
    // The half of a fence that has arrived must not push empty rows around, and
    // the text written after it is still drawn once the block opens.
    assert!(render("```", 40).is_empty());
    assert_eq!(texts(&render("```\n", 40)), Vec::<String>::new());
}

#[test]
fn code_rows_keep_the_block_indent_when_wrapped() {
    // The row after a break starts under the code, not at the first column.
    let rows = wrapped("```\naaaa bbbb cccc dddd\n```", 20);
    assert_eq!(rows[0], "  aaaa bbbb cccc ");
    assert_eq!(rows[1], "  dddd");
}

#[test]
fn a_bullet_list_uses_the_glyph_set_and_indents_its_continuation() {
    let rows = render("- first item", 40);
    assert_eq!(texts(&rows), vec![format!("{} first item", Symbols::default().bullet)]);
    assert_eq!(rows[0].indent(), 2);
    // The mark is the glyph set's, not a fixed `-`: a terminal without Unicode
    // gets the ASCII one.
    let ascii = markdown::render(
        "- first",
        &Theme::default(),
        crate::theme::SymbolPreset::Ascii.symbols(),
        40,
    );
    assert_eq!(texts(&ascii), vec!["* first"]);
}

#[test]
fn an_ordered_list_keeps_its_own_numbers() {
    let rows = render("1. one\n2. two", 40);
    assert_eq!(texts(&rows), vec!["1. one", "2. two"]);
    assert_eq!(rows[0].indent(), 3);
}

#[test]
fn a_quote_draws_a_bar_per_level() {
    let bar = Symbols::default().quote;
    let rows = render("> quoted\n>> nested", 40);
    assert_eq!(
        texts(&rows),
        vec![format!("{bar} quoted"), format!("{bar} {bar} nested")]
    );
    assert_eq!(rows[0].indent(), 2);
    assert_eq!(rows[1].indent(), 4);
}

#[test]
fn a_rule_spans_the_pane() {
    let rows = render("---", 10);
    assert_eq!(texts(&rows), vec![Symbols::default().rule.repeat(10)]);
}

#[test]
fn a_line_of_only_markers_does_not_panic() {
    // `***` is a rule, and `***` alone must not be read as an empty bold pair.
    assert_eq!(texts(&render("***", 20)), vec![Symbols::default().rule.repeat(20)]);
    assert_eq!(texts(&render("**", 20)), vec!["**"]);
    assert_eq!(texts(&render("__", 20)), vec!["__"]);
    assert_eq!(texts(&render("``", 20)), vec!["``"]);
}

#[test]
fn a_table_stays_as_typed() {
    let source = "| Name | Value |\n|------|-------|\n| alpha | 1 |";
    assert_eq!(texts(&render(source, 40)), source.lines().collect::<Vec<_>>().to_vec());
}

#[test]
fn tabs_in_code_advance_to_their_own_stop() {
    let rows = render("```\na\tb\n```", 40);
    assert_eq!(texts(&rows), vec!["  a   b"]);
}

#[test]
fn wide_characters_are_not_split_in_a_code_block() {
    let rows = wrapped("```\n한글한글한글 🚀\n```", 12);
    for row in &rows {
        assert!(
            unicode_width::UnicodeWidthStr::width(row.as_str()) <= 12,
            "row {row:?} is wider than the pane"
        );
        assert!(row.starts_with("  "), "row {row:?} lost its indent");
    }
}

#[test]
fn a_very_narrow_pane_still_terminates() {
    // Every branch reports rows, and a width the indent cannot fit must not loop
    // or produce rows wider than the pane by more than one wide character.
    for width in 0..=3 {
        let rows = wrapped("## head\n- item\n> quote\n```\ncode\n```\ntext", width);
        assert!(!rows.is_empty(), "width {width} drew nothing");
    }
}

#[test]
fn a_very_long_word_reaches_the_next_rows() {
    let word = "a".repeat(200);
    let rows = wrapped(&word, 10);
    assert_eq!(rows.len(), 20);
    assert_eq!(rows.iter().map(String::len).sum::<usize>(), 200);
}

#[test]
fn the_raw_mode_draws_the_source_as_typed() {
    let theme = Theme::default();
    let look = item_lines::Look {
        theme: &theme,
        symbols: Symbols::default(),
        width: 40,
        markdown: false,
    };
    let rows = item_lines::answer("## head with **bold**", &look);
    assert_eq!(texts(&rows), vec!["## head with **bold**"]);
    assert!(
        rows[0]
            .line()
            .spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::BOLD))
    );
}

#[test]
fn a_user_line_keeps_its_prefix_and_indent() {
    let theme = Theme::default();
    let look = item_lines::Look {
        theme: &theme,
        symbols: Symbols::default(),
        width: 40,
        markdown: true,
    };
    let item = Item::User("a **user** line".to_string());
    let rows = item_lines::lines(&item, &look);
    // Markdown is for the model's text only: the user's own words are as typed.
    assert_eq!(texts(&rows), vec!["> a **user** line"]);
    assert_eq!(rows[0].indent(), 2);
}

/// An app whose transcript can be filled and drawn, with no daemon behind it.
fn app() -> crate::app::App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    crate::app::App::new("test socket".to_string(), receiver)
}

/// Resolve settings with `env` as the only layer above the defaults.
fn settings_with(env: &[(&str, &str)]) -> crate::settings::Settings {
    let sources = crate::settings::Sources {
        global: std::path::PathBuf::from("/nonexistent/global.toml"),
        project: None,
        configs: Vec::new(),
    };
    let set = crate::settings::layers::LayerSet::new(&sources);
    let (settings, warnings) = set.resolve(|name| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
    });
    assert!(warnings.is_empty(), "no layer warns: {warnings:?}");
    settings
}

/// Draw one frame and return the screen as text.
fn screen(app: &mut crate::app::App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = crate::draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

/// Send one finished assistant message through the transcript.
fn say(app: &mut crate::app::App, text: &str) {
    app.transcript.text_delta(None, text);
    app.transcript.text_done();
    app.transcript.finish_stream();
}

#[test]
fn a_drawn_answer_shows_no_markers() {
    let mut app = app();
    say(&mut app, "## Head\n\n- one **two** and `three`");
    let drawn = screen(&mut app, 40, 20);
    assert!(drawn.contains("Head"), "{drawn}");
    assert!(drawn.contains("one two and three"), "{drawn}");
    assert!(!drawn.contains("##"), "the heading kept its hashes:\n{drawn}");
    assert!(!drawn.contains("**"), "the bold markers survived:\n{drawn}");
}

#[test]
fn the_setting_off_draws_the_source_as_typed() {
    let mut app = app();
    say(&mut app, "## Head\n\n- one **two**");
    app.refresh_settings(&settings_with(&[("HORON_TUI_TRANSCRIPT_MARKDOWN", "false")]));
    let drawn = screen(&mut app, 40, 20);
    assert!(drawn.contains("## Head"), "{drawn}");
    assert!(drawn.contains("**two**"), "{drawn}");
}

#[test]
fn a_turn_starts_below_a_blank_row() {
    let mut app = app();
    app.transcript.push_user("first");
    say(&mut app, "an answer");
    app.transcript.push_user("second");
    let drawn = screen(&mut app, 40, 20);
    let rows: Vec<&str> = drawn.lines().collect();
    let at = rows
        .iter()
        .position(|row| row.contains("> second"))
        .expect("the second turn is on screen");
    assert!(at > 0, "the second turn has no row above it:\n{drawn}");
    // The backend draws each row inside a pair of quotes, so a row that holds
    // nothing but padding is blank once those are taken off.
    assert!(
        rows[at - 1].trim_matches('"').trim().is_empty(),
        "the turn is not separated:\n{drawn}"
    );
}

#[test]
fn the_setting_reaches_the_applied_settings() {
    // The renderer asks the resolved settings on every frame, so the key has to
    // be declared for the default and for the override to mean anything.
    let theme = Theme::default();
    let look = |markdown| item_lines::Look {
        theme: &theme,
        symbols: Symbols::default(),
        width: 40,
        markdown,
    };
    assert!(item_lines::markdown_enabled(&settings_with(&[])));
    assert!(!item_lines::markdown_enabled(&settings_with(&[(
        "HORON_TUI_TRANSCRIPT_MARKDOWN",
        "false"
    )])));
    assert_ne!(
        texts(&item_lines::answer("**a**", &look(false))),
        texts(&item_lines::answer("**a**", &look(true)))
    );
}

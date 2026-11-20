//! Tests for the whole output kept behind a cut tool preview.
//!
//! Kept out of `tool_expand.rs` so production code stays inside its size budget,
//! and out of `tool_line_tests.rs` because these cover the kept table as well as
//! the marker a line draws from it. The memory bounds are part of the contract,
//! so most of them are tested directly rather than through a terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::App;
use crate::keys::{self, Bindings};
use crate::sdk_bridge::Incoming;
use crate::tool_expand::{self, ExpandState, MAX_KEPT_BYTES, MAX_KEPT_CALLS, NOTHING_TO_EXPAND, TRUNCATED};
use crate::tool_line::{self, DEFAULT_OUTPUT_LINES};
use crate::transcript::{Item, Transcript};

fn state_with(calls: &[(&str, &str)]) -> ExpandState {
    let mut state = ExpandState::default();
    for &(call_id, output) in calls {
        state.record(call_id, output);
    }
    state
}

/// The text a set of rendered rows shows, one row per line.
fn drawn(rows: &[ratatui::text::Line<'static>]) -> String {
    rows.iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel::<Incoming>();
    App::new("socket · session".to_string(), receiver)
}

/// Settings resolved from the registry defaults and one set of environment
/// overrides, which is how a display preference reaches the app.
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

fn tool_start(call_id: &str) -> ApiEvent {
    ApiEvent::ToolStart {
        session_id: "s1".to_string(),
        call_id: call_id.to_string(),
        name: "bash".to_string(),
    }
}

fn tool_done(call_id: &str, output: &str) -> ApiEvent {
    ApiEvent::ToolDone {
        session_id: "s1".to_string(),
        call_id: call_id.to_string(),
        name: "bash".to_string(),
        output: output.to_string(),
        error: None,
        verdict: None,
    }
}

fn ctrl_o() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)
}

fn alt_t() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT)
}

fn page_up() -> KeyEvent {
    KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)
}

fn draw_at(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = crate::draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

fn draw_to_string(app: &mut App) -> String {
    draw_at(app, 100, 50)
}

/// The first `count` rows of a drawn screen, where the transcript sits.
fn top_rows(screen: &str, count: usize) -> String {
    screen.lines().take(count).collect::<Vec<_>>().join("\n")
}

/// An app whose transcript holds `count` one-row notices.
fn app_with_lines(count: usize) -> App {
    let mut app = app();
    for index in 0..count {
        app.transcript.push_notice(format!("line {index:04}"));
    }
    app
}

/// The output the manual QA uses: 200 numbered rows.
fn big_output() -> String {
    let mut text = String::new();
    for row in 1..=200 {
        text.push_str(&format!("row-{row}\n"));
    }
    text
}

/// An output long enough that wrapping all of it would be a frame's whole cost.
fn long_output(rows: usize) -> String {
    let mut text = String::new();
    for row in 0..rows {
        text.push_str(&format!("output row {row:04}\n"));
    }
    text
}

#[test]
fn the_newest_call_opens_and_the_second_press_puts_the_preview_back() {
    let mut state = state_with(&[("c1", "a\nb\n"), ("c2", "c\nd\n")]);
    assert!(state.toggle(), "the press had nothing to do");
    assert!(state.is_expanded("c2"), "the newest call is the one the key expands");
    assert!(!state.is_expanded("c1"), "an older call opened too");
    assert!(state.toggle(), "the second press had nothing to do");
    assert!(!state.is_expanded("c2"), "the preview did not come back");
}

#[test]
fn a_newer_call_closes_the_one_expanded_before_it() {
    let mut state = state_with(&[("c1", "a\nb\n")]);
    assert!(state.toggle());
    state.record("c2", "c\nd\n");
    assert!(!state.is_expanded("c1"), "the older call stayed open");
    assert!(!state.is_expanded("c2"), "a call landed expanded");
}

#[test]
fn only_the_newest_call_names_the_key_while_it_is_shut() {
    let bindings = Bindings::default();
    let state = state_with(&[("c1", "a\n"), ("c2", "b\n")]);
    let frame = tool_expand::Frame::new(&state, &bindings);
    assert!(
        frame.retained("c2").key.is_some(),
        "the newest call does not name the key that expands it"
    );
    assert!(
        frame.retained("c1").key.is_none(),
        "an older call promised a view the key would not open"
    );
    assert!(frame.retained("nope").key.is_none(), "an unknown call named a key");
}

#[test]
fn an_open_call_draws_its_whole_text_and_stops_naming_the_key() {
    let bindings = Bindings::default();
    let mut state = state_with(&[("c1", "a\nb\n")]);
    assert!(state.toggle());
    let frame = tool_expand::Frame::new(&state, &bindings);
    let retained = frame.retained("c1");
    assert_eq!(retained.text, Some("a\nb\n"), "the whole output is not drawn");
    assert!(
        retained.key.is_none(),
        "an open call still promised the key that opens it"
    );
}

#[test]
fn a_replay_of_the_same_result_never_stores_one_call_twice() {
    let mut state = ExpandState::default();
    for index in 0..MAX_KEPT_CALLS {
        state.record(&format!("c{index}"), "text\n");
    }
    // A reconnect replays results, and the window is full: the replayed call
    // may enter it again, but two entries for one id would draw one call twice
    // and take a slot from a call that is still live.
    state.record("newest", "text\n");
    state.record("c1", "text\n");
    let calls: Vec<&str> = state.calls().collect();
    assert_eq!(calls.len(), MAX_KEPT_CALLS, "the window grew: {calls:?}");
    let unique: std::collections::BTreeSet<&str> = calls.iter().copied().collect();
    assert_eq!(unique.len(), calls.len(), "one call is stored twice: {calls:?}");
}

#[test]
fn only_the_sixteen_most_recent_calls_keep_their_text() {
    let mut state = ExpandState::default();
    for index in 0..=MAX_KEPT_CALLS {
        state.record(&format!("c{index}"), &format!("row {index}\n"));
    }
    let calls: Vec<&str> = state.calls().collect();
    assert_eq!(calls.len(), MAX_KEPT_CALLS, "the window is the wrong size: {calls:?}");
    assert!(!calls.contains(&"c0"), "the oldest call was not dropped");
    let frame = tool_expand::Frame::new(&state, &Bindings::default());
    assert!(
        frame.retained("c0").key.is_none(),
        "a call with no kept text still named the key"
    );
}

#[test]
fn a_call_without_an_id_keeps_nothing() {
    let mut state = ExpandState::default();
    state.record("", "a\nb\n");
    assert_eq!(state.calls().count(), 0, "a call that cannot be found was kept");
    assert!(!state.toggle(), "an empty table had something to toggle");
}

#[test]
fn clearing_drops_every_kept_output() {
    let mut state = state_with(&[("c1", "a\n"), ("c2", "b\n")]);
    assert!(state.toggle());
    state.clear();
    assert_eq!(state.calls().count(), 0, "a cleared table kept a call");
    assert!(!state.toggle(), "a cleared table still toggled");
}

#[test]
fn a_long_output_is_cut_on_a_character_boundary_with_a_note() {
    // One byte over the budget in a stream of three-byte characters: the cut has
    // to land before the character that crosses it, not inside it.
    let text = "가".repeat(MAX_KEPT_BYTES / 3 + 1);
    let mut state = ExpandState::default();
    state.record("c1", &text);
    assert!(state.toggle());
    let kept = tool_expand::Frame::new(&state, &Bindings::default())
        .retained("c1")
        .text
        .expect("the kept text")
        .to_string();
    assert!(
        kept.ends_with(TRUNCATED),
        "the cut copy does not say it was cut: {kept:?}"
    );
    let body = kept.trim_end_matches(TRUNCATED).trim_end_matches('\n');
    assert_eq!(
        body.chars().count(),
        MAX_KEPT_BYTES / 3,
        "the cut landed inside a character: {} chars kept",
        body.chars().count()
    );
    assert!(
        kept.len() <= MAX_KEPT_BYTES + TRUNCATED.len() + 1,
        "the kept copy is over its budget: {} bytes",
        kept.len()
    );
}

#[test]
fn a_cut_copy_of_text_without_a_final_newline_puts_the_note_on_its_own_line() {
    let text = "a".repeat(MAX_KEPT_BYTES + 1);
    let mut state = ExpandState::default();
    state.record("c1", &text);
    assert!(state.toggle());
    let kept = tool_expand::Frame::new(&state, &Bindings::default())
        .retained("c1")
        .text
        .expect("the kept text")
        .to_string();
    let last = kept.lines().next_back().unwrap_or_default();
    assert_eq!(last, TRUNCATED, "the note was spliced onto the output's own line");
}

#[test]
fn an_output_that_fits_is_kept_whole() {
    let text = "a".repeat(MAX_KEPT_BYTES);
    let mut state = ExpandState::default();
    state.record("c1", &text);
    assert!(state.toggle());
    let kept = tool_expand::Frame::new(&state, &Bindings::default())
        .retained("c1")
        .text
        .expect("the kept text")
        .to_string();
    assert_eq!(kept, text, "an output that fits was cut anyway");
}

#[test]
fn a_preview_that_hides_nothing_is_not_worth_keeping() {
    let whole = "a\nb\n";
    let head = tool_line::cut_output(whole, DEFAULT_OUTPUT_LINES).expect("a preview");
    assert!(!tool_expand::cut(whole, &head), "a complete preview was called cut");

    let tall = "a\nb\nc\nd\n";
    let head = tool_line::cut_output(tall, DEFAULT_OUTPUT_LINES).expect("a preview");
    assert!(tool_expand::cut(tall, &head), "hidden rows were not noticed");

    let wide = "a".repeat(400);
    let head = tool_line::cut_output(&wide, DEFAULT_OUTPUT_LINES).expect("a preview");
    assert!(tool_expand::cut(&wide, &head), "a cut row was not noticed");
}

#[test]
fn the_marker_names_the_key_only_while_the_output_is_kept() {
    let mut transcript = Transcript::new();
    transcript.push_user("run it");
    let output = big_output();
    transcript.tool_done("c1", "bash", Some(output.as_str()), None);
    let call = match transcript.items().last() {
        Some(Item::Tool(call)) => call.clone(),
        other => panic!("the call was not recorded: {other:?}"),
    };
    let head = call.output.clone().expect("a preview");
    let bindings = Bindings::default();
    let frame = tool_expand::Frame::new(transcript.expand_state(), &bindings);
    let kept = drawn(&tool_line::lines_with(
        &call,
        &crate::theme::Theme::default(),
        crate::theme::Symbols::default(),
        100,
        frame.retained("c1"),
    ));
    assert!(
        kept.contains(&format!("+{} lines · ctrl+o expands", head.hidden_rows())),
        "the marker does not name the key: {kept:?}"
    );

    // The same call drawn after a cleared table: the count is still there, and
    // the promise of a view is not.
    let empty = ExpandState::default();
    let kept = drawn(&tool_line::lines_with(
        &call,
        &crate::theme::Theme::default(),
        crate::theme::Symbols::default(),
        100,
        tool_expand::Frame::new(&empty, &bindings).retained("c1"),
    ));
    assert!(
        kept.contains(&format!("+{} lines", head.hidden_rows())),
        "the count went with the key: {kept:?}"
    );
    assert!(
        !kept.contains("expands"),
        "a dropped output still promised a view: {kept:?}"
    );
}

#[test]
fn an_open_call_is_drawn_as_every_row_of_its_output() {
    let mut transcript = Transcript::new();
    let output = big_output();
    transcript.tool_done("c1", "bash", Some(output.as_str()), None);
    let call = match transcript.items().last() {
        Some(Item::Tool(call)) => call.clone(),
        other => panic!("the call was not recorded: {other:?}"),
    };
    assert!(transcript.toggle_expand(), "there was nothing to expand");
    let frame = tool_expand::Frame::new(transcript.expand_state(), &Bindings::default());
    let rows = tool_line::lines_with(
        &call,
        &crate::theme::Theme::default(),
        crate::theme::Symbols::default(),
        100,
        frame.retained("c1"),
    );
    assert_eq!(rows.len(), 201, "the whole output is not drawn: {} rows", rows.len());
    let text = drawn(&rows);
    assert!(text.contains("row-200"), "the last row is missing: {text:?}");
    assert!(!text.contains("+197 lines"), "an open call still counts hidden rows");
}

#[test]
fn an_expanded_call_wraps_inside_the_pane_like_the_preview_does() {
    let mut transcript = Transcript::new();
    let long = format!("{}\nrow-2\n", "wide ".repeat(80));
    transcript.tool_done("c1", "bash", Some(long.as_str()), None);
    let call = match transcript.items().last() {
        Some(Item::Tool(call)) => call.clone(),
        other => panic!("the call was not recorded: {other:?}"),
    };
    assert!(transcript.toggle_expand(), "there was nothing to expand");
    let theme = crate::theme::Theme::default();
    let look = crate::item_lines::Look {
        theme: &theme,
        symbols: crate::theme::Symbols::default(),
        width: 40,
        markdown: false,
        visual_blocks: true,
    };
    let frame = tool_expand::Frame::new(transcript.expand_state(), &Bindings::default());
    let wrapped = crate::wrap::wrap_rows(crate::item_lines::lines(&Item::Tool(call), &look, Some(&frame)), 40);
    assert!(wrapped.len() > 2, "the long row did not wrap: {wrapped:?}");
    for row in &wrapped {
        let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= 40,
            "a row is wider than the pane: {text:?}"
        );
    }
    // Every row under the head keeps the preview's own column, so an expanded
    // call reads as one block instead of sliding back to the left edge.
    for row in wrapped.iter().skip(1) {
        let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
        assert!(
            text.starts_with("  "),
            "an expanded row slid back to the first column: {text:?}"
        );
    }
}

#[test]
fn a_preview_that_hid_nothing_keeps_nothing() {
    let mut transcript = Transcript::new();
    transcript.tool_done("c1", "bash", Some("one row\n"), None);
    assert!(!transcript.toggle_expand(), "a complete output was expandable");
}

#[test]
fn the_expand_key_says_so_when_no_call_has_kept_output() {
    let mut app = app();
    assert_eq!(app.on_key(ctrl_o()), None, "the key asked for an action");
    match app.transcript.items().last() {
        Some(Item::Notice(text)) => assert_eq!(text.as_str(), NOTHING_TO_EXPAND),
        other => panic!("no notice was shown: {other:?}"),
    }
}

#[test]
fn the_expand_key_opens_the_cut_call_and_the_next_press_shuts_it() {
    let mut app = app();
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &big_output()));

    let cut = draw_to_string(&mut app);
    assert!(
        cut.contains("ctrl+o expands"),
        "the cut view does not name the key:\n{cut}"
    );
    assert!(
        !cut.contains("row-200"),
        "the cut view already showed the last row:\n{cut}"
    );

    assert_eq!(app.on_key(ctrl_o()), None);
    let open = draw_to_string(&mut app);
    assert!(open.contains("row-200"), "the expanded view hid the last row:\n{open}");

    assert_eq!(app.on_key(ctrl_o()), None);
    let shut = draw_to_string(&mut app);
    assert!(
        !shut.contains("row-200"),
        "the second press did not fold the view:\n{shut}"
    );
    assert!(
        shut.contains("ctrl+o expands"),
        "the folded view lost its marker:\n{shut}"
    );
}

#[test]
fn a_cleared_transcript_forgets_what_it_expanded() {
    let mut app = app();
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &big_output()));

    assert_eq!(app.on_key(ctrl_o()), None);
    assert!(app.transcript.expand_state().is_expanded("c1"));

    app.transcript.clear();
    assert!(
        !app.transcript.expand_state().is_expanded("c1"),
        "the kept output outlived the conversation it belonged to"
    );
    assert!(!app.transcript.toggle_expand(), "a cleared transcript still expanded");
}

#[test]
fn the_binding_ships_on_ctrl_o_under_the_app_scope() {
    let bindings = Bindings::default();
    let bound = bindings.keys_for(keys::ids::APP_EXPAND_TOOL);
    assert_eq!(bound.len(), 1, "the action ships on more than one key: {bound:?}");
    assert_eq!(bound.first().map(ToString::to_string), Some("ctrl+o".to_string()));
    let pressed = keys::Key::from_event(ctrl_o()).expect("a named key");
    assert_eq!(
        bindings.resolve(keys::Scope::App, &pressed),
        Some(keys::ids::APP_EXPAND_TOOL),
        "ctrl+o does not resolve to the expand action"
    );
}

#[test]
fn recording_a_cut_call_reports_a_redraw_only_when_it_folds_one() {
    let mut state = ExpandState::default();
    assert!(
        !state.record("c1", "a\nb\n"),
        "the first cut call has no line under it to redraw"
    );
    assert!(
        !state.record("c2", "c\nd\n"),
        "a shut line keeps its rows when the key label moves off it"
    );
    assert!(state.toggle(), "the newest call opened");
    assert!(state.record("c3", "e\nf\n"), "the call that was open folded back");
    assert!(!state.record("c3", "e\nf\n"), "a replayed result changed nothing");
    assert!(!state.record("", "g\nh\n"), "a call with no id is not kept");
}

#[test]
fn a_newer_cut_call_recounts_the_rows_of_the_one_it_folds() {
    let mut app = app();
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &big_output()));
    assert_eq!(app.on_key(ctrl_o()), None, "the key asked for an action");
    assert_eq!(app.on_key(page_up()), None, "the view keeps its own key");
    draw_to_string(&mut app);
    let open = app.scroll.heights().expect("a measurement while held").to_vec();
    assert!(
        open.first().is_some_and(|rows| *rows > 100),
        "c1 was not measured while it was open: {open:?}"
    );

    // A second cut result folds c1 back to its preview. No key was pressed, so
    // the heights the pane counted for the open call have to be dropped by the
    // recording itself.
    app.apply_event(&tool_start("c2"));
    app.apply_event(&tool_done("c2", &big_output()));
    draw_to_string(&mut app);
    let counted = app.scroll.heights().expect("a measurement").to_vec();
    assert!(
        counted.first().is_some_and(|rows| *rows < 20),
        "the folded call kept its open height: {counted:?}"
    );

    // A measurement taken from nothing is the value the pane should have got.
    app.scroll.invalidate();
    assert_eq!(app.on_key(page_up()), None, "the view keeps its own key");
    draw_to_string(&mut app);
    let fresh = app.scroll.heights().expect("a measurement while held").to_vec();
    assert_eq!(counted, fresh, "a held view trusted heights from before the fold");
}

#[test]
fn a_failed_call_keeps_the_output_its_preview_hides() {
    let mut transcript = Transcript::new();
    let output = big_output();
    transcript.tool_done(
        "c1",
        "bash",
        Some(output.as_str()),
        Some("the command exited with status 1"),
    );
    let call = match transcript.items().last() {
        Some(Item::Tool(call)) => call.clone(),
        other => panic!("the call was not recorded: {other:?}"),
    };
    let head = call.output.clone().expect("a preview");
    let frame = tool_expand::Frame::new(transcript.expand_state(), &Bindings::default());
    let shut = drawn(&tool_line::lines_with(
        &call,
        &crate::theme::Theme::default(),
        crate::theme::Symbols::default(),
        100,
        frame.retained("c1"),
    ));
    assert!(
        shut.contains(&format!("+{} lines · ctrl+o expands", head.hidden_rows())),
        "a failed call's cut preview names no key: {shut:?}"
    );

    assert!(transcript.toggle_expand(), "a failed call kept nothing to expand");
    let frame = tool_expand::Frame::new(transcript.expand_state(), &Bindings::default());
    let open = drawn(&tool_line::lines_with(
        &call,
        &crate::theme::Theme::default(),
        crate::theme::Symbols::default(),
        100,
        frame.retained("c1"),
    ));
    assert!(
        open.contains("row-200"),
        "the failure's kept output is not drawn: {open:?}"
    );
}

#[test]
fn an_expanded_call_draws_its_text_even_where_the_preview_repeated_the_error() {
    let mut transcript = Transcript::new();
    let output = format!("boom\n{}", big_output());
    transcript.tool_done("c1", "bash", Some(output.as_str()), Some("boom"));
    assert!(
        transcript.toggle_expand(),
        "the call that drew only its failure text kept nothing"
    );
    let call = match transcript.items().last() {
        Some(Item::Tool(call)) => call.clone(),
        other => panic!("the call was not recorded: {other:?}"),
    };
    let frame = tool_expand::Frame::new(transcript.expand_state(), &Bindings::default());
    let rows = tool_line::lines_with(
        &call,
        &crate::theme::Theme::default(),
        crate::theme::Symbols::default(),
        100,
        frame.retained("c1"),
    );
    assert!(
        drawn(&rows).contains("row-200"),
        "the whole output is not drawn: {}",
        drawn(&rows)
    );
}

#[test]
fn a_line_the_gate_decides_keeps_no_output_to_expand() {
    let mut transcript = Transcript::new();
    transcript.decide_request("req_9", crate::transcript::Decision::Denied);
    let output = big_output();
    let refusal = "Tool 'bash' needs approval before it runs: bash runs a process. Request req_9 is \
                   queued; answer it through the approval surface. Nothing was executed.";
    transcript.tool_done("c1", "bash", Some(output.as_str()), Some(refusal));
    assert!(!transcript.toggle_expand(), "a decided line kept text it never draws");
}

#[test]
fn a_finished_call_grows_a_held_view_by_the_rows_it_gained() {
    let mut app = app_with_lines(60);
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &big_output()));
    app.apply_event(&tool_start("c2"));
    draw_at(&mut app, 40, 12);
    assert_eq!(app.on_key(page_up()), None, "the view keeps its own key");
    let held = draw_at(&mut app, 40, 12);
    let before = app.scroll.offset();
    assert!(before > 0, "the view did not hold:\n{held}");
    assert!(!held.contains("new output"), "{held}");

    // The running line gains its head, three preview rows and its marker: four
    // rows the held view has to be pushed down by, so the rows on screen stay
    // where the reader left them.
    app.apply_event(&tool_done("c2", &big_output()));
    let after = draw_at(&mut app, 40, 12);
    assert_eq!(
        app.scroll.offset(),
        before + 4,
        "the rows the finished call gained did not move the held view:\n{after}"
    );
    assert_eq!(
        top_rows(&held, 6),
        top_rows(&after, 6),
        "the rows on screen moved when the call finished"
    );
    assert!(
        after.contains("new output"),
        "the indicator missed the rows that arrived:\n{after}"
    );
}

#[test]
fn the_key_label_costs_the_line_no_rows_at_any_width() {
    let mut transcript = Transcript::new();
    let output = big_output();
    transcript.tool_done("c1", "bash", Some(output.as_str()), None);
    let call = match transcript.items().last() {
        Some(Item::Tool(call)) => call.clone(),
        other => panic!("the call was not recorded: {other:?}"),
    };
    // The same call as the newest kept entry (it names the key) and as the one
    // before it (it does not); the label must cost it no rows either way.
    let mut newest = ExpandState::default();
    newest.record("c1", output.as_str());
    let mut older = ExpandState::default();
    older.record("c1", output.as_str());
    older.record("c2", "c\nd\n");
    let bindings = Bindings::default();
    let theme = crate::theme::Theme::default();
    for width in [16, 20, 40, 100] {
        let look = crate::item_lines::Look {
            theme: &theme,
            symbols: crate::theme::Symbols::default(),
            width,
            markdown: false,
            visual_blocks: true,
        };
        let rows = |state: &ExpandState| {
            let frame = tool_expand::Frame::new(state, &bindings);
            crate::wrap::wrap_rows(
                crate::item_lines::lines(&Item::Tool(call.clone()), &look, Some(&frame)),
                width,
            )
        };
        let labelled = rows(&newest);
        let plain = rows(&older);
        assert_eq!(
            labelled.len(),
            plain.len(),
            "the label changed the rows the line takes at width {width}: {labelled:?} vs {plain:?}"
        );
        let text: String = labelled
            .iter()
            .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
            .collect();
        // `  +197 lines · ctrl+o expands` is 29 cells, so it fits on the marker's
        // one row from 29 columns up and is dropped below that.
        if width >= 29 {
            assert!(
                text.contains("expands"),
                "the hint is missing at width {width}: {text:?}"
            );
        } else {
            assert!(
                !text.contains("expands"),
                "the hint was kept at width {width}, where it cannot fit one row: {text:?}"
            );
        }
    }
}

#[test]
fn an_expanded_call_is_drawn_from_the_rows_the_pane_shows() {
    let mut app = app();
    let output = long_output(1200);
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &output));
    assert_eq!(app.on_key(ctrl_o()), None, "the key asked for an action");
    // A frame at the end shows the last forty rows of a block of twelve hundred,
    // and building the other eleven hundred and sixty is the cost this pins.
    crate::wrap::reset_built();
    let screen = draw_at(&mut app, 120, 40);
    let built = crate::wrap::built_rows();
    assert!(built < 100, "a frame built {built} rows for forty: {screen}");
    assert!(screen.contains("output row 1199"), "the end is not on screen: {screen}");
    assert!(
        !screen.contains("output row 0000"),
        "a row off screen was built: {screen}"
    );
}

#[test]
fn a_held_view_builds_only_the_rows_it_shows() {
    let mut app = app();
    let output = long_output(1200);
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &output));
    assert_eq!(app.on_key(ctrl_o()), None, "the key asked for an action");
    // The first held frame takes the height it measures and the next draws
    // against it, so the rows below the view are counted and dropped by both.
    assert_eq!(app.on_key(page_up()), None, "the view keeps its own key");
    let _ = draw_at(&mut app, 120, 40);
    crate::wrap::reset_built();
    let screen = draw_at(&mut app, 120, 40);
    let built = crate::wrap::built_rows();
    assert!(built < 200, "a held frame built {built} rows for forty: {screen}");
    assert!(screen.contains("rows below"), "the held view said nothing: {screen}");
}

#[test]
fn the_selected_call_is_the_one_the_expand_key_opens() {
    let mut state = state_with(&[("c1", "a\nb\n"), ("c2", "c\nd\n")]);
    assert_eq!(state.selected(), Some("c2"), "the newest call is not the default");
    assert!(state.select("c1"), "a kept call could not be selected");
    assert_eq!(state.selected(), Some("c1"));
    assert!(state.toggle());
    assert!(state.is_expanded("c1"), "the selected earlier call did not open");
    assert!(!state.is_expanded("c2"), "an unselected call opened too");
    let frame = tool_expand::Frame::new(&state, &Bindings::default());
    assert!(
        frame.retained("c1").selected,
        "the open call is not marked as the selection"
    );
    assert!(
        !frame.retained("c2").selected,
        "another call was marked as the selection"
    );
}

#[test]
fn a_selection_the_reader_moved_survives_the_next_result() {
    let mut state = state_with(&[("c1", "a\nb\n")]);
    assert!(state.select("c1"));
    state.record("c2", "c\nd\n");
    assert_eq!(
        state.selected(),
        Some("c1"),
        "a result arriving below a held view took the selection away"
    );
    state.follow_newest();
    assert_eq!(state.selected(), Some("c2"), "following the end kept an old selection");
}

#[test]
fn the_rows_in_view_decide_which_result_is_selected() {
    let mut transcript = Transcript::new();
    let output = big_output();
    transcript.tool_done("c1", "bash", Some(output.as_str()), None);
    for index in 0..5 {
        transcript.push_notice(format!("notice {index}"));
    }
    transcript.tool_done("c2", "bash", Some(output.as_str()), None);
    // c1 takes five rows, each notice one, and c2 another five.
    let heights = [5, 1, 1, 1, 1, 1, 5];
    transcript.select_tool_at(&heights, 0, 4);
    assert_eq!(transcript.expand_state().selected(), Some("c1"));
    transcript.select_tool_at(&heights, 6, 2);
    assert_eq!(
        transcript.expand_state().selected(),
        None,
        "a window showing only notices still selected a call"
    );
    transcript.select_tool_at(&heights, 10, 2);
    assert_eq!(transcript.expand_state().selected(), Some("c2"));
    // A stale measurement is not read against items it never saw.
    transcript.select_tool_at(&heights[..3], 0, 4);
    assert_eq!(transcript.expand_state().selected(), Some("c2"));
}

#[test]
fn an_earlier_result_opens_and_folds_without_moving_the_view() {
    let mut app = app_with_lines(5);
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &big_output()));
    app.transcript.push_notice("between the calls");
    app.apply_event(&tool_start("c2"));
    app.apply_event(&tool_done("c2", &big_output()));
    let _ = draw_at(&mut app, 60, 16);
    assert_eq!(app.on_key(page_up()), None, "the view keeps its own key");
    let held = draw_at(&mut app, 60, 16);
    let before_top = top_rows(&held, 7);
    let before_offset = app.scroll.offset();
    assert!(before_offset > 0, "the view did not hold:\n{held}");
    assert_eq!(
        app.transcript.expand_state().selected(),
        Some("c2"),
        "the newest call stopped being the selection before a key was pressed:\n{held}"
    );

    // The frame just drawn decides which call is on screen: the earlier one,
    // not the newest one the selection still named.
    assert_eq!(app.on_key(ctrl_o()), None);
    let open = draw_at(&mut app, 60, 16);
    assert!(
        app.transcript.expand_state().is_expanded("c1"),
        "the earlier result on screen did not open"
    );
    assert!(
        !app.transcript.expand_state().is_expanded("c2"),
        "the newest result opened instead of the one on screen"
    );
    assert_eq!(
        top_rows(&open, 7),
        before_top,
        "opening the result moved the view:\n{open}"
    );
    assert!(
        app.scroll.offset() > before_offset,
        "the rows the open result added did not move the held offset:\n{open}"
    );
    let open_offset = app.scroll.offset();

    assert_eq!(app.on_key(ctrl_o()), None);
    let shut = draw_at(&mut app, 60, 16);
    assert!(
        !app.transcript.expand_state().is_expanded("c1"),
        "the second press did not fold the result"
    );
    assert_eq!(
        top_rows(&shut, 7),
        before_top,
        "folding the result moved the view:\n{shut}"
    );
    assert!(
        app.scroll.offset() < open_offset,
        "the folded result did not give its rows back"
    );
}

#[test]
fn a_block_that_changes_above_the_view_leaves_its_offset_alone() {
    let mut scroll = crate::scroll::Scroll::new();
    scroll.up(1);
    scroll.measure(20, 3, [0, 0], 2, |_| 2);
    assert_eq!(scroll.offset(), 1);

    // The first block grows by eight rows, all above the two rows on screen.
    // The rows being read move down with the block, but the view shows the same
    // rows, which is exactly what leaving the offset alone does.
    scroll.reflow();
    scroll.measure(20, 3, [0, 0], 2, |index| if index == 0 { 10 } else { 2 });
    assert_eq!(scroll.offset(), 1, "a block growing above the view moved it");

    // Hiding it again has the same answer in the other direction.
    scroll.reflow();
    scroll.measure(20, 3, [0, 0], 2, |_| 2);
    assert_eq!(scroll.offset(), 1, "a block shrinking above the view moved it");
}

#[test]
fn a_block_that_changes_below_the_view_moves_its_offset_by_that_change() {
    let mut scroll = crate::scroll::Scroll::new();
    scroll.up(1);
    scroll.measure(20, 3, [0, 0], 2, |_| 2);
    assert_eq!(scroll.offset(), 1);

    // The last block grows by eight rows below the view's top edge: those rows
    // sit between the view and the end, so the offset grows with them.
    scroll.reflow();
    scroll.measure(20, 3, [0, 0], 2, |index| if index == 2 { 10 } else { 2 });
    assert_eq!(scroll.offset(), 9, "rows below the view did not push the offset");
}

#[test]
fn the_activity_setting_hides_and_restores_the_finished_calls() {
    let mut app = app();
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &big_output()));
    assert!(draw_to_string(&mut app).contains("ctrl+o expands"));

    app.refresh_settings(&settings_with(&[("HORON_TUI_TRANSCRIPT_SHOW_TOOL_ACTIVITY", "false")]));
    let hidden = draw_to_string(&mut app);
    assert!(!hidden.contains("bash"), "the setting did not hide the call:\n{hidden}");

    app.refresh_settings(&settings_with(&[("HORON_TUI_TRANSCRIPT_SHOW_TOOL_ACTIVITY", "true")]));
    let shown = draw_to_string(&mut app);
    assert!(
        shown.contains("ctrl+o expands"),
        "the kept output was dropped with the hidden line:\n{shown}"
    );
}

#[test]
fn hiding_tool_activity_keeps_the_running_call_and_the_parked_approval() {
    let mut app = app();
    app.apply_event(&tool_start("c1"));
    app.apply_event(&tool_done("c1", &big_output()));
    assert!(draw_to_string(&mut app).contains("bash"));

    assert_eq!(app.on_key(alt_t()), None);
    let hidden = draw_to_string(&mut app);
    assert!(!hidden.contains("bash"), "a finished call was still drawn:\n{hidden}");
    assert!(
        matches!(
            app.transcript.items().last(),
            Some(Item::Tool(call)) if call.state == crate::transcript::ToolState::Done
        ),
        "hiding the line stopped the result from reaching the transcript"
    );

    app.apply_event(&tool_start("c2"));
    let running = draw_to_string(&mut app);
    assert!(running.contains("bash"), "the running call vanished:\n{running}");

    let refusal = "Tool 'bash' needs approval before it runs: bash runs a process. Request req_1 is \
                   queued; answer it through the approval surface. Nothing was executed.";
    app.transcript
        .tool_done("c2", "bash", Some(big_output().as_str()), Some(refusal));
    app.transcript.park_request("req_1");
    let waiting = draw_to_string(&mut app);
    assert!(
        waiting.contains("needs approval"),
        "the parked approval vanished:\n{waiting}"
    );
}

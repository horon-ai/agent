//! What the `/context` panel reads and how it lays itself out.

use serde_json::json;

use crate::context::ContextReport;
use crate::item_lines::{self, Look};
use crate::theme::{Symbols, Theme};
use crate::wrap;

/// A payload shaped like the daemon's, with the numbers filled in.
fn payload(system: u64, tools: u64, messages: u64, window: u64) -> serde_json::Value {
    json!({
        "session_id": "session_x",
        "messages": [],
        "context_window": window,
        "tokens": {
            "system": { "static": system, "dynamic": 0, "total": system },
            "tools": {
                "eager": tools,
                "deferred": 0,
                "total": tools,
                "count": 18,
                "deferred_count": 0,
            },
            "messages": { "total": messages, "count": 4 },
            "context_window": window,
        },
    })
}

/// The panel's rows as plain text, wrapped the way the pane wraps them.
fn drawn(report: &ContextReport, width: u16) -> Vec<String> {
    let theme = Theme::default();
    let look = Look {
        theme: &theme,
        symbols: Symbols::default(),
        width,
        markdown: true,
    };
    wrap::wrap_rows(crate::context::lines(report, &look), width)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect()
}

#[test]
fn a_payload_without_the_numbers_yields_no_report() {
    // An older daemon answers with the data it has and no breakdown: the caller
    // must fall back to the command's own message rather than draw an empty
    // panel of zeros.
    assert_eq!(ContextReport::from_data(&json!({"session_id": "session_x"})), None);
    assert_eq!(ContextReport::from_data(&json!({"context_window": 200000})), None);
    assert!(ContextReport::from_data(&payload(10, 20, 30, 1000)).is_some());
}

#[test]
fn the_report_keeps_the_numbers_the_daemon_sent() {
    let report = ContextReport::from_data(&payload(3_120, 4_240, 1_340, 200_000)).unwrap();
    assert_eq!(report.system, 3_120);
    assert_eq!(report.tools, 4_240);
    assert_eq!(report.tool_count, 18);
    assert_eq!(report.messages, 1_340);
    assert_eq!(report.message_count, 4);
    assert_eq!(report.window, 200_000);
    assert_eq!(report.used(), 8_700);
    assert_eq!(report.percent(), Some(4));
}

#[test]
fn the_panel_names_every_section_and_fits_the_pane() {
    let report = ContextReport::from_data(&payload(3_120, 4_240, 1_340, 200_000)).unwrap();
    let rows = drawn(&report, 80);
    let text = rows.join("\n");
    assert!(rows[0].starts_with("context · ~8.7k of 200.0k tokens (4%)"), "{text}");
    for label in ["system prompt", "tools · 18", "messages · 4", "free"] {
        assert!(text.contains(label), "{label} missing from {text}");
    }
    // One bar's worth of cells per row: the sections a request spends and the
    // window nobody claimed.
    assert!(text.contains(Symbols::default().bar_full), "{text}");
    assert!(text.contains(Symbols::default().bar_empty), "{text}");
    for row in &rows {
        let width = unicode_width::UnicodeWidthStr::width(row.as_str());
        assert!(width <= 80, "row {row:?} is {width} cells wide");
    }
}

#[test]
fn a_narrow_pane_drops_the_bar_and_keeps_the_counts() {
    let report = ContextReport::from_data(&payload(3_120, 4_240, 1_340, 200_000)).unwrap();
    let rows = drawn(&report, 24);
    let text = rows.join("\n");
    assert!(!text.contains(Symbols::default().bar_full), "{text}");
    assert!(text.contains("3.1k"), "{text}");
    for row in &rows {
        let width = unicode_width::UnicodeWidthStr::width(row.as_str());
        assert!(width <= 24, "row {row:?} is {width} cells wide");
    }
}

#[test]
fn an_unknown_window_has_no_percentage_and_no_free_row() {
    // A build that cannot name the window still draws the numbers it has; a
    // percentage of nothing would be a lie, and a free row of nothing a gap.
    let report = ContextReport::from_data(&payload(3_120, 4_240, 1_340, 0)).unwrap();
    assert_eq!(report.percent(), None);
    let text = drawn(&report, 80).join("\n");
    assert!(text.contains("~8.7k tokens in play"), "{text}");
    assert!(!text.contains('%'), "{text}");
    assert!(!text.contains("free"), "{text}");
    assert!(!text.contains(Symbols::default().bar_empty), "{text}");
}

#[test]
fn a_report_near_the_limit_says_so() {
    let report = ContextReport::from_data(&payload(90_000, 60_000, 40_000, 200_000)).unwrap();
    assert_eq!(report.percent(), Some(95));
    let text = drawn(&report, 80).join("\n");
    assert!(text.contains("near the limit"), "{text}");
}

#[test]
fn the_count_of_deferred_tools_is_drawn_beside_the_tool_row() {
    let mut data = payload(10, 20, 30, 1_000);
    data["tokens"]["tools"]["deferred"] = json!(5);
    data["tokens"]["tools"]["total"] = json!(25);
    data["tokens"]["tools"]["deferred_count"] = json!(3);
    let report = ContextReport::from_data(&data).unwrap();
    assert_eq!(report.tools, 25);
    let text = drawn(&report, 80).join("\n");
    assert!(text.contains("tools · 18 (3 deferred)"), "{text}");
}

#[test]
fn a_report_is_drawn_above_the_rest_of_the_conversation() {
    // The panel is one item, so it is measured and scrolled as one block: this
    // pins the item count the scrollback's mark counts.
    let mut transcript = crate::transcript::Transcript::new();
    transcript.push_user("what fills the window?");
    let report = ContextReport::from_data(&payload(10, 20, 30, 1_000)).unwrap();
    transcript.push_context(report);
    let items = transcript.items();
    assert_eq!(items.len(), 2);
    assert!(matches!(items[1], crate::transcript::Item::Context(_)));
    // The item's own lines are what the pane measures, so a render must produce
    // the same rows the measurement sees.
    let theme = Theme::default();
    let look = Look {
        theme: &theme,
        symbols: Symbols::default(),
        width: 60,
        markdown: true,
    };
    assert!(item_lines::lines(&items[1], &look).len() > 1);
}

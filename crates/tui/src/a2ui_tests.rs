use std::sync::mpsc;

use horon_agent_sdk::HistoryMessage;
use horon_agent_sdk::api::ApiEvent;
use horon_agent_tool_types::a2ui::A2UI_BASIC_CATALOG_V1_0;
use serde_json::json;

use crate::app::App;
use crate::connect_options;
use crate::item_lines::{self, Look};
use crate::sdk_bridge::{AttachedSession, Incoming};
use crate::theme::{SymbolPreset, Symbols, Theme};
use crate::transcript::Item;
use crate::wrap;

fn app() -> App {
    let (_tx, rx) = mpsc::channel::<Incoming>();
    App::new("socket · s1".to_string(), rx)
}

fn look_with(width: u16, symbols: Symbols) -> Look<'static> {
    let theme = Box::leak(Box::new(Theme::default()));
    Look {
        theme,
        symbols,
        width,
        markdown: true,
        visual_blocks: true,
    }
}

fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[test]
fn default_tui_connect_options_keep_a2ui_unnegotiated() {
    let options = connect_options(None);
    assert!(options.a2ui.is_none());
}

#[test]
fn live_a2ui_envelopes_coalesce_in_sequence_order_and_ignore_other_sessions() {
    let mut app = app();
    app.session_id = Some("s1".to_string());

    app.apply_event(&ApiEvent::A2uiEnvelope {
        session_id: "s1".into(),
        tool_call_id: "call-1".into(),
        sequence: 1,
        envelope: json!({
            "version": "v0.9",
            "updateComponents": {
                "surfaceId": "deploy",
                "components": [
                    { "id": "root", "component": "Column", "children": ["title", "btn"] },
                    { "id": "title", "component": "Text", "text": "Approve\x1b[31m deploy\x1b[0m" },
                    { "id": "btn", "component": "Button", "child": "Confirm", "action": { "event": { "name": "confirm" } } }
                ]
            }
        }),
        fallback_text: Some("Approve deployment".into()),
    });

    app.apply_event(&ApiEvent::A2uiEnvelope {
        session_id: "s1".into(),
        tool_call_id: "call-1".into(),
        sequence: 0,
        envelope: json!({
            "version": "v0.9",
            "createSurface": { "surfaceId": "deploy", "catalogId": A2UI_BASIC_CATALOG_V1_0 }
        }),
        fallback_text: None,
    });

    // Duplicate sequence updates cleanly without adding another transcript item.
    app.apply_event(&ApiEvent::A2uiEnvelope {
        session_id: "s1".into(),
        tool_call_id: "call-1".into(),
        sequence: 0,
        envelope: json!({
            "version": "v0.9",
            "createSurface": { "surfaceId": "deploy", "catalogId": A2UI_BASIC_CATALOG_V1_0 }
        }),
        fallback_text: None,
    });

    // Stale event from a different session is dropped.
    app.apply_event(&ApiEvent::A2uiEnvelope {
        session_id: "s-other".into(),
        tool_call_id: "call-2".into(),
        sequence: 0,
        envelope: json!({
            "version": "v0.9",
            "createSurface": { "surfaceId": "foreign", "catalogId": A2UI_BASIC_CATALOG_V1_0 }
        }),
        fallback_text: Some("Foreign".into()),
    });

    let cards: Vec<_> = app
        .transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::A2ui(card) => Some(card),
            _ => None,
        })
        .collect();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].call_id, "call-1");
    assert_eq!(cards[0].sequences.len(), 2);
    assert_eq!(cards[0].lines[0], "[A2UI surface: deploy]");
    assert!(cards[0].lines.iter().any(|l| l == "Approve deploy"));
    assert!(cards[0].lines.iter().any(|l| l == "[Button Confirm -> confirm]"));

    let wide = look_with(80, SymbolPreset::Unicode.symbols());
    let rendered = wrap::wrap_rows(item_lines::lines(&Item::A2ui(cards[0].clone()), &wide, None), 80);
    let rows: Vec<String> = rendered.iter().map(line_text).collect();
    assert!(rows[0].contains("[A2UI surface: deploy] · read-only"));
    assert!(rows.iter().all(|row| !row.contains('\x1b')));

    let narrow_ascii = look_with(24, SymbolPreset::Ascii.symbols());
    let narrow_rows: Vec<String> = wrap::wrap_rows(
        item_lines::lines(&Item::A2ui(cards[0].clone()), &narrow_ascii, None),
        24,
    )
    .iter()
    .map(line_text)
    .collect();
    assert!(narrow_rows[0].starts_with("| [A2UI surface:"));
    assert!(!narrow_rows[0].contains("read-only"));
}

#[test]
fn attach_session_splits_stored_a2ui_cards_from_prose_and_ignores_fenced_examples() {
    let mut app = app();
    app.attach_session(
        AttachedSession {
            description: "socket · s2".to_string(),
            session_id: "s2".to_string(),
            status: "idle".to_string(),
            model: None,
            effort: None,
            title: None,
            history: vec![
                HistoryMessage {
                    role: "assistant".to_string(),
                    content: "```text\n[A2UI surface: example]\nNot a live card\n```".to_string(),
                    response_stats: None,
                    answer: None,
                    message_id: Some("msg-code".to_string()),
                },
                HistoryMessage {
                    role: "assistant".to_string(),
                    content: "Review before continuing.\n[A2UI surface: review]\nDeployment target\n- TextField: Environment\n[Button Ship -> ship]\n[A2UI closed: old]".to_string(),
                    response_stats: None,
                    answer: None,
                    message_id: Some("msg-card".to_string()),
                },
            ],
        },
        false,
    );

    let items = app.transcript.items();
    let assistants: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            Item::Assistant(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistants,
        vec![
            "```text\n[A2UI surface: example]\nNot a live card\n```",
            "Review before continuing.",
        ]
    );

    let cards: Vec<&[String]> = items
        .iter()
        .filter_map(|item| match item {
            Item::A2ui(card) => Some(card.lines.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(cards.len(), 2);
    assert_eq!(
        cards[0],
        &[
            "[A2UI surface: review]",
            "Deployment target",
            "- TextField: Environment",
            "[Button Ship -> ship]",
        ]
    );
    assert_eq!(cards[1], &["[A2UI closed: old]"]);
    assert_eq!(app.transcript.stored_ids().len(), items.len());
}

#[test]
fn oversized_a2ui_card_caps_visible_lines() {
    let mut raw = String::from("[A2UI surface: big]");
    for idx in 0..60 {
        raw.push_str(&format!("\n- TextField: Field {idx}"));
    }
    let card = super::A2uiCard::from_rendered_block(&raw);
    let look = look_with(80, SymbolPreset::Unicode.symbols());
    let rendered = super::lines(&card, &look);
    assert!(rendered.len() <= 37);
    let last = line_text(rendered.last().expect("summary line").line());
    assert!(last.contains("more lines"), "expected truncation marker, got {last}");
}

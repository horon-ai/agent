//! Status-line tests for the reasoning effort beside the model.
//!
//! The effort is part of the model segment: it is drawn with the separator the
//! preset uses, dropped when the daemon reports none, and given up before the
//! model name is cut when the line is too narrow.

use std::sync::mpsc;

use horon_agent_sdk::api::ApiEvent;

use super::{Layout, SegmentId, StatusData, render};
use crate::app::App;
use crate::sdk_bridge::Incoming;
use crate::theme::{SymbolPreset, Symbols, Theme};

fn only_model() -> Layout {
    Layout {
        left: vec![SegmentId::Model],
        right: Vec::new(),
        ..Layout::default()
    }
}

fn data(effort: Option<&'static str>) -> StatusData<'static> {
    StatusData {
        provider: Some("anthropic"),
        model: Some("claude-sonnet-4"),
        effort,
        ..StatusData::default()
    }
}

fn rendered(data: &StatusData<'_>, symbols: Symbols, width: u16) -> String {
    render(data, &only_model(), &Theme::default(), symbols, width).to_string()
}

#[test]
fn effort_shown_after_model_info() {
    let (_sender, receiver) = mpsc::channel::<Incoming>();
    let mut app = App::new("socket · session".to_string(), receiver);
    app.apply_event(&ApiEvent::ModelInfo {
        session_id: "s1".into(),
        provider: Some("anthropic".into()),
        model: Some("claude-sonnet-4".into()),
        reasoning_effort: Some("high".into()),
        auth_method: None,
    });
    let line = render(
        &app.status_data(),
        &Layout::default(),
        &Theme::default(),
        Symbols::default(),
        100,
    )
    .to_string();
    assert!(
        line.contains("anthropic/claude-sonnet-4 · high"),
        "the effort is missing from the model segment: {line:?}"
    );

    // A later report without an effort clears it: the effort belongs to the
    // model the daemon just named, not to the one before it.
    app.apply_event(&ApiEvent::ModelInfo {
        session_id: "s1".into(),
        provider: Some("anthropic".into()),
        model: Some("claude-sonnet-4".into()),
        reasoning_effort: None,
        auth_method: None,
    });
    let line = render(
        &app.status_data(),
        &Layout::default(),
        &Theme::default(),
        Symbols::default(),
        100,
    )
    .to_string();
    assert!(!line.contains("high"), "a cleared effort was drawn: {line:?}");
    assert!(
        line.contains("anthropic/claude-sonnet-4"),
        "the model name went with the effort: {line:?}"
    );
}

#[test]
fn effort_uses_the_preset_separator() {
    assert_eq!(
        rendered(&data(Some("high")), Symbols::default(), 80),
        "anthropic/claude-sonnet-4 · high"
    );
    assert_eq!(
        rendered(&data(Some("high")), SymbolPreset::Ascii.symbols(), 80),
        "anthropic/claude-sonnet-4 | high"
    );
}

#[test]
fn effort_omitted_when_unknown_or_blank() {
    let expected = "anthropic/claude-sonnet-4";
    assert_eq!(rendered(&data(None), Symbols::default(), 80), expected);
    // An empty or blank effort is no effort: drawn as it is, it would leave a
    // separator with nothing after it.
    assert_eq!(rendered(&data(Some("")), Symbols::default(), 80), expected);
    assert_eq!(rendered(&data(Some("  ")), Symbols::default(), 80), expected);
}

#[test]
fn effort_is_dropped_before_the_model_name_is_cut() {
    // "anthropic/claude-sonnet-4" is 25 cells, so the segment with " · high"
    // is 32; one cell less than that loses the effort alone.
    assert_eq!(
        rendered(&data(Some("high")), Symbols::default(), 32),
        "anthropic/claude-sonnet-4 · high"
    );
    assert_eq!(
        rendered(&data(Some("high")), Symbols::default(), 31),
        "anthropic/claude-sonnet-4"
    );
    // The model name still fits whole, so dropping the effort is all the line
    // gives up here.
    assert_eq!(
        rendered(&data(Some("high")), Symbols::default(), 25),
        "anthropic/claude-sonnet-4"
    );

    // A long effort does not get to push the model name out of the segment.
    let long = "x".repeat(80);
    let long_data = StatusData {
        provider: Some("anthropic"),
        model: Some("claude-sonnet-4"),
        effort: Some(&long),
        ..StatusData::default()
    };
    let text = rendered(&long_data, Symbols::default(), 31);
    assert_eq!(text, "anthropic/claude-sonnet-4");
    assert!(!text.contains('·'), "a dangling separator survived: {text:?}");
}

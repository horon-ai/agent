//! What a runtime command's structured answer reads as once drawn.
//!
//! The daemon answers most slash commands with its payload serialized as JSON.
//! These tests pin the shape that replaces it: a titled block of labelled rows,
//! with sections, readable booleans, hidden secrets and wrapped long values.

use horon_agent_sdk::RuntimeCommand;
use serde_json::{Value, json};

use crate::item_lines::Look;
use crate::report::Report;
use crate::theme::{Symbols, Theme};
use crate::wrap;

/// The rows of a report as plain text, wrapped the way the pane wraps them.
fn drawn(report: &Report, width: u16) -> Vec<String> {
    let theme = Theme::default();
    let look = Look {
        theme: &theme,
        symbols: Symbols::default(),
        width,
        markdown: true,
        visual_blocks: true,
    };
    wrap::wrap_rows(crate::report::lines(report, &look), width)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect()
}

/// The payload `/auth` answers with, as the daemon builds it.
fn auth_data() -> Value {
    json!({
        "scope": "runtime host",
        "openai_compatible_credentials": true,
        "profiles": [{
            "profile": "opencode",
            "base_url": "https://opencode.ai/v1",
            "default_model": "gpt-5",
            "api_key_env": "OPENCODE_API_KEY",
        }],
        "note": "Model requests go directly to the OpenAI-compatible profiles in config.toml. Secrets and raw credential files are never returned.",
    })
}

/// The fenced message the daemon sends beside that payload.
fn auth_message(data: &Value) -> String {
    format!(
        "# Runtime authentication\n\n```json\n{}\n```",
        serde_json::to_string_pretty(data).unwrap()
    )
}

#[test]
fn a_fenced_payload_is_drawn_as_labelled_rows() {
    let data = auth_data();
    let report = Report::from_answer(RuntimeCommand::Auth, &data, &auth_message(&data)).unwrap();
    assert_eq!(report.title(), "runtime authentication");
    let rows = drawn(&report, 80);
    let text = rows.join("\n");
    for expected in [
        "openai compatible credentials",
        "yes",
        "profiles · 1",
        "opencode",
        "base url",
        "default model",
        "api key env",
        "OPENCODE_API_KEY",
        "scope",
        "runtime host",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    assert!(!text.contains('{'), "the payload is still JSON:\n{text}");
    // A note is a sentence about the fields, so it reads after them.
    let note = rows
        .iter()
        .position(|row| row.contains("Model requests go directly"))
        .expect("the note is drawn");
    let scope = rows
        .iter()
        .position(|row| row.contains("runtime host"))
        .expect("scope is drawn");
    assert!(note > scope, "the note came before the fields:\n{text}");
}

#[test]
fn a_message_that_is_prose_keeps_its_own_words() {
    // `/skills` writes a markdown report and `/plan` a sentence: drawing their
    // payload as rows would throw away the wording they chose.
    assert!(
        Report::from_answer(
            RuntimeCommand::Skills,
            &json!({"skills": []}),
            "## Loaded skills\n\nNo skills loaded.",
        )
        .is_none()
    );
    assert!(Report::from_answer(RuntimeCommand::Plan, &json!({"enabled": true}), "Plan mode is on.").is_none());
}

#[test]
fn a_secret_value_is_hidden_while_the_variable_that_holds_it_stays_named() {
    let data = json!({
        "api_key": "sk-live-123456",
        "api_key_env": "OPENAI_API_KEY",
        "token": "abcdef",
        "profile": "main",
    });
    let report = Report::from_answer(RuntimeCommand::Logout, &data, &serde_json::to_string(&data).unwrap()).unwrap();
    let text = drawn(&report, 80).join("\n");
    assert!(!text.contains("sk-live-123456"), "{text}");
    assert!(!text.contains("abcdef"), "{text}");
    assert_eq!(text.matches("— hidden").count(), 2, "{text}");
    assert!(
        text.contains("OPENAI_API_KEY"),
        "the env name is not the secret:\n{text}"
    );
}

#[test]
fn booleans_and_absences_read_as_words() {
    let data = json!({
        "enabled": false,
        "config_default": true,
        "swarm_id": null,
        "role": null,
    });
    let report = Report::from_answer(
        RuntimeCommand::Swarm,
        &data,
        &serde_json::to_string_pretty(&data).unwrap(),
    )
    .unwrap();
    let rows = drawn(&report, 60);
    let text = rows.join("\n");
    let value_of = |label: &str| {
        rows.iter()
            .find(|row| row.trim_start().starts_with(label))
            .map(|row| row.trim_end().rsplit("  ").next().unwrap_or_default().to_string())
    };
    assert_eq!(value_of("config default").as_deref(), Some("yes"), "{text}");
    assert_eq!(value_of("enabled").as_deref(), Some("no"), "{text}");
    assert_eq!(value_of("role").as_deref(), Some("—"), "{text}");
    assert_eq!(value_of("swarm id").as_deref(), Some("—"), "{text}");
    assert!(!text.contains("null"), "a null value is drawn as an absence:\n{text}");
}

#[test]
fn a_long_value_wraps_under_its_own_column() {
    let data = json!({
        "approval_mode": "yolo",
        "profiles": [{
            "profile": "remote",
            "base_url": "https://example.invalid/a/very/long/path/that/will/not/fit/in/forty/columns",
        }],
    });
    let report = Report::from_answer(RuntimeCommand::Info, &data, &serde_json::to_string(&data).unwrap()).unwrap();
    let rows = drawn(&report, 40);
    assert!(rows.iter().all(|row| row.chars().count() <= 40), "{rows:?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("approval mode") && row.ends_with("yolo")),
        "{rows:?}"
    );
    let head = rows
        .iter()
        .position(|row| row.contains("base url"))
        .expect("the label is drawn");
    let joined: String = rows.iter().map(|row| row.trim()).collect();
    assert!(joined.contains("https://example.invalid/a/very/long/path/that/will/not/fit/in/forty/columns"));
    // A value the pane cannot fit is broken under the column its label opened,
    // which is where the continuation rows keep their text.
    let value_rows: Vec<&String> = rows[head + 1..].iter().collect();
    assert!(!value_rows.is_empty(), "the value did not wrap: {rows:?}");
    for row in value_rows {
        let indent = row.len() - row.trim_start().len();
        assert_eq!(indent, 14, "a value row left its column: {row:?}");
    }
}

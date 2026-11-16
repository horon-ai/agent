//! A structured line keeps its text.
//!
//! The line is assembled in one buffer with borrowed keys and reused values.
//! These tests hold a copy of the assembly it replaced and compare the two
//! byte for byte, in text and in JSON mode, and pin the lines whose text
//! depends on a rule (duplicate keys, quoting, cuts, redaction) as literals.

use super::{format_structured_event_as, sanitize_log_value, truncate};
use std::collections::BTreeMap;

/// The assembly this crate used before the one-buffer version.
fn previous_line(json: bool, event: &str, fields: &[(String, String)]) -> String {
    let event = sanitize_log_value(event);
    let mut ordered = BTreeMap::new();
    for (raw_key, value) in fields {
        ordered.insert(previous_key(raw_key), previous_redaction(raw_key, value));
    }

    if json {
        let mut object = serde_json::Map::new();
        object.insert("event".to_string(), serde_json::Value::String(event));
        for (key, value) in ordered {
            object.insert(key, serde_json::Value::String(value));
        }
        return format!("EVENT_JSON {}", serde_json::Value::Object(object));
    }

    let mut parts = Vec::with_capacity(ordered.len() + 1);
    parts.push(format!("event={}", previous_field_value(&event)));
    parts.extend(
        ordered
            .into_iter()
            .map(|(key, value)| format!("{}={}", key, previous_field_value(&value))),
    );
    format!("EVENT {}", parts.join(" "))
}

fn previous_key(key: &str) -> String {
    let sanitized: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "field".to_string()
    } else {
        truncate(&sanitized, 80)
    }
}

fn previous_field_value(value: &str) -> String {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '=' || c == '"' || c == '\'')
    {
        serde_json::to_string(value).unwrap_or_else(|_| "\"<unserializable>\"".to_string())
    } else {
        value.to_string()
    }
}

fn previous_redaction(key: &str, value: &str) -> String {
    let key = key.to_ascii_lowercase();
    if key.contains("token")
        || key.contains("secret")
        || key.contains("key")
        || key.contains("credential")
        || key.contains("callback")
        || key.contains("authorization")
        || key.contains("auth_code")
        || key.contains("oauth_code")
        || key.contains("code_verifier")
        || key.contains("code_challenge")
    {
        return "<redacted>".to_string();
    }
    sanitize_log_value(value)
}

fn owned(fields: &[(&str, &str)]) -> Vec<(String, String)> {
    fields
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn line(json: bool, event: &str, fields: &[(&str, &str)]) -> String {
    format_structured_event_as(json, event, fields.iter().copied())
}

/// Every field of the two lines a tool dispatch writes, more than twenty of
/// them, in the order the dispatcher gives them.
fn tool_lifecycle_fields() -> Vec<(String, String)> {
    owned(&[
        ("phase", "done"),
        ("tool_name", "read"),
        ("resolved_tool_name", "read"),
        ("session_id", "session_0123456789abcdef"),
        ("message_id", "msg_1"),
        ("tool_call_id", "call_1"),
        ("execution_mode", "Direct"),
        ("cwd", "/tmp/work dir"),
        ("input_json_bytes", "42"),
        ("input_keys", "file_path,limit"),
        ("touched_paths", "file_path:/tmp/work dir/a.txt"),
        ("touched_path_count", "1"),
        ("command_bytes", "12"),
        ("command_chars", "12"),
        ("elapsed_ms", "3"),
        ("execute_ms", "2"),
        ("hook_transform_ms", "0"),
        ("hook_gate_ms", "0"),
        ("post_feedback_ms", "0"),
        ("overflow_guard_ms", "0"),
        ("output_bytes", "120"),
        ("output_chars", "118"),
        ("image_count", "0"),
    ])
}

fn cases() -> Vec<(String, Vec<(String, String)>)> {
    let exact = "v".repeat(160);
    let over = "v".repeat(161);
    let multibyte_over = format!("a{}", "é".repeat(80));
    let long_with_break = format!("{}\n{}", "x".repeat(100), "y".repeat(100));
    let long_key = "x".repeat(81);
    let exact_key = "x".repeat(80);
    let mut cases = vec![
        (
            "plain".to_string(),
            owned(&[("a", "1"), ("b", "two"), ("c.d-e_f", "3")]),
        ),
        ("no_fields".to_string(), Vec::new()),
        (
            "unsorted".to_string(),
            owned(&[("z", "last"), ("m", "mid"), ("a", "first")]),
        ),
        (
            "case_order".to_string(),
            owned(&[("b", "1"), ("B", "2"), ("a", "3"), ("A", "4")]),
        ),
        (
            "raw_duplicates".to_string(),
            owned(&[("b", "2"), ("a", "1"), ("b", "3")]),
        ),
        ("sanitized_duplicates".to_string(), owned(&[("a b", "1"), ("a_b", "2")])),
        (
            "sanitized_duplicates_reversed".to_string(),
            owned(&[("a_b", "2"), ("a b", "1")]),
        ),
        (
            "triple_duplicates".to_string(),
            owned(&[("k1", "a"), ("k1", "b"), ("k0", "c"), ("k1", "d")]),
        ),
        ("event_field".to_string(), owned(&[("event", "inner"), ("a", "1")])),
        (
            "quoting".to_string(),
            owned(&[
                ("empty", ""),
                ("space", "a b"),
                ("eq", "x=y"),
                ("double", "say \"hi\""),
                ("single", "it's"),
                ("nl", "one\ntwo"),
                ("cr", "one\rtwo"),
                ("tab", "one\ttwo"),
                ("backslash", "a\\b"),
                ("unicode_space", "a\u{00a0}b"),
            ]),
        ),
        (
            "urls".to_string(),
            owned(&[
                ("url", "https://example.test/cb?code=secret&state=abc"),
                ("plain_url", "https://example.test/path"),
                ("http", "see http://example.test/a?b=c now"),
                ("question", "is this a question?"),
                ("glued", "failed\nhttps://login.example.test/cb?code=secret"),
            ]),
        ),
        (
            "cuts".to_string(),
            vec![
                ("exact".to_string(), exact),
                ("over".to_string(), over),
                ("multibyte".to_string(), multibyte_over),
                ("break".to_string(), long_with_break),
            ],
        ),
        (
            "credentials".to_string(),
            owned(&[
                ("API_KEY", "sk-secret"),
                ("Authorization", "Bearer abc"),
                ("OAuth_Code", "c"),
                ("refreshToken", "t"),
                ("client_SECRET", "s"),
                ("Credential", "c"),
                ("callback_url", "https://example.test/?code=secret"),
                ("code_verifier", "v"),
                ("CODE_CHALLENGE", "c"),
                ("auth_code", "a"),
                ("exit_code", "127"),
                ("tok", "not a marker"),
                ("ke", "y"),
            ]),
        ),
        (
            "keys".to_string(),
            vec![
                (String::new(), "empty key".to_string()),
                ("a/b c".to_string(), "v".to_string()),
                ("né".to_string(), "v".to_string()),
                ("한글".to_string(), "v".to_string()),
                (exact_key, "v".to_string()),
                (long_key, "v".to_string()),
            ],
        ),
        ("multi\nline\tevent".to_string(), owned(&[("a", "1")])),
        ("https://example.test/hook?token=abc".to_string(), owned(&[("a", "1")])),
        (String::new(), owned(&[("a", "1")])),
        ("TOOL_LIFECYCLE".to_string(), tool_lifecycle_fields()),
    ];
    let mut start = tool_lifecycle_fields();
    start.truncate(14);
    start[0].1 = "start".to_string();
    cases.push(("TOOL_LIFECYCLE".to_string(), start));
    cases
}

#[test]
fn text_line_equals_the_previous_assembly() {
    for (event, fields) in cases() {
        let borrowed = fields.iter().map(|(key, value)| (key.as_str(), value.as_str()));
        assert_eq!(
            format_structured_event_as(false, &event, borrowed),
            previous_line(false, &event, &fields),
            "text line of `{event}`",
        );
        assert_eq!(
            format_structured_event_as(false, &event, fields.clone()),
            previous_line(false, &event, &fields),
            "text line of `{event}` from owned fields",
        );
    }
}

#[test]
fn json_line_equals_the_previous_assembly() {
    for (event, fields) in cases() {
        let borrowed = fields.iter().map(|(key, value)| (key.as_str(), value.as_str()));
        assert_eq!(
            format_structured_event_as(true, &event, borrowed),
            previous_line(true, &event, &fields),
            "JSON line of `{event}`",
        );
        assert_eq!(
            format_structured_event_as(true, &event, fields.clone()),
            previous_line(true, &event, &fields),
            "JSON line of `{event}` from owned fields",
        );
    }
}

#[test]
fn last_duplicate_key_wins_in_both_modes() {
    let raw = [("b", "2"), ("a", "1"), ("b", "3")];
    assert_eq!(line(false, "ev", &raw), "EVENT event=ev a=1 b=3");
    assert_eq!(line(true, "ev", &raw), r#"EVENT_JSON {"a":"1","b":"3","event":"ev"}"#);

    assert_eq!(line(false, "ev", &[("a b", "1"), ("a_b", "2")]), "EVENT event=ev a_b=2");
    assert_eq!(line(false, "ev", &[("a_b", "2"), ("a b", "1")]), "EVENT event=ev a_b=1");

    // A field named `event` is written after the event in text mode and
    // replaces it in JSON mode.
    assert_eq!(line(false, "ev", &[("event", "inner")]), "EVENT event=ev event=inner");
    assert_eq!(
        line(true, "ev", &[("event", "inner")]),
        r#"EVENT_JSON {"event":"inner"}"#
    );
}

#[test]
fn values_are_quoted_and_cut_as_before() {
    assert_eq!(
        line(
            false,
            "ev",
            &[
                ("empty", ""),
                ("eq", "x=y"),
                ("nl", "one\ntwo"),
                ("quote", "say \"hi\""),
                ("single", "it's"),
                ("space", "a b"),
                ("tab", "a\tb"),
            ],
        ),
        r#"EVENT event=ev empty="" eq="x=y" nl="one two" quote="say \"hi\"" single="it's" space="a b" tab="a b""#,
    );

    let exact = "v".repeat(160);
    assert_eq!(
        line(false, "ev", &[("value", exact.as_str())]),
        format!("EVENT event=ev value={exact}")
    );
    let over = "v".repeat(161);
    assert_eq!(
        line(false, "ev", &[("value", over.as_str())]),
        format!("EVENT event=ev value={exact}...")
    );
    // The cut lands inside a two-byte character and moves back to its start.
    let multibyte = format!("a{}", "é".repeat(80));
    assert_eq!(
        line(false, "ev", &[("value", multibyte.as_str())]),
        format!("EVENT event=ev value=a{}...", "é".repeat(79))
    );
}

#[test]
fn keys_are_cleaned_and_cut_as_before() {
    assert_eq!(line(false, "ev", &[("", "v")]), "EVENT event=ev field=v");
    assert_eq!(line(false, "ev", &[("a/b c", "v")]), "EVENT event=ev a_b_c=v");
    assert_eq!(line(false, "ev", &[("né", "v")]), "EVENT event=ev n_=v");

    let exact = "x".repeat(80);
    assert_eq!(
        line(false, "ev", &[(exact.as_str(), "v")]),
        format!("EVENT event=ev {exact}=v")
    );
    let over = "x".repeat(81);
    assert_eq!(
        line(false, "ev", &[(over.as_str(), "v")]),
        format!("EVENT event=ev {exact}...=v")
    );
}

#[test]
fn credential_keys_are_redacted_in_any_case() {
    assert_eq!(
        line(
            false,
            "ev",
            &[
                ("refreshToken", "t"),
                ("API_KEY", "sk-secret"),
                ("exit_code", "1"),
                ("Authorization", "Bearer abc"),
                ("OAuth_Code", "c"),
            ],
        ),
        "EVENT event=ev API_KEY=<redacted> Authorization=<redacted> OAuth_Code=<redacted> exit_code=1 \
         refreshToken=<redacted>",
    );
}

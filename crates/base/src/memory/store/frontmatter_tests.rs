//! Tests for the strict frontmatter dialect.

use super::frontmatter::{Document, MAX_KEYS, Value};

fn document(fields: Vec<(&str, Value)>, body: &str) -> Document {
    Document {
        fields: fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        body: body.to_string(),
    }
}

#[test]
fn canonical_render_parses_back_to_the_same_document() {
    let doc = document(
        vec![
            ("title", Value::Str("아침 회의 선호".to_string())),
            ("kind", Value::Str("preference".to_string())),
            ("tags", Value::List(vec![Value::Str("schedule".to_string())])),
            (
                "links",
                Value::MapList(vec![vec![
                    ("kind".to_string(), Value::Str("relates_to".to_string())),
                    ("dst".to_string(), Value::Str("mem_2_b".to_string())),
                    ("weight".to_string(), Value::Float(0.25)),
                ]]),
            ),
            ("confidence", Value::Float(0.8)),
            ("whole", Value::Float(1.0)),
            ("count", Value::Int(7)),
            ("active", Value::Bool(true)),
            ("valid_to", Value::Null),
        ],
        "본문 첫 줄\n---\n본문 둘째 줄\n",
    );
    let text = doc.render();
    let parsed = Document::parse(&text).expect("parse");
    assert_eq!(parsed, doc);
    assert_eq!(parsed.render(), text);
    assert_eq!(parsed.body, "본문 첫 줄\n---\n본문 둘째 줄\n");
    assert!(
        text.contains("confidence: 0.8\n"),
        "floats keep their short form: {text}"
    );
    assert!(text.contains("whole: 1.0\n"), "a whole float stays a float: {text}");
}

#[test]
fn round_trip_is_byte_identical() {
    let doc = document(
        vec![
            ("title", Value::Str("t".to_string())),
            ("kind", Value::Str("fact".to_string())),
            ("tags", Value::List(vec![])),
            ("valid_from", Value::Null),
            ("confidence", Value::Float(0.42)),
        ],
        "",
    );
    let once = doc.render();
    let twice = Document::parse(&once).expect("parse").render();
    assert_eq!(once, twice);
}

#[test]
fn quotes_newlines_and_controls_cannot_break_out_of_a_value() {
    let hostile = "제목\"\nkind: evil\n---\n\u{7}\u{1b}[31m";
    let doc = document(
        vec![
            ("title", Value::Str(hostile.to_string())),
            ("kind", Value::Str("fact".to_string())),
        ],
        "본문",
    );
    let text = doc.render();
    // The injected `kind:` line and the injected closing delimiter are escapes
    // inside one quoted value, so the file still has exactly one frontmatter
    // block and one `kind` field.
    assert_eq!(text.matches("kind:").count(), 2, "{text}");
    let parsed = Document::parse(&text).expect("parse");
    assert_eq!(parsed.get_str("title"), Some(hostile));
    assert_eq!(parsed.get_str("kind"), Some("fact"));
    assert_eq!(parsed.body, "본문");
}

#[test]
fn hostile_input_is_refused() {
    let cases = [
        ("no opening delimiter", "title: \"x\"\n---\nbody"),
        ("no closing delimiter", "---\ntitle: \"x\"\n"),
        ("crlf", "---\r\ntitle: \"x\"\r\n---\r\n"),
        ("duplicate key", "---\ntitle: \"x\"\ntitle: \"y\"\n---\n"),
        ("unsupported escape", "---\ntitle: \"a\\qb\"\n---\n"),
        ("bare control character", "---\ntitle: \"a\tb\"\n---\n"),
        ("trailing text", "---\ntitle: \"a\" junk\n---\n"),
        ("uppercase key", "---\nTitle: \"a\"\n---\n"),
        ("missing value", "---\ntitle:\n---\n"),
        ("unknown literal", "---\ntitle: maybe\n---\n"),
        ("unclosed string", "---\ntitle: \"a\n---\n"),
        ("unclosed list", "---\ntags: [\"a\"\n---\n"),
        ("nested map", "---\nlinks: [{k: {j: 1}}]\n---\n"),
        ("short unicode escape", "---\ntitle: \"\\u12\"\n---\n"),
        ("non-hex unicode escape", "---\ntitle: \"\\u12zz\"\n---\n"),
    ];
    for (name, text) in cases {
        assert!(Document::parse(text).is_err(), "{name} must be refused");
    }
}

#[test]
fn oversized_frontmatter_is_refused() {
    let long_line = format!("---\ntitle: \"{}\"\n---\n", "x".repeat(9 * 1024));
    assert!(Document::parse(&long_line).is_err());

    let mut many = String::from("---\n");
    for index in 0..=MAX_KEYS {
        many.push_str(&format!("k{index}: \"v\"\n"));
    }
    many.push_str("---\n");
    assert!(
        Document::parse(&many).is_err(),
        "more than {MAX_KEYS} fields must be refused"
    );
}

#[test]
fn a_body_that_looks_like_frontmatter_is_just_a_body() {
    let body = "---\ntitle: \"가짜\"\n---\n끝";
    let doc = document(vec![("title", Value::Str("진짜".to_string()))], body);
    let text = doc.render();
    let parsed = Document::parse(&text).expect("parse");
    assert_eq!(parsed.get_str("title"), Some("진짜"));
    assert_eq!(parsed.body, body);
    assert_eq!(parsed.render(), text);
}

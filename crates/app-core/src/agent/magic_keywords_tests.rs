//! Scanner tests for `magic_keywords::scan`.
//!
//! Every test name contains `effort_keywords` so a single filter selects the
//! whole feature: `cargo test -p horon-agent-app-core effort_keywords`.

use super::magic_keywords::{EffortKeyword, scan};

#[test]
fn effort_keywords_plain_ultrathink_matches() {
    assert_eq!(scan("please ultrathink about this"), Some(EffortKeyword::Ultrathink));
}

#[test]
fn effort_keywords_plain_chillax_matches() {
    assert_eq!(scan("chillax, just answer briefly"), Some(EffortKeyword::Chillax));
}

#[test]
fn effort_keywords_is_case_sensitive() {
    // Exact lowercase only: `Ultrathink` and `CHILLAX` are ordinary words.
    assert_eq!(scan("Ultrathink this"), None);
    assert_eq!(scan("CHILLAX"), None);
    assert_eq!(scan("UltraThink"), None);
}

#[test]
fn effort_keywords_standalone_rejects_paths_and_flags() {
    assert_eq!(scan("ultrathink.ts"), None);
    assert_eq!(scan("src/ultrathink"), None);
    assert_eq!(scan("--ultrathink"), None);
    assert_eq!(scan("ultrathink_mode"), None);
    assert_eq!(scan("name:ultrathink"), None);
    assert_eq!(scan("(ultrathink)"), None);
    assert_eq!(scan("@ultrathink"), None);
    assert_eq!(scan("#ultrathink"), None);
    assert_eq!(scan("$ultrathink"), None);
    assert_eq!(scan("chillax.yaml"), None);
}

#[test]
fn effort_keywords_standalone_accepts_punctuation() {
    assert_eq!(scan("ultrathink."), Some(EffortKeyword::Ultrathink));
    assert_eq!(scan("ultrathink, please"), Some(EffortKeyword::Ultrathink));
    assert_eq!(scan("\"ultrathink\""), Some(EffortKeyword::Ultrathink));
    assert_eq!(scan("'chillax'"), Some(EffortKeyword::Chillax));
    assert_eq!(scan("really? ultrathink!"), Some(EffortKeyword::Ultrathink));
    assert_eq!(scan("(ultrathink)"), None, "a leading paren is a non-standalone char");
}

#[test]
fn effort_keywords_backtick_fence_is_skipped() {
    let text = "look:\n```\nultrathink\n```\nthanks";
    assert_eq!(scan(text), None);
}

#[test]
fn effort_keywords_tilde_fence_is_skipped() {
    let text = "~~~\nchillax\n~~~";
    assert_eq!(scan(text), None);
}

#[test]
fn effort_keywords_inline_code_is_skipped() {
    assert_eq!(scan("use `ultrathink` in code"), None);
    assert_eq!(scan("double ``chillax`` span"), None);
    // A shorter backtick run does not close a longer one.
    assert_eq!(scan("``ultrathink` still code``"), None);
}

#[test]
fn effort_keywords_html_comment_is_skipped() {
    assert_eq!(scan("<!-- ultrathink -->"), None);
    assert_eq!(scan("before <!-- chillax --> after"), None);
}

#[test]
fn effort_keywords_tag_is_skipped() {
    assert_eq!(scan("<div data-x=\"ultrathink\">"), None);
}

#[test]
fn effort_keywords_url_is_skipped() {
    assert_eq!(scan("see https://example.com/ultrathink for details"), None);
    assert_eq!(scan("see www.example.com/chillax"), None);
    assert_eq!(scan("https://example.com/ultrathink.ts"), None);
}

#[test]
fn effort_keywords_url_does_not_swallow_later_prose() {
    assert_eq!(
        scan("https://example.com/page but ultrathink now"),
        Some(EffortKeyword::Ultrathink)
    );
}

#[test]
fn effort_keywords_chillax_wins_when_both_present() {
    assert_eq!(scan("ultrathink and chillax"), Some(EffortKeyword::Chillax));
    assert_eq!(scan("chillax then ultrathink"), Some(EffortKeyword::Chillax));
}

#[test]
fn effort_keywords_absent_returns_none() {
    assert_eq!(scan(""), None);
    assert_eq!(scan("no keyword here"), None);
}

#[test]
fn effort_keywords_prose_after_code_still_matches() {
    assert_eq!(scan("`code` then ultrathink"), Some(EffortKeyword::Ultrathink));
}

//! Tests for the query sanitisers and the bigram expansion.

use super::text::{bigram_expansion, bigram_expression, normalize_entity, prepare_query};

#[test]
fn unspaced_korean_expands_into_overlapping_bigrams() {
    assert_eq!(bigram_expansion("아침회의선호"), "아침 침회 회의 의선 선호");
    // A body that separates the words shares the bigrams that matter.
    let body = bigram_expansion("아침 9시 이전 회의를 선호한다");
    let query = bigram_expansion("아침회의선호");
    let shared: Vec<&str> = query
        .split_whitespace()
        .filter(|term| body.split_whitespace().any(|part| part == *term))
        .collect();
    assert_eq!(shared, vec!["아침", "회의", "선호"], "the shared bigrams drive recall");
}

#[test]
fn latin_only_text_does_not_add_bigram_noise() {
    assert_eq!(bigram_expansion("deploy the release checklist"), "");
    assert_eq!(bigram_expression("deploy the release checklist"), None);
    // A run that mixes digits and Hangul still expands.
    assert_eq!(bigram_expansion("9시 회의"), "9시 회의");
}

#[test]
fn token_expression_quotes_every_word() {
    assert_eq!(
        prepare_query("배포 일정").tokens.as_deref(),
        Some("\"배포\" OR \"일정\"")
    );
    assert_eq!(prepare_query("   ").tokens, None);
    // FTS5 syntax in the query is just text inside a quoted phrase.
    let expression = prepare_query("NEAR(a b) OR *").tokens.expect("expression");
    assert!(expression.starts_with('"'), "{expression}");
    assert!(!expression.contains("OR(a"), "{expression}");
}

#[test]
fn trigram_expression_keeps_three_character_substrings() {
    assert_eq!(prepare_query("박준호").trigrams.as_deref(), Some("\"박준호\""));
    // Two characters are below the trigram minimum; the bigram stream covers
    // that case instead.
    assert_eq!(prepare_query("회의").trigrams, None);
    assert_eq!(prepare_query("a b c").trigrams.as_deref(), Some("\"abc\""));
    assert_eq!(prepare_query("  ").trigrams, None);
}

#[test]
fn compact_and_entity_normalisation_agree() {
    assert_eq!(prepare_query("SURI 프로덕션, 배포!").entity, "suri프로덕션배포");
    assert_eq!(normalize_entity("김 서연"), "김서연");
    assert_eq!(normalize_entity("SURI"), "suri");
    assert_eq!(prepare_query("김서연 팀장").entity, "김서연팀장");
}

#[test]
fn bigram_expression_refuses_queries_without_cjk_bigrams() {
    assert_eq!(bigram_expression("release"), None);
    assert!(bigram_expression("배포").is_some());
}

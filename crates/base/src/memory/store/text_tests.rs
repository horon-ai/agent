//! Tests for the query sanitisers and the bigram expansion.

use super::text::{
    bigram_expansion, bigram_expression, bigram_terms, compact, entity_query, normalize_entity, token_expression,
    trigram_expression,
};

#[test]
fn unspaced_korean_expands_into_overlapping_bigrams() {
    assert_eq!(bigram_expansion("아침회의선호"), "아침 침회 회의 의선 선호");
    // A body that separates the words shares the bigrams that matter.
    let body = bigram_expansion("아침 9시 이전 회의를 선호한다");
    let query = bigram_terms("아침회의선호");
    let shared: Vec<&String> = query
        .iter()
        .filter(|term| body.split_whitespace().any(|part| part == term.as_str()))
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
    assert_eq!(token_expression("배포 일정").as_deref(), Some("\"배포\" OR \"일정\""));
    assert_eq!(token_expression("   "), None);
    // FTS5 syntax in the query is just text inside a quoted phrase.
    let expression = token_expression("NEAR(a b) OR *").expect("expression");
    assert!(expression.starts_with('"'), "{expression}");
    assert!(!expression.contains("OR(a"), "{expression}");
}

#[test]
fn trigram_expression_keeps_three_character_substrings() {
    assert_eq!(trigram_expression("박준호").as_deref(), Some("\"박준호\""));
    // Two characters are below the trigram minimum; the bigram stream covers
    // that case instead.
    assert_eq!(trigram_expression("회의"), None);
    assert_eq!(trigram_expression("a b c").as_deref(), Some("\"abc\""));
    assert_eq!(trigram_expression("  "), None);
}

#[test]
fn compact_and_entity_normalisation_agree() {
    assert_eq!(compact("SURI 프로덕션, 배포!"), "suri프로덕션배포");
    assert_eq!(normalize_entity("김 서연"), "김서연");
    assert_eq!(normalize_entity("SURI"), "suri");
    assert_eq!(entity_query("김서연 팀장"), compact("김서연 팀장"));
}

#[test]
fn bigram_expression_refuses_queries_without_cjk_bigrams() {
    assert_eq!(bigram_expression("release"), None);
    assert!(bigram_expression("배포").is_some());
}

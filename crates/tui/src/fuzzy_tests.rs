//! The fuzzy matcher: ranks, positions, and the shapes a name can arrive in.

use crate::fuzzy;

#[test]
fn an_empty_needle_matches_everything_without_positions() {
    let found = fuzzy::find("", "model").expect("an empty needle matches every candidate");
    assert!(
        found.positions.is_empty(),
        "an empty needle reports no positions: {:?}",
        found.positions
    );
    assert_eq!(found.score, 0, "an empty needle ranks nothing above anything");
    assert!(fuzzy::find("", "").is_some(), "an empty pair still matches");
}

#[test]
fn an_exact_name_ranks_above_a_longer_prefix() {
    let exact = fuzzy::find("model", "model").expect("an exact name matches");
    let longer = fuzzy::find("model", "models").expect("a longer name starts with it");
    assert!(
        exact.score > longer.score,
        "the exact name did not win: {exact:?} against {longer:?}"
    );
}

#[test]
fn a_prefix_ranks_above_a_hit_inside_a_name() {
    let prefix = fuzzy::find("re", "resume").expect("a prefix matches");
    let inside = fuzzy::find("re", "autoreview").expect("the letters appear inside");
    assert!(
        prefix.score > inside.score,
        "the prefix did not win: {prefix:?} against {inside:?}"
    );
}

#[test]
fn a_hit_at_a_word_boundary_ranks_above_a_loose_one() {
    let boundary = fuzzy::find("re", "server_reload").expect("the second word starts with it");
    let loose = fuzzy::find("re", "autoreview").expect("the letters appear inside");
    assert_eq!(
        boundary.positions,
        vec![7, 8],
        "the hit inside the second word was not the one chosen: {boundary:?}"
    );
    assert!(
        boundary.score > loose.score,
        "the word boundary did not win: {boundary:?} against {loose:?}"
    );
}

#[test]
fn adjacent_characters_beat_a_scattered_hit() {
    let tight = fuzzy::find("abc", "x_abc").expect("a tight hit matches");
    let loose = fuzzy::find("abc", "x_a-b-c").expect("a scattered hit matches too");
    assert_eq!(tight.positions, vec![2, 3, 4]);
    assert_eq!(loose.positions, vec![2, 4, 6]);
    assert!(
        tight.score > loose.score,
        "the scattered hit was not ranked lower: {tight:?} against {loose:?}"
    );
}

#[test]
fn a_shorter_candidate_wins_a_tie() {
    let short = fuzzy::find("ab", "x_ab").expect("the short name matches");
    let long = fuzzy::find("ab", "x_ab_and_more").expect("the long name matches");
    assert!(
        short.score > long.score,
        "the shorter name did not win: {short:?} against {long:?}"
    );
}

#[test]
fn a_needle_that_does_not_appear_matches_nothing() {
    assert!(fuzzy::find("zzz", "model").is_none());
    assert!(
        fuzzy::find("modelling", "model").is_none(),
        "a longer name than the candidate cannot match"
    );
    assert!(fuzzy::find("abc", "ab").is_none(), "an unfinished needle matched");
    assert!(fuzzy::find("a", "").is_none(), "an empty candidate matched");
}

#[test]
fn positions_are_character_indices_in_a_unicode_candidate() {
    let found = fuzzy::find("델", "한모델").expect("a Hangul needle matches");
    assert_eq!(found.positions, vec![2], "the byte offset would be six");
    let found = fuzzy::find("x", "éx").expect("a needle after a wide letter matches");
    assert_eq!(found.positions, vec![1], "the byte offset would be two");
}

#[test]
fn case_is_ignored_on_both_sides() {
    let upper = fuzzy::find("MODEL", "model").expect("an upper-case needle matches");
    let lower = fuzzy::find("model", "model").expect("a lower-case needle matches");
    assert_eq!(upper.positions, vec![0, 1, 2, 3, 4]);
    assert_eq!(upper.score, lower.score);
    let folded = fuzzy::find("Ä", "ä").expect("a folded letter matches");
    assert_eq!(folded.positions, vec![0]);
}

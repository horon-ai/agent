//! Fuzzy matching for the slash popup and the pickers.
//!
//! The matcher answers one question: where the characters of a needle appear, in
//! order, in a candidate, and how good that hit is. It reports character indices
//! rather than bytes so a caller can cut a candidate at a matched character
//! without landing inside a multi-byte glyph.

/// A bonus for a candidate the needle spells out whole.
const EXACT: i32 = 100_000;
/// A bonus for a candidate that starts with the needle.
const PREFIX: i32 = 10_000;
/// A bonus for a hit that starts a word: `/mdl` ranks `model` over a name that
/// only holds the letters further in.
const BOUNDARY: i32 = 1_000;
/// A bonus per pair of adjacent characters, so a tight hit outranks a loose one.
const CONSECUTIVE: i32 = 100;

/// One hit: how good it is, and which characters of the candidate it used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Match {
    /// Higher is better; comparable only between candidates for one needle.
    pub score: i32,
    /// Character indices into the candidate, ascending.
    pub positions: Vec<usize>,
}

/// Find `needle` in `candidate`, case-insensitively, as a subsequence.
///
/// An empty needle matches every candidate with no positions, and a candidate
/// shorter than the needle matches nothing. Ranks, best first: an exact
/// spelling, a prefix, a hit that starts a word, then any subsequence; among
/// the hits that start a word or later, adjacent characters and a shorter
/// candidate score higher. Every prefix hit carries the same score, so their
/// relative order is the caller's to keep.
pub(crate) fn find(needle: &str, candidate: &str) -> Option<Match> {
    let mut wanted = needle.chars();
    let Some(first) = wanted.next() else {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    };
    let needle_len = needle.chars().count();
    let candidate_len = candidate.chars().count();
    if candidate_len < needle_len {
        return None;
    }
    let mut best: Option<(i32, usize)> = None;
    // One buffer for every attempt, refilled with the winning positions at the
    // end: a call allocates the list it returns and nothing else.
    let mut attempt: Vec<usize> = Vec::new();
    let mut previous: Option<char> = None;
    for (start, character) in candidate.chars().enumerate() {
        if same(character, first) && greedy(needle, candidate, start, &mut attempt) {
            let runs = consecutive(&attempt);
            let prefix = attempt.first().copied() == Some(0) && runs + 1 == attempt.len();
            let mut score = i32::try_from(runs).unwrap_or(i32::MAX).saturating_mul(CONSECUTIVE);
            if prefix {
                score += PREFIX;
                if attempt.len() == candidate_len {
                    score += EXACT;
                }
            } else {
                if start == 0 || previous.is_some_and(is_separator) {
                    score += BOUNDARY;
                }
                score -= i32::try_from(candidate_len).unwrap_or(i32::MAX);
            }
            let better = match best {
                Some((found, _)) => score > found,
                None => true,
            };
            if better {
                best = Some((score, start));
            }
        }
        previous = Some(character);
    }
    let (score, start) = best?;
    // The walk is the one the score came from, so it reproduces the winning
    // positions in the buffer the scan already holds.
    if !greedy(needle, candidate, start, &mut attempt) {
        return None;
    }
    Some(Match {
        score,
        positions: attempt,
    })
}

/// Collect the positions a left-to-right walk from `start` uses, if it finishes.
///
/// Greedy is enough here because the caller tries every occurrence of the first
/// character, and the earliest hit for each of them covers the tighter clusters
/// a later occurrence would find.
fn greedy(needle: &str, candidate: &str, start: usize, positions: &mut Vec<usize>) -> bool {
    positions.clear();
    let mut wanted = needle.chars();
    let Some(mut current) = wanted.next() else {
        return true;
    };
    for (index, character) in candidate.chars().enumerate().skip(start) {
        if same(current, character) {
            positions.push(index);
            match wanted.next() {
                Some(next) => current = next,
                None => return true,
            }
        }
    }
    false
}

/// How many pairs of adjacent positions the hit holds.
fn consecutive(positions: &[usize]) -> usize {
    let mut runs = 0;
    let mut previous: Option<usize> = None;
    for position in positions {
        if previous.is_some_and(|last| last + 1 == *position) {
            runs += 1;
        }
        previous = Some(*position);
    }
    runs
}

/// Characters that end a word, so the character after one starts a new one.
fn is_separator(character: char) -> bool {
    matches!(character, '-' | '_' | ':' | '/' | ' ')
}

/// Whether two characters are the same letter, ignoring case.
fn same(left: char, right: char) -> bool {
    left == right || left.to_lowercase().eq(right.to_lowercase())
}

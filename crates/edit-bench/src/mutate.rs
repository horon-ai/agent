//! Single-line bug mutations.
//!
//! Each mutation rewrites one place in a source file so that the file differs
//! from the original in exactly one line (two adjacent lines for
//! [`Mutation::SwapAdjacentLines`]). Candidates are only taken from code:
//! string/char literals and `//` comments are masked out, and operator
//! mutations require the rustfmt-style spaced form (` <= `, ` == `, ` + `) so
//! generic parameters (`Vec<String>`) and arrows (`->`, `=>`) never match.

use serde::{Deserialize, Serialize};

use crate::rng::SplitMix64;

/// The kinds of single-edit bugs the generator can introduce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mutation {
    SwapComparison,
    SwapEquality,
    SwapLogical,
    RemoveNegation,
    FlipBoolean,
    OffByOne,
    SwapArithmetic,
    SwapAdjacentLines,
}

/// Every mutation, in a stable order.
pub const ALL: [Mutation; 8] = [
    Mutation::SwapComparison,
    Mutation::SwapEquality,
    Mutation::SwapLogical,
    Mutation::RemoveNegation,
    Mutation::FlipBoolean,
    Mutation::OffByOne,
    Mutation::SwapArithmetic,
    Mutation::SwapAdjacentLines,
];

impl Mutation {
    /// The `kebab-case` name used in metadata, task ids, and prompt text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SwapComparison => "swap-comparison",
            Self::SwapEquality => "swap-equality",
            Self::SwapLogical => "swap-logical",
            Self::RemoveNegation => "remove-negation",
            Self::FlipBoolean => "flip-boolean",
            Self::OffByOne => "off-by-one",
            Self::SwapArithmetic => "swap-arithmetic",
            Self::SwapAdjacentLines => "swap-adjacent-lines",
        }
    }

    /// Short human description used inside the task prompt.
    pub fn description(self) -> &'static str {
        match self {
            Self::SwapComparison => "a comparison operator was swapped (< vs >, <= vs >=)",
            Self::SwapEquality => "an equality operator was swapped (== vs !=)",
            Self::SwapLogical => "a logical operator was swapped (&& vs ||)",
            Self::RemoveNegation => "a negation was removed from a condition",
            Self::FlipBoolean => "a boolean literal was flipped",
            Self::OffByOne => "an integer literal was shifted by one",
            Self::SwapArithmetic => "an arithmetic operator was swapped (+ vs -)",
            Self::SwapAdjacentLines => "two adjacent statements were swapped",
        }
    }
}

/// One applied mutation.
#[derive(Clone, Debug)]
pub(crate) struct Applied {
    /// 1-based line number of the first changed line.
    pub(crate) line: usize,
    /// Original text of the changed line(s), joined with `\n`.
    pub(crate) original: String,
    /// Mutated text of the changed line(s), joined with `\n`.
    pub(crate) mutated: String,
    /// Whole file with the mutation applied.
    pub(crate) mutated_source: String,
}

/// Apply `mutation` somewhere in `source`, choosing among all candidate sites
/// with `rng`. Returns `None` when the file offers no candidate site.
pub(crate) fn apply(mutation: Mutation, source: &str, rng: &mut SplitMix64) -> Option<Applied> {
    let lines: Vec<String> = source.split('\n').map(|line| line.to_string()).collect();

    if mutation == Mutation::SwapAdjacentLines {
        return apply_swap_adjacent(&lines, rng);
    }

    let mut candidates: Vec<(usize, usize, String, String)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        for (offset, from, to) in site_mutations(mutation, line) {
            candidates.push((index, offset, from, to));
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let (index, offset, from, to) = candidates.swap_remove(rng.below(candidates.len()));
    let line = &lines[index];
    let mut mutated_line = String::with_capacity(line.len());
    mutated_line.push_str(&line[..offset]);
    mutated_line.push_str(&to);
    mutated_line.push_str(&line[offset + from.len()..]);

    let mut mutated_lines = lines.clone();
    mutated_lines[index] = mutated_line.clone();

    Some(Applied {
        line: index + 1,
        original: line.clone(),
        mutated: mutated_line,
        mutated_source: mutated_lines.join("\n"),
    })
}

fn apply_swap_adjacent(lines: &[String], rng: &mut SplitMix64) -> Option<Applied> {
    let mut candidates = Vec::new();
    for (index, pair) in lines.windows(2).enumerate() {
        let (first, second) = (&pair[0], &pair[1]);
        if !is_swappable_statement(first) || !is_swappable_statement(second) {
            continue;
        }
        if leading_whitespace(first) != leading_whitespace(second) {
            continue;
        }
        candidates.push(index);
    }
    if candidates.is_empty() {
        return None;
    }
    let index = candidates[rng.below(candidates.len())];
    let mut mutated_lines = lines.to_vec();
    mutated_lines[index] = lines[index + 1].clone();
    mutated_lines[index + 1] = lines[index].clone();
    Some(Applied {
        line: index + 1,
        original: format!("{}\n{}", lines[index], lines[index + 1]),
        mutated: format!("{}\n{}", lines[index + 1], lines[index]),
        mutated_source: mutated_lines.join("\n"),
    })
}

fn is_swappable_statement(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && !trimmed.starts_with("//") && trimmed.ends_with(';')
}

fn leading_whitespace(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// All mutation sites on one line: `(byte_offset, from, to)`.
fn site_mutations(mutation: Mutation, line: &str) -> Vec<(usize, String, String)> {
    let mask = code_mask(line);
    let mut sites = match mutation {
        Mutation::SwapComparison => token_sites(
            line,
            &mask,
            &[(" <= ", " >= "), (" >= ", " <= "), (" < ", " > "), (" > ", " < ")],
        ),
        Mutation::SwapEquality => token_sites(line, &mask, &[(" == ", " != "), (" != ", " == ")]),
        Mutation::SwapLogical => token_sites(line, &mask, &[(" && ", " || "), (" || ", " && ")]),
        Mutation::SwapArithmetic => token_sites(line, &mask, &[(" + ", " - "), (" - ", " + ")]),
        Mutation::FlipBoolean => {
            let mut sites = Vec::new();
            for offset in word_offsets(line, "true", &mask) {
                sites.push((offset, "true".to_string(), "false".to_string()));
            }
            for offset in word_offsets(line, "false", &mask) {
                sites.push((offset, "false".to_string(), "true".to_string()));
            }
            sites
        }
        Mutation::OffByOne => integer_literals(line, &mask)
            .into_iter()
            .map(|(start, end)| {
                let literal = &line[start..end];
                (start, literal.to_string(), format!("{literal}+1"))
            })
            .collect(),
        Mutation::RemoveNegation => negation_offsets(line, &mask)
            .into_iter()
            .map(|offset| (offset, "!".to_string(), String::new()))
            .collect(),
        Mutation::SwapAdjacentLines => Vec::new(),
    };
    sites.sort_by_key(|(offset, _, _)| *offset);
    sites
}

fn token_sites(line: &str, mask: &[bool], rules: &[(&'static str, &'static str)]) -> Vec<(usize, String, String)> {
    let mut sites = Vec::new();
    for (from, to) in rules {
        for offset in token_offsets(line, from, mask) {
            sites.push((offset, from.to_string(), to.to_string()));
        }
    }
    sites
}

/// Offsets of `needle` that sit entirely inside code.
fn token_offsets(line: &str, needle: &str, mask: &[bool]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut from = 0;
    while let Some(position) = line[from..].find(needle) {
        let offset = from + position;
        if in_code(mask, offset, needle.len()) {
            offsets.push(offset);
        }
        from = offset + needle.len();
    }
    offsets
}

/// Offsets of a word-bounded identifier-like token that sits in code.
fn word_offsets(line: &str, word: &str, mask: &[bool]) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut offsets = Vec::new();
    let mut from = 0;
    while let Some(position) = line[from..].find(word) {
        let offset = from + position;
        from = offset + word.len();
        let before_ok = offset == 0 || !is_identifier_byte(bytes[offset - 1]);
        let after = offset + word.len();
        let after_ok = after >= bytes.len() || !is_identifier_byte(bytes[after]);
        if before_ok && after_ok && in_code(mask, offset, word.len()) {
            offsets.push(offset);
        }
    }
    offsets
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn negation_offsets(line: &str, mask: &[bool]) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut offsets = Vec::new();
    let mut search = 0;
    while let Some(position) = line[search..].find("if") {
        let start = search + position;
        search = start + 2;
        if start > 0 && is_identifier_byte(bytes[start - 1]) {
            continue;
        }
        if bytes.get(start + 2).is_some_and(|byte| is_identifier_byte(*byte)) {
            continue;
        }
        if !in_code(mask, start, 2) {
            continue;
        }
        let mut cursor = start + 2;
        while bytes.get(cursor) == Some(&b' ') {
            cursor += 1;
        }
        if bytes.get(cursor) == Some(&b'(') {
            cursor += 1;
            while bytes.get(cursor) == Some(&b' ') {
                cursor += 1;
            }
        }
        if bytes.get(cursor) != Some(&b'!') || !mask.get(cursor).copied().unwrap_or(false) {
            continue;
        }
        if bytes.get(cursor + 1) == Some(&b'=') {
            continue;
        }
        offsets.push(cursor);
    }
    offsets
}

/// Byte ranges of standalone decimal integer literals inside code.
fn integer_literals(line: &str, mask: &[bool]) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
            index += 1;
        }
        let end = index;
        let before_ok = start == 0 || !is_literal_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_literal_byte(bytes[end]);
        if before_ok && after_ok && in_code(mask, start, end - start) {
            ranges.push((start, end));
        }
    }
    ranges
}

fn is_literal_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

fn in_code(mask: &[bool], offset: usize, len: usize) -> bool {
    offset + len <= mask.len() && mask[offset..offset + len].iter().all(|byte| *byte)
}

/// Code/not-code mask for one line: `false` inside string and char literals
/// and inside a trailing `//` comment.
fn code_mask(line: &str) -> Vec<bool> {
    let bytes = line.as_bytes();
    let mut mask = vec![true; bytes.len()];
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            mask[index..].fill(false);
            break;
        }
        if bytes[index] == b'"' {
            let end = string_end(line, index);
            mask[index..end].fill(false);
            index = end.max(index + 1);
            continue;
        }
        if bytes[index] == b'\''
            && let Some(end) = char_end(line, index)
        {
            mask[index..end].fill(false);
            index = end;
            continue;
        }
        index += 1;
    }
    mask
}

/// End offset (exclusive) of the string literal opening at `quote`.
fn string_end(line: &str, quote: usize) -> usize {
    let bytes = line.as_bytes();
    let mut hashes = 0usize;
    let mut cursor = quote;
    while cursor > 0 && bytes[cursor - 1] == b'#' {
        hashes += 1;
        cursor -= 1;
    }
    let raw = cursor > 0 && bytes[cursor - 1] == b'r';

    let mut index = quote + 1;
    while index < bytes.len() {
        if raw {
            if bytes[index] == b'"' {
                let mut matched = 0usize;
                while matched < hashes && bytes.get(index + 1 + matched) == Some(&b'#') {
                    matched += 1;
                }
                if matched == hashes {
                    return (index + 1 + hashes).min(bytes.len());
                }
            }
            index += 1;
        } else if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == b'"' {
            return index + 1;
        } else {
            index += 1;
        }
    }
    bytes.len()
}

/// End offset (exclusive) of the char literal opening at `quote`, if it is
/// one (a lifetime like `'a` is not).
fn char_end(line: &str, quote: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let after = quote + 1;
    if after >= bytes.len() {
        return None;
    }
    if bytes[after] == b'\\' {
        let limit = (after + 12).min(bytes.len());
        let mut index = after + 1;
        while index < limit {
            if bytes[index] == b'\'' {
                return Some(index + 1);
            }
            index += 1;
        }
        return None;
    }
    let rest = &line[after..];
    let mut chars = rest.char_indices();
    chars.next()?;
    let (offset, next) = chars.next()?;
    (next == '\'').then_some(after + offset + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutate_once(mutation: Mutation, source: &str) -> Applied {
        let mut rng = SplitMix64::new(11);
        apply(mutation, source, &mut rng).expect("mutation should apply")
    }

    #[test]
    fn ignores_strings_and_comments() {
        for mutation in [
            Mutation::SwapEquality,
            Mutation::SwapComparison,
            Mutation::SwapLogical,
            Mutation::SwapArithmetic,
            Mutation::FlipBoolean,
            Mutation::OffByOne,
        ] {
            let source = "fn f() {\n    let a = \"x == y < z && true 42\";\n    // w == v >= 1 - 2\n}\n";
            let mut rng = SplitMix64::new(3);
            assert!(
                apply(mutation, source, &mut rng).is_none(),
                "{mutation:?} mutated a literal or comment"
            );
        }
        let negated = "fn f(flag: bool) {\n    // if !flag\n}\n";
        let mut rng = SplitMix64::new(3);
        assert!(apply(Mutation::RemoveNegation, negated, &mut rng).is_none());
    }

    #[test]
    fn comparison_operators_swap() {
        let applied = mutate_once(
            Mutation::SwapComparison,
            "fn f(a: i32, b: i32) -> bool {\n    a <= b\n}\n",
        );
        assert!(applied.mutated_source.contains("a >= b"));
        assert_eq!(applied.line, 2);
    }

    #[test]
    fn generics_are_not_comparisons() {
        let source = "fn f(v: Vec<String>) {\n    let _ = v;\n}\n";
        let mut rng = SplitMix64::new(5);
        assert!(apply(Mutation::SwapComparison, source, &mut rng).is_none());
    }

    #[test]
    fn off_by_one_appends_one() {
        let applied = mutate_once(Mutation::OffByOne, "fn f() -> i32 {\n    let x = 41;\n    x\n}\n");
        assert_eq!(applied.mutated, "    let x = 41+1;");
    }

    #[test]
    fn negation_is_removed_from_conditions_only() {
        let applied = mutate_once(
            Mutation::RemoveNegation,
            "fn f(flag: bool) -> bool {\n    let y = !flag;\n    if !flag { true } else { false }\n}\n",
        );
        assert!(applied.mutated_source.contains("if flag {"));
        assert!(applied.mutated_source.contains("let y = !flag;"));
    }

    #[test]
    fn escaped_and_raw_strings_are_masked() {
        let source = "fn f() {\n    let a = \"quote \\\" still == inside\";\n    let b = r#\"raw == text\"#;\n}\n";
        let mut rng = SplitMix64::new(9);
        assert!(apply(Mutation::SwapEquality, source, &mut rng).is_none());
    }

    #[test]
    fn every_mutation_changes_its_reported_lines_only() {
        let source = "\
fn f(a: i32, b: i32, flag: bool) -> i32 {
    let first = a + 1;
    let second = a - 1;
    if a <= b && flag {
        return 3;
    }
    if !flag {
        return 4;
    }
    if a == b || b != a {
        return 5;
    }
    let on = true;
    let off = false;
    first + second
}
";
        for mutation in ALL {
            let mut rng = SplitMix64::new(21);
            let applied = apply(mutation, source, &mut rng).expect("mutation should apply");
            let before: Vec<&str> = source.split('\n').collect();
            let after: Vec<&str> = applied.mutated_source.split('\n').collect();
            assert_eq!(before.len(), after.len(), "{mutation:?} changed line count");
            let changed: Vec<usize> = before
                .iter()
                .zip(after.iter())
                .enumerate()
                .filter(|(_, (left, right))| left != right)
                .map(|(index, _)| index + 1)
                .collect();
            let expected = if mutation == Mutation::SwapAdjacentLines {
                vec![applied.line, applied.line + 1]
            } else {
                vec![applied.line]
            };
            assert_eq!(changed, expected, "{mutation:?} changed {changed:?}");
        }
    }
}

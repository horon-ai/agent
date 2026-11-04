//! Deterministic classification of an unexpected turn stop.
//!
//! Pure over the provider stop reason and the final assistant text.

/// How much automatic recovery the caller is willing to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecoveryMode {
    /// Never act on any class.
    Off,
    /// Act on provable truncation and unclosed structure only.
    #[default]
    Conservative,
    /// Act on every non-clean class, including the semantic guess.
    Aggressive,
}

/// Why a turn ended, as far as the harness can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryClass {
    /// The turn ended the way a finished turn ends.
    CleanStop,
    /// The provider reported a length or token budget stop.
    BudgetTruncated,
    /// The final text leaves a fence or a list item open.
    UnclosedStructure,
    /// The final sentence announces work with nothing after it.
    SemanticPrematureStop,
}

/// Maximum synthetic continuations a single run may spend.
pub const MAX_AUTO_CONTINUATIONS: u8 = 2;

impl RecoveryClass {
    /// True when the turn needs no recovery.
    pub fn is_clean(self) -> bool {
        matches!(self, Self::CleanStop)
    }
}

impl RecoveryMode {
    /// Whether this mode acts on `class`.
    pub fn acts_on(self, class: RecoveryClass) -> bool {
        match self {
            Self::Off => false,
            Self::Conservative => matches!(class, RecoveryClass::BudgetTruncated | RecoveryClass::UnclosedStructure),
            Self::Aggressive => !class.is_clean(),
        }
    }
}

/// Classify a stopped turn from its stop reason and final assistant text.
pub fn classify(stop_reason: &str, final_text: &str) -> RecoveryClass {
    if is_budget_stop(stop_reason) {
        return RecoveryClass::BudgetTruncated;
    }
    if has_unclosed_fence(final_text) || has_trailing_empty_list_marker(final_text) {
        return RecoveryClass::UnclosedStructure;
    }
    if has_premature_announcement(final_text) {
        return RecoveryClass::SemanticPrematureStop;
    }
    RecoveryClass::CleanStop
}

fn is_budget_stop(stop_reason: &str) -> bool {
    let reason = stop_reason.trim().to_ascii_lowercase();
    !reason.is_empty()
        && (reason.contains("length")
            || reason.contains("max_tokens")
            || reason.contains("max_output_tokens")
            || reason.contains("trunc")
            || reason.contains("budget"))
}

#[derive(Debug, Clone, Copy)]
struct FenceLine<'a> {
    delim: char,
    len: usize,
    info: &'a str,
}

fn leading_indent(line: &str) -> usize {
    let mut cols = 0;
    for ch in line.chars() {
        match ch {
            ' ' => cols += 1,
            '\t' => cols += 4,
            _ => break,
        }
    }
    cols
}

fn fence_line(line: &str) -> Option<FenceLine<'_>> {
    if leading_indent(line) >= 4 {
        return None;
    }
    let rest = line.trim_start();
    let delim = rest.chars().next()?;
    if delim != '`' && delim != '~' {
        return None;
    }
    let len = rest.chars().take_while(|ch| *ch == delim).count();
    if len < 3 {
        return None;
    }
    let info = &rest[len..];
    if delim == '`' && info.contains('`') {
        return None;
    }
    Some(FenceLine { delim, len, info })
}

fn has_unclosed_fence(text: &str) -> bool {
    let mut open: Option<(char, usize)> = None;
    for line in text.split('\n') {
        let Some(fence) = fence_line(line) else {
            continue;
        };
        match open {
            None => open = Some((fence.delim, fence.len)),
            Some((delim, len)) => {
                if fence.delim == delim && fence.len >= len && fence.info.trim().is_empty() {
                    open = None;
                }
            }
        }
    }
    open.is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    Unordered(char),
    Ordered(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Marker {
    indent: usize,
    kind: MarkerKind,
}

fn parse_marker(line: &str) -> Option<(Marker, usize)> {
    let indent = leading_indent(line);
    if indent >= 4 {
        return None;
    }
    let body = line.trim();
    let bytes = body.as_bytes();
    let (kind, len) = match bytes.first()? {
        b'-' | b'*' | b'+' => (MarkerKind::Unordered(bytes[0] as char), 1),
        b'0'..=b'9' => {
            let digits = bytes.iter().take_while(|byte| byte.is_ascii_digit()).count();
            if digits > 9 {
                return None;
            }
            let delim = *bytes.get(digits)?;
            if delim != b'.' && delim != b')' {
                return None;
            }
            (MarkerKind::Ordered(delim as char), digits + 1)
        }
        _ => return None,
    };
    let after = body.get(len..)?;
    if !after.is_empty() && !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    Some((Marker { indent, kind }, len))
}

fn has_trailing_empty_list_marker(text: &str) -> bool {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(last) = lines.iter().rposition(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Some((marker, marker_len)) = parse_marker(lines[last]) else {
        return false;
    };
    if lines[last].trim().len() != marker_len {
        return false;
    }
    let Some(previous) = lines[..last].iter().rev().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Some((previous_marker, _)) = parse_marker(previous) else {
        return false;
    };
    previous_marker.indent == marker.indent && previous_marker.kind == marker.kind
}

const ANNOUNCEMENT_MARKERS: &[&str] = &[
    "i'll ",
    "i will ",
    "i'm going to ",
    "i am going to ",
    "let me now ",
    "next, i'll ",
    "next i'll ",
    "now i'll ",
];

fn has_premature_announcement(text: &str) -> bool {
    let Some(last) = text.split('\n').rev().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let lower = last.trim().to_ascii_lowercase();
    if !(lower.ends_with('.') || lower.ends_with(':') || lower.ends_with('…')) {
        return false;
    }
    let sentence = strip_list_marker(&lower);
    ANNOUNCEMENT_MARKERS
        .iter()
        .any(|marker| sentence_starts_with(sentence, marker))
}

fn strip_list_marker(line: &str) -> &str {
    match parse_marker(line) {
        Some((_, len)) => {
            let rest = &line[len..];
            if rest.is_empty() { line } else { rest.trim_start() }
        }
        None => line,
    }
}

fn sentence_starts_with(line: &str, marker: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = line[from..].find(marker) {
        let idx = from + pos;
        if idx == 0 || is_sentence_boundary(&line[..idx]) {
            return true;
        }
        from = idx + marker.len();
    }
    false
}

fn is_sentence_boundary(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    trimmed.ends_with('.')
        || trimmed.ends_with('?')
        || trimmed.ends_with('!')
        || trimmed.ends_with(':')
        || trimmed.ends_with(';')
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNCLOSED_FENCE: &str = "Here is the patch:\n```rust\nfn main() {}";
    const CLOSED_FENCE_WITH_INNER: &str = "````markdown\n```rust\nfn main() {}\n```\n````\nDone.";
    const INDENTED_FENCE: &str = "Example:\n\n    ```\n    indented\n    ```\n";
    const TRAILING_EMPTY_ITEM: &str = "Steps taken:\n- read the plan\n- ran the tests\n-";
    const TRAILING_NUMBER: &str = "The total is\n42.";
    const PROMISE: &str = "I'll now run the full test suite.";
    const CRLF_UNCLOSED: &str = "Start:\r\n```\r\ncode\r\n";
    const CRLF_CLOSED: &str = "Start:\r\n```\r\ncode\r\n```\r\n";

    #[test]
    fn classifies_each_required_shape() {
        let cases: &[(&str, &str, &str, RecoveryClass)] = &[
            (
                "length_stop",
                "Length",
                "a partial answer",
                RecoveryClass::BudgetTruncated,
            ),
            ("max_tokens_stop", "max_tokens", "", RecoveryClass::BudgetTruncated),
            (
                "unclosed_triple_backtick",
                "Stop",
                UNCLOSED_FENCE,
                RecoveryClass::UnclosedStructure,
            ),
            (
                "closed_fence_with_shorter_inner_fence",
                "Stop",
                CLOSED_FENCE_WITH_INNER,
                RecoveryClass::CleanStop,
            ),
            (
                "indented_fence_is_not_a_fence",
                "Stop",
                INDENTED_FENCE,
                RecoveryClass::CleanStop,
            ),
            (
                "trailing_empty_list_item",
                "Stop",
                TRAILING_EMPTY_ITEM,
                RecoveryClass::UnclosedStructure,
            ),
            (
                "ordered_marker_without_list_is_clean",
                "Stop",
                TRAILING_NUMBER,
                RecoveryClass::CleanStop,
            ),
            (
                "promise_sentence",
                "Stop",
                PROMISE,
                RecoveryClass::SemanticPrematureStop,
            ),
            ("empty_text", "Stop", "", RecoveryClass::CleanStop),
            (
                "crlf_unclosed_fence",
                "Stop",
                CRLF_UNCLOSED,
                RecoveryClass::UnclosedStructure,
            ),
            ("crlf_closed_fence", "Stop", CRLF_CLOSED, RecoveryClass::CleanStop),
            ("fence_only", "Stop", "```", RecoveryClass::UnclosedStructure),
            ("fence_pair_only", "Stop", "```\n```", RecoveryClass::CleanStop),
            (
                "tilde_fence_inside_backtick_block",
                "Stop",
                "```\n~~~\n```",
                RecoveryClass::CleanStop,
            ),
            (
                "backtick_info_with_backtick_is_not_an_opener",
                "Stop",
                "``` a`b\nplain text",
                RecoveryClass::CleanStop,
            ),
            (
                "shorter_backtick_fence_does_not_close",
                "Stop",
                "````\n```\n",
                RecoveryClass::UnclosedStructure,
            ),
            (
                "ordered_empty_marker_after_ordered_item",
                "Stop",
                "1. first\n2.",
                RecoveryClass::UnclosedStructure,
            ),
            (
                "ordered_empty_marker_after_different_delimiter",
                "Stop",
                "1) first\n2.",
                RecoveryClass::CleanStop,
            ),
            (
                "empty_marker_after_indented_item",
                "Stop",
                "- first\n    -\n",
                RecoveryClass::CleanStop,
            ),
            (
                "closing_fence_with_info_does_not_close",
                "Stop",
                "```\ncode\n```rust\n",
                RecoveryClass::UnclosedStructure,
            ),
            (
                "multibyte_text_with_trailing_marker",
                "Stop",
                "한국어 ```\n- 항목\n-",
                RecoveryClass::UnclosedStructure,
            ),
            (
                "completed_sentence_without_promise",
                "Stop",
                "The migration is complete.",
                RecoveryClass::CleanStop,
            ),
        ];

        for (label, stop_reason, text, expected) in cases {
            assert_eq!(classify(stop_reason, text), *expected, "case {label} misclassified");
        }
    }

    #[test]
    fn conservative_ignores_the_semantic_class() {
        assert!(RecoveryMode::Conservative.acts_on(RecoveryClass::BudgetTruncated));
        assert!(RecoveryMode::Conservative.acts_on(RecoveryClass::UnclosedStructure));
        assert!(!RecoveryMode::Conservative.acts_on(RecoveryClass::SemanticPrematureStop));
        assert!(!RecoveryMode::Conservative.acts_on(RecoveryClass::CleanStop));
    }

    #[test]
    fn aggressive_acts_on_every_non_clean_class() {
        assert!(RecoveryMode::Aggressive.acts_on(RecoveryClass::BudgetTruncated));
        assert!(RecoveryMode::Aggressive.acts_on(RecoveryClass::UnclosedStructure));
        assert!(RecoveryMode::Aggressive.acts_on(RecoveryClass::SemanticPrematureStop));
        assert!(!RecoveryMode::Aggressive.acts_on(RecoveryClass::CleanStop));
    }

    #[test]
    fn off_ignores_every_class() {
        for class in [
            RecoveryClass::CleanStop,
            RecoveryClass::BudgetTruncated,
            RecoveryClass::UnclosedStructure,
            RecoveryClass::SemanticPrematureStop,
        ] {
            assert!(!RecoveryMode::Off.acts_on(class), "off acted on {class:?}");
        }
    }

    #[test]
    fn continuation_cap_is_two() {
        assert_eq!(MAX_AUTO_CONTINUATIONS, 2);
    }
}

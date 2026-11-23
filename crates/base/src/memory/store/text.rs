//! Text handling for the index: query sanitisers and the bigram expansion.
//!
//! Every query string reaches SQLite as a quoted FTS5 phrase built here, so no
//! caller text can inject FTS5 syntax. The expansion exists because Korean and
//! other CJK writing does not separate words with spaces: an unspaced query
//! ("아침회의선호") shares no token with a body that writes the same words apart
//! ("아침 9시 이전 회의를 선호한다"), so the index stores overlapping bigrams
//! and the query is expanded the same way. A three-character substring
//! ("박준호") is additionally served by the `trigram` table.

/// Most phrases one FTS expression may hold.
const MAX_PHRASES: usize = 32;

/// Whether `ch` is a Han, Hangul, Hiragana or Katakana character, the scripts
/// written without spaces between words.
pub fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x11FF        // Hangul Jamo
        | 0x3040..=0x30FF      // Hiragana, Katakana
        | 0x3130..=0x318F      // Hangul compatibility Jamo
        | 0x3400..=0x4DBF      // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF      // CJK Unified Ideographs
        | 0xA960..=0xA97F      // Hangul Jamo Extended-A
        | 0xAC00..=0xD7FF      // Hangul Syllables
        | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
        | 0xFF66..=0xFF9D      // Halfwidth Katakana
        | 0x20000..=0x2FA1F    // CJK Unified Ideographs Extension B and later
    )
}

/// A normalised entity name for matching: lowercase, with every character that
/// is not a letter or digit removed. `SURI` and `s u r i` both become `suri`.
pub fn normalize_entity(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        }
    }
    out
}

/// Expand every CJK-bearing word into overlapping bigrams, space separated.
///
/// A run of letters and digits that contains at least one CJK character becomes
/// its 2-character windows (`회의를선호` -> `회의 의를 를선 선호`); a run of Latin
/// text alone is left out, because the token index already covers it.
pub fn bigram_expansion(text: &str) -> String {
    bigram_expansion_parts(&[text])
}

/// Expand every CJK-bearing word across `parts` into overlapping bigrams
/// without allocating a concatenated temporary string.
pub fn bigram_expansion_parts(parts: &[&str]) -> String {
    let mut out = String::new();
    for part in parts {
        push_bigram_expansion(part, &mut out);
    }
    out
}

fn push_bigram_expansion(text: &str, out: &mut String) {
    let mut run_start: Option<usize> = None;
    let mut char_count = 0usize;
    let mut has_cjk = false;
    for (idx, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            if run_start.is_none() {
                run_start = Some(idx);
            }
            char_count += 1;
            if is_cjk(ch) {
                has_cjk = true;
            }
        } else if let Some(start) = run_start.take() {
            if char_count >= 2 && has_cjk {
                emit_bigrams(&text[start..idx], out);
            }
            char_count = 0;
            has_cjk = false;
        }
    }
    if let Some(start) = run_start
        && char_count >= 2
        && has_cjk
    {
        emit_bigrams(&text[start..], out);
    }
}

fn emit_bigrams(slice: &str, out: &mut String) {
    let mut prev: Option<char> = None;
    for ch in slice.chars() {
        if let Some(first) = prev {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push(first);
            out.push(ch);
        }
        prev = Some(ch);
    }
}

fn words_and_compact(text: &str) -> (Vec<String>, String) {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut compacted = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                current.push(lower);
                compacted.push(lower);
            }
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    (words, compacted)
}

/// Append one term as a quoted FTS5 phrase, doubling any embedded quote.
fn push_phrase(out: &mut String, term: &str) {
    out.push('"');
    for ch in term.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
}

fn disjunction_strs<'a>(terms: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut out = String::new();
    let mut count = 0usize;
    for term in terms {
        if count > 0 {
            out.push_str(" OR ");
        }
        push_phrase(&mut out, term);
        count += 1;
    }
    if count == 0 { None } else { Some(out) }
}

fn trigram_expression_from<'a>(words: &'a [String], all: &'a str) -> Option<String> {
    let mut terms: Vec<&'a str> = Vec::new();
    let mut push = |term: &'a str| {
        if !terms.contains(&term) && terms.len() < MAX_PHRASES {
            terms.push(term);
        }
    };
    for word in words {
        if word.chars().count() >= 3 {
            push(word.as_str());
        }
    }
    if all.chars().count() >= 3 {
        push(all);
    }
    disjunction_strs(terms)
}

/// The `pages_grams` expression for a query, or `None` when no CJK bigram can be
/// formed (Latin words are already covered by [`prepare_query`]).
pub fn bigram_expression(text: &str) -> Option<String> {
    let expanded = bigram_expansion(text);
    let mut terms: Vec<&str> = Vec::new();
    for term in expanded.split_whitespace() {
        if !terms.contains(&term) {
            terms.push(term);
        }
        if terms.len() >= MAX_PHRASES {
            break;
        }
    }
    disjunction_strs(terms)
}

/// All four query-stream representations computed in one pass over `query_text`.
pub(super) struct PreparedQuery {
    pub tokens: Option<String>,
    pub trigrams: Option<String>,
    pub bigrams: Option<String>,
    pub entity: String,
}

pub(super) fn prepare_query(text: &str) -> PreparedQuery {
    let (words, entity) = words_and_compact(text);
    let tokens = disjunction_strs(words.iter().take(MAX_PHRASES).map(String::as_str));
    let trigrams = trigram_expression_from(&words, &entity);
    let bigrams = bigram_expression(text);
    PreparedQuery {
        tokens,
        trigrams,
        bigrams,
        entity,
    }
}

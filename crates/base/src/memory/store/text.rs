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

/// Lowercase `text` and drop everything that is not a letter or digit. This is
/// the form the trigram and entity streams compare.
pub fn compact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
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
    let mut out = String::with_capacity(text.len());
    let mut run: Vec<char> = Vec::new();
    let flush = |run: &mut Vec<char>, out: &mut String| {
        if run.len() >= 2 && run.iter().any(|ch| is_cjk(*ch)) {
            for pair in run.windows(2) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.extend(pair.iter());
            }
        }
        run.clear();
    };
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            run.push(ch);
        } else {
            flush(&mut run, &mut out);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// The distinct bigram terms of a query, in first-seen order.
pub fn bigram_terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for term in bigram_expansion(text).split_whitespace() {
        if !terms.iter().any(|existing| existing == term) {
            terms.push(term.to_string());
        }
        if terms.len() >= MAX_PHRASES {
            break;
        }
    }
    terms
}

/// Split `text` into the words the `unicode61` tokenizer would see: runs of
/// letters and digits, lowercased.
pub fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                current.push(lower);
            }
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Turn one term into an FTS5 phrase, doubling an embedded quote.
fn phrase(term: &str) -> String {
    let mut quoted = String::with_capacity(term.len() + 2);
    quoted.push('"');
    for ch in term.chars() {
        if ch == '"' {
            quoted.push('"');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

fn disjunction(terms: Vec<String>) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .into_iter()
            .map(|term| phrase(&term))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// The `pages_fts` expression for a query, or `None` when it has no word.
pub fn token_expression(text: &str) -> Option<String> {
    let mut terms = words(text);
    terms.truncate(MAX_PHRASES);
    disjunction(terms)
}

/// The `pages_bigram` (trigram) expression: quoted substrings of three or more
/// characters, which the trigram tokenizer matches anywhere in the text.
pub fn trigram_expression(text: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut push = |term: String| {
        if !terms.iter().any(|existing| existing == &term) && terms.len() < MAX_PHRASES {
            terms.push(term);
        }
    };
    for word in words(text) {
        if word.chars().count() >= 3 {
            push(word);
        }
    }
    let all = compact(text);
    if all.chars().count() >= 3 {
        push(all);
    }
    disjunction(terms)
}

/// The `pages_grams` expression for a query, or `None` when no CJK bigram can be
/// formed (Latin words are already covered by [`token_expression`]).
pub fn bigram_expression(text: &str) -> Option<String> {
    disjunction(bigram_terms(text))
}

/// The compacted, lowercased query used to match entity names.
pub fn entity_query(text: &str) -> String {
    compact(text)
}

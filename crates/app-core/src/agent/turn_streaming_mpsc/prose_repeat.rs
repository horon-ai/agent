//! Detects pathological repetition of visible prose while a response streams.
//!
//! The guard is deliberately narrow. It fires only when the exact same long
//! sentence appears several times in a row, and it looks only at prose:
//! fenced code, table rows, headings and list items are skipped, and any other
//! sentence between two copies breaks the run. A model that makes the same point twice, repeats a
//! line of code, or fills a table with similar rows never trips it.
//!
//! Text arrives in arbitrary pieces, so the guard keeps the unfinished tail of
//! a sentence between calls and judges a sentence only once it is complete.

/// Only a sentence at least this long can be a repetition: short lines repeat in
/// ordinary prose. A shorter sentence still breaks a run of identical ones.
const MIN_SENTENCE_CHARS: usize = 40;
/// Fragments this short, such as `e.g.`, come from splitting at an abbreviation
/// and are ignored altogether.
const MIN_FRAGMENT_CHARS: usize = 12;
/// The same sentence this many times in a row is treated as a repetition loop.
pub(super) const STOP_AFTER: usize = 4;

#[derive(Default)]
pub(super) struct ProseRepeatGuard {
    tail: String,
    in_fence: bool,
    last: Option<String>,
    run: usize,
    fired: bool,
}

impl ProseRepeatGuard {
    /// Feed the next piece of assistant text. Returns the repeated sentence the
    /// first time it reaches [`STOP_AFTER`] consecutive occurrences, and never
    /// again afterwards.
    pub(super) fn feed(&mut self, delta: &str) -> Option<String> {
        if self.fired {
            return None;
        }
        self.tail.push_str(delta);
        while let Some(end) = self.next_boundary() {
            let segment: String = self.tail.drain(..end).collect();
            if let Some(sentence) = self.observe(&segment) {
                self.fired = true;
                self.tail.clear();
                return Some(sentence);
            }
        }
        None
    }

    /// Byte length of the first complete segment in the tail: a line, or a
    /// sentence ended by `.`, `!` or `?` followed by whitespace. A terminator at
    /// the very end of the tail is not a boundary yet, because the next piece
    /// may continue the word.
    fn next_boundary(&self) -> Option<usize> {
        let mut chars = self.tail.char_indices().peekable();
        while let Some((index, current)) = chars.next() {
            if current == '\n' {
                return Some(index + 1);
            }
            if matches!(current, '.' | '!' | '?')
                && let Some(&(_, next)) = chars.peek()
                && next.is_whitespace()
            {
                return Some(index + current.len_utf8());
            }
        }
        None
    }

    fn observe(&mut self, segment: &str) -> Option<String> {
        let trimmed = segment.trim();
        if trimmed.starts_with("```") {
            self.in_fence = !self.in_fence;
            self.reset();
            return None;
        }
        if self.in_fence || is_structure(trimmed) {
            self.reset();
            return None;
        }
        let sentence = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        let length = sentence.chars().count();
        if length < MIN_FRAGMENT_CHARS {
            return None;
        }
        if length < MIN_SENTENCE_CHARS {
            self.reset();
            return None;
        }
        if self.last.as_deref() == Some(sentence.as_str()) {
            self.run += 1;
        } else {
            self.last = Some(sentence.clone());
            self.run = 1;
        }
        (self.run >= STOP_AFTER).then_some(sentence)
    }

    fn reset(&mut self) {
        self.last = None;
        self.run = 0;
    }
}

/// Lines that carry structure rather than prose: table rows, headings, quotes
/// and list items.
fn is_structure(line: &str) -> bool {
    if line.starts_with(['|', '#', '>']) || line.starts_with("- ") || line.starts_with("* ") {
        return true;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(['.', ')'])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTENCE: &str = "The migration is complete and every check passed without errors. ";

    fn feed_in_pieces(guard: &mut ProseRepeatGuard, text: &str, piece: usize) -> usize {
        let chars: Vec<char> = text.chars().collect();
        chars
            .chunks(piece)
            .filter(|chunk| guard.feed(&chunk.iter().collect::<String>()).is_some())
            .count()
    }

    #[test]
    fn the_same_sentence_across_stream_boundaries_fires_exactly_once() {
        for piece in [1, 7, 31, 500] {
            let mut guard = ProseRepeatGuard::default();
            let text = SENTENCE.repeat(STOP_AFTER + 3);
            assert_eq!(feed_in_pieces(&mut guard, &text, piece), 1, "piece size {piece}");
        }
    }

    #[test]
    fn fewer_repeats_than_the_threshold_do_not_fire() {
        let mut guard = ProseRepeatGuard::default();
        assert_eq!(feed_in_pieces(&mut guard, &SENTENCE.repeat(STOP_AFTER - 1), 9), 0);
    }

    #[test]
    fn code_tables_lists_and_ordinary_talking_points_never_fire() {
        let code = format!(
            "```rust\n{}```\n",
            "let value = compute_the_expensive_thing(input);\n".repeat(10)
        );
        let table = "| name | value | description of the column |\n".repeat(10);
        let list = "- This item restates the identical talking point for the reader\n".repeat(10);
        let numbered = "1. This item restates the identical talking point for the reader\n".repeat(10);
        let varied = format!(
            "{} We checked the parser. {} We checked the writer. {} We checked the reader. {} ",
            SENTENCE.trim(),
            SENTENCE.trim(),
            SENTENCE.trim(),
            SENTENCE.trim()
        );
        for (name, text) in [
            ("code", code),
            ("table", table),
            ("list", list),
            ("numbered", numbered),
            ("varied prose", varied),
        ] {
            let mut guard = ProseRepeatGuard::default();
            assert_eq!(feed_in_pieces(&mut guard, &text, 13), 0, "{name}");
        }
    }
}

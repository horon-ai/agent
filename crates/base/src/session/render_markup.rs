//! Reasoning-line markdown formatting.
//!
//! Persisted reasoning traces are re-rendered as the same dim+italic markdown the
//! live streaming path emitted, so a reloaded, resumed, or remotely synced
//! session reads exactly like the stream it replaced. These pure string helpers
//! live with the session renderer that consumes them.

/// Invisible separator placed just inside both ends of an emphasis run so the
/// flanking `*` are always adjacent to non-whitespace (see
/// [`reasoning_line_markup`]).
pub(crate) const REASONING_SENTINEL: &str = "\u{2063}";

const REASONING_ESCAPES: &str = "\\*_`[]<>&~|$";

/// Recover the original Markdown from a line produced by
/// [`reasoning_line_markup`]. Native frontends can style reasoning themselves
/// instead of interpreting the terminal's escaped emphasis wrapper. Ordinary
/// Markdown is never unescaped here.
pub fn reasoning_line_content(line: &str) -> Option<String> {
    let body = line
        .trim_end_matches([' ', '\r', '\n'])
        .strip_prefix("*\u{2063}")?
        .strip_suffix("\u{2063}*")?;
    let mut chars = body.chars().peekable();
    let mut content = String::with_capacity(body.len());
    while let Some(ch) = chars.next() {
        let escaped = (ch == '\\')
            .then(|| chars.next_if(|next| REASONING_ESCAPES.contains(*next)))
            .flatten();
        content.push(escaped.unwrap_or(ch));
    }
    Some(content)
}

/// Split a rendered history body into the reasoning it starts with and the
/// answer that follows.
///
/// The daemon writes a stored assistant row as the reasoning markup, the block
/// terminator, then the answer. Blank lines inside the reasoning are paragraph
/// breaks and belong to it; the blank line that ends the block is dropped.
/// Answers `None` when the body carries no reasoning markup, so a caller can
/// keep the body as it is for every other row.
pub fn split_rendered_reasoning(content: &str) -> Option<(String, String)> {
    // Every reasoning line is wrapped in the sentinel, so a body without one
    // carries no reasoning: answer before walking lines or filling buffers.
    if !content.contains(REASONING_SENTINEL) {
        return None;
    }
    let mut reasoning: Vec<String> = Vec::new();
    let mut blanks = 0usize;
    let mut answer: Vec<&str> = Vec::new();
    let mut in_answer = false;
    for line in content.split('\n') {
        if in_answer {
            answer.push(line);
            continue;
        }
        if line.trim().is_empty() {
            blanks += 1;
            continue;
        }
        match reasoning_line_content(line) {
            Some(text) => {
                for _ in 0..blanks {
                    reasoning.push(String::new());
                }
                blanks = 0;
                reasoning.push(text);
            }
            None => {
                answer.push(line);
                in_answer = true;
            }
        }
    }
    if reasoning.is_empty() {
        return None;
    }
    Some((reasoning.join("\n"), answer.join("\n")))
}

/// Escape the characters that would otherwise be interpreted as inline markdown
/// inside a reasoning line, so the body renders literally inside the dim/italic
/// emphasis run.
fn escape_reasoning_inline_markdown(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    for ch in line.chars() {
        if REASONING_ESCAPES.contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Wrap a completed reasoning line as dim+italic markdown.
///
/// Empty lines become a bare newline (no empty emphasis run). The result always
/// ends in a CommonMark hard break (`"  \n"`).
///
/// The trailing two spaces are a CommonMark *hard break*: without them,
/// consecutive reasoning lines (each terminated by a single `\n`) collapse into
/// one paragraph where the line breaks render as spaces, so multi-line thinking
/// shows up as a single run-on line. The hard break keeps each reasoning line on
/// its own visual row, matching the model's line structure.
///
/// The sentinel must wrap both ends because CommonMark's emphasis flanking rules
/// require the opening `*` to not be followed by whitespace and the closing `*`
/// to not be preceded by whitespace. A reasoning line that starts or ends with
/// whitespace (or is whitespace-only) would otherwise leave the asterisks as
/// literal text and break the dim/italic styling. The zero-width sentinels
/// guarantee both asterisks are flanked by non-whitespace regardless of the body.
pub fn reasoning_line_markup(line: &str) -> String {
    if line.is_empty() {
        "\n".to_string()
    } else {
        format!(
            "*{0}{1}{0}*  \n",
            REASONING_SENTINEL,
            escape_reasoning_inline_markdown(line)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_line_markup_matches_streamed_markup() {
        assert_eq!(
            reasoning_line_markup("step one"),
            format!("*{REASONING_SENTINEL}step one{REASONING_SENTINEL}*  \n")
        );
    }

    #[test]
    fn reasoning_line_markup_escapes_inline_markdown() {
        assert_eq!(
            reasoning_line_markup("a*b_c"),
            format!("*{REASONING_SENTINEL}a\\*b\\_c{REASONING_SENTINEL}*  \n")
        );
    }

    #[test]
    fn reasoning_line_markup_blank_line_is_bare_newline() {
        assert_eq!(reasoning_line_markup(""), "\n");
    }

    #[test]
    fn reasoning_markup_round_trips_original_markdown() {
        for text in [
            "**Checking top live tabs**",
            "  **é文字** and `code`  ",
            r"literal \*star\*, C:\work, [docs](https://example.com), $x_1$",
            "\\*_`[]<>&~|$",
            " ",
        ] {
            let markup = reasoning_line_markup(text);
            assert_eq!(reasoning_line_content(&markup).as_deref(), Some(text));
        }
    }

    #[test]
    fn reasoning_decoder_requires_both_sentinels_and_wrapper() {
        for text in ["**heading**", r"\*literal\*", "*\u{2063}unfinished", "*ordinary*", ""] {
            assert_eq!(reasoning_line_content(text), None);
        }
    }

    /// A body without reasoning markup is answered `None`, and one carrying
    /// the markup the renderer writes splits into its lines and the answer.
    #[test]
    fn split_rendered_reasoning_names_the_answer_only_when_reasoning_precedes_it() {
        assert_eq!(split_rendered_reasoning("the answer\nsecond line"), None);
        let body = format!(
            "{}{}the answer",
            reasoning_line_markup("step one"),
            reasoning_line_markup("step two"),
        );
        assert_eq!(
            split_rendered_reasoning(&body),
            Some(("step one\nstep two".to_string(), "the answer".to_string()))
        );
    }
}

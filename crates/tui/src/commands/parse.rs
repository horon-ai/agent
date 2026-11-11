//! Reading a command file, and the substitution applied when it runs.
//!
//! The block between two `---` lines is not YAML. Only `key: value` lines are
//! read, which covers `description` and `argument-hint` without adding a
//! parser dependency for a two-key block. An unreadable line is skipped rather
//! than failing the file: a command that still has its body is more useful than
//! no command at all.
//!
//! Substitution is one pass over the body. `$ARGUMENTS` is the whole argument
//! string and `$1`…`$9` are the whitespace-split words; a placeholder with no
//! value becomes empty. Argument text is never scanned again, so an argument
//! that contains a dollar sign cannot inject a placeholder.

/// A command file after its frontmatter has been separated from its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// The frontmatter `description`, when the file has one.
    pub description: Option<String>,
    /// The frontmatter `argument-hint`, when the file has one.
    pub argument_hint: Option<String>,
    /// The text sent as a message, before substitution.
    pub body: String,
}

/// Split a command file into frontmatter and body.
///
/// A file that does not start with a `---` line has no frontmatter. A block
/// that never closes is not frontmatter either: the whole file is the body,
/// because a missing delimiter must not silently delete text.
pub fn parse(text: &str) -> ParsedCommand {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return ParsedCommand {
            description: None,
            argument_hint: None,
            body: text.trim_end().to_string(),
        };
    }
    let mut description = None;
    let mut argument_hint = None;
    let mut closed = false;
    for line in lines.by_ref() {
        let line = line.trim();
        if line == "---" {
            closed = true;
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "description" => description = Some(value),
            "argument-hint" => argument_hint = Some(value),
            _ => {}
        }
    }
    if !closed {
        return ParsedCommand {
            description: None,
            argument_hint: None,
            body: text.trim_end().to_string(),
        };
    }
    let body = lines.collect::<Vec<&str>>().join("\n");
    ParsedCommand {
        description,
        argument_hint,
        body: body.trim_end().to_string(),
    }
}

/// The first non-empty body line, trimmed to 60 characters.
///
/// A file without a frontmatter description still needs one line in `/help`,
/// and its body's first line is the closest thing to a summary it has.
pub fn fallback_description(body: &str) -> String {
    let line = match body.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line,
        None => return String::new(),
    };
    let mut text: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        text.push_str("...");
    }
    text
}

/// Replace `$ARGUMENTS` and `$1`…`$9` in one pass.
pub fn substitute(body: &str, arguments: &str) -> String {
    let arguments = arguments.trim();
    let words: Vec<&str> = arguments.split_whitespace().collect();
    let mut text = String::with_capacity(body.len() + arguments.len());
    let mut rest = body;
    while let Some(index) = rest.find('$') {
        text.push_str(&rest[..index]);
        let tail = &rest[index + 1..];
        if let Some(after) = tail.strip_prefix("ARGUMENTS") {
            text.push_str(arguments);
            rest = after;
            continue;
        }
        let mut characters = tail.chars();
        match characters.next() {
            // A second digit means the line names position 10 or more, which a
            // command file has no way to receive; leave it as written.
            Some(digit @ '1'..='9') if !matches!(characters.clone().next(), Some('0'..='9')) => {
                let position = usize::from(digit as u8 - b'0');
                if let Some(word) = words.get(position - 1) {
                    text.push_str(word);
                }
                rest = characters.as_str();
            }
            _ => {
                text.push('$');
                rest = tail;
            }
        }
    }
    text.push_str(rest);
    text
}

/// Strip one matching pair of quotes around a frontmatter value.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

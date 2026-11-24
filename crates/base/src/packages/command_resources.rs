//! Slash command resources a plugin declares.
//!
//! A plugin's `plugin.toml` may declare Markdown files under `commands`. Each
//! one becomes `/id:<file stem>`. The file is body text and expands into a user
//! message with the arguments typed after the command; it never runs a process
//! and never pulls another file in. The executable commands a package declares
//! in `extension.toml` share the name space of these stems, so one package
//! cannot offer the same name twice and the installer refuses it.
//!
//! Both boundaries are checked here:
//!
//! - Install: [`validate_command_files`] runs from `validate_resources`, so a
//!   path that leaves the package, an empty body, an oversized file or a name
//!   the TUI could not type is refused before anything is published.
//! - Generation load: [`read_commands`] reads the installed copy the catalog
//!   record names, proves every declared file still matches the recorded length
//!   and SHA-256, and returns the parsed command beside the file identities the
//!   text cache revalidates.
//!
//! Every read goes through `manifest::open_package_file`, which refuses an
//! absolute path, a `..` component and a link, so a declared command can only
//! ever come from inside the installed package root.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::PackageRecord;
use super::manifest::{open_package_file, relative_path};
use super::text_resources::{read_capped_prefix, read_verified, resource_stem};
use crate::skill::FileStamp;

/// Upper bound on one command resource.
pub const MAX_COMMAND_BYTES: u64 = 64 * 1024;
/// Upper bound on every command resource one package declares together.
pub const MAX_PACKAGE_COMMAND_BYTES: usize = 256 * 1024;

/// One Markdown command a package declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCommand {
    /// ID of the package that declared the command.
    pub package_id: String,
    /// The name typed after the slash: `<package id>:<file stem>`.
    pub name: String,
    /// The file stem, which is the name the package declares.
    pub command: String,
    /// The frontmatter description, or the body's first line.
    pub description: String,
    /// The frontmatter `argument-hint`, when the file has one.
    pub argument_hint: Option<String>,
    /// The body, before `$ARGUMENTS` substitution.
    pub body: String,
    /// Package-relative path as the manifest declared it.
    pub relative_path: String,
}

/// The parsed parts of one command file.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ParsedCommand {
    pub(super) description: Option<String>,
    pub(super) argument_hint: Option<String>,
    pub(super) body: String,
}

/// The identity of every file one load read, for the text cache to revalidate.
pub(super) type CommandFiles = Vec<(PathBuf, FileStamp)>;

/// Read, verify and parse every declared command of one installed copy.
///
/// The file identities come back with the commands so the caller's cache can
/// tell an unchanged copy from one whose bytes moved.
pub(super) fn read_commands(
    root: &Path,
    record: &PackageRecord,
    declared: &[String],
) -> Result<(Vec<PackageCommand>, CommandFiles)> {
    let mut commands = Vec::with_capacity(declared.len());
    let mut files = Vec::with_capacity(declared.len());
    let mut names = BTreeSet::new();
    let mut total = 0usize;
    for path in declared {
        let file = read_verified(root, record, path, MAX_COMMAND_BYTES)?;
        ensure!(!file.text.trim().is_empty(), "package command is empty: {path}");
        total += file.text.len();
        ensure!(
            total <= MAX_PACKAGE_COMMAND_BYTES,
            "package commands exceed {MAX_PACKAGE_COMMAND_BYTES} bytes"
        );
        let stem = resource_stem(path, "command")?;
        ensure!(names.insert(stem), "duplicate package command name: {stem}");
        let parsed = parse(&file.text);
        ensure!(!parsed.body.trim().is_empty(), "package command has no body: {path}");
        commands.push(PackageCommand {
            package_id: record.id.clone(),
            name: format!("{}:{stem}", record.id),
            command: stem.to_owned(),
            description: match parsed.description {
                Some(description) => description,
                None => fallback_description(&parsed.body),
            },
            argument_hint: parsed.argument_hint,
            body: parsed.body,
            relative_path: file.relative,
        });
        let stamp = FileStamp::capture(&file.path).with_context(|| format!("inspect the package command {path}"))?;
        files.push((file.path, stamp));
    }
    commands.sort_by(|left, right| left.name.cmp(&right.name));
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files.dedup_by(|left, right| left.0 == right.0);
    Ok((commands, files))
}

/// Validate the command files a plugin declares inside `root`.
///
/// Called from `validate_resources` before anything is published, and again on
/// the staged copy by the store.
pub fn validate_command_files(root: &Path, declared: &[String]) -> Result<()> {
    let mut names = BTreeSet::new();
    let mut total = 0usize;
    for path in declared {
        let relative = relative_path(path)?;
        let mut file = open_package_file(root, &relative)?;
        let raw = read_capped_prefix(&mut file, MAX_COMMAND_BYTES, &format!("the package command {path}"))?;
        ensure!(
            raw.len() as u64 <= MAX_COMMAND_BYTES,
            "package command {path} exceeds {MAX_COMMAND_BYTES} bytes"
        );
        let bytes = String::from_utf8(raw).with_context(|| format!("read the package command {path}"))?;
        total += bytes.len();
        ensure!(
            total <= MAX_PACKAGE_COMMAND_BYTES,
            "package commands exceed {MAX_PACKAGE_COMMAND_BYTES} bytes"
        );
        ensure!(!bytes.trim().is_empty(), "package command is empty: {path}");
        let stem = resource_stem(path, "command")?;
        ensure!(names.insert(stem), "duplicate package command name: {stem}");
        let parsed = parse(&bytes);
        ensure!(!parsed.body.trim().is_empty(), "package command has no body: {path}");
    }
    Ok(())
}

/// Expand one template with the arguments typed after it.
///
/// `$ARGUMENTS` is the whole argument string and `$1`…`$9` are the
/// whitespace-split words; a placeholder with no value becomes empty. The
/// argument text is copied, never scanned again, so an argument holding a dollar
/// sign cannot inject a placeholder. This is the rule a project command file
/// follows, restated here because the terminal client owns that parser.
pub fn expand(body: &str, arguments: &str) -> String {
    let Some(first) = body.find('$') else {
        return body.to_owned();
    };
    let arguments = arguments.trim();
    let mut words_cache: Option<([&str; 9], usize)> = None;
    let mut text = String::with_capacity(body.len() + arguments.len());
    text.push_str(&body[..first]);
    let mut rest = &body[first..];
    while let Some(index) = rest.find('$') {
        text.push_str(&rest[..index]);
        let tail = &rest[index + 1..];
        if let Some(after) = tail.strip_prefix("ARGUMENTS") {
            text.push_str(arguments);
            rest = after;
            continue;
        }
        if let Some(&digit @ b'1'..=b'9') = tail.as_bytes().first()
            && !matches!(tail.as_bytes().get(1), Some(b'0'..=b'9'))
        {
            let (words, count) = words_cache.get_or_insert_with(|| {
                let mut buf = [""; 9];
                let mut len = 0usize;
                for word in arguments.split_whitespace().take(9) {
                    buf[len] = word;
                    len += 1;
                }
                (buf, len)
            });
            let position = usize::from(digit - b'0');
            if position <= *count {
                text.push_str(words[position - 1]);
            }
            rest = &tail[1..];
        } else {
            text.push('$');
            rest = tail;
        }
    }
    text.push_str(rest);
    text
}

/// The first non-empty body line, trimmed to 60 characters.
///
/// A file without a frontmatter description still needs one line in a listing.
pub fn fallback_description(body: &str) -> String {
    let Some(line) = body.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return String::new();
    };
    match line.char_indices().nth(60) {
        Some((cutoff, _)) => {
            let mut text = String::with_capacity(cutoff + 3);
            text.push_str(&line[..cutoff]);
            text.push_str("...");
            text
        }
        None => line.to_owned(),
    }
}

/// Split one command file into frontmatter and body.
///
/// The block between two `---` lines is not YAML. Only `key: value` lines are
/// read, which covers `description` and `argument-hint`. An unreadable line is
/// skipped rather than failing the file, and a block that never closes is not
/// frontmatter at all: the whole file is the body, because a missing delimiter
/// must not silently delete text.
fn parse(text: &str) -> ParsedCommand {
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
        let key = key.trim();
        if key.eq_ignore_ascii_case("description") {
            description = Some(value.to_owned());
        } else if key.eq_ignore_ascii_case("argument-hint") {
            argument_hint = Some(value.to_owned());
        }
    }
    if !closed {
        return ParsedCommand {
            description: None,
            argument_hint: None,
            body: text.trim_end().to_string(),
        };
    }
    let body_len: usize = lines
        .clone()
        .enumerate()
        .map(|(idx, line)| line.len() + usize::from(idx > 0))
        .sum();
    let mut body = String::with_capacity(body_len);
    for (idx, line) in lines.enumerate() {
        if idx > 0 {
            body.push('\n');
        }
        body.push_str(line);
    }
    body.truncate(body.trim_end().len());
    ParsedCommand {
        description,
        argument_hint,
        body,
    }
}

/// Strip one matching pair of quotes around a frontmatter value.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_substitutes_the_whole_string_and_each_word() {
        assert_eq!(
            expand("review $ARGUMENTS now", "  src/main.rs tests  "),
            "review src/main.rs tests now"
        );
        assert_eq!(expand("first $1 second $2", "alpha"), "first alpha second ");
        assert_eq!(expand("keep $10 and $x", "alpha"), "keep $10 and $x");
        assert_eq!(expand("$1 $1", "same"), "same same");
    }

    #[test]
    fn an_argument_is_never_scanned_for_a_placeholder() {
        assert_eq!(expand("run $ARGUMENTS", "$1"), "run $1");
    }

    #[test]
    fn frontmatter_supplies_the_description_and_hint() {
        let parsed = parse("---\ndescription: Explain a file\nargument-hint: <path>\n---\nRead $1.\n");
        assert_eq!(parsed.description.as_deref(), Some("Explain a file"));
        assert_eq!(parsed.argument_hint.as_deref(), Some("<path>"));
        assert_eq!(parsed.body, "Read $1.");
    }

    #[test]
    fn an_unclosed_block_stays_body_text() {
        let parsed = parse("---\ndescription: broken\nthe whole file\n");
        assert_eq!(parsed.description, None);
        assert_eq!(parsed.body, "---\ndescription: broken\nthe whole file");
    }

    #[test]
    fn a_missing_description_falls_back_to_the_first_body_line() {
        assert_eq!(
            fallback_description("\n\n  Summarize the tree  \nmore"),
            "Summarize the tree"
        );
        assert_eq!(fallback_description(&"x".repeat(80)), format!("{}...", "x".repeat(60)));
    }

    #[test]
    fn a_command_name_follows_the_typed_grammar() {
        assert_eq!(
            resource_stem("commands/review.md", "command").expect("a valid name"),
            "review"
        );
        assert!(resource_stem("commands/Review.md", "command").is_err());
        assert!(resource_stem("commands/has space.md", "command").is_err());
    }
}

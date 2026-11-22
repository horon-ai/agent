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
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::PackageRecord;
use super::manifest::{open_package_file, relative_path};
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

/// Read, verify and parse every declared command of one installed copy.
///
/// The file identities come back with the commands so the caller's cache can
/// tell an unchanged copy from one whose bytes moved.
pub(super) fn read_commands(
    root: &Path,
    record: &PackageRecord,
    declared: &[String],
) -> Result<(Vec<PackageCommand>, Vec<(PathBuf, FileStamp)>)> {
    let mut commands = Vec::with_capacity(declared.len());
    let mut files = Vec::with_capacity(declared.len());
    let mut names = BTreeSet::new();
    let mut total = 0usize;
    for path in declared {
        let (relative, absolute, bytes) = read_verified(root, record, path)?;
        ensure!(!bytes.trim().is_empty(), "package command is empty: {path}");
        total += bytes.len();
        ensure!(
            total <= MAX_PACKAGE_COMMAND_BYTES,
            "package commands exceed {MAX_PACKAGE_COMMAND_BYTES} bytes"
        );
        let stem = command_stem(path)?;
        ensure!(
            names.insert(stem.to_owned()),
            "duplicate package command name: {stem}"
        );
        let parsed = parse(&bytes);
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
            relative_path: relative,
        });
        files.push((absolute, FileStamp::capture(&absolute).with_context(|| {
            format!("inspect the package command {path}")
        })?));
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
        let file = open_package_file(root, &relative)?;
        let mut bytes = String::new();
        file.take(MAX_COMMAND_BYTES + 1)
            .read_to_string(&mut bytes)
            .with_context(|| format!("read the package command {path}"))?;
        ensure!(
            bytes.len() as u64 <= MAX_COMMAND_BYTES,
            "package command {path} exceeds {MAX_COMMAND_BYTES} bytes"
        );
        total += bytes.len();
        ensure!(
            total <= MAX_PACKAGE_COMMAND_BYTES,
            "package commands exceed {MAX_PACKAGE_COMMAND_BYTES} bytes"
        );
        ensure!(!bytes.trim().is_empty(), "package command is empty: {path}");
        let stem = command_stem(path)?;
        ensure!(
            names.insert(stem.to_owned()),
            "duplicate package command name: {stem}"
        );
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
            // command has no way to receive; leave it as written.
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

/// The first non-empty body line, trimmed to 60 characters.
///
/// A file without a frontmatter description still needs one line in a listing.
pub fn fallback_description(body: &str) -> String {
    let Some(line) = body.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return String::new();
    };
    let mut text: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        text.push_str("...");
    }
    text
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
    ParsedCommand {
        description,
        argument_hint,
        body: lines.collect::<Vec<&str>>().join("\n").trim_end().to_string(),
    }
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

/// The file stem of one declared command, as a name a user can type.
fn command_stem(declared: &str) -> Result<&str> {
    let stem = Path::new(declared)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .with_context(|| format!("command resource needs a name: {declared}"))?;
    ensure!(
        !stem.is_empty()
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'),
        "invalid package command name: {declared}"
    );
    Ok(stem)
}

/// Read one declared file and prove its bytes are the recorded ones.
fn read_verified(root: &Path, record: &PackageRecord, declared: &str) -> Result<(String, PathBuf, String)> {
    let stored = record
        .files
        .iter()
        .find(|file| file.path == declared)
        .with_context(|| format!("{declared} is not a recorded file of package {}", record.id))?;
    let relative = relative_path(declared)?;
    let file = open_package_file(root, &relative)?;
    let mut bytes = Vec::new();
    file.take(MAX_COMMAND_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {declared}"))?;
    ensure!(
        bytes.len() as u64 <= MAX_COMMAND_BYTES,
        "{declared} exceeds {MAX_COMMAND_BYTES} bytes"
    );
    ensure!(
        bytes.len() as u64 == stored.len,
        "package file {declared} no longer matches the recorded length"
    );
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    ensure!(
        hex::encode(hasher.finalize()) == stored.sha256,
        "package file {declared} no longer matches the recorded SHA-256"
    );
    let text = String::from_utf8(bytes).with_context(|| format!("package file {declared} is not UTF-8"))?;
    Ok((declared.to_owned(), root.join(&relative), text))
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
        assert_eq!(fallback_description("\n\n  Summarize the tree  \nmore"), "Summarize the tree");
        assert_eq!(
            fallback_description(&"x".repeat(80)),
            format!("{}...", "x".repeat(60))
        );
    }

    #[test]
    fn a_command_name_follows_the_typed_grammar() {
        assert_eq!(command_stem("commands/review.md").expect("a valid name"), "review");
        assert!(command_stem("commands/Review.md").is_err());
        assert!(command_stem("commands/has space.md").is_err());
    }
}

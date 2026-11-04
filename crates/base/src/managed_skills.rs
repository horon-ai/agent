//! Skills the model authors itself through `skill_manage create`/`update`.
//!
//! Managed skills live beside the authored ones but in a directory of their
//! own (`~/.horon-agent/managed-skills/<name>/SKILL.md`), so an authored skill
//! always wins a name collision and deleting the directory removes everything
//! the model wrote. Only [`sanitize_description`]d text is ever persisted: the
//! description reaches the system prompt verbatim, so markup characters and
//! invisible format characters are removed on the way in and again on load.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Largest `SKILL.md` this module writes. A skill longer than this is a
/// document: the body rides in the system prompt once the skill is loaded, so
/// the cap keeps one managed skill from crowding out everything else.
pub const MAX_SKILL_BYTES: usize = 64_000;

/// Directory holding every managed skill.
pub fn dir() -> Result<PathBuf> {
    Ok(crate::storage::horon_agent_dir()?.join("managed-skills"))
}

/// Validates and normalizes a managed skill name.
///
/// Names are lowercase letters, digits, and hyphens (1-64 characters, starting
/// with a letter or digit). The error names the raw input, because a model that
/// passed `My_Skill` must see what it actually asked for.
pub fn validate_name(raw: &str) -> Result<String> {
    let name = raw.trim().to_lowercase();
    let mut chars = name.chars();
    let valid = name.len() <= 64
        && chars
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if !valid {
        anyhow::bail!(
            "Invalid skill name \"{raw}\". Use lowercase letters, digits, and hyphens (1-64 chars, starting with a letter or digit)."
        );
    }
    Ok(name)
}

/// Strips the characters that must not reach the prompt from a description.
///
/// Control and format characters become spaces, `<`, `>`, and `` ` `` are
/// deleted (they would break the prompt's markup), a run of `~` collapses to
/// one, and all whitespace collapses to single spaces.
pub fn sanitize_description(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_control() || is_format_char(ch) {
            cleaned.push(' ');
        } else if !matches!(ch, '<' | '>' | '`') {
            cleaned.push(ch);
        }
    }

    let mut out = String::with_capacity(cleaned.len());
    for word in cleaned.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut previous_tilde = false;
        for ch in word.chars() {
            if ch == '~' {
                if previous_tilde {
                    continue;
                }
                previous_tilde = true;
            } else {
                previous_tilde = false;
            }
            out.push(ch);
        }
    }
    out
}

/// Unicode "format" (Cf) characters plus the byte-order mark, which are
/// invisible in a terminal but split words and hide text in a prompt.
fn is_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
    )
}

/// Path of one managed skill's file.
pub fn skill_file(name: &str) -> Result<PathBuf> {
    Ok(dir()?.join(name).join("SKILL.md"))
}

/// Whether a managed skill with this (already validated) name is on disk.
pub fn exists(name: &str) -> Result<bool> {
    Ok(skill_file(name)?.is_file())
}

/// Writes a managed skill and returns the file's path.
///
/// Refuses a symlinked skill directory or file: the write would otherwise land
/// outside the managed directory. The description is sanitized here, and the
/// body is trimmed before the trailing newline so a re-read reproduces it.
pub fn write(name: &str, description: &str, body: &str) -> Result<PathBuf> {
    let skill_dir = dir()?.join(name);
    refuse_symlink(&skill_dir)?;
    let file = skill_dir.join("SKILL.md");
    refuse_symlink(&file)?;

    let description = sanitize_description(description);
    anyhow::ensure!(
        !description.is_empty(),
        "A managed skill needs a description: the skills list shows it to the model."
    );
    let body = body.trim();
    anyhow::ensure!(!body.is_empty(), "A managed skill needs a body.");

    let content = format!(
        "---\nname: {name}\ndescription: {description}\n---\n\n{body}\n",
        description = yaml_scalar(&description)
    );
    anyhow::ensure!(
        content.len() <= MAX_SKILL_BYTES,
        "Skill file is {} bytes; the limit is {MAX_SKILL_BYTES}.",
        content.len()
    );

    crate::storage::write_bytes(&file, content.as_bytes())?;
    Ok(file)
}

fn refuse_symlink(path: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        anyhow::bail!(
            "Refusing to write a managed skill through a symlink: {}",
            path.display()
        );
    }
    Ok(())
}

/// Renders `value` as a single-line YAML scalar.
///
/// A plain description such as `Review pull requests` stays unquoted; one that
/// YAML would read differently (`Use for: parsing`) is quoted, so the file the
/// loader parses back is always the file that was written.
fn yaml_scalar(value: &str) -> String {
    match serde_yaml::from_str::<String>(value) {
        Ok(parsed) if parsed == value => value.to_string(),
        _ => serde_yaml::to_string(value)
            .map(|rendered| rendered.trim_end().to_string())
            .unwrap_or_else(|_| value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_removes_prompt_markup_and_collapses_space() {
        assert_eq!(
            sanitize_description("  Review <b>pull</b> `requests`\n\n  and ~~merge~~ "),
            "Review bpull/b requests and ~merge~"
        );
        assert_eq!(sanitize_description("zero\u{200b}width\ttext"), "zero width text");
        assert_eq!(sanitize_description("~~~"), "~");
    }

    #[test]
    fn name_validation_lowercases_and_rejects_separators() {
        assert_eq!(validate_name("  Review-PR ").unwrap(), "review-pr");
        assert!(validate_name("").is_err());
        assert!(validate_name("-leading").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name(&"a".repeat(65)).is_err());
        assert_eq!(
            validate_name("Bad Name").unwrap_err().to_string(),
            "Invalid skill name \"Bad Name\". Use lowercase letters, digits, and hyphens (1-64 chars, starting with a letter or digit)."
        );
    }

    #[test]
    fn yaml_scalar_quotes_only_when_plain_text_would_change() {
        assert_eq!(yaml_scalar("Review pull requests"), "Review pull requests");
        assert_eq!(yaml_scalar("Use for: parsing"), "'Use for: parsing'");
    }
}

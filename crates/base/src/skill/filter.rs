//! Per-process skill allow list.
//!
//! `--skills <patterns>` keeps only matching skills and `--no-skills` keeps none.
//! Every read of the registry (listing, lookup, slash invocation) goes through
//! [`allows`], so a filtered skill is absent from the prompt, from the picker
//! and from a model's load request alike. A skill is judged by both the key it
//! is stored under and the name in its frontmatter, so a namespaced alias of a
//! filtered skill stays filtered too.

use anyhow::{Result, bail};

/// Set to `1` to hide every skill from this process.
pub const NO_SKILLS_ENV: &str = "HORON_AGENT_NO_SKILLS";
/// Comma-separated allow patterns; `*` matches any run of characters.
pub const SKILLS_ALLOW_ENV: &str = "HORON_AGENT_SKILLS";

const MAX_PATTERNS: usize = 32;
const MAX_PATTERN_LEN: usize = 64;

/// Split and check a pattern list, so a bad one fails before the first request.
pub fn parse_patterns(raw: &str) -> Result<Vec<String>> {
    let patterns: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_string)
        .collect();
    if patterns.is_empty() {
        bail!("--skills needs at least one name or pattern");
    }
    if patterns.len() > MAX_PATTERNS {
        bail!(
            "--skills accepts at most {MAX_PATTERNS} patterns, got {}",
            patterns.len()
        );
    }
    for pattern in &patterns {
        if pattern.len() > MAX_PATTERN_LEN {
            bail!("--skills pattern {pattern:?} is longer than {MAX_PATTERN_LEN} bytes");
        }
        if let Some(bad) = pattern
            .chars()
            .find(|ch| !(ch.is_alphanumeric() || matches!(ch, '-' | '_' | '/' | ':' | '*')))
        {
            bail!("--skills pattern {pattern:?} contains {bad:?}; use letters, digits, '-', '_', '/', ':' and '*'");
        }
    }
    Ok(patterns)
}

/// Whether the skill stored under `key`, whose frontmatter name is `name`, is visible.
pub(crate) fn allows(key: &str, name: &str) -> bool {
    if std::env::var(NO_SKILLS_ENV).is_ok_and(|value| value == "1") {
        return false;
    }
    let Ok(raw) = std::env::var(SKILLS_ALLOW_ENV) else {
        return true;
    };
    let patterns: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .collect();
    patterns
        .iter()
        .any(|pattern| matches(pattern, key) && matches(pattern, name))
}

/// `*` wildcard match, linear in the text with one backtrack point.
fn matches(pattern: &str, text: &str) -> bool {
    let (pattern, text) = (pattern.as_bytes(), text.as_bytes());
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|byte| *byte == b'*')
}

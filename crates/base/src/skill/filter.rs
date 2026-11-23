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

/// Compiled per-process skill allow filter, evaluated once per registry read.
pub(crate) enum SkillFilter {
    Disabled,
    AllowAll,
    Patterns(String),
}

impl SkillFilter {
    /// Snapshot `HORON_AGENT_NO_SKILLS` and `HORON_AGENT_SKILLS` once.
    pub(crate) fn from_env() -> Self {
        if std::env::var_os(NO_SKILLS_ENV).is_some_and(|value| value == "1") {
            return Self::Disabled;
        }
        let Ok(raw) = std::env::var(SKILLS_ALLOW_ENV) else {
            return Self::AllowAll;
        };
        if raw.split(',').all(|part| part.trim().is_empty()) {
            return Self::Disabled;
        }
        Self::Patterns(raw)
    }

    pub(crate) fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// Collect references to every skill in `skills` permitted by this filter.
    pub(crate) fn select<'a>(
        &self,
        skills: &'a std::collections::HashMap<String, super::Skill>,
    ) -> Vec<&'a super::Skill> {
        match self {
            Self::Disabled => Vec::new(),
            Self::AllowAll => skills.values().collect(),
            Self::Patterns(raw) => {
                let mut inline = [(0u32, 0u32); MAX_PATTERNS];
                let mut inline_len = 0usize;
                let mut overflow = Vec::new();
                let mut cursor = 0usize;
                for part in raw.split(',') {
                    let part_start = cursor;
                    cursor += part.len() + 1;
                    let leading = part.len() - part.trim_start().len();
                    let trimmed = part[leading..].trim_end();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let start = part_start + leading;
                    let end = start + trimmed.len();
                    if inline_len < MAX_PATTERNS
                        && let (Ok(s), Ok(e)) = (u32::try_from(start), u32::try_from(end))
                    {
                        inline[inline_len] = (s, e);
                        inline_len += 1;
                    } else {
                        overflow.push((start, end));
                    }
                }
                let mut matched = Vec::with_capacity(skills.len().min(64));
                for (key, skill) in skills {
                    let name = skill.name.as_str();
                    let same = key.as_str() == name;
                    let allowed = inline[..inline_len]
                        .iter()
                        .map(|&(start, end)| &raw[start as usize..end as usize])
                        .chain(overflow.iter().map(|&(start, end)| &raw[start..end]))
                        .any(|pattern| matches(pattern, key) && (same || matches(pattern, name)));
                    if allowed {
                        matched.push(skill);
                    }
                }
                matched
            }
        }
    }

    /// Whether the skill stored under `key`, whose frontmatter name is `name`, is visible.
    pub(crate) fn allows(&self, key: &str, name: &str) -> bool {
        match self {
            Self::Disabled => false,
            Self::AllowAll => true,
            Self::Patterns(raw) => {
                let same = key == name;
                raw.split(',')
                    .map(str::trim)
                    .filter(|pattern| !pattern.is_empty())
                    .any(|pattern| matches(pattern, key) && (same || matches(pattern, name)))
            }
        }
    }
}

/// Whether the skill stored under `key`, whose frontmatter name is `name`, is visible.
pub(crate) fn allows(key: &str, name: &str) -> bool {
    if std::env::var_os(NO_SKILLS_ENV).is_some_and(|value| value == "1") {
        return false;
    }
    let Ok(raw) = std::env::var(SKILLS_ALLOW_ENV) else {
        return true;
    };
    let same = key == name;
    raw.split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| matches(pattern, key) && (same || matches(pattern, name)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::SkillRegistry;
    use crate::skill::test_support::test_skill;
    use std::ffi::OsString;

    struct SkillEnvGuard {
        prev_no_skills: Option<OsString>,
        prev_allow: Option<OsString>,
    }

    impl SkillEnvGuard {
        fn set(no_skills: Option<&str>, allow: Option<&str>) -> Self {
            let prev_no_skills = std::env::var_os(NO_SKILLS_ENV);
            let prev_allow = std::env::var_os(SKILLS_ALLOW_ENV);
            match no_skills {
                Some(val) => crate::env::set_var(NO_SKILLS_ENV, val),
                None => crate::env::remove_var(NO_SKILLS_ENV),
            }
            match allow {
                Some(val) => crate::env::set_var(SKILLS_ALLOW_ENV, val),
                None => crate::env::remove_var(SKILLS_ALLOW_ENV),
            }
            Self {
                prev_no_skills,
                prev_allow,
            }
        }
    }

    impl Drop for SkillEnvGuard {
        fn drop(&mut self) {
            match self.prev_no_skills.take() {
                Some(val) => crate::env::set_var(NO_SKILLS_ENV, val),
                None => crate::env::remove_var(NO_SKILLS_ENV),
            }
            match self.prev_allow.take() {
                Some(val) => crate::env::set_var(SKILLS_ALLOW_ENV, val),
                None => crate::env::remove_var(SKILLS_ALLOW_ENV),
            }
        }
    }

    #[test]
    fn parse_patterns_validates_empty_count_length_and_characters() {
        assert!(parse_patterns("").is_err());
        assert!(parse_patterns(" ,  , ").is_err());

        let too_many = std::iter::repeat_n("a", MAX_PATTERNS + 1).collect::<Vec<_>>().join(",");
        assert!(parse_patterns(&too_many).is_err());

        let exact_max = std::iter::repeat_n("a", MAX_PATTERNS).collect::<Vec<_>>().join(",");
        assert_eq!(parse_patterns(&exact_max).expect("32 patterns").len(), MAX_PATTERNS);

        let too_long = "x".repeat(MAX_PATTERN_LEN + 1);
        assert!(parse_patterns(&too_long).is_err());

        assert!(parse_patterns("bad name").is_err());
        assert!(parse_patterns("foo?bar").is_err());
        assert!(parse_patterns("foo.bar").is_err());

        assert_eq!(
            parse_patterns(" alpha-*, beta_1/sub:ns:* ").expect("valid patterns"),
            vec!["alpha-*".to_string(), "beta_1/sub:ns:*".to_string()]
        );
    }

    #[test]
    fn no_skills_env_hides_all_skills_and_overrides_allow_list() {
        let _lock = crate::storage::lock_test_env();
        let _env = SkillEnvGuard::set(Some("1"), Some("alpha"));
        let mut registry = SkillRegistry::default();
        registry.insert_skill(test_skill("alpha", "Alpha", "body"));
        registry.insert_skill(test_skill("beta", "Beta", "body"));

        assert!(registry.list().is_empty());
        assert!(registry.get("alpha").is_none());
        assert!(!registry.contains("alpha"));
        assert_eq!(registry.resolve_invocation("/skill:alpha"), None);
    }

    #[test]
    fn allow_patterns_filter_registry_and_shadowed_key_and_name_together() {
        let _lock = crate::storage::lock_test_env();
        let mut registry = SkillRegistry::default();
        registry.insert_skill(test_skill("alpha-one", "Alpha", "body"));
        registry.insert_skill(test_skill("beta-one", "Beta", "body"));
        registry.insert_skill(test_skill("gamma-one", "Gamma", "body"));

        {
            let _env = SkillEnvGuard::set(None, Some(" alpha-* , beta-* "));
            let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, vec!["alpha-one", "beta-one"]);
            assert!(registry.contains("alpha-one"));
            assert!(!registry.contains("gamma-one"));
            assert!(registry.resolve_invocation("/skill:alpha-one").is_some());
            assert_eq!(registry.resolve_invocation("/skill:gamma-one"), None);

            // Both the stored key and the frontmatter name must match a pattern.
            assert!(!allows("plugin/gamma", "gamma"));
        }

        {
            let _env = SkillEnvGuard::set(None, Some("plugin/*"));
            assert!(!allows("plugin/secret", "secret"));
            assert!(allows("plugin/secret", "plugin/secret"));
        }

        {
            let _env = SkillEnvGuard::set(None, Some(""));
            assert!(registry.list().is_empty());
            assert!(registry.get("alpha-one").is_none());
        }
    }
}

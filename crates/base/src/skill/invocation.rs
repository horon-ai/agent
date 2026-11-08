use super::SkillRegistry;

/// A slash-command skill invocation with an optional trailing prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillInvocation<'a> {
    pub name: &'a str,
    pub prompt: Option<&'a str>,
}

/// Whether a name can be addressed as `/name`.
///
/// A registered name may contain any character a skill name can carry: spaces,
/// a `/` between namespace and name, and non-ASCII letters. The one thing it
/// must not look like is a filesystem path, because a terminal file drop such as
/// `/tmp/screenshot.png` is ordinary user input: a segment with a dot or a
/// backslash reads as a path, and such a name stays unaddressable even when a
/// skill happens to be registered under it.
fn is_addressable(name: &str) -> bool {
    !name.contains('\\')
        && name
            .split('/')
            .all(|segment| !segment.is_empty() && !segment.contains('.'))
}

/// The identifier shape [`SkillRegistry::parse_invocation`] accepts on its own.
fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

impl SkillRegistry {
    /// Parse `/skill-name` and `/skill-name prompt...` invocations.
    ///
    /// The trailing prompt is kept verbatim apart from surrounding whitespace.
    /// Quotes are intentionally not interpreted as shell syntax, so incomplete
    /// or literal quotes can never put the input path into a continuation state.
    /// Skill command tokens are limited to identifier-like names. In particular,
    /// path separators and filename punctuation are rejected so a terminal file
    /// drop such as `/tmp/screenshot.png` remains ordinary user input.
    pub fn parse_invocation(input: &str) -> Option<SkillInvocation<'_>> {
        let trimmed = input.trim();
        let invocation = trimmed.strip_prefix('/')?;
        let name_end = invocation.find(char::is_whitespace).unwrap_or(invocation.len());
        let name = &invocation[..name_end];
        if !is_identifier(name) {
            return None;
        }

        let prompt = invocation[name_end..].trim();
        let prompt = match prompt.as_bytes() {
            [b'"', .., b'"'] | [b'\'', .., b'\''] if prompt.len() >= 2 => &prompt[1..prompt.len() - 1],
            _ => prompt,
        };
        Some(SkillInvocation {
            name,
            prompt: (!prompt.is_empty()).then_some(prompt),
        })
    }

    /// Resolve a slash invocation against registered names.
    ///
    /// Registered names may contain spaces and a namespace separator, so every
    /// whitespace boundary is tried longest first and the first registered,
    /// addressable name wins. Unknown names fall back to the identifier-shaped
    /// result from [`Self::parse_invocation`], which keeps a terminal file drop
    /// as ordinary input.
    pub fn resolve_invocation<'a>(&self, input: &'a str) -> Option<SkillInvocation<'a>> {
        let invocation = input.trim().strip_prefix('/')?;
        let boundaries = std::iter::once(invocation.len()).chain(
            invocation
                .char_indices()
                .rev()
                .filter_map(|(index, ch)| ch.is_whitespace().then_some(index)),
        );

        for end in boundaries {
            let name = &invocation[..end];
            if name.is_empty() || !is_addressable(name) || !self.skills.contains_key(name) {
                continue;
            }
            return Some(SkillInvocation {
                name,
                prompt: split_prompt(&invocation[end..]),
            });
        }

        Self::parse_invocation(input)
    }
}

/// The trailing prompt of one invocation, without its surrounding quotes.
fn split_prompt(rest: &str) -> Option<&str> {
    let prompt = rest.trim();
    let prompt = match prompt.as_bytes() {
        [b'"', .., b'"'] | [b'\'', .., b'\''] if prompt.len() >= 2 => &prompt[1..prompt.len() - 1],
        _ => prompt,
    };
    (!prompt.is_empty()).then_some(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::test_support::test_skill;

    fn registry(names: &[&str]) -> SkillRegistry {
        let mut registry = SkillRegistry::default();
        for name in names {
            registry.insert_skill(test_skill(name, "Test skill", "content"));
        }
        registry
    }

    #[test]
    fn matches_multi_word_skill_names() {
        let registry = registry(&["My Custom Skill"]);
        assert_eq!(
            registry.resolve_invocation("/My Custom Skill"),
            Some(SkillInvocation {
                name: "My Custom Skill",
                prompt: None,
            })
        );
    }

    #[test]
    fn keeps_trailing_prompt_after_multi_word_name() {
        let registry = registry(&["My Custom Skill"]);
        assert_eq!(
            registry.resolve_invocation("/My Custom Skill only the staged diff"),
            Some(SkillInvocation {
                name: "My Custom Skill",
                prompt: Some("only the staged diff"),
            })
        );
        assert_eq!(
            registry.resolve_invocation("/My Custom Skill 'only the staged diff'"),
            registry.resolve_invocation("/My Custom Skill only the staged diff")
        );
    }

    #[test]
    fn prefers_longest_registered_match() {
        let registry = registry(&["My", "My Custom Skill"]);
        assert_eq!(
            registry.resolve_invocation("/My Custom Skill please"),
            Some(SkillInvocation {
                name: "My Custom Skill",
                prompt: Some("please"),
            })
        );
    }

    #[test]
    fn falls_back_to_single_token_when_no_registry_match_exists() {
        let registry = SkillRegistry::default();
        assert_eq!(
            registry.resolve_invocation("/not-a-real-skill with a prompt"),
            Some(SkillInvocation {
                name: "not-a-real-skill",
                prompt: Some("with a prompt"),
            })
        );
    }

    #[test]
    fn does_not_bypass_terminal_file_drop_guard() {
        let registry = registry(&["tmp/screenshot.png"]);
        assert_eq!(registry.resolve_invocation("/tmp/screenshot.png"), None);
        assert_eq!(registry.resolve_invocation("/tmp/screenshot.png describe this"), None);
    }

    #[test]
    fn preserves_single_word_skill_invocations() {
        let registry = registry(&["frontend-design"]);
        assert_eq!(
            registry.resolve_invocation("/frontend-design build a settings page"),
            Some(SkillInvocation {
                name: "frontend-design",
                prompt: Some("build a settings page"),
            })
        );
    }

    #[test]
    fn matches_a_namespaced_skill_under_its_registered_name() {
        let registry = registry(&["name", "claude/name"]);
        assert_eq!(
            registry.resolve_invocation("/claude/name do the thing"),
            Some(SkillInvocation {
                name: "claude/name",
                prompt: Some("do the thing"),
            })
        );
        assert_eq!(
            registry.resolve_invocation("/name do the thing"),
            Some(SkillInvocation {
                name: "name",
                prompt: Some("do the thing"),
            })
        );
    }

    #[test]
    fn namespaced_names_with_spaces_and_unicode_are_invocable() {
        let registry = registry(&["claude/My Skill", "플러그인/x"]);
        assert_eq!(
            registry.resolve_invocation("/claude/My Skill do it"),
            Some(SkillInvocation {
                name: "claude/My Skill",
                prompt: Some("do it"),
            })
        );
        assert_eq!(
            registry.resolve_invocation("/플러그인/x go"),
            Some(SkillInvocation {
                name: "플러그인/x",
                prompt: Some("go"),
            })
        );
    }

    #[test]
    fn a_path_shaped_registered_name_stays_uninvocable() {
        let registry = registry(&["tmp/x.png"]);
        assert_eq!(registry.resolve_invocation("/tmp/x.png"), None);
    }

    #[test]
    fn namespaced_match_takes_the_longest_registered_name() {
        let registry = registry(&["claude/name", "claude/name~2"]);
        assert_eq!(
            registry.resolve_invocation("/claude/name~2 do the thing"),
            Some(SkillInvocation {
                name: "claude/name~2",
                prompt: Some("do the thing"),
            })
        );
    }
}

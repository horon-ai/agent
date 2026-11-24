//! Package rules as a lower-priority, source-labelled prompt block.
//!
//! A package's rules are guidance the operator installed, not the session's
//! instructions: they render after AGENTS.md, the prompt overlay and the
//! preferred-tools guidance, and they say so in the block itself. The block
//! never replaces the base system prompt and never changes what AGENTS.md
//! contributes, so an install cannot silently rewrite the session.
//!
//! The rendered block is bounded twice: install refuses a package whose own
//! rules exceed `packages::MAX_PACKAGE_RULE_BYTES`, and this renderer stops
//! adding rule files once the whole block reaches [`MAX_RULES_SECTION_BYTES`],
//! so a stack of packages cannot crowd the session prompt out. Rule text is
//! inserted as body text; no project-file citation inside it is expanded.

use super::SplitSystemPrompt;
use crate::packages::PackageTextResources;

/// Byte cap on the whole rendered block, across every package.
pub const MAX_RULES_SECTION_BYTES: usize = 16 * 1024;

const RULES_HEADER: &str = "# Package Rules\n\n\
     Rules below come from installed packages. They rank below AGENTS.md and the session instructions above: \
     where a package rule conflicts with an earlier instruction, the earlier instruction wins.";

/// Render the package rule block, or `None` when no package declares a rule.
///
/// The order is the loader's: packages by ID, rules by their package-relative
/// path, so the same resources render the same bytes on every request.
pub fn render_rules(resources: &PackageTextResources) -> Option<String> {
    resources.rules().next()?;
    let mut total_len = RULES_HEADER.len();
    let mut omitted = 0usize;
    for rule in resources.rules() {
        let block_len = "\n\n## Package ".len()
            + rule.package_id.len()
            + " (".len()
            + rule.relative_path.len()
            + ")\n\n".len()
            + rule.text.trim().len();
        if total_len + block_len > MAX_RULES_SECTION_BYTES {
            omitted += 1;
            continue;
        }
        total_len += block_len;
    }
    let capacity = if omitted > 0 { total_len + 96 } else { total_len };
    let mut section = String::with_capacity(capacity);
    section.push_str(RULES_HEADER);
    for rule in resources.rules() {
        let body = rule.text.trim();
        let block_len = "\n\n## Package ".len()
            + rule.package_id.len()
            + " (".len()
            + rule.relative_path.len()
            + ")\n\n".len()
            + body.len();
        if section.len() + block_len > MAX_RULES_SECTION_BYTES {
            continue;
        }
        section.push_str("\n\n## Package ");
        section.push_str(&rule.package_id);
        section.push_str(" (");
        section.push_str(&rule.relative_path);
        section.push_str(")\n\n");
        section.push_str(body);
    }
    if omitted > 0
        && std::fmt::Write::write_fmt(
            &mut section,
            format_args!(
                "\n\n({omitted} package rule file(s) omitted to keep this block within {MAX_RULES_SECTION_BYTES} bytes.)"
            ),
        )
        .is_err()
    {
        return Some(section);
    }
    Some(section)
}

/// Append the package rule block to a split prompt's static part.
///
/// The block is static for the same reason the other captured sections are: a
/// turn that changed nothing must produce the same cacheable prefix. A package
/// install or removal changes it only from the next turn, because the generation
/// that supplies `resources` is the one the turn captured.
pub fn append_package_rules(split: &mut SplitSystemPrompt, resources: &PackageTextResources) {
    let Some(section) = render_rules(resources) else {
        return;
    };
    if !split.static_part.is_empty() {
        split.static_part.push_str("\n\n");
    }
    split.static_part.push_str(&section);
    debug_assert!(
        section.len() <= MAX_RULES_SECTION_BYTES + 256,
        "the package rules block stays within its budget plus the notice"
    );
}

//! `skill://` reads: a skill's instructions, or a file inside its directory.
//!
//! `skill://<name>` returns the body of the skill's SKILL.md under a short
//! header naming its directory, and `skill://<name>/<path>` returns a file
//! relative to that directory. Names may carry a namespace (`claude/review`),
//! so the longest registered name that prefixes the reference wins and the
//! remainder is the path.
//!
//! The registry is the session's effective one: global skills plus the
//! workspace overlay of `ctx.working_dir`, so a project skill is readable only
//! from its own workspace. A file is resolved to its real location, which must
//! stay inside the skill's real directory; the deny policy then judges that
//! real path, so a skill cannot be used to read a file the policy refuses.

use super::{ReadInput, page_text};
use crate::skill::{Skill, SkillRegistry};
use crate::tool::{ToolContext, ToolOutput};
use anyhow::{Result, anyhow};
use serde_json::json;
use std::path::{Path, PathBuf};

pub(super) const SCHEME: &str = "skill://";

pub(super) async fn read_skill_url(params: &ReadInput, ctx: &ToolContext) -> Result<ToolOutput> {
    let (uri, selector) = crate::tool::read_selector::split_uri_and_selector(&params.file_path)?;
    let reference = uri
        .get(..SCHEME.len())
        .filter(|scheme| scheme.eq_ignore_ascii_case(SCHEME))
        .map(|_| &uri[SCHEME.len()..])
        .ok_or_else(|| anyhow!("not a skill:// reference: {uri}"))?
        .trim()
        .trim_end_matches('/');
    if reference.is_empty() {
        return Err(anyhow!("skill:// needs a skill name, as in skill://<name>"));
    }

    let registry = effective_registry(ctx).await;
    let (skill, relative) = split_reference(&registry, reference)
        .ok_or_else(|| anyhow!("skill:// names no installed skill: {reference}"))?;

    let output = match relative {
        None => {
            let body = skill.body()?;
            let page = page_text(params, &selector, &body)?;
            format!(
                "## Skill: {}\nBase directory: {}\n\n{page}",
                skill.name,
                skill_dir(skill).display()
            )
        }
        Some(relative) => {
            let target = resolve_inside(skill, relative)?;
            crate::tool::path_policy::enforce("read", &json!({ "file_path": target.to_string_lossy() }), ctx)?;
            let content = tokio::fs::read_to_string(&target)
                .await
                .map_err(|error| anyhow!("skill://{}/{relative}: {error}", skill.name))?;
            page_text(params, &selector, &content)?
        }
    };
    Ok(ToolOutput::new(output).with_title(params.file_path.clone()))
}

/// The session's skills: the shared global registry with this workspace's
/// overlay on top.
async fn effective_registry(ctx: &ToolContext) -> SkillRegistry {
    let global = SkillRegistry::shared_registry();
    let base = global.read().await.clone();
    SkillRegistry::effective_for_working_dir(&base, ctx.working_dir.as_deref())
}

/// Split `reference` into the longest registered skill name it starts with and
/// the path after it. `None` when no registered name prefixes it.
fn split_reference<'r, 'a>(registry: &'r SkillRegistry, reference: &'a str) -> Option<(&'r Skill, Option<&'a str>)> {
    if let Some(skill) = registry.get(reference) {
        return Some((skill, None));
    }
    reference
        .char_indices()
        .rev()
        .filter(|(_, ch)| *ch == '/')
        .find_map(|(index, _)| {
            let relative = &reference[index + 1..];
            registry
                .get(&reference[..index])
                .filter(|_| !relative.is_empty())
                .map(|skill| (skill, Some(relative)))
        })
}

fn skill_dir(skill: &Skill) -> &Path {
    skill.path.parent().unwrap_or(Path::new("."))
}

/// The real path of `relative` inside the skill directory, refused when it
/// leaves that directory through `..`, an absolute path, or a symlink.
fn resolve_inside(skill: &Skill, relative: &str) -> Result<PathBuf> {
    let dir = skill_dir(skill);
    let root = dir
        .canonicalize()
        .map_err(|error| anyhow!("skill://{}: cannot open the skill directory: {error}", skill.name))?;
    let target = dir
        .join(relative)
        .canonicalize()
        .map_err(|error| anyhow!("skill://{}/{relative}: {error}", skill.name))?;
    if !target.starts_with(&root) {
        return Err(anyhow!("skill:// path resolves outside the skill directory"));
    }
    Ok(target)
}

#[cfg(test)]
#[path = "read_skill_tests.rs"]
mod tests;

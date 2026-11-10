//! `skill_manage install` (issue #79): clone a skill into the managed
//! directory.
//!
//! The installer itself lives in the skill crate (`skill_install`); this file
//! is the tool's front end: it decides which source the call named, applies the
//! `[tools] managed_skills` switch, and reloads the registry after the files
//! land.

use super::{MANAGED_SKILLS_DISABLED, SkillTool, ToolOutput, managed_skills_enabled};
use anyhow::Result;

impl SkillTool {
    /// `install`: clones a skill into the managed directory.
    ///
    /// A name that matches an endorsed entry installs the catalog's fixed
    /// repository; a `source` installs the caller's git source, and a name
    /// given beside it chooses the skill inside a repository that holds more
    /// than one. The `[tools] managed_skills` switch applies here as it does to
    /// `create`, because the install writes into the same directory.
    pub(super) async fn install_skill(
        &self,
        name: Option<String>,
        source: Option<String>,
        git_ref: Option<String>,
    ) -> Result<ToolOutput> {
        if !managed_skills_enabled() {
            return Ok(ToolOutput::new(MANAGED_SKILLS_DISABLED));
        }
        let name = name.map(|name| name.trim().to_string()).filter(|name| !name.is_empty());
        let source = source
            .map(|source| source.trim().to_string())
            .filter(|source| !source.is_empty());
        let endorsed = match (&name, &source) {
            (Some(name), None) => Some(crate::skill_install::endorsed(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "No endorsed skill is named '{name}'. Use action=list to see the catalog, or pass `source` with the git URL of the skill to install."
                )
            })?),
            (None, None) => anyhow::bail!(
                "An install needs either the name of an endorsed skill or a git `source`."
            ),
            _ => None,
        };
        let hint = name.clone();
        let installed = tokio::task::spawn_blocking(move || match (endorsed, source) {
            (Some(entry), _) => crate::skill_install::install_endorsed(entry),
            (None, Some(source)) => crate::skill_install::install(&source, git_ref.as_deref(), hint.as_deref()),
            (None, None) => unreachable!("both sources were checked above"),
        })
        .await
        .map_err(|error| anyhow::anyhow!("the skill install task failed: {error}"))??;
        let reload_note = self.reload_after_managed_change(&installed.name).await;
        Ok(ToolOutput::new(format!(
            "Installed skill '{}' from {} at {}\n\nPath: {}\nDescription: {}{reload_note}",
            installed.name,
            installed.source,
            &installed.commit[..installed.commit.len().min(12)],
            installed.path.display(),
            installed.description
        ))
        .with_title(format!("Skills: Installed {}", installed.name)))
    }
}

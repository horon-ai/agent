//! The prompt inputs an agent captures once per session boundary.
//!
//! AGENTS.md, the base prompt, the prompt overlay, the preferred-tools guidance
//! and the skills list all feed the cached static prefix. They are read when a
//! session starts, is restored or cleared, or moves to another working
//! directory, and never between two requests of the same session: a request
//! that changed nothing reads no file, and a file edited mid-session cannot
//! move the prefix a provider has already cached.

use super::*;

impl Agent {
    pub(super) fn refresh_agents_md_snapshot(&mut self) {
        let working_dir = self.session.working_dir.as_deref().map(std::path::Path::new);
        self.agents_md_snapshot = crate::prompt::load_agents_md_files_from_dir(working_dir);
        self.refresh_static_prompt_sources();
        self.refresh_prompt_skills_snapshot();
    }

    /// Re-read the base prompt, the overlay and the preferred-tools guidance
    /// for the session's current working directory.
    pub(super) fn refresh_static_prompt_sources(&mut self) {
        let working_dir = self.session.working_dir.as_deref().map(std::path::Path::new);
        self.static_prompt_sources = crate::prompt::StaticPromptSources::load(working_dir);
    }

    /// Re-capture the frozen skills list and render its system-prompt section.
    /// Only called at session boundaries (new/restored session, working
    /// directory change), never mid-turn.
    pub(super) fn refresh_prompt_skills_snapshot(&mut self) {
        self.prompt_skills_snapshot = self.current_prompt_skill_infos();
        self.prompt_skills_section = crate::prompt::build_available_skills_section(&self.prompt_skills_snapshot);
        self.announced_skills = self
            .prompt_skills_snapshot
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        self.announced_skills_scan_index = 0;
    }

    /// The authored skills, without package entries.
    ///
    /// This is what the boundary snapshot and the late-skill reminder compare,
    /// so a package skill is announced by its prompt section alone instead of
    /// twice.
    pub(super) fn current_prompt_skill_infos(&self) -> Vec<crate::prompt::SkillInfo> {
        self.session_skills_snapshot()
            .list()
            .iter()
            .map(Self::skill_info)
            .collect()
    }

    fn skill_info(skill: &&crate::skill::Skill) -> crate::prompt::SkillInfo {
        crate::prompt::SkillInfo {
            name: skill.name.clone(),
            description: skill.description.clone(),
        }
    }

    /// Global and project-local skills for this session, without packages.
    ///
    /// Global skills come from the process-wide shared registry; the
    /// project-local overlay is composed fresh from this session's workspace
    /// root so per-repo skills are session-scoped, immediately visible, and
    /// never leak across sessions (issue #457).
    pub(super) fn session_skills_snapshot(&self) -> std::sync::Arc<crate::skill::SkillRegistry> {
        let global = self
            .registry
            .skills()
            .try_read()
            .map(|skills| std::sync::Arc::new(skills.clone()))
            .unwrap_or_else(|_| self.skills.clone());
        let working_dir = self.session.working_dir.as_deref().map(std::path::Path::new);
        std::sync::Arc::new(crate::skill::SkillRegistry::effective_for_working_dir(
            &global,
            working_dir,
        ))
    }

    /// The skills this turn may use: the authored ones plus the packages'.
    ///
    /// Package skills come from the generation this turn captured, so they
    /// appear from the next turn after an install and disappear from the next
    /// turn after a removal. They are the lowest priority source: a name an
    /// authored skill already claims shadows the package entry.
    pub(super) fn current_skills_snapshot(&self) -> std::sync::Arc<crate::skill::SkillRegistry> {
        let effective = self.session_skills_snapshot();
        match self.package_text_resources() {
            Some(resources) => {
                std::sync::Arc::new(crate::skill::SkillRegistry::with_package_skills(&effective, &resources))
            }
            None => effective,
        }
    }

    /// The text resources of the packages this turn's snapshot lists.
    ///
    /// `None` before a turn captured a snapshot and on a home with no package,
    /// so a session without packages reads nothing here. The read goes through
    /// [`crate::packages::PackageTextResources::load`], which reuses an unchanged
    /// package's cached text and revalidates a replaced or tampered copy.
    pub(super) fn package_text_resources(&self) -> Option<std::sync::Arc<crate::packages::PackageTextResources>> {
        let snapshot = self.package_snapshot()?;
        if snapshot.is_empty() {
            return None;
        }
        let store = match crate::packages::PackageStore::for_current_user() {
            Ok(store) => store,
            Err(error) => {
                logging::warn(&format!(
                    "package text resources for session {} are unavailable: {error:#}",
                    self.session.id
                ));
                return None;
            }
        };
        let resources = crate::packages::PackageTextResources::load(&store, snapshot.workspace(), snapshot.packages());
        if resources.is_empty() {
            return None;
        }
        Some(resources)
    }

    /// The "Available Skills" section for this request.
    ///
    /// Without a package skill this is the section frozen at the session
    /// boundary. With one, the list is re-rendered from the turn's snapshot, so
    /// a package installed or removed since the boundary is listed from the next
    /// turn while an unchanged package set renders the same bytes. Package skills
    /// are deliberately absent from the boundary snapshot and from the late-skill
    /// reminder: this section is the one place they are announced.
    pub(super) fn request_skills_section(
        &self,
        package_text: Option<&crate::packages::PackageTextResources>,
    ) -> Option<std::borrow::Cow<'_, str>> {
        if let Some(section) = self.package_skills_section(package_text) {
            return Some(std::borrow::Cow::Owned(section));
        }
        self.prompt_skills_section.as_deref().map(std::borrow::Cow::Borrowed)
    }

    fn package_skills_section(&self, package_text: Option<&crate::packages::PackageTextResources>) -> Option<String> {
        package_text?.skills().next()?;
        let infos = self
            .current_skills_snapshot()
            .list()
            .iter()
            .map(Self::skill_info)
            .collect::<Vec<_>>();
        crate::prompt::build_available_skills_section(&infos)
    }

    /// The prompt a command named, from the packages this turn's snapshot lists.
    ///
    /// Nothing on the prompt path calls this: a package prompt reaches a turn
    /// only because a command asked for it by name.
    pub fn package_prompt(&self, name: &str) -> Option<String> {
        let resources = self.package_text_resources()?;
        resources.prompt(name).map(str::to_owned)
    }

    /// Whether the listed-tools index has been stored, without copying it.
    pub(super) fn listed_tools_index_built(&self) -> bool {
        self.listed_tools_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// The index rows, or `None` while the index has not been built.
    pub(super) fn listed_tools_rows(&self) -> Option<Vec<(String, String)>> {
        self.listed_tools_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|index| index.rows.clone())
    }

    /// The rendered index section, when this session lists any tool.
    ///
    /// A session that reaches the prompt before anything built the index gets no
    /// section and a warning: guessing here would seed the cached prefix with a
    /// list the session never resolved, and staying silent would hide a missing
    /// call site.
    pub(crate) fn listed_tools_section(&self) -> Option<String> {
        let slot = self
            .listed_tools_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(index) = slot.as_ref() {
            return index.section.clone();
        }
        drop(slot);
        logging::warn("listed tools index not built before prompt");
        None
    }

    /// Token estimate of the system prompt for the prefix log line.
    ///
    /// The static part is estimated once per distinct text and the turn
    /// context per request, so the sum can differ from one pass over the
    /// joined prompt by the rounding of one part.
    pub(super) fn system_prompt_token_estimate(&self, split: &crate::prompt::SplitSystemPrompt) -> usize {
        if split.static_part.is_empty() {
            return split.estimated_tokens();
        }
        let mut cache = self
            .static_token_estimate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let static_tokens = match cache.as_ref() {
            Some((text, tokens)) if *text == split.static_part => *tokens,
            _ => {
                let tokens = crate::util::estimate_tokens(&split.static_part);
                *cache = Some((split.static_part.clone(), tokens));
                tokens
            }
        };
        drop(cache);
        if split.dynamic_part.is_empty() {
            static_tokens
        } else {
            static_tokens + crate::util::estimate_tokens(&split.dynamic_part)
        }
    }
}

/// The listed-tools index of a session: its rows and the prompt section
/// rendered from them when they were stored.
pub(crate) struct ListedToolsIndex {
    pub(super) rows: Vec<(String, String)>,
    pub(super) section: Option<String>,
}

impl ListedToolsIndex {
    pub(super) fn new(rows: Vec<(String, String)>) -> Self {
        let section = crate::prompt::build_additional_tools_section(&rows);
        Self { rows, section }
    }
}

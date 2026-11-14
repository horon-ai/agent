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

    /// Re-capture the frozen skills list. Only called at session boundaries
    /// (new/restored session, working directory change), never mid-turn.
    pub(super) fn refresh_prompt_skills_snapshot(&mut self) {
        self.prompt_skills_snapshot = self.current_prompt_skill_infos();
        self.announced_skills = self
            .prompt_skills_snapshot
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        self.announced_skills_scan_index = 0;
    }

    pub(super) fn current_prompt_skill_infos(&self) -> Vec<crate::prompt::SkillInfo> {
        self.current_skills_snapshot()
            .list()
            .iter()
            .map(|skill| crate::prompt::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect()
    }
}

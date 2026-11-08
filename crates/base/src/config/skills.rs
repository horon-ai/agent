//! `[skills]`: which skill collections beside horon-agent's own are read.
//!
//! horon-agent always reads its own skill directories (`~/.horon-agent/skills`
//! and the workspace `.horon-agent/skills`). Directories owned by other tools
//! are a separate decision: reading them imports another agent's instructions,
//! so each collection has its own switch and every switch is off by default.
//! The source table in `skill::sources` consults these values before it opens a
//! directory.

use serde::{Deserialize, Serialize};

/// Skill sources horon-agent does not own.
///
/// `[skills]` in config.toml. With a switch off, the directory is never opened
/// and startup logs one line naming how many skills are waiting there, so the
/// operator can opt in without first discovering the path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// Read `~/.claude/skills` and the skills of installed Claude Code plugins
    /// under `~/.claude/plugins`. Off by default.
    pub claude_user: bool,
    /// Read `~/.codex/skills`. Off by default.
    pub codex_user: bool,
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;

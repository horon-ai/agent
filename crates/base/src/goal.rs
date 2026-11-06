use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub use horon_agent_task_types::{Goal, GoalMilestone, GoalScope, GoalStatus, GoalStep, GoalUpdate};

#[derive(Debug, Clone, Default)]
pub struct GoalCreateInput {
    pub id: Option<String>,
    pub title: String,
    pub scope: GoalScope,
    pub description: Option<String>,
    pub why: Option<String>,
    pub success_criteria: Vec<String>,
    pub milestones: Vec<GoalMilestone>,
    pub next_steps: Vec<String>,
    pub blockers: Vec<String>,
    pub current_milestone_id: Option<String>,
    pub progress_percent: Option<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct GoalUpdateInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub why: Option<String>,
    pub status: Option<GoalStatus>,
    pub success_criteria: Option<Vec<String>>,
    pub milestones: Option<Vec<GoalMilestone>>,
    pub next_steps: Option<Vec<String>>,
    pub blockers: Option<Vec<String>>,
    pub current_milestone_id: Option<Option<String>>,
    pub progress_percent: Option<Option<u8>>,
    pub checkpoint_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GoalAttachment {
    goal_id: String,
    scope: GoalScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_hash: Option<String>,
    title: String,
    attached_at: DateTime<Utc>,
}

pub fn create_goal(input: GoalCreateInput, working_dir: Option<&Path>) -> Result<Goal> {
    if input.title.trim().is_empty() {
        anyhow::bail!("goal title cannot be empty");
    }
    let mut goal = Goal::new(&input.title, input.scope);
    if let Some(id) = input.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        goal.id = horon_agent_task_types::sanitize_goal_id(id);
    }
    goal.id = next_available_goal_id(&goal.id, goal.scope, working_dir)?;
    goal.description = input.description.unwrap_or_default().trim().to_string();
    goal.why = input.why.unwrap_or_default().trim().to_string();
    goal.success_criteria = trim_vec(input.success_criteria);
    goal.milestones = input.milestones;
    goal.next_steps = trim_vec(input.next_steps);
    goal.blockers = trim_vec(input.blockers);
    goal.current_milestone_id = input.current_milestone_id;
    goal.progress_percent = input.progress_percent.map(|p| p.min(100));
    goal.updated_at = Utc::now();
    save_goal(&goal, working_dir)?;
    sync_goal_memory(&goal, working_dir)?;
    Ok(goal)
}

pub fn update_goal(
    id: &str,
    scope_hint: Option<GoalScope>,
    working_dir: Option<&Path>,
    update: GoalUpdateInput,
) -> Result<Option<Goal>> {
    let Some(mut goal) = load_goal(id, scope_hint, working_dir)? else {
        return Ok(None);
    };

    if let Some(title) = update.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        goal.title = title.to_string();
    }
    if let Some(description) = update.description {
        goal.description = description.trim().to_string();
    }
    if let Some(why) = update.why {
        goal.why = why.trim().to_string();
    }
    if let Some(status) = update.status {
        goal.status = status;
    }
    if let Some(criteria) = update.success_criteria {
        goal.success_criteria = trim_vec(criteria);
    }
    if let Some(milestones) = update.milestones {
        goal.milestones = milestones;
    }
    if let Some(next_steps) = update.next_steps {
        goal.next_steps = trim_vec(next_steps);
    }
    if let Some(blockers) = update.blockers {
        goal.blockers = trim_vec(blockers);
    }
    if let Some(current_milestone_id) = update.current_milestone_id {
        goal.current_milestone_id = current_milestone_id.map(|s| s.trim().to_string());
    }
    if let Some(progress_percent) = update.progress_percent {
        goal.progress_percent = progress_percent.map(|p| p.min(100));
    }
    if let Some(summary) = update
        .checkpoint_summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        goal.updates.push(GoalUpdate {
            at: Utc::now(),
            summary: summary.to_string(),
        });
    }
    goal.updated_at = Utc::now();
    save_goal(&goal, working_dir)?;
    sync_goal_memory(&goal, working_dir)?;
    Ok(Some(goal))
}

pub fn load_goal(id: &str, scope_hint: Option<GoalScope>, working_dir: Option<&Path>) -> Result<Option<Goal>> {
    let id = horon_agent_task_types::sanitize_goal_id(id);
    let mut candidates = Vec::new();
    match scope_hint {
        Some(GoalScope::Global) => candidates.push(goal_file_in_dir(&global_goals_dir()?, &id)),
        Some(GoalScope::Project) => {
            if let Some(dir) = project_goals_dir(working_dir)? {
                candidates.push(goal_file_in_dir(&dir, &id));
            }
        }
        None => {
            if let Some(dir) = project_goals_dir(working_dir)? {
                candidates.push(goal_file_in_dir(&dir, &id));
            }
            candidates.push(goal_file_in_dir(&global_goals_dir()?, &id));
        }
    }

    for path in candidates {
        if path.exists() {
            let goal: Goal =
                crate::storage::read_json(&path).with_context(|| format!("failed to read goal {}", path.display()))?;
            return Ok(Some(goal));
        }
    }
    Ok(None)
}

pub fn list_relevant_goals(working_dir: Option<&Path>) -> Result<Vec<Goal>> {
    let mut goals = load_goals_in_dir(&global_goals_dir()?)?;
    if let Some(project_dir) = project_goals_dir(working_dir)? {
        goals.extend(load_goals_in_dir(&project_dir)?);
    }
    sort_goals(&mut goals);
    Ok(goals)
}

pub fn resume_goal(session_id: &str, working_dir: Option<&Path>) -> Result<Option<Goal>> {
    if let Some(goal) = load_attached_goal(session_id, working_dir)?
        && goal.status.is_resumable()
    {
        return Ok(Some(goal));
    }

    let mut goals = list_relevant_goals(working_dir)?;
    goals.retain(|goal| goal.status.is_resumable());
    Ok(goals.into_iter().next())
}

pub fn attach_goal_to_session(session_id: &str, goal: &Goal, working_dir: Option<&Path>) -> Result<()> {
    let attachment = GoalAttachment {
        goal_id: goal.id.clone(),
        scope: goal.scope,
        project_hash: if goal.scope == GoalScope::Project {
            Some(project_hash(working_dir.ok_or_else(|| {
                anyhow::anyhow!("working_dir required for project goal")
            })?))
        } else {
            None
        },
        title: goal.title.clone(),
        attached_at: Utc::now(),
    };
    let path = session_attachment_path(session_id)?;
    crate::storage::write_json_fast(&path, &attachment)
}

pub fn load_attached_goal(session_id: &str, working_dir: Option<&Path>) -> Result<Option<Goal>> {
    let path = session_attachment_path(session_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let attachment: GoalAttachment = crate::storage::read_json(&path)?;
    if attachment.scope == GoalScope::Project {
        let Some(dir) = working_dir else {
            return Ok(None);
        };
        let current_hash = project_hash(dir);
        if attachment.project_hash.as_deref() != Some(current_hash.as_str()) {
            return Ok(None);
        }
    }
    load_goal(&attachment.goal_id, Some(attachment.scope), working_dir)
}

pub fn render_goals_overview(goals: &[Goal]) -> String {
    let mut out = String::from("# Goals\n\n");
    if goals.is_empty() {
        out.push_str("No goals yet. Use the `goal` tool or `/goals show <id>` after creating one.\n");
        return out;
    }

    for goal in goals {
        out.push_str(&format!(
            "## {}\n- Status: {}\n- Scope: {}\n",
            goal.title,
            goal.status.as_str(),
            goal.scope.as_str()
        ));
        if let Some(progress) = goal.progress_percent {
            out.push_str(&format!("- Progress: {}%\n", progress));
        }
        if let Some(milestone) = goal.current_milestone() {
            out.push_str(&format!("- Current milestone: {}\n", milestone.title));
        }
        if let Some(next_step) = goal.next_steps.first() {
            out.push_str(&format!("- Next step: {}\n", next_step));
        }
        out.push_str(&format!("- Id: `{}`\n\n", goal.id));
    }
    out
}

pub fn render_goal_detail(goal: &Goal) -> String {
    let mut out = format!(
        "# Goal: {}\n\n**Status:** {}  \n**Scope:** {}  \n**Updated:** {}  \n",
        goal.title,
        goal.status.as_str(),
        goal.scope.as_str(),
        goal.updated_at.format("%Y-%m-%d %H:%M")
    );
    if let Some(progress) = goal.progress_percent {
        out.push_str(&format!("**Progress:** {}%  \n", progress));
    }
    out.push('\n');

    if !goal.description.trim().is_empty() {
        out.push_str("## Description\n");
        out.push_str(goal.description.trim());
        out.push_str("\n\n");
    }
    if !goal.why.trim().is_empty() {
        out.push_str("## Why\n");
        out.push_str(goal.why.trim());
        out.push_str("\n\n");
    }
    if !goal.success_criteria.is_empty() {
        out.push_str("## Success criteria\n");
        for item in &goal.success_criteria {
            out.push_str(&format!("- {}\n", item));
        }
        out.push('\n');
    }
    if let Some(milestone) = goal.current_milestone() {
        out.push_str(&format!("## Current milestone\n### {}\n", milestone.title));
        if milestone.steps.is_empty() {
            out.push_str(&format!("- Status: {}\n\n", milestone.status));
        } else {
            for step in &milestone.steps {
                let checked = if step.status == "completed" { "x" } else { " " };
                out.push_str(&format!("- [{}] {}\n", checked, step.content));
            }
            out.push('\n');
        }
    }
    if !goal.milestones.is_empty() {
        out.push_str("## Milestones\n");
        for milestone in &goal.milestones {
            let marker = if goal.current_milestone_id.as_deref() == Some(milestone.id.as_str()) {
                "→"
            } else {
                "-"
            };
            out.push_str(&format!("{} {} ({})\n", marker, milestone.title, milestone.status));
        }
        out.push('\n');
    }
    if !goal.next_steps.is_empty() {
        out.push_str("## Next steps\n");
        for (idx, step) in goal.next_steps.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", idx + 1, step));
        }
        out.push('\n');
    }
    if !goal.blockers.is_empty() {
        out.push_str("## Blockers\n");
        for blocker in &goal.blockers {
            out.push_str(&format!("- {}\n", blocker));
        }
        out.push('\n');
    }
    if !goal.updates.is_empty() {
        out.push_str("## Recent updates\n");
        for update in goal.updates.iter().rev().take(8) {
            out.push_str(&format!("- {}: {}\n", update.at.format("%Y-%m-%d"), update.summary));
        }
    }
    out
}

fn save_goal(goal: &Goal, working_dir: Option<&Path>) -> Result<()> {
    let path = goal_file(goal, working_dir)?;
    crate::storage::write_json_fast(&path, goal)
}

fn goal_file(goal: &Goal, working_dir: Option<&Path>) -> Result<PathBuf> {
    let dir = match goal.scope {
        GoalScope::Global => global_goals_dir()?,
        GoalScope::Project => {
            project_goals_dir(working_dir)?.ok_or_else(|| anyhow::anyhow!("working_dir required for project goal"))?
        }
    };
    Ok(goal_file_in_dir(&dir, &goal.id))
}

fn goal_file_in_dir(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.json", horon_agent_task_types::sanitize_goal_id(id)))
}

fn global_goals_dir() -> Result<PathBuf> {
    Ok(crate::storage::horon_agent_dir()?.join("goals").join("global"))
}

fn project_goals_dir(working_dir: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(dir) = working_dir else {
        return Ok(None);
    };
    Ok(Some(
        crate::storage::horon_agent_dir()?
            .join("goals")
            .join("projects")
            .join(project_hash(dir)),
    ))
}

fn load_goals_in_dir(dir: &Path) -> Result<Vec<Goal>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut goals = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let goal: Goal =
            crate::storage::read_json(&path).with_context(|| format!("failed to read goal {}", path.display()))?;
        goals.push(goal);
    }
    sort_goals(&mut goals);
    Ok(goals)
}

fn sort_goals(goals: &mut [Goal]) {
    goals.sort_by(|a, b| {
        a.status
            .sort_rank()
            .cmp(&b.status.sort_rank())
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.title.cmp(&b.title))
    });
}

fn project_hash(path: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn session_attachment_path(session_id: &str) -> Result<PathBuf> {
    Ok(crate::storage::horon_agent_dir()?
        .join("goals")
        .join("sessions")
        .join(format!("{}.json", session_id)))
}

fn next_available_goal_id(base: &str, scope: GoalScope, working_dir: Option<&Path>) -> Result<String> {
    let mut candidate = horon_agent_task_types::sanitize_goal_id(base);
    let mut idx = 2;
    while load_goal(&candidate, Some(scope), working_dir)?.is_some() {
        candidate = format!("{}-{}", horon_agent_task_types::sanitize_goal_id(base), idx);
        idx += 1;
    }
    Ok(candidate)
}

fn trim_vec(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn sync_goal_memory(goal: &Goal, working_dir: Option<&Path>) -> Result<String> {
    use crate::memory::{MemoryCategory, MemoryEntry, MemoryManager, TrustLevel};

    let manager = match goal.scope {
        GoalScope::Project => MemoryManager::new().with_project_dir(
            working_dir.ok_or_else(|| anyhow::anyhow!("working_dir required for project goal memory sync"))?,
        ),
        GoalScope::Global => MemoryManager::new(),
    };

    let mut entry = MemoryEntry::new(MemoryCategory::Custom("goal".to_string()), goal_memory_content(goal))
        .with_source(format!("goal:{}", goal.id))
        .with_trust(TrustLevel::High)
        .with_tags(goal_memory_tags(goal));
    entry.id = goal_memory_id(goal);
    entry.updated_at = goal.updated_at;
    entry.created_at = goal.created_at;

    match goal.scope {
        GoalScope::Project => manager.upsert_project_memory(entry),
        GoalScope::Global => manager.upsert_global_memory(entry),
    }
}

fn goal_memory_id(goal: &Goal) -> String {
    format!("goal:{}", goal.id)
}

fn goal_memory_tags(goal: &Goal) -> Vec<String> {
    let mut tags = vec![
        "goal".to_string(),
        format!("goal:{}", goal.id),
        format!("goal_status:{}", goal.status.as_str()),
        format!("goal_scope:{}", goal.scope.as_str()),
    ];
    if let Some(current) = goal.current_milestone_id.as_deref() {
        tags.push(format!("goal_milestone:{}", current));
    }
    if !goal.title.trim().is_empty() {
        tags.extend(
            goal.title
                .split(|ch: char| !ch.is_ascii_alphanumeric())
                .map(|part| part.trim().to_ascii_lowercase())
                .filter(|part| part.len() >= 4)
                .take(4)
                .map(|part| format!("goal_term:{}", part)),
        );
    }
    tags.sort();
    tags.dedup();
    tags
}

fn goal_memory_content(goal: &Goal) -> String {
    let mut out = format!(
        "Goal: {}\nStatus: {}\nScope: {}",
        goal.title,
        goal.status.as_str(),
        goal.scope.as_str()
    );
    if let Some(progress) = goal.progress_percent {
        out.push_str(&format!("\nProgress: {}%", progress));
    }
    if let Some(milestone) = goal.current_milestone() {
        out.push_str(&format!("\nCurrent milestone: {}", milestone.title));
    }
    if !goal.description.trim().is_empty() {
        out.push_str(&format!("\nDescription: {}", goal.description.trim()));
    }
    if !goal.why.trim().is_empty() {
        out.push_str(&format!("\nWhy: {}", goal.why.trim()));
    }
    if !goal.next_steps.is_empty() {
        out.push_str("\nNext steps:");
        for step in goal.next_steps.iter().take(3) {
            out.push_str(&format!("\n- {}", step));
        }
    }
    if !goal.blockers.is_empty() {
        out.push_str("\nBlockers:");
        for blocker in goal.blockers.iter().take(3) {
            out.push_str(&format!("\n- {}", blocker));
        }
    }
    out
}

#[cfg(test)]
#[path = "goal_tests.rs"]
mod goal_tests;

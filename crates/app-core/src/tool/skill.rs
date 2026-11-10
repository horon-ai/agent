//! Skill tool - load, list, reload, read, author, install, and read skills

use super::{Tool, ToolContext, ToolOutput};
use crate::skill::SkillRegistry;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::RwLock;

/// `skill_manage install` (issue #79): the managed directory is the only one the
/// action reaches, and plan mode never reaches it at all.
#[path = "skill_install_action.rs"]
mod skill_install_action;

#[cfg(test)]
#[path = "skill_delete_tests.rs"]
mod skill_delete_tests;

#[cfg(test)]
#[path = "skill_install_tests.rs"]
mod skill_install_tests;

pub struct SkillTool {
    registry: Arc<RwLock<SkillRegistry>>,
}

impl SkillTool {
    pub fn new(registry: Arc<RwLock<SkillRegistry>>) -> Self {
        Self { registry }
    }

    /// Effective skill set for this call: shared global registry plus the
    /// session's project-local overlay resolved from the tool context working
    /// dir (issue #457). The overlay is read fresh from disk so edits are
    /// visible without daemon restarts and never enter the shared registry.
    ///
    /// Inside [`crate::skill::SkillRegistry::with_request_overlay`] this reuses
    /// the overlay that request already scanned, so one request scans the
    /// project directories once however many skill lookups it makes. Nothing
    /// survives the request, so the next one still reads the disk.
    async fn effective_registry(&self, working_dir: Option<&std::path::Path>) -> SkillRegistry {
        let global = self.registry.read().await;
        SkillRegistry::effective_for_working_dir(&global, working_dir)
    }
}

#[derive(Deserialize)]
struct SkillInput {
    /// Action to perform: load (default), list, reload, reload_all, read,
    /// create, update, delete.
    /// `list` shows both loaded skills and the horon-agent-endorsed catalog.
    #[serde(default = "default_action")]
    action: String,
    /// Skill name (required for load, reload, read, create, update, delete)
    #[serde(alias = "skill")]
    #[serde(default)]
    name: Option<String>,
    /// Optional Claude-compatible Skill wrapper argument. The skill loader only
    /// needs to load the prompt, so args are currently accepted and ignored.
    #[serde(default)]
    args: Option<String>,
    /// Managed `create`/`update`: the one-line description shown in the skills
    /// list. Sanitized before it reaches the prompt.
    #[serde(default)]
    description: Option<String>,
    /// Managed `create`/`update`: the markdown body of the skill.
    #[serde(default)]
    body: Option<String>,
    /// `install`: a git source to install from. Exactly one of `name` and
    /// `source` selects the source.
    #[serde(default)]
    source: Option<String>,
    /// `install`: branch, tag, or commit to clone.
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
}

fn default_action() -> String {
    "load".to_string()
}

/// Answer for `create`/`update` while `[tools] managed_skills` is off.
pub(super) const MANAGED_SKILLS_DISABLED: &str =
    "Managed skills are disabled. Set [tools] managed_skills = true to let the model create skills.";

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill_manage"
    }

    fn description(&self) -> &str {
        "Manage skills."
    }

    /// `install` clones, so it reaches the network and the disk; authoring and
    /// deletion write; every other action only reads. Plan mode refuses any
    /// call that writes, which is the gate the install action needs.
    fn effects_for_input(&self, input: &Value) -> crate::tool::ToolEffects {
        match input.get("action").and_then(Value::as_str) {
            Some("install") => crate::tool::ToolEffects::read()
                .union(crate::tool::ToolEffects::write())
                .union(crate::tool::ToolEffects::network()),
            Some("create" | "update" | "delete") => {
                crate::tool::ToolEffects::read().union(crate::tool::ToolEffects::write())
            }
            _ => crate::tool::ToolEffects::read(),
        }
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["load", "list", "reload", "reload_all", "read", "create", "update", "delete", "install"],
                    "description": "Action."
                },
                "name": {
                    "type": "string",
                    "description": "Skill name."
                },
                "description": {
                    "type": "string",
                    "description": "Managed skills only: one-line description."
                },
                "body": {
                    "type": "string",
                    "description": "Managed skills only: markdown body."
                },
                "source": {
                    "type": "string",
                    "description": "Install only: git URL or path to clone the skill from, either alone or with a name to choose one skill inside it."
                },
                "ref": {
                    "type": "string",
                    "description": "Install only: branch, tag, or commit to clone. Defaults to the remote's default branch."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: SkillInput = serde_json::from_value(input)?;
        let action_label = params.action.clone();
        let name_label = params.name.clone().unwrap_or_else(|| "<none>".to_string());
        let _args = params.args.as_deref();
        let working_dir = ctx.working_dir.clone();

        // One overlay scan per tool call: every skill lookup below, including
        // the ones `reload_all` makes after reloading, resolves against the
        // same project-local snapshot. The scope ends with this call, so the
        // next request reads the project directories again.
        SkillRegistry::with_request_overlay(working_dir.as_deref(), async {
            match params.action.as_str() {
                "load" => self.load_skill(params.name, working_dir.as_deref()).await,
                "list" => self.list_skills(working_dir.as_deref()).await,
                "reload" => self.reload_skill(params.name).await,
                "reload_all" => self.reload_all_skills(working_dir.as_deref()).await,
                "read" => self.read_skill(params.name, working_dir.as_deref()).await,
                "create" => {
                    self.create_managed_skill(working_dir.as_deref(), params.name, params.description, params.body)
                        .await
                }
                "update" => {
                    self.update_managed_skill(params.name, params.description, params.body)
                        .await
                }
                "delete" => self.delete_managed_skill(working_dir.as_deref(), params.name).await,
                "install" => self.install_skill(params.name, params.source, params.git_ref).await,
                _ => Ok(ToolOutput::new(format!(
                    "Unknown action: {}. Use 'load', 'list', 'reload', 'reload_all', 'read', 'create', 'update', 'delete', or 'install'.",
                    params.action
                ))),
            }
        })
        .await
        .map_err(|err| {
            crate::logging::warn(&format!(
                "[tool:skill_manage] action failed action={} skill={} session_id={} error={}",
                action_label, name_label, ctx.session_id, err
            ));
            err
        })
    }
}

impl SkillTool {
    async fn load_skill(&self, name: Option<String>, working_dir: Option<&std::path::Path>) -> Result<ToolOutput> {
        let name = normalize_skill_name(name, "load")?;

        let registry = self.effective_registry(working_dir).await;
        let skill = registry.get(&name).ok_or_else(|| {
            // Endorsed skills are advertised in `list` but are not bundled;
            // a bare "not found" here reads like a bug (issue #445). Point at
            // the actual install command instead.
            if let Some(endorsed) = crate::skill::endorsed_skills()
                .iter()
                .find(|endorsed| endorsed.name == name)
            {
                match endorsed.install {
                    Some(install) => anyhow::anyhow!(
                        "Skill '{}' is endorsed but not installed. Run skill_manage with action=install and name='{}', or install it with `{}`, then run skill_manage reload_all.",
                        name,
                        name,
                        install
                    ),
                    None => anyhow::anyhow!(
                        "Skill '{}' is endorsed but not installed (source: {}). Install it into ~/.horon/skills/{}/SKILL.md, then run skill_manage reload_all.",
                        name,
                        endorsed.source,
                        name
                    ),
                }
            } else {
                anyhow::anyhow!("Skill '{}' not found", name)
            }
        })?;

        let base_dir = skill
            .path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".to_string());

        let prompt = skill
            .get_prompt()
            .map_err(|error| anyhow::anyhow!("Failed to read skill '{}': {error}", skill.name))?;

        Ok(ToolOutput::new(format!(
            "## Skill: {}\n\n**Base directory**: {}\n\n{}",
            skill.name, base_dir, prompt
        ))
        .with_title(format!("skill: {}", skill.name)))
    }

    async fn list_skills(&self, working_dir: Option<&std::path::Path>) -> Result<ToolOutput> {
        let registry = self.effective_registry(working_dir).await;
        let mut skills = registry.list();
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        let installed: std::collections::HashSet<&str> = skills.iter().map(|s| s.name.as_str()).collect();

        let mut output = if skills.is_empty() {
            format!(
                "No skills loaded.\n\n{}Create a SKILL.md file with YAML frontmatter:\n---\nname: my-skill\n\
                 description: What this skill does\nallowed-tools: bash, read, write\n---\n\n# Skill content here\n",
                skill_source_help()
            )
        } else {
            let mut output = format!("Loaded skills: {}\n\n", skills.len());
            for skill in &skills {
                output.push_str(&format!("## /{}\n", skill.name));
                output.push_str(&format!("  {}\n", skill.description));
                output.push_str(&format!("  Path: {}\n", skill.path.display()));
                if let Some(ref tools) = skill.allowed_tools {
                    output.push_str(&format!("  Tools: {}\n", tools.join(", ")));
                }
                output.push('\n');
            }
            output
        };

        append_endorsed_skills(&mut output, &installed);

        // A skill that lost a name collision still exists under its namespaced
        // address; the merge already wrote these lines to the log, and repeating
        // them here is what tells the operator where the other one went.
        for warning in registry.collision_warnings() {
            output.push_str(&format!("\n{warning}\n"));
        }

        Ok(ToolOutput::new(output).with_title("Skills: List"))
    }

    async fn reload_skill(&self, name: Option<String>) -> Result<ToolOutput> {
        let name = normalize_skill_name(name, "reload")?;

        let mut registry = self.registry.write().await;

        match registry.reload(&name) {
            Ok(true) => {
                // Re-read to get updated info
                if let Some(skill) = registry.get(&name) {
                    Ok(ToolOutput::new(format!(
                        "Reloaded skill '{}'\n\nDescription: {}\nPath: {}",
                        name,
                        skill.description,
                        skill.path.display()
                    ))
                    .with_title(format!("Skills: Reloaded {}", name)))
                } else {
                    Ok(ToolOutput::new(format!("Reloaded skill '{}'", name))
                        .with_title(format!("Skills: Reloaded {}", name)))
                }
            }
            Ok(false) => Ok(ToolOutput::new(format!(
                "Skill '{}' not found or was deleted.\n\nUse 'list' to see available skills.",
                name
            ))
            .with_title("Skills: Not found")),
            Err(e) => {
                crate::logging::warn(&format!("[tool:skill_manage] reload failed skill={} error={}", name, e));
                Ok(ToolOutput::new(format!("Failed to reload skill '{}': {}", name, e))
                    .with_title("Skills: Reload failed"))
            }
        }
    }

    async fn reload_all_skills(&self, working_dir: Option<&std::path::Path>) -> Result<ToolOutput> {
        // Reload the shared GLOBAL registry; the project-local overlay is
        // session-scoped, so it is only dropped from the per-workspace cache and
        // rescanned for this session. Reloading it into the shared registry
        // would leak this session's project skills to other sessions (#457).
        SkillRegistry::forget_project_overlay(working_dir);
        let reloaded = {
            let mut registry = self.registry.write().await;
            registry.reload_global()
        };

        match reloaded {
            Ok(global_count) => {
                // The request scope pinned the overlay from before the reload,
                // so the effective count is composed from a fresh scan.
                let effective = {
                    let global = self.registry.read().await;
                    SkillRegistry::compose_overlay(&global, SkillRegistry::load_project_overlay(working_dir)?)
                };
                let skills = effective.list();
                let mut output = format!(
                    "Reloaded {} global skills ({} effective for this session)\n\n",
                    global_count,
                    skills.len()
                );

                for skill in skills {
                    output.push_str(&format!("- /{}: {}\n", skill.name, skill.description));
                }

                Ok(ToolOutput::new(output).with_title(format!("Skills: Reloaded {}", global_count)))
            }
            Err(e) => {
                crate::logging::warn(&format!("[tool:skill_manage] reload_all failed error={}", e));
                Ok(ToolOutput::new(format!("Failed to reload skills: {}", e)).with_title("Skills: Reload failed"))
            }
        }
    }

    /// `create`: writes a new managed skill, refusing any existing name.
    ///
    /// An authored skill — global or project-local — always wins a collision:
    /// managed skills exist so the model can add a new procedure, never so it
    /// can silently rewrite instructions a person wrote.
    async fn create_managed_skill(
        &self,
        working_dir: Option<&std::path::Path>,
        name: Option<String>,
        description: Option<String>,
        body: Option<String>,
    ) -> Result<ToolOutput> {
        if !managed_skills_enabled() {
            return Ok(ToolOutput::new(MANAGED_SKILLS_DISABLED));
        }
        let name = crate::managed_skills::validate_name(name.as_deref().unwrap_or_default())?;
        if self.effective_registry(working_dir).await.get(&name).is_some() {
            anyhow::bail!("A skill named '{name}' already exists; managed skills cannot override authored ones.");
        }
        self.write_managed_skill("create", name, description, body).await
    }

    /// `update`: replaces an existing managed skill's description and body.
    async fn update_managed_skill(
        &self,
        name: Option<String>,
        description: Option<String>,
        body: Option<String>,
    ) -> Result<ToolOutput> {
        if !managed_skills_enabled() {
            return Ok(ToolOutput::new(MANAGED_SKILLS_DISABLED));
        }
        let name = crate::managed_skills::validate_name(name.as_deref().unwrap_or_default())?;
        self.write_managed_skill("update", name, description, body).await
    }

    /// `delete`: removes a skill the agent wrote itself.
    ///
    /// Only the managed directory is touched. A name this session resolves to
    /// an authored skill is refused together with its source, because removing
    /// instructions a person wrote is not what this action is for. The
    /// `[tools] managed_skills` switch is deliberately not consulted: authoring
    /// can be off while a stale managed skill still needs cleaning up, and the
    /// guards inside `managed_skills::remove` are what keep the removal inside
    /// the managed directory.
    async fn delete_managed_skill(
        &self,
        working_dir: Option<&std::path::Path>,
        name: Option<String>,
    ) -> Result<ToolOutput> {
        let name = crate::managed_skills::validate_name(name.as_deref().unwrap_or(""))?;
        if let Some(skill) = self.effective_registry(working_dir).await.get(&name)
            && !crate::managed_skills::contains_path(&skill.path)
        {
            let source = crate::skill::describe_source(&skill.path, working_dir)
                .unwrap_or_else(|| format!("a skill at {}", skill.path.display()));
            anyhow::bail!(
                "Skill '{name}' is {source}, so skill_manage cannot delete it. Only skills in {} can be deleted, and nothing was removed.",
                crate::managed_skills::dir()?.display()
            );
        }
        let path = crate::managed_skills::remove(&name)?;
        let reload_note = self.reload_after_managed_change(&name).await;
        Ok(ToolOutput::new(format!(
            "Deleted managed skill '{name}'\n\nPath: {}{reload_note}",
            path.display()
        ))
        .with_title(format!("Skills: Deleted {name}")))
    }

    /// Reload the shared registry after a managed skill changed on disk.
    ///
    /// The skills list is part of the system prompt and the late-install
    /// reminder reads the shared registry, so the next turn only sees a
    /// created, updated, or deleted skill once this runs. A failed reload is
    /// reported in the tool output instead of failing the change: the file
    /// operation already happened.
    pub(super) async fn reload_after_managed_change(&self, name: &str) -> String {
        let mut registry = self.registry.write().await;
        match registry.reload_global() {
            Ok(_) => String::new(),
            Err(err) => {
                crate::logging::warn(&format!(
                    "[tool:skill_manage] managed skill reload failed name={name} error={err}"
                ));
                format!("\n\nThe skills registry reload failed: {err}. Run skill_manage reload_all.")
            }
        }
    }

    /// Write one managed skill, then reload the shared registry.
    async fn write_managed_skill(
        &self,
        action: &str,
        name: String,
        description: Option<String>,
        body: Option<String>,
    ) -> Result<ToolOutput> {
        let write = write_managed_skill_file(action, &name, description, body)?;
        let reload_note = self.reload_after_managed_change(&write.name).await;
        Ok(ToolOutput::new(format!(
            "{} managed skill '{}'\n\nPath: {}\nDescription: {}{reload_note}",
            write.verb,
            write.name,
            write.path.display(),
            write.description
        ))
        .with_title(format!("Skills: {} {}", write.verb, write.name)))
    }

    async fn read_skill(&self, name: Option<String>, working_dir: Option<&std::path::Path>) -> Result<ToolOutput> {
        let name = normalize_skill_name(name, "read")?;

        let registry = self.effective_registry(working_dir).await;

        if let Some(skill) = registry.get(&name) {
            let mut output = format!("# Skill: {}\n\n", skill.name);
            output.push_str(&format!("**Description:** {}\n", skill.description));
            output.push_str(&format!("**Path:** {}\n", skill.path.display()));
            if let Some(ref tools) = skill.allowed_tools {
                output.push_str(&format!("**Allowed tools:** {}\n", tools.join(", ")));
            }
            output.push_str("\n---\n\n");
            let body = skill
                .body()
                .map_err(|error| anyhow::anyhow!("Failed to read skill '{}': {error}", skill.name))?;
            output.push_str(&body);

            Ok(ToolOutput::new(output).with_title(format!("Skills: {}", name)))
        } else {
            Ok(ToolOutput::new(format!(
                "Skill '{}' not found.\n\nUse 'list' to see available skills.",
                name
            ))
            .with_title("Skills: Not found"))
        }
    }
}

/// Whether `[tools] managed_skills` lets the model author skills.
pub(crate) fn managed_skills_enabled() -> bool {
    crate::config::config().tools.managed_skills
}

/// One managed-skill file write, described for a tool result.
pub(crate) struct ManagedSkillWrite {
    /// `Created` or `Updated`.
    pub(crate) verb: &'static str,
    /// The validated name, which is also the directory name.
    pub(crate) name: String,
    pub(crate) path: std::path::PathBuf,
    /// The stored description, sanitized the same way the file stores it.
    pub(crate) description: String,
}

/// The file half of `skill_manage create|update`.
///
/// `action` is `create` (refuses a name the managed directory already holds)
/// or `update` (refuses a name it does not hold). The two differ only in that
/// existence rule, so the memory tool's `learn` action shares this code path
/// instead of writing `SKILL.md` itself (issue #61). Callers keep what
/// surrounds it: `skill_manage` also refuses a name an authored skill owns and
/// reloads the skills list afterwards, and `learn` deliberately does neither.
pub(crate) fn write_managed_skill_file(
    action: &str,
    name: &str,
    description: Option<String>,
    body: Option<String>,
) -> Result<ManagedSkillWrite> {
    let name = crate::managed_skills::validate_name(name)?;
    let verb = match action {
        "create" => {
            anyhow::ensure!(
                !crate::managed_skills::exists(&name)?,
                "A skill named '{name}' already exists; managed skills cannot override authored ones."
            );
            "Created"
        }
        "update" => {
            anyhow::ensure!(
                crate::managed_skills::exists(&name)?,
                "Managed skill '{name}' does not exist. Use action=create to add it."
            );
            "Updated"
        }
        other => anyhow::bail!("Unknown skill action for a lesson: {other}. Use create or update."),
    };
    let description = description.unwrap_or_default();
    let path = crate::managed_skills::write(&name, &description, body.as_deref().unwrap_or_default())?;
    Ok(ManagedSkillWrite {
        verb,
        name,
        path,
        description: crate::managed_skills::sanitize_description(&description),
    })
}

/// Where skills are read from, highest priority first.
///
/// The workspace paths come from the source table itself
/// (`skill::project_skill_paths`), so this text cannot list a directory the
/// loader no longer reads. The user rows are spelled out with their switches,
/// because a switch that is off is exactly why a skill the operator installed
/// elsewhere does not appear here.
fn skill_source_help() -> String {
    let mut help = String::from("Skills are loaded from, highest priority first:\n");
    for path in crate::skill::project_skill_paths() {
        help.push_str(&format!("- ./{path}/<skill-name>/SKILL.md (workspace)\n"));
    }
    help.push_str("- ~/.horon/skills/<skill-name>/SKILL.md (user)\n");
    help.push_str("- ~/.claude/skills and installed Claude Code plugins ([skills] claude_user, off by default)\n");
    help.push_str("- ~/.agents/skills (user)\n");
    help.push_str("- ~/.codex/skills ([skills] codex_user, off by default)\n");
    help.push_str("- ~/.horon/managed-skills ([tools] managed_skills, off by default)\n\n");
    help.push_str("A workspace skill wins over a user skill of the same name.\n\n");
    help
}

/// Append the curated horon-agent-endorsed skill catalog to `output`, grouped by
/// category and marked with installed/not-installed status. `installed` is the
/// set of skill names currently loaded in the registry.
fn append_endorsed_skills(output: &mut String, installed: &std::collections::HashSet<&str>) {
    let endorsed = crate::skill::endorsed_skills();
    if endorsed.is_empty() {
        return;
    }

    output.push_str("\nEndorsed skills (recommended by horon-agent)\n");

    // Group by category, preserving first-seen order.
    let mut category_order: Vec<&str> = Vec::new();
    for skill in endorsed {
        if !category_order.contains(&skill.category) {
            category_order.push(skill.category);
        }
    }

    for category in category_order {
        let in_category: Vec<_> = endorsed.iter().filter(|e| e.category == category).collect();
        let installed_count = in_category.iter().filter(|e| installed.contains(e.name)).count();
        output.push_str(&format!(
            "\n  {} ({}/{} installed)\n",
            category,
            installed_count,
            in_category.len()
        ));
        for skill in in_category {
            let is_installed = installed.contains(skill.name);
            let status = if is_installed { "installed" } else { "not installed" };
            output.push_str(&format!("  - /{} [{}]\n", skill.name, status));
            output.push_str(&format!("      {}\n", skill.description));
            output.push_str(&format!("      source: {}\n", skill.source));
            if !is_installed && let Some(install) = skill.install {
                output.push_str(&format!("      install: {}\n", install));
            }
        }
    }

    output.push_str(
        "\nActivate a loaded skill by loading it with skill_manage (action=load) or typing its slash command.\n",
    );
    output.push_str("NVIDIA CUDA-X skills come from the official catalog at https://github.com/NVIDIA/skills.\n");
}

fn normalize_skill_name(name: Option<String>, action: &str) -> Result<String> {
    let name = name.ok_or_else(|| anyhow::anyhow!("'name' is required for {} action", action))?;
    let trimmed = name.trim().trim_start_matches('/').to_string();
    if trimmed.is_empty() {
        anyhow::bail!("'name' is required for {} action", action);
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `body` on a fresh runtime while the process-wide test env lock is
    /// held on this thread. The managed-skill tests below mutate
    /// `HORON_HOME`, so they must stay serialized, but the `std` guard
    /// belongs outside the async body rather than held across an await point.
    fn run_with_env_lock(body: impl Future<Output = ()>) {
        let _guard = crate::storage::lock_test_env();
        tokio::runtime::Runtime::new().expect("runtime").block_on(body);
    }

    fn create_test_tool() -> SkillTool {
        let registry = Arc::new(RwLock::new(SkillRegistry::default()));
        SkillTool::new(registry)
    }

    fn create_test_tool_with_skill(name: &str) -> (SkillTool, tempfile::TempDir) {
        let temp_dir = tempfile::tempdir().unwrap();
        let skill_dir = temp_dir.path().join(".horon").join("skills").join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test skill\n---\n\n# Test Skill\n\nUse this test skill."),
        )
        .unwrap();

        let registry = SkillRegistry::load_for_working_dir(Some(temp_dir.path())).unwrap();
        let tool = SkillTool::new(Arc::new(RwLock::new(registry)));
        (tool, temp_dir)
    }

    fn create_test_context() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            message_id: "test-message".to_string(),
            tool_call_id: "test-tool-call".to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        }
    }

    #[test]
    fn test_tool_name() {
        let tool = create_test_tool();
        assert_eq!(tool.name(), "skill_manage");
    }

    #[test]
    fn test_tool_description() {
        let tool = create_test_tool();
        assert!(tool.description().contains("skill"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = create_test_tool();
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["name"].is_object());
    }

    #[tokio::test]
    async fn test_list_empty() {
        let tool = create_test_tool();
        let ctx = create_test_context();
        let input = json!({"action": "list"});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("No skills loaded"));
        // The help text names every switch that gates a source, so a skill the
        // operator installed elsewhere is explained rather than missing.
        for expected in [
            "./.horon/skills/<skill-name>/SKILL.md (workspace)",
            "./.claude/skills/<skill-name>/SKILL.md (workspace)",
            "./.agents/skills/<skill-name>/SKILL.md (workspace)",
            "~/.horon/skills/<skill-name>/SKILL.md (user)",
            "[skills] claude_user, off by default",
            "[skills] codex_user, off by default",
            "[tools] managed_skills, off by default",
        ] {
            assert!(
                result.output.contains(expected),
                "the help text must mention {expected}; got:\n{}",
                result.output
            );
        }
        assert!(result.output.contains("A workspace skill wins over a user skill"));
        // Even with no skills loaded, the endorsed catalog should be listed.
        assert!(result.output.contains("Endorsed skills"));
    }

    #[tokio::test]
    async fn test_list_includes_endorsed_skills() {
        let tool = create_test_tool();
        let ctx = create_test_context();
        let input = json!({"action": "list"});

        let result = tool.execute(input, ctx).await.unwrap();
        // Every endorsed skill should appear with an install-status marker.
        for endorsed in crate::skill::endorsed_skills() {
            assert!(
                result.output.contains(&format!("/{}", endorsed.name)),
                "expected endorsed skill /{} in:\n{}",
                endorsed.name,
                result.output
            );
        }
        // No skills are loaded in this tool, so they should be "not installed".
        assert!(result.output.contains("[not installed]"));
    }

    #[tokio::test]
    async fn test_load_missing_name() {
        let tool = create_test_tool();
        let ctx = create_test_context();
        let input = json!({"action": "load"});

        let result = tool.execute(input, ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("name"));
    }

    #[tokio::test]
    async fn test_load_accepts_skill_alias_and_args() {
        let (tool, _temp_dir) = create_test_tool_with_skill("optimization");
        let ctx = create_test_context();
        let input = json!({"skill": "optimization", "args": "optimize this"});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("## Skill: optimization"));
        assert_eq!(result.title.as_deref(), Some("skill: optimization"));
    }

    /// A skill whose body is not valid UTF-8 still lists, and the read that
    /// needs the bytes reports the encoding failure as a tool error instead of
    /// panicking or returning truncated text.
    #[tokio::test]
    async fn test_load_reports_an_unreadable_body_as_a_tool_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let skill_dir = temp_dir.path().join(".horon").join("skills").join("bad-utf8");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let mut bytes = b"---\nname: bad-utf8\ndescription: Bad UTF-8 body\n---\n\n".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, 0xfd]);
        std::fs::write(skill_dir.join("SKILL.md"), bytes).unwrap();

        let registry = SkillRegistry::load_for_working_dir(Some(temp_dir.path())).unwrap();
        let tool = SkillTool::new(Arc::new(RwLock::new(registry)));

        let error = tool
            .execute(json!({"action": "load", "name": "bad-utf8"}), create_test_context())
            .await
            .expect_err("an unreadable body must surface as a tool error");
        let message = error.to_string();
        assert!(
            message.contains("Failed to read skill 'bad-utf8'"),
            "expected the tool error to name the skill, got: {message}"
        );
        assert!(
            message.contains("UTF-8") || message.contains("utf-8"),
            "expected the tool error to carry the encoding failure, got: {message}"
        );
    }

    #[tokio::test]
    async fn test_load_strips_leading_slash_from_name() {
        let (tool, _temp_dir) = create_test_tool_with_skill("optimization");
        let ctx = create_test_context();
        let input = json!({"action": "load", "name": "/optimization"});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("## Skill: optimization"));
    }

    #[tokio::test]
    async fn test_reload_missing_name() {
        let tool = create_test_tool();
        let ctx = create_test_context();
        let input = json!({"action": "reload"});

        let result = tool.execute(input, ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("name"));
    }

    #[tokio::test]
    async fn test_read_missing_name() {
        let tool = create_test_tool();
        let ctx = create_test_context();
        let input = json!({"action": "read"});

        let result = tool.execute(input, ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("name"));
    }

    #[tokio::test]
    async fn test_reload_nonexistent() {
        let tool = create_test_tool();
        let ctx = create_test_context();
        let input = json!({"action": "reload", "name": "nonexistent"});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn test_unknown_action() {
        let tool = create_test_tool();
        let ctx = create_test_context();
        let input = json!({"action": "invalid"});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("Unknown action"));
    }

    #[tokio::test]
    async fn test_reload_all() {
        let tool = create_test_tool();
        let ctx = create_test_context();
        let input = json!({"action": "reload_all"});

        let result = tool.execute(input, ctx).await.unwrap();
        // The output format is "Reloaded N skills" where N is any number
        // (depends on what skills exist on the system)
        assert!(
            result.output.contains("Reloaded"),
            "Expected 'Reloaded' in output, got: {}",
            result.output
        );
        assert!(
            result.output.contains("skills"),
            "Expected 'skills' in output, got: {}",
            result.output
        );
    }

    fn context_with_working_dir(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: Some(dir.to_path_buf()),
            ..create_test_context()
        }
    }

    fn write_project_skill(root: &std::path::Path, name: &str) {
        let skill_dir = root.join(".agents").join("skills").join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Project skill {name}\n---\n\nBody."),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn list_shows_where_a_colliding_skill_moved() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mine = temp_dir.path().join(".horon").join("skills").join("shared");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(
            mine.join("SKILL.md"),
            "---\nname: shared\ndescription: My skill shared\n---\n\nMy body.",
        )
        .unwrap();
        let claude = temp_dir.path().join(".claude").join("skills").join("shared");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            claude.join("SKILL.md"),
            "---\nname: shared\ndescription: Claude skill shared\n---\n\nClaude body.",
        )
        .unwrap();

        let tool = create_test_tool();
        let result = tool
            .execute(json!({"action": "list"}), context_with_working_dir(temp_dir.path()))
            .await
            .unwrap();

        assert!(
            result.output.contains("## /shared") && result.output.contains("## /claude/shared"),
            "both skills must be listed under their addresses, got:\n{}",
            result.output
        );
        assert!(
            result.output.contains("Skills: name collision: \"shared\"")
                && result.output.contains("available as \"claude/shared\""),
            "the list must say where the colliding skill moved, got:\n{}",
            result.output
        );
    }

    #[tokio::test]
    async fn skill_snapshot_budget_omitted_skill_still_loads_through_skill_manage() {
        let infos: Vec<crate::prompt::SkillInfo> = (0..1_000)
            .map(|index| crate::prompt::SkillInfo {
                name: format!("budget-skill-{index:04}-padded-skill-name-for-budget-tests"),
                description: "Do a thing with the project files and report back.".to_string(),
            })
            .collect();
        let section = crate::prompt::build_available_skills_section(&infos).expect("skills section");
        let omitted_name = "budget-skill-0999-padded-skill-name-for-budget-tests";
        assert!(
            !section.contains(&format!("`/{omitted_name} `")),
            "the budget must drop the last skill alphabetically"
        );

        let temp_dir = tempfile::tempdir().unwrap();
        let skill_dir = temp_dir.path().join(".horon").join("skills").join(omitted_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {omitted_name}\ndescription: Omitted skill\n---\n\nOmitted body."),
        )
        .unwrap();

        let tool = SkillTool::new(Arc::new(RwLock::new(SkillRegistry::default())));
        let result = tool
            .execute(
                json!({"action": "load", "name": omitted_name}),
                context_with_working_dir(temp_dir.path()),
            )
            .await
            .unwrap();

        assert!(
            result.output.contains("Omitted body."),
            "an omitted skill must still load through skill_manage, got: {}",
            result.output
        );
    }

    /// A private `HORON_HOME` with one `[tools]` table, restored on drop.
    struct ManagedHome(Option<std::ffi::OsString>, tempfile::TempDir);

    impl Drop for ManagedHome {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => crate::env::set_var("HORON_HOME", value),
                None => crate::env::remove_var("HORON_HOME"),
            }
            crate::config::Config::invalidate_cache();
        }
    }

    fn managed_home(tools_table: &str) -> ManagedHome {
        let temp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        std::fs::write(temp.path().join("config.toml"), format!("[tools]\n{tools_table}")).unwrap();
        crate::config::Config::invalidate_cache();
        ManagedHome(previous, temp)
    }

    #[test]
    fn managed_skill_create_writes_a_sanitized_file_and_reloads() {
        run_with_env_lock(managed_skill_create_writes_a_sanitized_file_and_reloads_inner());
    }

    async fn managed_skill_create_writes_a_sanitized_file_and_reloads_inner() {
        let home = managed_home("managed_skills = true\n");
        let tool = create_test_tool();

        let result = tool
            .execute(
                json!({
                    "action": "create",
                    "name": "Review-PR",
                    "description": "Review pull requests <fast> `now`",
                    "body": "# Steps\n\n1. Read the diff."
                }),
                create_test_context(),
            )
            .await
            .unwrap();

        assert!(
            result.output.contains("Created managed skill 'review-pr'"),
            "{}",
            result.output
        );
        let content = std::fs::read_to_string(home.1.path().join("managed-skills/review-pr/SKILL.md")).unwrap();
        assert!(
            content.contains("description: Review pull requests fast now"),
            "the description must lose <, >, and backticks: {content}"
        );
        assert!(content.contains("1. Read the diff."), "{content}");

        // The create reloads the shared registry, so the skill is usable at once.
        let registry = tool.registry.read().await;
        let skill = registry.get("review-pr").expect("the created skill is loaded");
        assert_eq!(skill.description, "Review pull requests fast now");
    }

    #[test]
    fn managed_skill_create_refuses_an_authored_name() {
        run_with_env_lock(managed_skill_create_refuses_an_authored_name_inner());
    }

    async fn managed_skill_create_refuses_an_authored_name_inner() {
        let home = managed_home("managed_skills = true\n");
        let authored = home.1.path().join("skills").join("authored");
        std::fs::create_dir_all(&authored).unwrap();
        std::fs::write(
            authored.join("SKILL.md"),
            "---\nname: authored\ndescription: Authored skill\n---\n\nBody.",
        )
        .unwrap();

        let tool = create_test_tool();
        tool.registry
            .write()
            .await
            .reload_global()
            .expect("the authored skill loads");

        let error = tool
            .execute(
                json!({"action": "create", "name": "authored", "description": "Mine", "body": "Body."}),
                create_test_context(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "A skill named 'authored' already exists; managed skills cannot override authored ones."
        );
        assert!(!home.1.path().join("managed-skills/authored/SKILL.md").exists());
    }

    #[test]
    fn managed_skill_update_only_touches_an_existing_managed_skill() {
        run_with_env_lock(managed_skill_update_only_touches_an_existing_managed_skill_inner());
    }

    async fn managed_skill_update_only_touches_an_existing_managed_skill_inner() {
        let _home = managed_home("managed_skills = true\n");
        let tool = create_test_tool();

        let missing = tool
            .execute(
                json!({"action": "update", "name": "not-there", "description": "d", "body": "b"}),
                create_test_context(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            missing,
            "Managed skill 'not-there' does not exist. Use action=create to add it."
        );

        tool.execute(
            json!({"action": "create", "name": "notes", "description": "First", "body": "One."}),
            create_test_context(),
        )
        .await
        .unwrap();
        let updated = tool
            .execute(
                json!({"action": "update", "name": "notes", "description": "Second", "body": "Two."}),
                create_test_context(),
            )
            .await
            .unwrap();
        assert!(
            updated.output.contains("Updated managed skill 'notes'"),
            "{}",
            updated.output
        );
        let read = tool
            .execute(json!({"action": "read", "name": "notes"}), create_test_context())
            .await
            .unwrap();
        assert!(read.output.contains("Second"), "{}", read.output);
        assert!(read.output.contains("Two."), "{}", read.output);
    }

    /// Issue #457: project-local skills must be session-scoped. Two contexts
    /// with different working dirs share one registry but must each see only
    /// their own project skills, immediately and without reload_all.
    #[tokio::test]
    async fn test_project_skills_are_scoped_to_tool_context_working_dir() {
        let tool = create_test_tool();
        let repo_a = tempfile::tempdir().unwrap();
        let repo_b = tempfile::tempdir().unwrap();
        write_project_skill(repo_a.path(), "repo-a-skill");
        write_project_skill(repo_b.path(), "repo-b-skill");

        // Immediately visible in each session without any reload.
        let list_a = tool
            .execute(json!({"action": "list"}), context_with_working_dir(repo_a.path()))
            .await
            .unwrap();
        assert!(list_a.output.contains("repo-a-skill"));
        assert!(
            !list_a.output.contains("repo-b-skill"),
            "session A must not see session B's project skills"
        );

        let list_b = tool
            .execute(json!({"action": "list"}), context_with_working_dir(repo_b.path()))
            .await
            .unwrap();
        assert!(list_b.output.contains("repo-b-skill"));
        assert!(!list_b.output.contains("repo-a-skill"));

        // reload_all in session A must not leak A's project skills into the
        // shared registry that session B reads.
        tool.execute(json!({"action": "reload_all"}), context_with_working_dir(repo_a.path()))
            .await
            .unwrap();
        let shared = tool.registry.read().await;
        assert!(
            shared.get("repo-a-skill").is_none(),
            "shared registry must stay free of project-local skills"
        );
        drop(shared);

        // Skill file edits are visible without any reload/restart.
        let skill_md = repo_a.path().join(".agents/skills/repo-a-skill/SKILL.md");
        std::fs::write(
            &skill_md,
            "---\nname: repo-a-skill\ndescription: Updated description\n---\n\nNew body.",
        )
        .unwrap();
        let read = tool
            .execute(
                json!({"action": "read", "name": "repo-a-skill"}),
                context_with_working_dir(repo_a.path()),
            )
            .await
            .unwrap();
        assert!(
            read.output.contains("Updated description"),
            "skill edits must be visible without daemon restart, got: {}",
            read.output
        );
    }
}

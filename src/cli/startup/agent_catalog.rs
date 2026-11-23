use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;

use crate::agent_defs::discover_from_roots;

pub(super) fn run(cwd: Option<&Path>) -> Result<()> {
    let project = match cwd {
        Some(cwd) => cwd.canonicalize()?,
        None => std::env::current_dir()?.canonicalize()?,
    };
    let home = crate::storage::horon_agent_dir()?;
    let allow_project = crate::workspace_trust::allows_shell_hooks(&project);
    report(&project, &home, allow_project, io::stdout().lock())
}

fn report(project: &Path, home: &Path, allow_project: bool, mut out: impl Write) -> Result<()> {
    let project_dir = project.join(".horon/agents");
    let definitions = discover_from_roots(allow_project.then_some(project), home);
    if !allow_project {
        writeln!(out, "Project definitions are hidden until the workspace is trusted.")?;
    }
    if definitions.is_empty() {
        writeln!(out, "No agent definitions found.")?;
    }
    for definition in definitions {
        let scope = if allow_project && definition.path.parent() == Some(project_dir.as_path()) {
            "project"
        } else {
            "user"
        };
        writeln!(out, "{:?} ({scope}): {:?}", definition.name, definition.path)?;
        if let Some(model) = definition.model {
            writeln!(out, "  model: {model:?}")?;
        }
        if let Some(effort) = definition.effort {
            writeln!(out, "  effort: {effort:?}")?;
        }
        if let Some(tools) = definition.tools {
            writeln!(out, "  configured tools: {tools:?}")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_definition_wins_only_when_project_is_allowed() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let project = fixture.path().join("project");
        let home = fixture.path().join("home");
        for (dir, model) in [
            (project.join(".horon/agents"), "project-model"),
            (home.join("agents"), "user-model"),
        ] {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join("reviewer.md"),
                format!("---\nname: reviewer\nmodel: {model}\neffort: high\ntools: [read]\n---\nPRIVATE ROLE PROMPT\n"),
            )?;
        }
        let mut output = Vec::new();
        report(&project, &home, true, &mut output)?;
        let text = String::from_utf8(output)?;
        assert!(text.contains("\"reviewer\" (project):") && text.contains("project-model"));
        assert!(!text.contains("user-model") && !text.contains("PRIVATE ROLE PROMPT"));
        assert!(text.contains("configured tools: [\"read\"]"));

        let mut output = Vec::new();
        report(&project, &home, false, &mut output)?;
        let text = String::from_utf8(output)?;
        assert!(text.contains("\"reviewer\" (user):") && text.contains("user-model"));
        assert!(!text.contains("project-model"));
        Ok(())
    }
}

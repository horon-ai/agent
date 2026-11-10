//! Named agent definitions for `swarm spawn`.
//!
//! A definition is a Markdown file whose frontmatter carries the parts of a
//! child session a caller would otherwise restate in every prompt: the tool
//! allow list, the model, the reasoning effort, the structured-result schema,
//! and a role prompt in the body. The vocabulary matches this repository's
//! spawn parameters (`tools`, `model`, `effort`, `output_schema`) rather than
//! another tool's key names, so a definition maps onto a spawn request without
//! translation.
//!
//! Discovery is deliberately small: `<workspace>/.horon/agents/` and
//! horon-agent's own home (`~/.horon/agents/`), read in that order, with the
//! first file carrying a name winning. A project definition therefore shadows a
//! home definition of the same name, which is what lets a repository pin its own
//! reviewer.
//!
//! A missing or empty directory is not an error: it means spawn behaves exactly
//! as it does without definitions. A file that cannot be parsed is skipped with
//! a debug line, because one malformed definition must not hide the rest.

use anyhow::Result;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Project-level definition directory, relative to the workspace root.
const PROJECT_DIR: &str = ".horon/agents";
/// User-level definition directory, relative to horon-agent's home.
const HOME_DIR: &str = "agents";

/// One discovered agent definition.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDefinition {
    /// Name the caller passes as `agent: "<name>"`.
    pub name: String,
    /// One-line summary shown by the discovery action.
    pub description: Option<String>,
    /// Tool allow list for the child session. `None` leaves the child's tool
    /// surface untouched.
    pub tools: Option<Vec<String>>,
    /// Plain model name (not a role) for the child.
    pub model: Option<String>,
    /// Reasoning effort for the child.
    pub effort: Option<String>,
    /// JSON Schema the child's structured result must satisfy.
    pub output_schema: Option<serde_json::Value>,
    /// The Markdown body, used as the child's role prompt.
    pub prompt: String,
    /// File the definition was read from.
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct DefinitionFrontmatter {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tools: Option<ToolList>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    output_schema: Option<serde_json::Value>,
}

/// A `tools:` entry, written as a YAML sequence or a comma-delimited string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ToolList {
    CommaDelimited(String),
    Sequence(Vec<String>),
}

impl ToolList {
    fn into_tools(self) -> Vec<String> {
        let raw = match self {
            Self::CommaDelimited(tools) => tools.split(',').map(str::to_string).collect(),
            Self::Sequence(tools) => tools,
        };
        raw.into_iter()
            .map(|tool| tool.trim().to_string())
            .filter(|tool| !tool.is_empty())
            .collect()
    }
}

/// A definition lookup failure, phrased for the model that caused it.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentDefinitionError {
    /// No definition carries the requested name.
    Unknown { name: String, known: Vec<String> },
    /// A file for the requested name exists but could not be read or parsed.
    Unreadable { name: String, reason: String },
}

impl std::fmt::Display for AgentDefinitionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { name, known } if known.is_empty() => write!(
                formatter,
                "Unknown agent '{name}': no agent definitions exist. \
                 Add a .md file under .horon/agents/ or ~/.horon/agents/."
            ),
            Self::Unknown { name, known } => {
                write!(formatter, "Unknown agent '{name}'. Known agents: {}.", known.join(", "))
            }
            Self::Unreadable { name, reason } => {
                write!(formatter, "Agent definition '{name}' could not be read: {reason}")
            }
        }
    }
}

impl std::error::Error for AgentDefinitionError {}

/// Every definition visible from `working_dir`, ordered by name.
///
/// A name that appears in both directories keeps the project file. Files whose
/// frontmatter does not parse are skipped here: discovery lists what the caller
/// can actually spawn.
pub fn discover(working_dir: Option<&Path>) -> Vec<AgentDefinition> {
    let mut by_name: BTreeMap<String, AgentDefinition> = BTreeMap::new();
    for directory in search_dirs(working_dir) {
        for definition in read_dir(&directory) {
            by_name.entry(definition.name.clone()).or_insert(definition);
        }
    }
    by_name.into_values().collect()
}

/// Resolve exactly one definition by name, reporting the known names on a miss.
///
/// The lookup is exact: a definition is a role the caller asked for, not a
/// fuzzy suggestion.
pub fn resolve(name: &str, working_dir: Option<&Path>) -> Result<AgentDefinition, AgentDefinitionError> {
    let name = name.trim();
    let mut known = Vec::new();
    for directory in search_dirs(working_dir) {
        for definition in read_dir(&directory) {
            if definition.name == name {
                return Ok(definition);
            }
            known.push(definition.name);
        }
    }
    known.sort();
    known.dedup();
    // A file named after the request may exist while failing to parse; say so
    // instead of pretending the name was never there.
    for directory in search_dirs(working_dir) {
        let candidate = directory.join(format!("{name}.md"));
        if candidate.is_file()
            && let Err(error) = parse_definition(&candidate)
        {
            return Err(AgentDefinitionError::Unreadable {
                name: name.to_string(),
                reason: error.to_string(),
            });
        }
    }
    Err(AgentDefinitionError::Unknown {
        name: name.to_string(),
        known,
    })
}

fn search_dirs(working_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let project_root = working_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    if let Some(root) = project_root {
        dirs.push(root.join(PROJECT_DIR));
    }
    if let Ok(home) = crate::storage::horon_agent_dir() {
        dirs.push(home.join(HOME_DIR));
    }
    dirs
}

fn read_dir(directory: &Path) -> Vec<AgentDefinition> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            crate::logging::debug(&format!(
                "Agent definitions: {} could not be read: {error}",
                directory.display()
            ));
            return Vec::new();
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("md"))
        .collect();
    // A directory listing has no order, so sort for a deterministic shadowing
    // result and a stable discovery listing.
    paths.sort();
    paths.iter().filter_map(|path| parse_definition(path).ok()).collect()
}

fn parse_definition(path: &Path) -> Result<AgentDefinition> {
    let content = std::fs::read_to_string(path)?;
    let (frontmatter, body) = parse_text(&content)?;
    let name = frontmatter.name.trim().to_string();
    if name.is_empty() {
        anyhow::bail!("name must not be blank");
    }
    Ok(AgentDefinition {
        name,
        description: clean(frontmatter.description),
        tools: frontmatter
            .tools
            .map(ToolList::into_tools)
            .filter(|tools| !tools.is_empty()),
        model: clean(frontmatter.model),
        effort: clean(frontmatter.effort),
        output_schema: frontmatter.output_schema,
        prompt: body,
        path: path.to_path_buf(),
    })
}

fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_text(content: &str) -> Result<(DefinitionFrontmatter, String)> {
    let content = content.trim_start_matches('\u{feff}').trim();
    let rest = content
        .strip_prefix("---")
        .ok_or_else(|| anyhow::anyhow!("missing YAML frontmatter"))?;
    let end = rest
        .find("\n---")
        .ok_or_else(|| anyhow::anyhow!("unclosed YAML frontmatter"))?;
    let frontmatter: DefinitionFrontmatter = serde_yaml::from_str(&rest[..end])?;
    Ok((frontmatter, rest[end + 4..].trim().to_string()))
}

/// Put a definition's role prompt in front of the caller's task message.
///
/// The prompt rides in the first user message rather than the system prompt
/// because a spawned session is created before its first turn; the separator
/// makes the boundary between the role and the task explicit.
pub fn compose_initial_message(prompt: &str, message: Option<String>) -> Option<String> {
    let role = prompt.trim();
    let message = message
        .map(|message| message.trim().to_string())
        .filter(|message| !message.is_empty());
    match (role.is_empty(), message) {
        (true, message) => message,
        (false, None) => Some(role.to_string()),
        (false, Some(message)) => Some(format!("{role}\n\n---\n\n{message}")),
    }
}

#[cfg(test)]
#[path = "agent_defs_tests.rs"]
mod agent_defs_tests;

//! The files the static system prompt is assembled from.
//!
//! Every loader here goes to disk. A one-off prompt build calls them directly;
//! a long-lived agent reads them once per session boundary into
//! [`StaticPromptSources`] and its AGENTS.md snapshot, and hands those to the
//! builder on every request. Each probe of a candidate file is recorded with
//! [`horon_agent_perf_counters::record_prompt_source_read`], so the cost of an
//! unchanged request is a number the structural gate can read.

use std::path::Path;

use super::{ContextInfo, DEFAULT_SYSTEM_PROMPT};

/// Load the base system prompt, allowing the user to fully replace the built-in
/// [`DEFAULT_SYSTEM_PROMPT`]. Precedence: project `./.horon/system-prompt.md`,
/// then global `~/.horon/system-prompt.md`, then the built-in default.
///
/// This is a *replacement* hook. To merely add guidance on top of the default,
/// use `.horon/prompt-overlay.md` instead.
pub fn load_base_system_prompt(working_dir: Option<&Path>) -> String {
    let project_dir = working_dir.unwrap_or(Path::new("."));
    let candidates = [
        Some(project_dir.join(".horon").join("system-prompt.md")),
        crate::storage::horon_agent_dir()
            .ok()
            .map(|dir| dir.join("system-prompt.md")),
    ];
    for path in candidates.into_iter().flatten() {
        horon_agent_perf_counters::record_prompt_source_read();
        if let Ok(content) = std::fs::read_to_string(&path) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    // Trimmed like a replacement file, so the parts join with one blank line
    // and not two.
    DEFAULT_SYSTEM_PROMPT.trim_end().to_string()
}

pub(super) fn base_system_prompt_parts(working_dir: Option<&Path>) -> Vec<String> {
    vec![load_base_system_prompt(working_dir)]
}

/// The static prompt sources that live outside AGENTS.md: the base prompt, the
/// prompt overlay and the preferred-tools guidance.
///
/// Reading them costs six file probes. An agent captures them where it captures
/// AGENTS.md (a new or restored session, a cleared conversation, a new working
/// directory), so a request that changed nothing reads no file and an edit made
/// mid-session cannot move the cached prefix.
#[derive(Debug, Clone)]
pub struct StaticPromptSources {
    pub(super) base: String,
    pub(super) overlay: (Option<String>, usize),
    pub(super) preferred_tools: (Option<String>, usize),
}

impl StaticPromptSources {
    /// Read the three sources for `working_dir` from disk.
    pub fn load(working_dir: Option<&Path>) -> Self {
        Self {
            base: load_base_system_prompt(working_dir),
            overlay: load_prompt_overlay_files_from_dir(working_dir),
            preferred_tools: load_preferred_tools_files_from_dir(working_dir),
        }
    }
}

fn same_canonical_path(first: &Path, second: &Path) -> bool {
    match (std::fs::canonicalize(first), std::fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => false,
    }
}

pub(crate) fn load_agents_md_files_from_dirs(
    project_dir: &Path,
    global_agents_md: Option<&Path>,
) -> (Option<String>, ContextInfo) {
    let mut contents = vec![];
    let mut info = ContextInfo::default();

    // Load a file if it exists: (formatted_content, expanded_size_chars)
    let load_file = |path: &Path, label: &str| -> Option<(String, usize)> {
        horon_agent_perf_counters::record_prompt_source_read();
        if path.exists() {
            std::fs::read_to_string(path).ok().map(|content| {
                let content = crate::context_import::expand_context_imports(&content, path, project_dir);
                let formatted = format!("# {}\n\n{}", label, content.trim());
                (formatted, content.len())
            })
        } else {
            None
        }
    };

    let project_agents_md = project_dir.join("AGENTS.md");
    if let Some((content, size)) = load_file(&project_agents_md, "Project Instructions (AGENTS.md)") {
        info.has_project_agents_md = true;
        info.project_agents_md_chars = size;
        contents.push(content);
    }

    // Canonical file identity handles cwd=$HOME as well as symlinked aliases.
    // If either file is absent or cannot be resolved, loading below remains the
    // source of truth and simply skips unreadable files.
    let global_duplicates_project =
        global_agents_md.is_some_and(|global_agents_md| same_canonical_path(&project_agents_md, global_agents_md));

    if !global_duplicates_project
        && let Some(global_agents_md) = global_agents_md
        && let Some((content, size)) = load_file(global_agents_md, "Global Instructions (~/AGENTS.md)")
    {
        info.has_global_agents_md = true;
        info.global_agents_md_chars = size;
        contents.push(content);
    }

    if contents.is_empty() {
        (None, info)
    } else {
        (Some(contents.join("\n\n")), info)
    }
}

/// Load AGENTS.md files from a specific working directory.
pub fn load_agents_md_files_from_dir(working_dir: Option<&Path>) -> (Option<String>, ContextInfo) {
    if super::discovery::context_files_disabled() {
        return (None, ContextInfo::default());
    }
    let project_dir = working_dir.unwrap_or(Path::new("."));
    let global_agents_md = crate::storage::user_home_path("AGENTS.md").ok();
    load_agents_md_files_from_dirs(project_dir, global_agents_md.as_deref())
}

/// Load optional prompt overlay markdown from ~/.horon/ and ./.horon/
pub(super) fn load_prompt_overlay_files_from_dir(working_dir: Option<&Path>) -> (Option<String>, usize) {
    if super::discovery::rules_disabled() {
        return (None, 0);
    }
    let mut contents = vec![];
    let mut total_chars = 0usize;

    let load_file = |path: &Path, label: &str| -> Option<(String, usize)> {
        horon_agent_perf_counters::record_prompt_source_read();
        if path.exists() {
            std::fs::read_to_string(path).ok().map(|content| {
                let raw_size = content.len();
                let formatted = format!("# {}\n\n{}", label, content.trim());
                (formatted, raw_size)
            })
        } else {
            None
        }
    };

    let project_dir = working_dir.unwrap_or(Path::new("."));
    let project_overlay = project_dir.join(".horon").join("prompt-overlay.md");
    if let Some((content, size)) = load_file(&project_overlay, "Project Prompt Overlay (.horon/prompt-overlay.md)") {
        total_chars += size;
        contents.push(content);
    }

    if let Ok(global_overlay) = crate::storage::horon_agent_dir().map(|dir| dir.join("prompt-overlay.md"))
        && !same_canonical_path(&project_overlay, &global_overlay)
        && let Some((content, size)) = load_file(&global_overlay, "Global Prompt Overlay (~/.horon/prompt-overlay.md)")
    {
        total_chars += size;
        contents.push(content);
    }

    if contents.is_empty() {
        (None, 0)
    } else {
        (Some(contents.join("\n\n")), total_chars)
    }
}

/// Load optional preferred-tool guidance from ~/.horon/ and ./.horon/
pub(super) fn load_preferred_tools_files_from_dir(working_dir: Option<&Path>) -> (Option<String>, usize) {
    if super::discovery::rules_disabled() {
        return (None, 0);
    }
    let mut contents = vec![];
    let mut total_chars = 0usize;

    let load_file = |path: &Path, label: &str| -> Option<(String, usize)> {
        horon_agent_perf_counters::record_prompt_source_read();
        if path.exists() {
            std::fs::read_to_string(path).ok().map(|content| {
                let raw_size = content.len();
                let formatted = format!("# {}\n\n{}", label, content.trim());
                (formatted, raw_size)
            })
        } else {
            None
        }
    };

    let project_dir = working_dir.unwrap_or(Path::new("."));
    let project_preferred_tools = project_dir.join(".horon").join("preferred-tools.md");
    if let Some((content, size)) = load_file(
        &project_preferred_tools,
        "Project Preferred Tools (.horon/preferred-tools.md)",
    ) {
        total_chars += size;
        contents.push(content);
    }

    if let Ok(global_preferred_tools) = crate::storage::horon_agent_dir().map(|dir| dir.join("preferred-tools.md"))
        && !same_canonical_path(&project_preferred_tools, &global_preferred_tools)
        && let Some((content, size)) = load_file(
            &global_preferred_tools,
            "Global Preferred Tools (~/.horon/preferred-tools.md)",
        )
    {
        total_chars += size;
        contents.push(content);
    }

    if contents.is_empty() {
        (None, 0)
    } else {
        (Some(contents.join("\n\n")), total_chars)
    }
}

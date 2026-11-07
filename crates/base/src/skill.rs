use anyhow::Result;
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(not(test))]
use std::sync::OnceLock;
use tokio::sync::RwLock;

mod invocation;
pub use invocation::SkillInvocation;

tokio::task_local! {
    /// Project-local overlay already scanned for the request currently
    /// executing, keyed by the workspace root it was scanned for.
    ///
    /// Scoping it to one task keeps the sharing inside a single request: the
    /// next request starts outside the scope and therefore reads the disk
    /// again, so edits, deletions, and re-pointed symlinks stay visible and no
    /// mtime cache is trusted. The key stops a scope installed for one
    /// workspace from answering for another.
    static REQUEST_OVERLAY: (Option<PathBuf>, Arc<SkillRegistry>);
}

/// A skill definition from SKILL.md
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Option<Vec<String>>,
    /// Markdown body, shared behind an `Arc`. Cloning a registry for a request
    /// snapshot, or merging a project overlay over the globals, therefore
    /// shares the text instead of copying every skill body.
    pub content: Arc<str>,
    pub path: PathBuf,
    /// Normalized search text, built on first use. Normalizing every skill
    /// body eagerly dominated `load_global` (the TUI loads it before its first
    /// frame), yet only memory-entry conversion ever reads it. Shared for the
    /// same reason as `content`: once built, a clone reuses it.
    search_text: Arc<std::sync::OnceLock<String>>,
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(rename = "allowed-tools")]
    allowed_tools: Option<AllowedTools>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AllowedTools {
    CommaDelimited(String),
    Sequence(Vec<String>),
}

/// Registry of available skills
///
/// The map itself is shared behind an `Arc`, so a read snapshot is a refcount
/// bump rather than a copy of every entry. Mutation goes through
/// [`Arc::make_mut`], which copies the map only while a snapshot is alive.
#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    skills: Arc<HashMap<String, Skill>>,
}

/// Maximum directory depth scanned under a Claude Code plugin root when
/// looking for `skills/<name>/SKILL.md` entries. Plugin layouts vary across
/// Claude Code versions (`cache/<marketplace>/<plugin>/<version>/skills/...`,
/// `repos/<owner>/<repo>/skills/...`, nested `.claude/skills/...`), so we scan
/// defensively but with a bound to avoid walking arbitrarily deep trees.
const PLUGIN_SCAN_MAX_DEPTH: usize = 5;

/// Outcome of importing one external skills directory.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SkillCopyStats {
    copied: usize,
    skipped: usize,
    failed: usize,
}

impl SkillRegistry {
    /// Process-wide shared mutable registry used by both `skill_manage` and
    /// direct slash invocation paths. Keeping a single registry prevents slash
    /// commands from seeing a stale startup-only skill snapshot after reloads.
    ///
    /// Holds GLOBAL skills only (plugins, `~/.horon-agent/skills/`,
    /// `~/.agents/skills/`). Project-local skills are a per-session overlay
    /// composed at read time from the session's workspace root (issue #457);
    /// they must never enter this shared registry, and the daemon's startup
    /// cwd must never influence its contents.
    pub fn shared_registry() -> Arc<RwLock<Self>> {
        #[cfg(test)]
        {
            Arc::new(RwLock::new(Self::load_global().unwrap_or_default()))
        }

        #[cfg(not(test))]
        {
            static SHARED: OnceLock<Arc<RwLock<SkillRegistry>>> = OnceLock::new();
            SHARED
                .get_or_init(|| Arc::new(RwLock::new(SkillRegistry::load_global().unwrap_or_default())))
                .clone()
        }
    }

    /// Load a process-wide shared immutable snapshot of global skills for
    /// startup paths that only need read access.
    ///
    /// The snapshot shares the registry's entries and bodies: taking it is an
    /// `Arc` bump, never a copy of every skill body.
    pub fn shared_snapshot() -> Arc<Self> {
        #[cfg(test)]
        {
            Arc::new(Self::load_global().unwrap_or_default())
        }

        #[cfg(not(test))]
        {
            if let Ok(skills) = Self::shared_registry().try_read() {
                Arc::new(skills.clone())
            } else {
                Arc::new(SkillRegistry::load_global().unwrap_or_default())
            }
        }
    }

    /// Import skills from Claude Code and Codex CLI on first run.
    /// Only runs if ~/.horon-agent/skills/ doesn't exist yet.
    fn import_from_external() {
        let horon_agent_skills = match crate::storage::horon_agent_dir() {
            Ok(dir) => dir.join("skills"),
            Err(_) => return,
        };

        if horon_agent_skills.exists() {
            return; // Not first run
        }

        let mut sources = Vec::new();
        let mut copied = Vec::new();

        // Import from Claude Code (~/.claude/skills/)
        if let Ok(claude_skills) = crate::storage::user_home_path(".claude/skills")
            && claude_skills.is_dir()
        {
            let count = Self::copy_skills_dir(&claude_skills, &horon_agent_skills).copied;
            if count > 0 {
                sources.push(format!("{} from Claude Code", count));
                copied.extend(Self::list_skill_names(&horon_agent_skills));
            }
        }

        // Import from Codex CLI (~/.codex/skills/)
        if let Ok(codex_skills) = crate::storage::user_home_path(".codex/skills")
            && codex_skills.is_dir()
        {
            let count = Self::copy_skills_dir(&codex_skills, &horon_agent_skills).copied;
            if count > 0 {
                sources.push(format!("{} from Codex CLI", count));
                copied.extend(Self::list_skill_names(&horon_agent_skills));
            }
        }

        if !sources.is_empty() {
            // Deduplicate names
            copied.sort();
            copied.dedup();
            crate::logging::info(&format!(
                "Skills: Imported {} ({}) from {}",
                copied.len(),
                copied.join(", "),
                sources.join(" + "),
            ));
        }
    }

    /// Copy skill directories from src to dst.
    fn copy_skills_dir(src: &Path, dst: &Path) -> SkillCopyStats {
        let mut stats = SkillCopyStats::default();
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(_) => return stats,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Skip Codex system skills
            if name.starts_with('.') {
                continue;
            }

            // Only copy if SKILL.md exists
            if !path.join("SKILL.md").exists() {
                continue;
            }

            let dest = dst.join(&name);
            // An earlier source (e.g. ~/.claude/skills) may already have imported
            // this skill, often because ~/.codex/skills links to the same
            // directory. Copying over it fails on read-only files such as
            // `.git/objects/**` (issue #1404), and the first copy wins anyway.
            if dest.exists() {
                stats.skipped += 1;
                continue;
            }
            if let Err(e) = Self::copy_dir_recursive(&path, &dest) {
                crate::logging::error(&format!("Failed to copy skill '{}': {}", name, e));
                stats.failed += 1;
                continue;
            }
            stats.copied += 1;
        }
        stats
    }

    /// Recursively copy a directory
    fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if src_path.is_dir() {
                Self::copy_dir_recursive(&src_path, &dst_path)?;
            } else if src_path.is_symlink() {
                // Resolve symlink and copy the target
                let target = std::fs::read_link(&src_path)?;
                // Try to create symlink, fall back to copying the file
                if horon_agent_platform::symlink_or_copy(&target, &dst_path).is_err()
                    && let Ok(resolved) = std::fs::canonicalize(&src_path)
                {
                    std::fs::copy(&resolved, &dst_path)?;
                }
            } else {
                std::fs::copy(&src_path, &dst_path)?;
            }
        }
        Ok(())
    }

    /// List skill directory names
    fn list_skill_names(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .ok()
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().to_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Load skills from all standard locations
    pub fn load() -> Result<Self> {
        Self::load_for_working_dir(None)
    }

    /// Load only the shared global skill sources: Claude Code plugin installs,
    /// `~/.horon-agent/skills/`, and `~/.agents/skills/`.
    ///
    /// This is what the process-wide shared registry holds. Project-local
    /// skills are intentionally excluded: they are a per-session overlay
    /// resolved from the active session's workspace root (issue #457), never
    /// from the daemon's startup cwd, and never shared across sessions.
    pub fn load_global() -> Result<Self> {
        // First-run import from Claude Code / Codex CLI
        Self::import_from_external();

        let mut registry = Self::default();

        // Load skills provided by Claude Code plugins/marketplace installs
        // first, so explicit horon-agent/agents skills with the same name win below.
        if let Some(plugins_root) = Self::claude_plugins_root() {
            registry.load_plugin_skills_from_root(&plugins_root);
        }

        // Load from ~/.horon-agent/skills/ (horon-agent's own global skills)
        if let Ok(horon_agent_dir) = crate::storage::horon_agent_dir() {
            let horon_agent_skills = horon_agent_dir.join("skills");
            if horon_agent_skills.exists() {
                registry.load_from_dir(&horon_agent_skills)?;
            }
        }

        // Load from ~/.agents/skills/ (shared cross-tool `.agents` convention)
        if let Ok(agents_skills) = crate::storage::user_home_path(".agents/skills")
            && agents_skills.exists()
        {
            registry.load_from_dir(&agents_skills)?;
        }

        // Model-managed skills (`skill_manage create`/`update`) come last, so an
        // authored skill always wins a name collision. The switch is off by
        // default: with it off, no managed directory is ever read.
        if crate::config::config().tools.managed_skills
            && let Ok(managed) = crate::managed_skills::dir()
        {
            registry.load_managed_dir(&managed);
        }

        Ok(registry)
    }

    /// Load only the project-local skill overlay for a workspace root:
    /// `./.horon-agent/skills/`, `./.agents/skills/`, and `./.claude/skills/`.
    ///
    /// Loaded fresh from disk on access so edits are visible without daemon
    /// restarts and two sessions in different repositories never see each
    /// other's project skills. `working_dir = None` resolves against the
    /// process cwd (single-process CLI mode).
    ///
    /// This is the only overlay disk scan. Call it once per request and reuse
    /// the result through [`Self::compose_overlay`]: there is deliberately no
    /// persistent mtime cache, so the next request still reads the disk and
    /// sees edits, deletions, and re-pointed symlinks.
    pub fn load_project_overlay(working_dir: Option<&Path>) -> Result<Self> {
        let mut overlay = Self::default();
        overlay.load_project_local_dirs(working_dir)?;
        Ok(overlay)
    }

    /// Merge a project-local overlay into this registry. Overlay skills win
    /// over same-named global skills, mirroring the historical load order
    /// (project dirs loaded last).
    pub fn merge_overlay(&mut self, overlay: Self) {
        let skills = Arc::make_mut(&mut self.skills);
        for (name, skill) in overlay.skills.iter() {
            skills.insert(name.clone(), skill.clone());
        }
    }

    /// Compose an effective registry from an already-scanned overlay without
    /// touching the disk.
    ///
    /// Request paths call this with one [`Self::load_project_overlay`] result so
    /// a single request composes the session's skill set once and every lookup
    /// inside it shares the same entries.
    pub fn compose_overlay(base: &Self, overlay: Self) -> Self {
        let mut effective = base.clone();
        effective.merge_overlay(overlay);
        effective
    }

    /// Insert or replace one skill. Shared bodies make the clone cheap, and
    /// `Arc::make_mut` keeps an already handed-out snapshot untouched.
    pub(crate) fn insert_skill(&mut self, skill: Skill) {
        Arc::make_mut(&mut self.skills).insert(skill.name.clone(), skill);
    }

    fn remove_skill(&mut self, name: &str) -> Option<Skill> {
        Arc::make_mut(&mut self.skills).remove(name)
    }

    /// Effective skills for a session: shared global skills plus the
    /// project-local overlay for the session's workspace root.
    ///
    /// Inside [`Self::with_request_overlay`] the overlay is the one scanned for
    /// that request, so a request that resolves the set several times scans the
    /// disk once. Outside any scope it scans on every call, which is what keeps
    /// the next request seeing the current files.
    pub fn effective_for_working_dir(base: &Self, working_dir: Option<&Path>) -> Self {
        if let Some(overlay) = Self::current_request_overlay(working_dir) {
            return Self::compose_overlay(base, overlay.as_ref().clone());
        }
        match Self::load_project_overlay(working_dir) {
            Ok(overlay) => Self::compose_overlay(base, overlay),
            Err(_) => base.clone(),
        }
    }

    /// Shared effective set for one request: the returned registry is a single
    /// immutable snapshot that every lookup in that request can hold.
    pub fn effective_for_working_dir_shared(base: &Self, working_dir: Option<&Path>) -> Arc<Self> {
        Arc::new(Self::effective_for_working_dir(base, working_dir))
    }

    /// Overlay scanned for this request when it matches `working_dir`.
    fn current_request_overlay(working_dir: Option<&Path>) -> Option<Arc<SkillRegistry>> {
        REQUEST_OVERLAY
            .try_with(|(scoped_dir, overlay)| (scoped_dir.as_deref() == working_dir).then(|| Arc::clone(overlay)))
            .ok()
            .flatten()
    }

    /// Run `body` with the project-local overlay for `working_dir` scanned
    /// once and shared by every skill lookup inside it.
    ///
    /// The scope ends with the future, so nothing is carried into the next
    /// request. Authorization decisions are not part of the overlay and are
    /// never cached here.
    pub async fn with_request_overlay<F, T>(working_dir: Option<&Path>, body: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let overlay = Self::load_project_overlay(working_dir).unwrap_or_default();
        REQUEST_OVERLAY
            .scope((working_dir.map(Path::to_path_buf), Arc::new(overlay)), body)
            .await
    }

    /// Load skills from all standard locations, with project-local locations
    /// resolved against an optional active session working directory.
    pub fn load_for_working_dir(working_dir: Option<&Path>) -> Result<Self> {
        let mut registry = Self::load_global()?;
        registry.load_project_local_dirs(working_dir)?;
        Ok(registry)
    }

    fn project_local_dir(working_dir: Option<&Path>, name: &str) -> PathBuf {
        let path = Path::new(name).join("skills");
        working_dir.map(|dir| dir.join(&path)).unwrap_or(path)
    }

    fn load_project_local_dirs(&mut self, working_dir: Option<&Path>) -> Result<()> {
        // Load from ./.horon-agent/skills/ (project-local horon-agent skills)
        let local_horon_agent = Self::project_local_dir(working_dir, ".horon-agent");
        if local_horon_agent.exists() {
            self.load_from_dir(&local_horon_agent)?;
        }

        // Load from ./.agents/skills/ (shared cross-tool `.agents` convention)
        let local_agents = Self::project_local_dir(working_dir, ".agents");
        if local_agents.exists() {
            self.load_from_dir(&local_agents)?;
        }

        // Fallback: ./.claude/skills/ (project-local Claude skills for compatibility)
        let local_claude = Self::project_local_dir(working_dir, ".claude");
        if local_claude.exists() {
            self.load_from_dir(&local_claude)?;
        }

        Ok(())
    }

    /// Root of the Claude Code plugin store (`~/.claude/plugins`), if present.
    fn claude_plugins_root() -> Option<PathBuf> {
        crate::storage::user_home_path(".claude/plugins")
            .ok()
            .filter(|p| p.is_dir())
    }

    /// Load skills provided by Claude Code plugins under `plugins_root`.
    /// Returns the number of skills loaded. Errors are skipped so a broken
    /// plugin never prevents horon-agent's own skills from loading.
    fn load_plugin_skills_from_root(&mut self, plugins_root: &Path) -> usize {
        let mut count = 0;
        for dir in Self::plugin_skill_dirs_under(plugins_root) {
            count += self.load_from_dir_count(&dir).unwrap_or(0);
        }
        count
    }

    /// Discover `skills/` directories provided by Claude Code plugins under
    /// the given plugins root (normally `~/.claude/plugins`).
    ///
    /// Sources, in order of trust:
    /// - `installed_plugins.json` install paths (current Claude Code layout,
    ///   pointing into `cache/<marketplace>/<plugin>/<version>/`).
    /// - `repos/` checkouts (legacy plugin layout).
    /// - `cache/` as a fallback only when the manifest is missing/unparsable,
    ///   since the cache holds installed plugins.
    ///
    /// `marketplaces/` is intentionally not scanned: it mirrors the full
    /// marketplace catalog, including plugins the user never installed.
    fn plugin_skill_dirs_under(plugins_root: &Path) -> Vec<PathBuf> {
        if !plugins_root.is_dir() {
            return Vec::new();
        }

        let mut roots: Vec<PathBuf> = Self::installed_plugin_paths(&plugins_root.join("installed_plugins.json"));
        if roots.is_empty() {
            let cache = plugins_root.join("cache");
            if cache.is_dir() {
                roots.push(cache);
            }
        }
        let repos = plugins_root.join("repos");
        if repos.is_dir() {
            roots.push(repos);
        }

        let mut dirs = std::collections::BTreeSet::new();
        for root in roots {
            Self::collect_plugin_skills_dirs(&root, PLUGIN_SCAN_MAX_DEPTH, &mut dirs);
        }
        dirs.into_iter().collect()
    }

    /// Parse install paths from a Claude Code `installed_plugins.json`
    /// manifest. Tolerates both a list of installs per plugin (version 2) and
    /// a single install object, and skips paths that no longer exist.
    fn installed_plugin_paths(manifest: &Path) -> Vec<PathBuf> {
        let Ok(raw) = std::fs::read_to_string(manifest) else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Vec::new();
        };
        let Some(plugins) = value.get("plugins").and_then(|p| p.as_object()) else {
            return Vec::new();
        };

        let mut paths = Vec::new();
        for installs in plugins.values() {
            let installs: Vec<&serde_json::Value> = match installs {
                serde_json::Value::Array(list) => list.iter().collect(),
                other => vec![other],
            };
            for install in installs {
                if let Some(path) = install.get("installPath").and_then(|p| p.as_str()) {
                    let path = PathBuf::from(path);
                    if path.is_dir() {
                        paths.push(path);
                    }
                }
            }
        }
        paths
    }

    /// Recursively collect directories named `skills` that contain at least
    /// one `<name>/SKILL.md`, up to `depth` levels below `root`.
    fn collect_plugin_skills_dirs(root: &Path, depth: usize, out: &mut std::collections::BTreeSet<PathBuf>) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        horon_agent_perf_counters::record_skill_dir_read();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || path.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || name == "node_modules" {
                continue;
            }
            if name == "skills" && Self::dir_contains_skill(&path) {
                out.insert(path);
                continue;
            }
            Self::collect_plugin_skills_dirs(&path, depth - 1, out);
        }
    }

    /// True if `dir` has at least one immediate subdirectory with a SKILL.md.
    fn dir_contains_skill(dir: &Path) -> bool {
        horon_agent_perf_counters::record_skill_dir_read();
        std::fs::read_dir(dir)
            .ok()
            .is_some_and(|entries| entries.flatten().any(|e| e.path().join("SKILL.md").is_file()))
    }

    /// Load skills from a directory
    fn load_from_dir(&mut self, dir: &Path) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        horon_agent_perf_counters::record_skill_dir_read();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let skill_file = path.join("SKILL.md");
                if skill_file.exists()
                    && let Ok(skill) = Self::parse_skill(&skill_file)
                {
                    self.insert_skill(skill);
                }
            }
        }

        Ok(())
    }

    /// Load the model-managed skill directory and return how many were added.
    ///
    /// A managed skill never replaces a name that is already loaded: an
    /// authored skill (global or project) always wins. Symlinked directories
    /// and files are refused, and the description is sanitized again on load
    /// because the file may have been edited after `skill_manage` wrote it.
    fn load_managed_dir(&mut self, dir: &Path) -> usize {
        if !dir.is_dir() {
            return 0;
        }

        horon_agent_perf_counters::record_skill_dir_read();
        let mut count = 0;
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let skill_file = path.join("SKILL.md");
            let is_regular_file = std::fs::symlink_metadata(&skill_file)
                .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
                .unwrap_or(false);
            if !is_regular_file {
                continue;
            }
            let Ok(skill) = Self::parse_skill(&skill_file) else {
                continue;
            };
            if self.skills.contains_key(&skill.name) {
                continue;
            }
            let description = crate::managed_skills::sanitize_description(&skill.description);
            if description.is_empty() {
                continue;
            }
            self.insert_skill(Skill { description, ..skill });
            count += 1;
        }
        count
    }

    /// Parse a SKILL.md file
    fn parse_skill(path: &Path) -> Result<Skill> {
        Self::parse_skill_inner(path).map_err(|error| {
            crate::logging::warn(&format!("Failed to parse skill file '{}': {}", path.display(), error));
            error
        })
    }

    fn parse_skill_inner(path: &Path) -> Result<Skill> {
        let content = std::fs::read_to_string(path)?;

        // Parse YAML frontmatter
        let (frontmatter, body) = Self::parse_frontmatter(&content)?;

        let SkillFrontmatter {
            name,
            description,
            allowed_tools,
        } = frontmatter;

        let allowed_tools = allowed_tools.map(|tools| match tools {
            AllowedTools::CommaDelimited(tools) => tools.split(',').map(|tool| tool.trim().to_string()).collect(),
            AllowedTools::Sequence(tools) => tools,
        });
        Ok(Skill {
            name,
            description,
            allowed_tools,
            content: Arc::from(body),
            path: path.to_path_buf(),
            search_text: Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Parse YAML frontmatter from markdown
    fn parse_frontmatter(content: &str) -> Result<(SkillFrontmatter, String)> {
        let content = content.trim();

        if !content.starts_with("---") {
            anyhow::bail!("Missing YAML frontmatter");
        }

        let rest = &content[3..];
        let end = rest
            .find("---")
            .ok_or_else(|| anyhow::anyhow!("Unclosed frontmatter"))?;

        let yaml = &rest[..end];
        let body = rest[end + 3..].trim().to_string();

        let frontmatter: SkillFrontmatter = serde_yaml::from_str(yaml)?;

        Ok((frontmatter, body))
    }

    /// Get a skill by name
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// List all available skills.
    ///
    /// Sorted by skill name so the ordering is deterministic. The backing store
    /// is a `HashMap`, whose iteration order is randomized per instance; without
    /// this sort, two snapshots of the same skill set (e.g. the lock-contended
    /// `self.skills.clone()` fallback in `current_skills_snapshot`) could emit
    /// the "Available Skills" prompt section in different orders. That produces a
    /// system prompt with identical length but different bytes, silently busting
    /// the Anthropic strict-prefix KV cache mid-conversation.
    pub fn list(&self) -> Vec<&Skill> {
        let mut skills: Vec<&Skill> = self.skills.values().collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }

    /// Reload a specific skill by name
    pub fn reload(&mut self, name: &str) -> Result<bool> {
        // Find the skill's path first
        let path = self.skills.get(name).map(|s| s.path.clone());

        if let Some(path) = path {
            if path.exists() {
                let skill = Self::parse_skill(&path)?;
                self.insert_skill(skill);
                Ok(true)
            } else {
                // Skill file was deleted
                self.remove_skill(name);
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    /// Reload all skills from all locations
    pub fn reload_all(&mut self) -> Result<usize> {
        self.reload_global()
    }

    /// Reload the shared global skill sources (plugins, `~/.horon-agent/skills/`,
    /// `~/.agents/skills/`) into this registry.
    ///
    /// Project-local skills are intentionally NOT loaded here: they are a
    /// per-session overlay composed at read time via
    /// [`Self::effective_for_working_dir`], so one session's `reload_all`
    /// can never leak its project skills into the shared registry that other
    /// sessions see (issue #457).
    pub fn reload_global(&mut self) -> Result<usize> {
        // The available-skills list is embedded in the static system prompt,
        // so a reload that changes it legitimately invalidates warm KV cache
        // prefixes. Document it so the miss is attributed instead of alarmed.
        crate::cache_invalidation::record(
            "skill reload",
            "reloaded all skills; the skills list in the system prompt may have changed",
        );
        Arc::make_mut(&mut self.skills).clear();

        let mut count = 0;

        // Load skills provided by Claude Code plugins/marketplace installs
        // first, so explicit horon-agent/agents skills with the same name win below.
        if let Some(plugins_root) = Self::claude_plugins_root() {
            count += self.load_plugin_skills_from_root(&plugins_root);
        }

        // Load from ~/.horon-agent/skills/ (horon-agent's own global skills)
        if let Ok(horon_agent_dir) = crate::storage::horon_agent_dir() {
            let horon_agent_skills = horon_agent_dir.join("skills");
            if horon_agent_skills.exists() {
                count += self.load_from_dir_count(&horon_agent_skills)?;
            }
        }

        // Load from ~/.agents/skills/ (shared cross-tool `.agents` convention)
        if let Ok(agents_skills) = crate::storage::user_home_path(".agents/skills")
            && agents_skills.exists()
        {
            count += self.load_from_dir_count(&agents_skills)?;
        }

        // Managed skills load last and never replace an authored name.
        if crate::config::config().tools.managed_skills
            && let Ok(managed) = crate::managed_skills::dir()
        {
            count += self.load_managed_dir(&managed);
        }

        Ok(count)
    }

    /// Load skills from a directory and return count
    fn load_from_dir_count(&mut self, dir: &Path) -> Result<usize> {
        if !dir.is_dir() {
            return Ok(0);
        }

        let mut count = 0;
        horon_agent_perf_counters::record_skill_dir_read();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let skill_file = path.join("SKILL.md");
                if skill_file.exists()
                    && let Ok(skill) = Self::parse_skill(&skill_file)
                {
                    self.insert_skill(skill);
                    count += 1;
                }
            }
        }

        Ok(count)
    }

    /// Parse `/skill-name` and `/skill-name prompt...` invocations.
    ///
    /// The trailing prompt is kept verbatim apart from surrounding whitespace.
    /// Quotes are intentionally not interpreted as shell syntax, so incomplete
    /// or literal quotes can never put the input path into a continuation state.
    /// Skill command tokens are limited to identifier-like names. In particular,
    /// path separators and filename punctuation are rejected so a terminal file
    /// drop such as `/tmp/screenshot.png` remains ordinary user input.
    pub fn parse_invocation(input: &str) -> Option<SkillInvocation<'_>> {
        let trimmed = input.trim();
        let invocation = trimmed.strip_prefix('/')?;
        let name_end = invocation.find(char::is_whitespace).unwrap_or(invocation.len());
        let name = &invocation[..name_end];
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return None;
        }

        let prompt = invocation[name_end..].trim();
        let prompt = match prompt.as_bytes() {
            [b'"', .., b'"'] | [b'\'', .., b'\''] if prompt.len() >= 2 => &prompt[1..prompt.len() - 1],
            _ => prompt,
        };
        Some(SkillInvocation {
            name,
            prompt: (!prompt.is_empty()).then_some(prompt),
        })
    }

    /// Return true if a skill with the given name is currently loaded.
    pub fn contains(&self, name: &str) -> bool {
        self.skills.contains_key(name)
    }
}

/// A skill recommended/curated by horon-agent that the user may want to install.
#[derive(Debug, Clone, Copy)]
pub struct EndorsedSkill {
    /// Skill name (matches the `name` field in SKILL.md and the slash command).
    pub name: &'static str,
    /// One-line description of what the skill does.
    pub description: &'static str,
    /// Grouping label used to organize the endorsed list (e.g. "horon-agent",
    /// "NVIDIA CUDA-X").
    pub category: &'static str,
    /// Where users can get the skill (repo path, URL, or short note).
    pub source: &'static str,
    /// Optional install command/hint shown when the skill is not installed.
    pub install: Option<&'static str>,
}

/// Curated list of skills endorsed by horon-agent. Used by the `/skills` command to
/// show users which recommended skills they have installed and which they are
/// missing. This is the single source of truth for endorsed skills.
///
/// The NVIDIA CUDA-X entries mirror the official NVIDIA-verified catalog at
/// <https://github.com/NVIDIA/skills>; install them with
/// `npx skills add nvidia/skills --skill <name> --yes`.
pub const ENDORSED_SKILLS: &[EndorsedSkill] = &[
    EndorsedSkill {
        name: "optimization",
        description: "Improve performance, latency, throughput, memory usage, or general efficiency by defining metrics, measuring, attributing bottlenecks, and prioritizing macro-optimizations.",
        category: "horon-agent",
        source: "install into ~/.horon-agent/skills/optimization/SKILL.md",
        install: None,
    },
    EndorsedSkill {
        name: "todo-planning-skill",
        description: "Create thorough, well-structured todo lists for long tasks, including reflection, static analysis, verification, and next-step updates.",
        category: "horon-agent",
        source: "bundled with horon-agent / Claude Code skills",
        install: None,
    },
    EndorsedSkill {
        name: "firefox-browser",
        description: "Control the user's Firefox browser with their logins and cookies intact to browse, fill forms, click, screenshot, and read authenticated pages.",
        category: "horon-agent",
        source: "bundled with horon-agent / Claude Code skills",
        install: None,
    },
    // Anthropic official skills (github.com/anthropics/skills, Apache-2.0).
    EndorsedSkill {
        name: "frontend-design",
        description: "Create distinctive, production-grade frontend interfaces with high design quality (web components, pages, apps). Generates creative, polished code that avoids generic AI aesthetics.",
        category: "Anthropic Design",
        source: "anthropics/skills (official Anthropic catalog)",
        install: Some(
            "npx skills add anthropics/skills --skill frontend-design --yes (or Claude Code: /plugin marketplace add anthropics/skills)",
        ),
    },
    // NVIDIA CUDA-X / GPU accelerated-computing skills from the official
    // NVIDIA-verified catalog (github.com/NVIDIA/skills).
    EndorsedSkill {
        name: "cuopt-developer",
        description: "Modify, build, test, debug, and contribute to NVIDIA cuOpt (C++/CUDA, Python, server, CI) — solver internals, PRs, DCO, and code conventions.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-developer --yes"),
    },
    EndorsedSkill {
        name: "cuopt-install",
        description: "Install NVIDIA cuOpt for Python, C, or server via pip, conda, or Docker, and verify the install.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-install --yes"),
    },
    EndorsedSkill {
        name: "cuopt-numerical-optimization-api-c",
        description: "Solve LP, MILP, and QP (beta) with the cuOpt C API for embedding optimization in C/C++.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-numerical-optimization-api-c --yes"),
    },
    EndorsedSkill {
        name: "cuopt-numerical-optimization-api-cli",
        description: "Solve LP, MILP, and QP (beta) with cuOpt from MPS files via the cuopt_cli command line.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-numerical-optimization-api-cli --yes"),
    },
    EndorsedSkill {
        name: "cuopt-numerical-optimization-api-python",
        description: "Solve LP, MILP, and QP (beta) with the cuOpt Python API — linear/quadratic objectives, integer variables, scheduling, portfolio, and least squares.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-numerical-optimization-api-python --yes"),
    },
    EndorsedSkill {
        name: "cuopt-numerical-optimization-formulation",
        description: "LP, MILP, and QP concepts and formulation patterns (parameters, constraints, decisions, objective). Concepts only; no API.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-numerical-optimization-formulation --yes"),
    },
    EndorsedSkill {
        name: "cuopt-routing-api-python",
        description: "Solve vehicle routing (VRP, TSP, PDP) with the cuOpt Python API.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-routing-api-python --yes"),
    },
    EndorsedSkill {
        name: "cuopt-routing-formulation",
        description: "Vehicle routing (VRP, TSP, PDP) problem types and data requirements. Domain concepts; no API or interface.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-routing-formulation --yes"),
    },
    EndorsedSkill {
        name: "cuopt-server-api-python",
        description: "Run the cuOpt REST server — start it, call endpoints, and use Python/curl client examples.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-server-api-python --yes"),
    },
    EndorsedSkill {
        name: "cuopt-server-common",
        description: "Understand what the cuOpt REST server does and how requests flow. Concepts only; no deploy or client code.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-server-common --yes"),
    },
    EndorsedSkill {
        name: "cuopt-user-rules",
        description: "Base rules for end users calling NVIDIA cuOpt (routing/LP/MILP/QP/install/server).",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cuopt-user-rules --yes"),
    },
    EndorsedSkill {
        name: "cupynumeric-install",
        description: "Install and verify NVIDIA cuPyNumeric (NumPy/SciPy on multi-node multi-GPU) for Python — requirements, commands, and verification.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cupynumeric-install --yes"),
    },
    EndorsedSkill {
        name: "cupynumeric-migration-readiness",
        description: "Assess NumPy code before porting to cuPyNumeric — which patterns scale on GPU, what must be refactored, and a READY/REFACTOR/NOT-RECOMMENDED verdict.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cupynumeric-migration-readiness --yes"),
    },
    EndorsedSkill {
        name: "cupynumeric-hdf5",
        description: "Read and write large cuPyNumeric arrays to HDF5 with Legate's parallel, distributed HDF5 I/O (legate.io.hdf5), including GPUDirect Storage.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cupynumeric-hdf5 --yes"),
    },
    EndorsedSkill {
        name: "cupynumeric-parallel-data-load",
        description: "Load sharded on-disk datasets (.npy, Parquet/Arrow, raw binary, sharded HDF5) into a distributed cuPyNumeric ndarray via manual partition + leaf task launch.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cupynumeric-parallel-data-load --yes"),
    },
    EndorsedSkill {
        name: "accelerated-computing-cudf",
        description: "Official NVIDIA guidance for cuDF GPU DataFrames, pandas acceleration, dask-cuDF, ETL, joins, groupby, CSV/Parquet I/O, and multi-GPU DataFrame workloads.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill accelerated-computing-cudf --yes"),
    },
    EndorsedSkill {
        name: "cudaq-guide",
        description: "NVIDIA CUDA-Q (CUDA Quantum) onboarding guide for installation, test programs, GPU simulation, QPU hardware, and quantum applications.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill cudaq-guide --yes"),
    },
    EndorsedSkill {
        name: "tilegym-adding-cutile-kernel",
        description: "Add a new cuTile GPU kernel operator to NVIDIA TileGym — dispatch registration, cuTile backend implementation, exports, tests, and benchmarks.",
        category: "NVIDIA CUDA-X",
        source: "NVIDIA/skills (official NVIDIA-verified catalog)",
        install: Some("npx skills add nvidia/skills --skill tilegym-adding-cutile-kernel --yes"),
    },
];

/// Return the curated list of skills endorsed by horon-agent.
pub fn endorsed_skills() -> &'static [EndorsedSkill] {
    ENDORSED_SKILLS
}

impl Skill {
    /// Get the full prompt content for this skill
    pub fn get_prompt(&self) -> String {
        format!("# Skill: {}\n\n{}\n\n{}", self.name, self.description, self.content)
    }

    /// Load additional files from the skill directory
    pub fn load_file(&self, filename: &str) -> Result<String> {
        let skill_dir = self.path.parent().ok_or_else(|| anyhow::anyhow!("No parent dir"))?;
        let file_path = skill_dir.join(filename);
        Ok(std::fs::read_to_string(file_path)?)
    }

    pub fn as_memory_entry(&self) -> crate::memory::MemoryEntry {
        let now = Utc::now() - chrono::Duration::days(365);
        let mut entry = crate::memory::MemoryEntry::new(
            crate::memory::MemoryCategory::Custom("Skills".to_string()),
            format!("Use skill `/{} ` when relevant.\n\n{}", self.name, self.get_prompt()),
        )
        .with_id(format!("skill:{}", self.name))
        .with_tags(vec!["skill".to_string(), self.name.clone()])
        .with_source("skill_registry")
        .with_trust(crate::memory::TrustLevel::Medium)
        .with_timestamps(now, now);
        // Use the precomputed skill search text rather than the tag-derived one.
        entry.search_text = self
            .search_text
            .get_or_init(|| build_skill_search_text(&self.name, &self.description, &self.content))
            .clone();
        entry
    }
}

fn build_skill_search_text(name: &str, description: &str, content: &str) -> String {
    normalize_skill_search_text(&format!("{}\n{}\n{}", name, description, content))
}

fn normalize_skill_search_text(text: &str) -> String {
    // Single pass: lowercase, map non-alphanumerics to separators, and
    // collapse whitespace runs. Equivalent to lowercase -> map -> split -> join.
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        } else {
            pending_space = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_skill(name: &str, description: &str, content: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            allowed_tools: None,
            content: Arc::from(content),
            path: PathBuf::from(format!("/tmp/{name}/SKILL.md")),
            search_text: Arc::new(std::sync::OnceLock::from(build_skill_search_text(
                name,
                description,
                content,
            ))),
        }
    }

    fn write_test_skill(root: &Path, scope: &str, name: &str) {
        let dir = root.join(scope).join("skills").join(name);
        std::fs::create_dir_all(&dir).expect("create skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test skill {name}\n---\n\nUse {name}.\n"),
        )
        .expect("write skill");
    }

    fn write_skill_file(skills_dir: &Path, name: &str, content: &str) -> PathBuf {
        let dir = skills_dir.join(name);
        std::fs::create_dir_all(&dir).expect("create skill dir");
        let path = dir.join("SKILL.md");
        std::fs::write(&path, content).expect("write skill");
        path
    }

    #[cfg(unix)]
    #[test]
    fn copy_skills_dir_skips_skill_already_imported_from_linked_source() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp dir");
        let claude = temp.path().join("claude-skills");
        let skill = claude.join("s");
        std::fs::create_dir_all(skill.join("sub")).expect("create skill");
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: s\ndescription: test skill\n---\nbody\n",
        )
        .expect("write skill");
        let readonly = skill.join("sub").join("obj");
        std::fs::write(&readonly, "x").expect("write obj");
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o444)).expect("chmod");
        let codex = temp.path().join("codex-skills");
        std::os::unix::fs::symlink(&claude, &codex).expect("symlink");
        let dest = temp.path().join("horon-agent-skills");

        let first = SkillRegistry::copy_skills_dir(&claude, &dest);
        assert_eq!((first.copied, first.skipped, first.failed), (1, 0, 0));
        let second = SkillRegistry::copy_skills_dir(&codex, &dest);
        assert_eq!((second.copied, second.skipped, second.failed), (0, 1, 0));
        assert_eq!(
            std::fs::read_to_string(dest.join("s").join("sub").join("obj")).expect("read obj"),
            "x"
        );
    }

    #[test]
    fn allowed_tools_accepts_legacy_string_sequence_and_absence() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cases = [
            (
                "allowed-tools: bash, read, write\n",
                Some(vec!["bash", "read", "write"]),
            ),
            (
                "allowed-tools:\n  - bash\n  - read\n  - write\n",
                Some(vec!["bash", "read", "write"]),
            ),
            ("", None),
        ];

        for (index, (allowed_tools, expected)) in cases.into_iter().enumerate() {
            let path = temp.path().join(format!("skill-{index}.md"));
            std::fs::write(
                &path,
                format!("---\nname: test\ndescription: Test skill\n{allowed_tools}---\n\nBody\n"),
            )
            .expect("write skill");
            let skill = SkillRegistry::parse_skill_inner(&path).expect("parse skill");
            let expected = expected.map(|tools| tools.into_iter().map(str::to_string).collect::<Vec<String>>());
            assert_eq!(skill.allowed_tools, expected);
        }
    }

    #[test]
    fn allowed_tools_rejects_non_string_sequence_values() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("SKILL.md");
        std::fs::write(
            &path,
            "---\nname: test\ndescription: Test skill\nallowed-tools: [bash, 1]\n---\n\nBody\n",
        )
        .expect("write skill");

        assert!(SkillRegistry::parse_skill_inner(&path).is_err());
    }

    #[test]
    fn parse_invocation_supports_a_trailing_prompt() {
        assert_eq!(
            SkillRegistry::parse_invocation("/frontend-design build a settings page"),
            Some(SkillInvocation {
                name: "frontend-design",
                prompt: Some("build a settings page"),
            })
        );
        assert_eq!(
            SkillRegistry::parse_invocation("  /frontend-design   \"build a settings page\"  "),
            Some(SkillInvocation {
                name: "frontend-design",
                prompt: Some("build a settings page"),
            })
        );
    }

    #[test]
    fn parse_invocation_handles_bare_and_incomplete_quoted_prompts_without_blocking() {
        assert_eq!(
            SkillRegistry::parse_invocation("/optimization"),
            Some(SkillInvocation {
                name: "optimization",
                prompt: None,
            })
        );
        assert_eq!(
            SkillRegistry::parse_invocation("/optimization \"make this faster"),
            Some(SkillInvocation {
                name: "optimization",
                prompt: Some("\"make this faster"),
            })
        );
        assert_eq!(SkillRegistry::parse_invocation("/"), None);
    }

    #[test]
    fn parse_invocation_rejects_terminal_file_drop_paths() {
        for input in [
            "/tmp/screenshot.png",
            "/Users/example/My\\ File.txt inspect this",
            "/home/example/project/file.rs",
            "/.hidden-file",
            "/network\\share\\file.txt",
        ] {
            assert_eq!(
                SkillRegistry::parse_invocation(input),
                None,
                "filesystem path must not parse as a skill invocation: {input}"
            );
        }
    }

    #[test]
    fn list_is_sorted_by_name_regardless_of_insertion_order() {
        // The "Available Skills" system-prompt section is built from `list()`.
        // The backing store is a HashMap (per-instance randomized iteration
        // order), and `current_skills_snapshot` can hand back a *different*
        // HashMap instance via its lock-contended `self.skills.clone()` fallback.
        // If `list()` did not sort, two snapshots of the same skill set could
        // serialize the section in different orders: a same-length but
        // different-bytes system prompt that silently busts the KV cache.
        let names = ["zebra", "alpha", "mango", "beta", "yak"];

        let mut reg_a = SkillRegistry::default();
        for name in names {
            reg_a.insert_skill(test_skill(name, "d", "c"));
        }

        // Build a second registry with the reverse insertion order to maximize
        // the chance of a differing HashMap layout.
        let mut reg_b = SkillRegistry::default();
        for name in names.iter().rev() {
            reg_b.insert_skill(test_skill(name, "d", "c"));
        }

        let order_a: Vec<&str> = reg_a.list().iter().map(|s| s.name.as_str()).collect();
        let order_b: Vec<&str> = reg_b.list().iter().map(|s| s.name.as_str()).collect();

        assert_eq!(order_a, vec!["alpha", "beta", "mango", "yak", "zebra"]);
        assert_eq!(
            order_a, order_b,
            "list() ordering must be identical across HashMap instances"
        );
    }

    #[test]
    fn skill_as_memory_entry_formats_invocation_and_prompt() {
        let skill = test_skill(
            "firefox-browser",
            "Control Firefox browser sessions and logged-in pages",
            "Use this skill when you need to open websites, click buttons, or interact with browser pages.",
        );

        let entry = skill.as_memory_entry();

        assert_eq!(entry.id, "skill:firefox-browser");
        assert!(matches!(
            entry.category,
            crate::memory::MemoryCategory::Custom(ref name) if name == "Skills"
        ));
        assert!(entry.content.contains("/firefox-browser"));
        assert!(entry.content.contains("# Skill: firefox-browser"));
        assert_eq!(entry.source.as_deref(), Some("skill_registry"));
    }

    #[test]
    fn load_for_working_dir_reads_project_local_horon_agent_skills() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_test_skill(temp.path(), ".horon-agent", "wd-only");

        let registry = SkillRegistry::load_for_working_dir(Some(temp.path())).expect("load skills");

        let skill = registry.get("wd-only").expect("working-dir local skill should load");
        assert_eq!(skill.description, "Test skill wd-only");
        assert!(skill.path.starts_with(temp.path()));
    }

    #[test]
    fn load_for_working_dir_reads_project_local_agents_skills() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_test_skill(temp.path(), ".agents", "agents-only");

        let registry = SkillRegistry::load_for_working_dir(Some(temp.path())).expect("load skills");

        let skill = registry
            .get("agents-only")
            .expect("project-local .agents skill should load");
        assert_eq!(skill.description, "Test skill agents-only");
        assert!(skill.path.starts_with(temp.path()));
    }

    #[test]
    fn malformed_skill_does_not_block_directory_loaders() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills_dir = temp.path().join("skills");
        write_skill_file(
            &skills_dir,
            "valid",
            "---\nname: valid\ndescription: Valid skill\n---\n\nUse valid.\n",
        );
        write_skill_file(
            &skills_dir,
            "malformed",
            "---\nname: malformed\ndescription: Triggers: invalid yaml\n---\n\nBroken.\n",
        );

        let mut registry = SkillRegistry::default();
        registry
            .load_from_dir(&skills_dir)
            .expect("load skills while skipping malformed file");
        assert!(registry.contains("valid"));
        assert!(!registry.contains("malformed"));

        let mut counted_registry = SkillRegistry::default();
        let count = counted_registry
            .load_from_dir_count(&skills_dir)
            .expect("count skills while skipping malformed file");
        assert_eq!(count, 1);
        assert!(counted_registry.contains("valid"));
        assert!(!counted_registry.contains("malformed"));
    }

    #[test]
    fn project_overlay_is_session_scoped_and_composes_over_globals() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_test_skill(temp.path(), ".horon-agent", "session-skill");

        // The overlay resolves against the given workspace root, not the
        // process cwd or a shared registry (issue #457).
        let overlay = SkillRegistry::load_project_overlay(Some(temp.path())).expect("load overlay");
        assert!(overlay.get("session-skill").is_some());

        // A different workspace root sees nothing from this project.
        let other = tempfile::tempdir().expect("tempdir");
        let other_overlay = SkillRegistry::load_project_overlay(Some(other.path())).expect("load overlay");
        assert!(other_overlay.get("session-skill").is_none());

        // Effective set = base globals + overlay, with overlay winning on name.
        let mut base = SkillRegistry::default();
        base.insert_skill(test_skill("session-skill", "global variant", "global body"));
        let effective = SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
        let skill = effective.get("session-skill").expect("overlay skill should be present");
        assert!(
            skill.path.starts_with(temp.path()),
            "project-local overlay must win over a same-named global skill"
        );
    }

    #[test]
    fn reload_global_excludes_project_local_skills() {
        // chdir is process-global; serialize with other env-sensitive tests.
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        write_test_skill(temp.path(), ".horon-agent", "session-skill");

        let prev_cwd = std::env::current_dir().expect("cwd");
        // reload_global must not pick up project-local skills even when the
        // process cwd contains them (daemon startup cwd independence).
        std::env::set_current_dir(temp.path()).expect("chdir");
        let mut registry = SkillRegistry::default();
        let result = registry.reload_global();
        std::env::set_current_dir(prev_cwd).expect("restore cwd");

        result.expect("reload skills");
        assert!(
            registry.get("session-skill").is_none(),
            "shared/global reload must never absorb project-local skills"
        );
    }

    #[test]
    fn endorsed_skills_have_unique_nonempty_metadata() {
        let endorsed = endorsed_skills();
        assert!(!endorsed.is_empty(), "expected at least one endorsed skill");

        let mut seen = std::collections::HashSet::new();
        for skill in endorsed {
            assert!(!skill.name.is_empty(), "endorsed skill name must be set");
            assert!(
                !skill.description.is_empty(),
                "endorsed skill {} needs a description",
                skill.name
            );
            assert!(
                !skill.category.is_empty(),
                "endorsed skill {} needs a category",
                skill.name
            );
            assert!(!skill.source.is_empty(), "endorsed skill {} needs a source", skill.name);
            assert!(
                !skill.name.starts_with('/'),
                "endorsed skill name should not include the leading slash"
            );
            if let Some(install) = skill.install {
                assert!(
                    install.contains(skill.name),
                    "endorsed skill {} install hint should reference its name",
                    skill.name
                );
            }
            assert!(seen.insert(skill.name), "duplicate endorsed skill name: {}", skill.name);
        }
    }

    #[test]
    fn endorsed_skills_include_nvidia_cuda_x_catalog() {
        let endorsed = endorsed_skills();
        // Spot-check representative NVIDIA CUDA-X skills sourced from the
        // official NVIDIA/skills catalog.
        for expected in [
            "cuopt-numerical-optimization-api-python",
            "cupynumeric-install",
            "accelerated-computing-cudf",
            "cudaq-guide",
            "tilegym-adding-cutile-kernel",
        ] {
            let skill = endorsed
                .iter()
                .find(|s| s.name == expected)
                .unwrap_or_else(|| panic!("expected endorsed NVIDIA skill {expected}"));
            assert_eq!(skill.category, "NVIDIA CUDA-X");
            assert!(
                skill.install.is_some_and(|cmd| cmd.contains("nvidia/skills")),
                "NVIDIA skill {expected} should have an nvidia/skills install hint"
            );
        }
    }

    #[test]
    fn endorsed_skills_include_anthropic_frontend_design() {
        let skill = endorsed_skills()
            .iter()
            .find(|s| s.name == "frontend-design")
            .expect("expected endorsed Anthropic frontend-design skill");
        assert_eq!(skill.category, "Anthropic Design");
        assert!(
            skill.source.contains("anthropics/skills"),
            "frontend-design should be sourced from anthropics/skills"
        );
        assert!(
            skill.install.is_some_and(|cmd| cmd.contains("anthropics/skills")),
            "frontend-design should have an anthropics/skills install hint"
        );
    }

    #[test]
    fn registry_contains_reports_loaded_skills() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        write_test_skill(temp.path(), ".horon-agent", "present-skill");

        let registry = SkillRegistry::load_for_working_dir(Some(temp.path())).expect("load skills");
        assert!(registry.contains("present-skill"));
        assert!(!registry.contains("missing-skill"));
    }

    /// Write `SKILL.md` for `name` inside `<plugin_dir>/skills/<name>/`.
    fn write_plugin_skill(plugin_dir: &Path, name: &str) {
        write_plugin_skill_with_description(plugin_dir, name, &format!("Plugin skill {name}"));
    }

    fn write_plugin_skill_with_description(plugin_dir: &Path, name: &str, description: &str) {
        let dir = plugin_dir.join("skills").join(name);
        std::fs::create_dir_all(&dir).expect("create plugin skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nUse {name}.\n"),
        )
        .expect("write plugin skill");
    }

    fn write_installed_plugins_manifest(plugins_root: &Path, install_paths: &[&Path]) {
        let plugins: serde_json::Map<String, serde_json::Value> = install_paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                (
                    format!("plugin-{i}@test-marketplace"),
                    serde_json::json!([{ "scope": "user", "installPath": path, "version": "1.0.0" }]),
                )
            })
            .collect();
        std::fs::write(
            plugins_root.join("installed_plugins.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 2,
                "plugins": plugins,
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");
    }

    #[test]
    fn plugin_skills_load_from_installed_plugins_manifest() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins_root = temp.path();

        // Mirror the real Claude Code layout:
        // cache/<marketplace>/<plugin>/<version>/skills/<skill>/SKILL.md
        let install = plugins_root.join("cache/test-marketplace/vercel/0.40.1");
        write_plugin_skill(&install, "ai-gateway");
        // Nested `.claude/skills` variant inside the same install.
        write_plugin_skill(&install.join(".claude"), "benchmark-agents");
        write_installed_plugins_manifest(plugins_root, &[&install]);

        let mut registry = SkillRegistry::default();
        let count = registry.load_plugin_skills_from_root(plugins_root);

        assert_eq!(count, 2);
        assert!(registry.contains("ai-gateway"));
        assert!(registry.contains("benchmark-agents"));
    }

    #[test]
    fn plugin_skills_fall_back_to_cache_scan_without_manifest() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins_root = temp.path();

        let install = plugins_root.join("cache/test-marketplace/my-plugin/1.0.0");
        write_plugin_skill(&install, "cache-skill");

        let mut registry = SkillRegistry::default();
        let count = registry.load_plugin_skills_from_root(plugins_root);

        assert_eq!(count, 1);
        assert!(registry.contains("cache-skill"));
    }

    #[test]
    fn plugin_skills_load_from_repos_layout() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins_root = temp.path();

        let repo = plugins_root.join("repos/owner/my-plugin");
        write_plugin_skill(&repo, "repo-skill");

        let mut registry = SkillRegistry::default();
        let count = registry.load_plugin_skills_from_root(plugins_root);

        assert_eq!(count, 1);
        assert!(registry.contains("repo-skill"));
    }

    #[test]
    fn plugin_scan_skips_marketplace_catalog_when_manifest_exists() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins_root = temp.path();

        // Installed plugin listed in the manifest.
        let install = plugins_root.join("cache/test-marketplace/installed/1.0.0");
        write_plugin_skill(&install, "installed-skill");
        write_installed_plugins_manifest(plugins_root, &[&install]);

        // Marketplace catalog entry the user never installed.
        write_plugin_skill(
            &plugins_root.join("marketplaces/test-marketplace/plugins/uninstalled"),
            "uninstalled-skill",
        );
        // Cache entry not referenced by the manifest (stale install).
        write_plugin_skill(&plugins_root.join("cache/test-marketplace/stale/0.1.0"), "stale-skill");

        let mut registry = SkillRegistry::default();
        registry.load_plugin_skills_from_root(plugins_root);

        assert!(registry.contains("installed-skill"));
        assert!(
            !registry.contains("uninstalled-skill"),
            "marketplace catalog skills must not load"
        );
        assert!(
            !registry.contains("stale-skill"),
            "cache entries outside the manifest must not load when a manifest exists"
        );
    }

    #[test]
    fn plugin_scan_respects_depth_bound() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins_root = temp.path();

        // Deeper than PLUGIN_SCAN_MAX_DEPTH below the cache root.
        let too_deep = plugins_root.join("cache/a/b/c/d/e/f");
        write_plugin_skill(&too_deep, "too-deep-skill");

        let mut registry = SkillRegistry::default();
        let count = registry.load_plugin_skills_from_root(plugins_root);

        assert_eq!(count, 0);
        assert!(!registry.contains("too-deep-skill"));
    }

    #[test]
    fn explicit_horon_agent_skill_wins_over_plugin_skill_with_same_name() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins_root = temp.path().join("plugins");

        let install = plugins_root.join("cache/test-marketplace/my-plugin/1.0.0");
        write_plugin_skill_with_description(&install, "shared-name", "plugin version");

        // Explicit horon-agent skill with the same name.
        write_test_skill(temp.path(), ".horon-agent", "shared-name");

        // Mirror load ordering: plugins first, then explicit skill dirs, so
        // the later (explicit) insert wins in the registry map.
        let mut registry = SkillRegistry::default();
        registry.load_plugin_skills_from_root(&plugins_root);
        registry
            .load_from_dir(&temp.path().join(".horon-agent/skills"))
            .expect("load explicit skills");

        let skill = registry.get("shared-name").expect("skill present");
        assert_eq!(
            skill.description, "Test skill shared-name",
            "explicit horon-agent skill must override the plugin-provided one"
        );
    }

    #[test]
    fn plugin_skill_dirs_empty_for_missing_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("does-not-exist");
        assert!(SkillRegistry::plugin_skill_dirs_under(&missing).is_empty());
    }

    fn write_request_skill(root: &Path, scope: &str, name: &str, body: &str) {
        let dir = root.join(scope).join("skills").join(name);
        std::fs::create_dir_all(&dir).expect("create skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Request skill {name}\n---\n\n{body}\n"),
        )
        .expect("write skill");
    }

    fn current_thread_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    #[test]
    fn skill_snapshot_budget_registry_clone_shares_skill_bodies() {
        let mut registry = SkillRegistry::default();
        let body = "x".repeat(64 * 1024);
        registry.insert_skill(test_skill("big", "Big skill", &body));

        let snapshot = registry.clone();
        let composed = SkillRegistry::compose_overlay(&registry, SkillRegistry::default());

        let original = registry.get("big").expect("original skill");
        let cloned = snapshot.get("big").expect("cloned skill");
        let overlay = composed.get("big").expect("composed skill");
        assert!(Arc::ptr_eq(&original.content, &cloned.content));
        assert!(Arc::ptr_eq(&original.content, &overlay.content));
    }

    #[test]
    fn skill_snapshot_budget_overlay_is_scanned_once_per_request() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..5 {
            write_request_skill(temp.path(), ".horon-agent", &format!("request-skill-{index}"), "Body.");
        }

        let base = SkillRegistry::default();
        let (inside, second, outside) = current_thread_runtime().block_on(async {
            let before = horon_agent_perf_counters::snapshot().skill_dir_reads;
            let inside = SkillRegistry::with_request_overlay(Some(temp.path()), async {
                for _ in 0..3 {
                    let effective = SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
                    assert!(effective.get("request-skill-0").is_some());
                }
                horon_agent_perf_counters::snapshot().skill_dir_reads - before
            })
            .await;

            let before_second = horon_agent_perf_counters::snapshot().skill_dir_reads;
            let second = SkillRegistry::with_request_overlay(Some(temp.path()), async {
                SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
                SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
                horon_agent_perf_counters::snapshot().skill_dir_reads - before_second
            })
            .await;

            let before_outside = horon_agent_perf_counters::snapshot().skill_dir_reads;
            SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
            let outside = horon_agent_perf_counters::snapshot().skill_dir_reads - before_outside;
            (inside, second, outside)
        });

        assert!(inside >= 1, "the request must scan the overlay: {inside}");
        assert!(
            inside <= 3,
            "one request must not scan once per lookup: {inside} scans for 3 lookups"
        );
        assert!(second >= 1, "the next request must scan again, not reuse the scope");
        assert!(outside >= 1, "a lookup outside any scope must read the disk");
    }

    #[test]
    fn skill_snapshot_budget_overlay_reads_fresh_between_requests() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        write_request_skill(temp.path(), ".horon-agent", "live-skill", "first body");
        let skill_md = temp.path().join(".horon-agent/skills/live-skill/SKILL.md");
        let base = SkillRegistry::default();

        current_thread_runtime().block_on(async {
            let first = SkillRegistry::with_request_overlay(Some(temp.path()), async {
                SkillRegistry::effective_for_working_dir(&base, Some(temp.path()))
            })
            .await;
            assert_eq!(first.get("live-skill").expect("skill").content.as_ref(), "first body");

            let modified = std::fs::metadata(&skill_md)
                .expect("metadata")
                .modified()
                .expect("mtime");
            std::fs::write(
                &skill_md,
                "---\nname: live-skill\ndescription: Request skill live-skill\n---\n\nother body\n",
            )
            .expect("rewrite skill");
            std::fs::File::options()
                .write(true)
                .open(&skill_md)
                .and_then(|file| file.set_modified(modified))
                .expect("restore mtime");

            let second = SkillRegistry::with_request_overlay(Some(temp.path()), async {
                SkillRegistry::effective_for_working_dir(&base, Some(temp.path()))
            })
            .await;
            assert_eq!(
                second.get("live-skill").expect("skill").content.as_ref(),
                "other body",
                "the second request must see a same-size same-mtime body replacement"
            );
        });
    }

    #[test]
    fn skill_snapshot_budget_overlay_scope_is_keyed_by_workspace() {
        let _env_guard = crate::storage::lock_test_env();
        let repo_a = tempfile::tempdir().expect("tempdir");
        let repo_b = tempfile::tempdir().expect("tempdir");
        write_request_skill(repo_a.path(), ".agents", "repo-a-skill", "Body A.");
        write_request_skill(repo_b.path(), ".agents", "repo-b-skill", "Body B.");

        let base = SkillRegistry::default();
        current_thread_runtime().block_on(async {
            let a = SkillRegistry::with_request_overlay(Some(repo_a.path()), async {
                SkillRegistry::effective_for_working_dir(&base, Some(repo_a.path()))
            })
            .await;
            assert!(a.get("repo-a-skill").is_some());
            assert!(a.get("repo-b-skill").is_none());

            let crossed = SkillRegistry::with_request_overlay(Some(repo_a.path()), async {
                SkillRegistry::effective_for_working_dir(&base, Some(repo_b.path()))
            })
            .await;
            assert!(
                crossed.get("repo-b-skill").is_some(),
                "a scope installed for workspace A must not answer a lookup for workspace B"
            );
            assert!(crossed.get("repo-a-skill").is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn skill_snapshot_budget_reload_sees_deleted_and_relinked_skills() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let skills_dir = temp.path().join(".horon-agent/skills");
        let target = temp.path().join("target-skill");
        std::fs::create_dir_all(&target).expect("target dir");
        std::fs::write(
            target.join("SKILL.md"),
            "---\nname: linked-skill\ndescription: Linked skill\n---\n\nBody.\n",
        )
        .expect("write skill");
        std::fs::create_dir_all(&skills_dir).expect("skills dir");
        let link = skills_dir.join("linked-skill");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let first = SkillRegistry::load_for_working_dir(Some(temp.path())).expect("load");
        assert!(first.get("linked-skill").is_some());

        std::fs::remove_file(&link).expect("remove link");
        let other = temp.path().join("other-skill");
        std::fs::create_dir_all(&other).expect("other dir");
        std::fs::write(
            other.join("SKILL.md"),
            "---\nname: relinked-skill\ndescription: Relinked skill\n---\n\nNew body.\n",
        )
        .expect("write skill");
        std::os::unix::fs::symlink(&other, &link).expect("relink");

        let relinked = SkillRegistry::load_for_working_dir(Some(temp.path())).expect("reload");
        assert!(relinked.get("linked-skill").is_none());
        assert!(relinked.get("relinked-skill").is_some());

        std::fs::remove_file(&link).expect("remove link");
        let deleted = SkillRegistry::load_for_working_dir(Some(temp.path())).expect("reload after delete");
        assert!(deleted.get("relinked-skill").is_none());
    }

    #[test]
    fn skill_snapshot_budget_omitted_skill_still_invokes() {
        let omitted_name = "skill-0999-padded-skill-name-for-budget-tests".to_string();
        let infos: Vec<crate::prompt::SkillInfo> = (0..1_000)
            .map(|index| crate::prompt::SkillInfo {
                name: format!("skill-{index:04}-padded-skill-name-for-budget-tests"),
                description: "Do a thing with the project files and report back.".to_string(),
            })
            .collect();
        let section = crate::prompt::build_available_skills_section(&infos).expect("skills section");
        assert!(
            !section.contains(&format!("`/{omitted_name} `")),
            "the alphabetically last skill is expected to be omitted at 1000 skills"
        );
        assert!(crate::prompt::omitted_skill_count(&infos) > 0);

        let mut registry = SkillRegistry::default();
        registry.insert_skill(test_skill(&omitted_name, "Omitted skill", "body"));
        assert!(registry.get(&omitted_name).is_some());
        assert_eq!(
            registry.resolve_invocation(&format!("/{omitted_name} do it")),
            Some(SkillInvocation {
                name: omitted_name.as_str(),
                prompt: Some("do it"),
            })
        );
    }

    #[test]
    fn skill_snapshot_budget_thousand_skills_in_temp_home_keep_prompt_bounded_and_fresh() {
        let _env_guard = crate::storage::lock_test_env();
        let prev_home = std::env::var_os("HORON_AGENT_HOME");
        let temp = tempfile::tempdir().expect("tempdir");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());
        std::fs::create_dir_all(temp.path().join("external")).expect("external dir");
        let skills_dir = temp.path().join("skills");
        for index in 0..1_000 {
            let name = format!("home-skill-{index:04}-padded-skill-name-for-budget-tests");
            let dir = skills_dir.join(&name);
            std::fs::create_dir_all(&dir).expect("skill dir");
            std::fs::write(
                dir.join("SKILL.md"),
                format!(
                    "---\nname: {name}\ndescription: Skill {index} with a description long enough to matter.\n---\n\nbody-{index}\n"
                ),
            )
            .expect("write skill");
        }

        let registry = SkillRegistry::load_global().expect("load global skills");
        let loaded = registry.list().len();
        let infos: Vec<crate::prompt::SkillInfo> = registry
            .list()
            .iter()
            .map(|skill| crate::prompt::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect();
        let (split, info) = crate::prompt::build_system_prompt_split(None, &infos, None, None);
        let section = crate::prompt::build_available_skills_section(&infos).expect("skills section");
        let section_bytes = info.skills_chars;
        let omitted = crate::prompt::omitted_skill_count(&infos);
        let target = "home-skill-0500-padded-skill-name-for-budget-tests";
        let first_body = registry.get(target).expect("skill").content.to_string();

        std::fs::write(
            skills_dir.join(target).join("SKILL.md"),
            format!(
                "---\nname: {target}\ndescription: Skill 500 with a description long enough to matter.\n---\n\nbody-500-edited\n"
            ),
        )
        .expect("edit skill");
        let second_registry = SkillRegistry::load_global().expect("reload global skills");
        let second_body = second_registry.get(target).expect("skill").content.to_string();

        if let Some(prev_home) = prev_home {
            crate::env::set_var("HORON_AGENT_HOME", prev_home);
        } else {
            crate::env::remove_var("HORON_AGENT_HOME");
        }

        assert_eq!(loaded, 1_000);
        assert_eq!(section_bytes, section.len());
        assert!(
            section_bytes <= crate::prompt::SKILLS_SECTION_MAX_BYTES,
            "rendered prompt skill section is {section_bytes} bytes for 1000 skills"
        );
        assert!(split.static_part.contains(&section));
        assert!(omitted > 0, "1000 skills must report an omitted count");
        assert!(section.contains(&format!("{omitted} more skills are installed")));
        assert_eq!(first_body, "body-500");
        assert_eq!(second_body, "body-500-edited");
    }
}

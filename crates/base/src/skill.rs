use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(not(test))]
use std::sync::OnceLock;
use tokio::sync::RwLock;

mod body;
mod collision;
#[cfg(test)]
#[path = "skill/collision_tests.rs"]
mod collision_tests;
mod frontmatter;
#[cfg(test)]
#[path = "skill/frontmatter_tests.rs"]
mod frontmatter_tests;
mod invocation;
mod overlay_cache;
#[cfg(test)]
#[path = "skill/overlay_cache_tests.rs"]
mod overlay_cache_tests;
mod sources;
#[cfg(test)]
#[path = "skill/sources_tests.rs"]
mod sources_tests;
#[cfg(test)]
#[path = "skill/tests/support.rs"]
mod test_support;
pub use invocation::SkillInvocation;
pub use sources::project_skill_paths;

use frontmatter::{AllowedTools, SkillFrontmatter};

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
///
/// Discovery reads only the frontmatter; the markdown body loads on first use
/// through [`Skill::body`]. The cache is shared behind an `Arc<Mutex<..>>`, so a
/// registry clone (a request snapshot or an overlay merge) shares both the
/// loaded text and the file identity that says when to read it again.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Option<Vec<String>>,
    pub path: PathBuf,
    body: body::BodyCache,
}

/// Registry of available skills
///
/// The map itself is shared behind an `Arc`, so a read snapshot is a refcount
/// bump rather than a copy of every entry. Mutation goes through
/// [`Arc::make_mut`], which copies the map only while a snapshot is alive.
#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    skills: Arc<HashMap<String, Skill>>,
    collision_warnings: Arc<[String]>,
}

impl SkillRegistry {
    /// Process-wide shared mutable registry used by both `skill_manage` and
    /// direct slash invocation paths. Keeping a single registry prevents slash
    /// commands from seeing a stale startup-only skill snapshot after reloads.
    ///
    /// Holds GLOBAL skills only (the user rows of [`sources::SOURCES`]:
    /// `~/.horon-agent/skills/`, `~/.agents/skills/`, and another tool's
    /// collection once its switch is on). Project-local skills are a
    /// per-session overlay composed at read time from the session's workspace
    /// root (issue #457); they must never enter this shared registry, and the
    /// daemon's startup cwd must never influence its contents.
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

    /// Load skills from all standard locations
    pub fn load() -> Result<Self> {
        Self::load_for_working_dir(None)
    }

    /// Load only the user-level rows of [`sources::SOURCES`]: horon-agent's own
    /// `~/.horon-agent/skills/`, `~/.agents/skills/`, and the collections other
    /// tools own once their switch is on.
    ///
    /// This is what the process-wide shared registry holds. Project-local
    /// skills are intentionally excluded: they are a per-session overlay
    /// resolved from the active session's workspace root (issue #457), never
    /// from the daemon's startup cwd, and never shared across sessions.
    pub fn load_global() -> Result<Self> {
        let mut registry = Self::default();
        sources::load_global(&mut registry);
        Ok(registry)
    }

    /// Load only the project-level rows of [`sources::SOURCES`] for a
    /// workspace root: `./.horon-agent/skills/`, `./.claude/skills/`, and
    /// `./.agents/skills/`.
    ///
    /// Served from a per-workspace cache that is revalidated with `stat` on
    /// every call (see `overlay_cache`), so an edit, addition, deletion or
    /// re-pointed symlink is visible on the next call without a daemon
    /// restart, while an unchanged workspace reads no skill file at all. Two
    /// sessions in different repositories never see each other's project
    /// skills. `working_dir = None` resolves against the process cwd.
    pub fn load_project_overlay(working_dir: Option<&Path>) -> Result<Self> {
        Ok(overlay_cache::overlay(working_dir).as_ref().clone())
    }

    /// Forget the cached overlay of `working_dir`, so the next lookup rescans
    /// the workspace from disk.
    pub fn forget_project_overlay(working_dir: Option<&Path>) {
        overlay_cache::forget(working_dir);
    }

    /// Merge a project-local overlay into this registry. Overlay skills win
    /// over same-named global skills, mirroring the historical load order
    /// (project dirs loaded last).
    pub fn merge_overlay(&mut self, overlay: Self) {
        let skills = Arc::make_mut(&mut self.skills);
        for (name, skill) in overlay.skills.iter() {
            skills.insert(name.clone(), skill.clone());
        }
        // Both loads' collisions apply to the composed set.
        self.extend_collision_warnings(&overlay.collision_warnings);
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
    /// Inside [`Self::with_request_overlay`] the overlay is the one validated for
    /// that request. Outside any scope the cached overlay is revalidated with
    /// `stat`, which is what keeps the next request seeing the current files.
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
        let overlay = overlay_cache::overlay(working_dir);
        REQUEST_OVERLAY
            .scope((working_dir.map(Path::to_path_buf), overlay), body)
            .await
    }

    /// Load skills from all standard locations, with project-local locations
    /// resolved against an optional active session working directory.
    pub fn load_for_working_dir(working_dir: Option<&Path>) -> Result<Self> {
        let mut registry = Self::load_global()?;
        registry.load_project_local_dirs(working_dir);
        Ok(registry)
    }

    /// Read the project rows of the source table into this registry.
    fn load_project_local_dirs(&mut self, working_dir: Option<&Path>) {
        sources::load_project(self, working_dir);
    }

    /// Read one directory of `<name>/SKILL.md` skills into this registry.
    ///
    /// The production loaders read through [`sources`]; this keeps a direct
    /// handle on one directory for the discovery tests.
    #[cfg(test)]
    fn load_from_dir(&mut self, dir: &Path) -> Result<()> {
        self.load_from_dir_with_stats(dir).map(|_stats| ())
    }

    /// The same read, reporting what the frontmatter reads cost.
    #[cfg(test)]
    fn load_from_dir_with_stats(&mut self, dir: &Path) -> Result<frontmatter::ReadStats> {
        let (skills, stats) = sources::scan_skills_dir(dir);
        for skill in skills {
            self.insert_skill(skill);
        }
        Ok(stats)
    }

    /// Read one directory and return how many skills it held.
    #[cfg(test)]
    fn load_from_dir_count(&mut self, dir: &Path) -> Result<usize> {
        Ok(self.load_from_dir_with_stats(dir)?.files as usize)
    }

    /// Parse a SKILL.md file
    fn parse_skill(path: &Path) -> Result<Skill> {
        Self::parse_skill_with_stats(path).map(|(skill, _stats)| skill)
    }

    fn parse_skill_with_stats(path: &Path) -> Result<(Skill, frontmatter::ReadStats)> {
        Self::parse_skill_inner_with_stats(path).map_err(|error| {
            crate::logging::warn(&format!("Failed to parse skill file '{}': {}", path.display(), error));
            error
        })
    }

    #[cfg(test)]
    fn parse_skill_inner(path: &Path) -> Result<Skill> {
        Self::parse_skill_inner_with_stats(path).map(|(skill, _stats)| skill)
    }

    /// Parse a SKILL.md, returning what the frontmatter read actually cost.
    ///
    /// Discovery needs the metadata only: this reads the frontmatter and leaves
    /// the body on disk until [`Skill::body`] asks for it. The returned
    /// [`frontmatter::ReadStats`] is the caller's own measurement, which stays
    /// correct while other tests run in parallel.
    fn parse_skill_inner_with_stats(path: &Path) -> Result<(Skill, frontmatter::ReadStats)> {
        let (frontmatter, stats) = frontmatter::read_frontmatter(path)?;
        let SkillFrontmatter {
            name,
            description,
            allowed_tools,
        } = frontmatter;

        let skill = Skill {
            name,
            description,
            allowed_tools: allowed_tools.map(AllowedTools::into_tools),
            path: path.to_path_buf(),
            body: body::new_cache(),
        };
        Ok((skill, stats))
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

    /// Reload one entry under the key it was asked for: a namespaced entry's
    /// frontmatter still carries the bare name, so storing by the parsed name
    /// would move it onto another source's key.
    pub fn reload(&mut self, name: &str) -> Result<bool> {
        let path = self.skills.get(name).map(|s| s.path.clone());

        if let Some(path) = path {
            if path.exists() {
                let mut skill = Self::parse_skill(&path)?;
                skill.name = name.to_string();
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
        self.clear_collision_warnings();
        Ok(sources::load_global(self))
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
    /// The markdown body, read from disk the first time it is asked for.
    ///
    /// Repeated calls stat the file once and reuse the cached text while the
    /// file identity is unchanged, so a warm lookup never reads the bytes again.
    /// A changed file (including a same-size edit with the mtime restored) or a
    /// deleted one is re-read or reported instead of silently serving old text.
    pub fn body(&self) -> Result<Arc<str>> {
        body::load(&self.path, &self.body)
    }

    /// Get the full prompt content for this skill
    pub fn get_prompt(&self) -> Result<String> {
        let body = self.body()?;
        Ok(Self::render_prompt(&self.name, &self.description, &body))
    }

    pub fn as_memory_entry(&self) -> Result<crate::memory::MemoryEntry> {
        let prompt = self.get_prompt()?;
        let now = Utc::now() - chrono::Duration::days(365);
        let mut entry = crate::memory::MemoryEntry::new(
            crate::memory::MemoryCategory::Custom("Skills".to_string()),
            format!("Use skill `/{} ` when relevant.\n\n{}", self.name, prompt),
        )
        .with_id(format!("skill:{}", self.name))
        .with_tags(vec!["skill".to_string(), self.name.clone()])
        .with_source("skill_registry")
        .with_trust(crate::memory::TrustLevel::Medium)
        .with_timestamps(now, now);
        // Use the search text built with the body, rather than the tag-derived one.
        entry.search_text = body::with_cached(&self.path, &self.body, |cached, _read| {
            cached
                .search_text(|| build_skill_search_text(&self.name, &self.description, cached.body()))
                .to_string()
        })?;
        Ok(entry)
    }

    fn render_prompt(name: &str, description: &str, body: &str) -> String {
        format!("# Skill: {name}\n\n{description}\n\n{body}")
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
    use super::test_support::test_skill;
    use super::*;

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

        let entry = skill.as_memory_entry().expect("memory entry");

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
        assert!(Arc::ptr_eq(&original.body, &cloned.body));
        assert!(Arc::ptr_eq(&original.body, &overlay.body));
        assert!(Arc::ptr_eq(
            &original.body().expect("original body"),
            &cloned.body().expect("cloned body")
        ));
    }

    #[test]
    fn skill_snapshot_budget_overlay_is_scanned_only_when_it_changed() {
        let _env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..5 {
            write_request_skill(temp.path(), ".horon-agent", &format!("request-skill-{index}"), "Body.");
        }

        // Counted per test thread: the process-wide skill_dir_reads counter is
        // shared with the tests running in parallel, so its delta is not a
        // measurement this test can own.
        let base = SkillRegistry::default();
        let (inside, second, outside) = current_thread_runtime().block_on(async {
            let before = test_support::dir_scans();
            let inside = SkillRegistry::with_request_overlay(Some(temp.path()), async {
                for _ in 0..3 {
                    let effective = SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
                    assert!(effective.get("request-skill-0").is_some());
                }
                test_support::dir_scans() - before
            })
            .await;

            let before_second = test_support::dir_scans();
            let second = SkillRegistry::with_request_overlay(Some(temp.path()), async {
                SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
                SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
                test_support::dir_scans() - before_second
            })
            .await;

            let before_outside = test_support::dir_scans();
            SkillRegistry::effective_for_working_dir(&base, Some(temp.path()));
            let outside = test_support::dir_scans() - before_outside;
            (inside, second, outside)
        });

        assert!(
            inside >= 1,
            "the first request scans the workspace once for 3 lookups, scanned {inside}"
        );
        assert_eq!(second, 0, "an unchanged workspace is revalidated with stat only");
        assert_eq!(outside, 0, "a lookup outside any scope revalidates the same cache");
    }

    #[cfg(unix)]
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
            assert_eq!(
                first.get("live-skill").expect("skill").body().expect("body").as_ref(),
                "first body"
            );

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
                second.get("live-skill").expect("skill").body().expect("body").as_ref(),
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
        let first_body = registry.get(target).expect("skill").body().expect("body").to_string();

        std::fs::write(
            skills_dir.join(target).join("SKILL.md"),
            format!(
                "---\nname: {target}\ndescription: Skill 500 with a description long enough to matter.\n---\n\nbody-500-edited\n"
            ),
        )
        .expect("edit skill");
        let second_registry = SkillRegistry::load_global().expect("reload global skills");
        let second_body = second_registry
            .get(target)
            .expect("skill")
            .body()
            .expect("body")
            .to_string();

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

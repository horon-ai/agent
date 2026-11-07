//! Where skills come from: one table for every source, read in parallel.
//!
//! horon-agent reads skills from several collections whose owners differ: its
//! own global directory, the workspace directories of several conventions, and
//! other tools' directories. The table below is the single description of that
//! set, so the two loaders ([`load_global`], [`load_project`]) cannot drift and
//! a path can be added in exactly one place.
//!
//! Three rules come from the table rather than from call order:
//!
//! - Every project row outranks every user row. The loaders keep the levels
//!   apart (globals first, the workspace overlay merged on top), which is what
//!   makes a project skill win a name collision.
//! - Within a level, a higher `priority` wins a name collision, so the source
//!   that should be believed is stated once and not left to insertion order.
//! - A source owned by another tool is off unless its switch says otherwise.
//!   Reading another agent's instructions is a decision the operator makes.
//!
//! Sources are read concurrently because they are independent directories on
//! the same disk. A source that cannot be read loses only itself: it reports a
//! warning and contributes nothing, so one unreadable directory never hides the
//! skills of every other source.

use super::frontmatter::ReadStats;
use super::{Skill, SkillRegistry};
use std::path::{Path, PathBuf};

/// Maximum directory depth scanned under a Claude Code plugin root when
/// looking for `skills/<name>/SKILL.md` entries. Plugin layouts vary across
/// Claude Code versions (`cache/<marketplace>/<plugin>/<version>/skills/...`,
/// `repos/<owner>/<repo>/skills/...`, nested `.claude/skills/...`), so we scan
/// defensively but with a bound to avoid walking arbitrarily deep trees.
const PLUGIN_SCAN_MAX_DEPTH: usize = 5;

/// Whether a source belongs to the workspace or to the user's home.
///
/// The derived order is the merge order across levels: `Project` rows are read
/// into the session overlay, which is composed over the globals that hold the
/// `User` rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Project,
    User,
}

/// Where one entry of a source row lives.
#[derive(Debug, Clone, Copy)]
enum PathRule {
    /// Relative to the workspace root, or to the process cwd when a session has
    /// no workspace yet.
    Project(&'static str),
    /// Relative to horon-agent's own home (`~/.horon-agent`).
    AgentHome(&'static str),
    /// Relative to the user's home, sandboxed under `HORON_AGENT_HOME/external`
    /// while a sandbox is configured.
    UserHome(&'static str),
    /// horon-agent's managed-skill directory (`skill_manage create`/`update`).
    ManagedSkills,
}

/// How the entries of a row are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    /// A directory of `<name>/SKILL.md` skills.
    SkillsDir,
    /// A Claude Code plugin store, scanned for the `skills/` of installed
    /// plugins only.
    ClaudePlugins,
    /// The model-managed directory: no symlinks are followed, descriptions are
    /// sanitized again on load, and an empty description is refused.
    Managed,
}

/// The switch that has to be on before a row is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enable {
    /// horon-agent's own collections, always read.
    Always,
    /// `[skills] claude_user`.
    ClaudeUser,
    /// `[skills] codex_user`.
    CodexUser,
    /// `[tools] managed_skills`, shared with `skill_manage create`/`update`.
    ManagedSkills,
}

/// One entry of a source row: a path rule plus how to read what it resolves to.
struct SourceEntry {
    rule: PathRule,
    kind: EntryKind,
}

const fn entry(rule: PathRule, kind: EntryKind) -> SourceEntry {
    SourceEntry { rule, kind }
}

/// One place skills are read from.
pub(crate) struct SourceRow {
    /// Short name used in logs and, from a later change, as the namespace of a
    /// skill that lost a name collision.
    pub(crate) id: &'static str,
    pub(crate) level: Level,
    /// Higher wins a name collision against a lower priority in the same level.
    pub(crate) priority: u8,
    entries: &'static [SourceEntry],
    enable: Enable,
}

/// Every source, highest priority first within each level.
///
/// horon-agent's own directories hold the instructions written for this agent,
/// so they win over a directory owned by another tool at the same level. The
/// managed directory comes last: a skill the model wrote itself must never
/// replace instructions a person wrote.
pub(crate) const SOURCES: &[SourceRow] = &[
    SourceRow {
        id: "horon-agent",
        level: Level::Project,
        priority: 100,
        entries: &[entry(PathRule::Project(".horon-agent/skills"), EntryKind::SkillsDir)],
        enable: Enable::Always,
    },
    SourceRow {
        id: "claude",
        level: Level::Project,
        priority: 80,
        entries: &[entry(PathRule::Project(".claude/skills"), EntryKind::SkillsDir)],
        enable: Enable::Always,
    },
    SourceRow {
        id: "agents",
        level: Level::Project,
        priority: 70,
        entries: &[entry(PathRule::Project(".agents/skills"), EntryKind::SkillsDir)],
        enable: Enable::Always,
    },
    SourceRow {
        id: "horon-agent",
        level: Level::User,
        priority: 100,
        entries: &[entry(PathRule::AgentHome("skills"), EntryKind::SkillsDir)],
        enable: Enable::Always,
    },
    SourceRow {
        id: "claude",
        level: Level::User,
        priority: 80,
        entries: &[
            entry(PathRule::UserHome(".claude/skills"), EntryKind::SkillsDir),
            entry(PathRule::UserHome(".claude/plugins"), EntryKind::ClaudePlugins),
        ],
        enable: Enable::ClaudeUser,
    },
    SourceRow {
        id: "agents",
        level: Level::User,
        priority: 70,
        entries: &[entry(PathRule::UserHome(".agents/skills"), EntryKind::SkillsDir)],
        enable: Enable::Always,
    },
    SourceRow {
        id: "codex",
        level: Level::User,
        priority: 70,
        entries: &[entry(PathRule::UserHome(".codex/skills"), EntryKind::SkillsDir)],
        enable: Enable::CodexUser,
    },
    SourceRow {
        id: "managed",
        level: Level::User,
        priority: 5,
        entries: &[entry(PathRule::ManagedSkills, EntryKind::Managed)],
        enable: Enable::ManagedSkills,
    },
];

/// The three switches the table consults, snapshotted once per load so one
/// read of the same table sees one consistent configuration.
struct Switches {
    claude_user: bool,
    codex_user: bool,
    managed_skills: bool,
}

impl Switches {
    fn current() -> Self {
        let config = crate::config::config();
        Self {
            claude_user: config.skills.claude_user,
            codex_user: config.skills.codex_user,
            managed_skills: config.tools.managed_skills,
        }
    }

    fn is_on(&self, enable: Enable) -> bool {
        match enable {
            Enable::Always => true,
            Enable::ClaudeUser => self.claude_user,
            Enable::CodexUser => self.codex_user,
            Enable::ManagedSkills => self.managed_skills,
        }
    }
}

impl Enable {
    /// The setting that turns this row on, as the operator would write it.
    ///
    /// `None` for a row that is always read and for the managed directory:
    /// that one belongs to horon-agent itself, so a stale file there is not
    /// another tool's skills waiting to be switched on.
    fn setting(self) -> Option<&'static str> {
        match self {
            Enable::Always | Enable::ManagedSkills => None,
            Enable::ClaudeUser => Some("[skills] claude_user = true"),
            Enable::CodexUser => Some("[skills] codex_user = true"),
        }
    }
}

/// One row with its entries resolved to concrete paths.
struct ResolvedSource {
    row: &'static SourceRow,
    entries: Vec<(PathBuf, EntryKind)>,
}

impl ResolvedSource {
    /// Resolve one row, keeping only the entries whose path can be named.
    ///
    /// A path that cannot be resolved (no home directory, for example) drops
    /// its entry instead of failing the whole load: the remaining sources still
    /// hold the skills the operator can use.
    fn new(row: &'static SourceRow, working_dir: Option<&Path>) -> Self {
        let entries = row
            .entries
            .iter()
            .filter_map(|entry| resolve_path(entry.rule, working_dir).map(|path| (path, entry.kind)))
            .collect();
        Self { row, entries }
    }
}

fn resolve_path(rule: PathRule, working_dir: Option<&Path>) -> Option<PathBuf> {
    let resolved = match rule {
        PathRule::Project(relative) => {
            let path = Path::new(relative);
            Ok(match working_dir {
                Some(root) => root.join(path),
                None => path.to_path_buf(),
            })
        }
        PathRule::AgentHome(relative) => crate::storage::horon_agent_dir().map(|dir| dir.join(relative)),
        PathRule::UserHome(relative) => crate::storage::user_home_path(relative),
        PathRule::ManagedSkills => crate::managed_skills::dir(),
    };
    match resolved {
        Ok(path) => Some(path),
        Err(error) => {
            // A path that cannot be named (no home directory, for example) drops
            // its own entry only; the remaining sources still hold usable skills.
            crate::logging::debug(&format!("Skills: source path unavailable: {error}"));
            None
        }
    }
}

/// Skills found in one source, with the cost of the discovery read.
#[derive(Default)]
struct Scanned {
    skills: Vec<Skill>,
    stats: ReadStats,
}

/// Load the global (user-level) rows of [`SOURCES`] into `registry`.
///
/// Returns the number of skills added. Within this one load a name is claimed
/// by the highest-priority source that offers it, so the user-level rows never
/// depend on which directory happened to be read first.
pub(crate) fn load_global(registry: &mut SkillRegistry) -> usize {
    log_disabled_sources_once();
    load_level(registry, Level::User, None)
}

/// Load the project rows of [`SOURCES`] for `working_dir` into `registry`.
pub(crate) fn load_project(registry: &mut SkillRegistry, working_dir: Option<&Path>) -> usize {
    load_level(registry, Level::Project, working_dir)
}

/// Read the enabled rows of one level concurrently, then merge them in
/// priority order.
fn load_level(registry: &mut SkillRegistry, level: Level, working_dir: Option<&Path>) -> usize {
    let switches = Switches::current();
    let mut sources: Vec<ResolvedSource> = SOURCES
        .iter()
        .filter(|row| row.level == level)
        .filter(|row| switches.is_on(row.enable))
        .map(|row| ResolvedSource::new(row, working_dir))
        .filter(|source| !source.entries.is_empty())
        .collect();
    // The table is already in this order; sorting states it for the merge and
    // keeps a reordered table from silently changing which skill wins.
    sources.sort_by(|a, b| b.row.priority.cmp(&a.row.priority));

    let scanned = read_in_parallel(&sources);
    // The sources are read on worker threads, so each thread's own count is not
    // the caller's: add what they report here, once, on the calling thread.
    #[cfg(test)]
    for _ in 0..scanned.iter().map(|scanned| scanned.stats.dirs).sum::<u64>() {
        super::test_support::record_dir_scan();
    }

    // One entry per name per load: sources are read in priority order, so the
    // first source that offers a name is the one that should be believed. The
    // set is claimed here rather than by the registry, because a registry that
    // already holds the name is a different level of the same skill: the
    // project overlay is composed over the globals and must win there.
    let mut added = 0;
    let mut claimed = std::collections::HashSet::new();
    for scanned in &scanned {
        for skill in &scanned.skills {
            if !claimed.insert(skill.name.as_str()) {
                continue;
            }
            registry.insert_skill(skill.clone());
            added += 1;
        }
    }
    added
}

/// Read every resolved source at the same time.
///
/// A source that panics is dropped with a warning instead of taking the caller
/// down with it; the remaining sources are still merged. Results come back in
/// the same order as `sources`, so the merge order is the table order.
fn read_in_parallel(sources: &[ResolvedSource]) -> Vec<Scanned> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = sources
            .iter()
            .map(|source| (source, scope.spawn(move || scan_source(source))))
            .collect();
        handles
            .into_iter()
            .filter_map(|(source, handle)| match handle.join() {
                Ok(scanned) => Some(scanned),
                Err(_) => {
                    crate::logging::warn(&format!(
                        "Skills: source '{}' failed while reading; its skills are not loaded",
                        source.row.id
                    ));
                    None
                }
            })
            .collect()
    })
}

fn scan_source(source: &ResolvedSource) -> Scanned {
    let mut scanned = Scanned::default();
    for (path, kind) in &source.entries {
        let (skills, stats) = scan_entry(path, *kind);
        scanned.skills.extend(skills);
        scanned.stats.add(stats);
    }
    // `read_dir` returns entries in filesystem order, which differs between
    // machines and between two runs on the same machine. Sorting here is what
    // makes the same directories always produce the same list.
    scanned.skills.sort_by(|a, b| a.path.cmp(&b.path));
    scanned
}

fn scan_entry(path: &Path, kind: EntryKind) -> (Vec<Skill>, ReadStats) {
    match kind {
        EntryKind::SkillsDir => scan_skills_dir(path),
        EntryKind::ClaudePlugins => scan_plugins_root(path),
        EntryKind::Managed => scan_managed_dir(path),
    }
}

/// Read the frontmatter of every `<name>/SKILL.md` directly under `dir`.
///
/// The body stays on disk until [`Skill::body`] asks for it. A directory that
/// cannot be enumerated contributes nothing and says so once: one unreadable
/// collection must not hide every other collection's skills.
pub(super) fn scan_skills_dir(dir: &Path) -> (Vec<Skill>, ReadStats) {
    let mut stats = ReadStats::default();
    if !dir.is_dir() {
        return (Vec::new(), stats);
    }

    horon_agent_perf_counters::record_skill_dir_read();
    stats.dirs += 1;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            crate::logging::warn(&format!(
                "Skills: cannot read {} ({error}); its skills are not loaded",
                dir.display()
            ));
            return (Vec::new(), stats);
        }
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_file = path.join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }
        // The parse helper already logs the file it could not read.
        if let Ok((skill, read)) = SkillRegistry::parse_skill_with_stats(&skill_file) {
            stats.add(read);
            skills.push(skill);
        }
    }
    (skills, stats)
}

/// Read the model-managed directory.
///
/// A managed skill never replaces an authored one, which the merge in
/// [`load_level`] provides: this row has the lowest priority. Symlinked entries
/// and files are refused, and the description is sanitized again on load
/// because the file may have been edited after `skill_manage` wrote it.
pub(super) fn scan_managed_dir(dir: &Path) -> (Vec<Skill>, ReadStats) {
    let mut stats = ReadStats::default();
    if !dir.is_dir() {
        return (Vec::new(), stats);
    }

    horon_agent_perf_counters::record_skill_dir_read();
    stats.dirs += 1;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), stats);
    };

    let mut skills = Vec::new();
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
        let Ok((skill, read)) = SkillRegistry::parse_skill_with_stats(&skill_file) else {
            continue;
        };
        stats.add(read);
        let description = crate::managed_skills::sanitize_description(&skill.description);
        if description.is_empty() {
            continue;
        }
        skills.push(Skill { description, ..skill });
    }
    (skills, stats)
}

/// Read every skill reachable from a Claude Code plugin store.
pub(super) fn scan_plugins_root(root: &Path) -> (Vec<Skill>, ReadStats) {
    let mut skills = Vec::new();
    let mut stats = ReadStats::default();
    for dir in plugin_skill_dirs_under(root) {
        let (found, read) = scan_skills_dir(&dir);
        skills.extend(found);
        stats.add(read);
    }
    (skills, stats)
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
pub(super) fn plugin_skill_dirs_under(plugins_root: &Path) -> Vec<PathBuf> {
    if !plugins_root.is_dir() {
        return Vec::new();
    }

    let mut roots: Vec<PathBuf> = installed_plugin_paths(&plugins_root.join("installed_plugins.json"));
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
        collect_plugin_skills_dirs(&root, PLUGIN_SCAN_MAX_DEPTH, &mut dirs);
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
        if name == "skills" && dir_contains_skill(&path) {
            out.insert(path);
            continue;
        }
        collect_plugin_skills_dirs(&path, depth - 1, out);
    }
}

/// True if `dir` has at least one immediate subdirectory with a SKILL.md.
///
/// A directory that cannot be enumerated holds no skills as far as this scan
/// can tell, and the read attempt is already counted: the loader retries it as
/// its own source and reports the failure there.
fn dir_contains_skill(dir: &Path) -> bool {
    horon_agent_perf_counters::record_skill_dir_read();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| e.path().join("SKILL.md").is_file())
}

/// One line per disabled external source that has skills waiting, so the
/// operator can opt in without first discovering the path.
///
/// Only rows of another tool are reported, and only while their switch is off:
/// a directory that simply does not exist is not news, and horon-agent's own
/// directories are not a decision anyone has to make.
pub(crate) fn disabled_source_notices() -> Vec<String> {
    let switches = Switches::current();
    let mut notices = Vec::new();
    for row in SOURCES {
        let Some(setting) = row.enable.setting() else {
            continue;
        };
        if switches.is_on(row.enable) {
            continue;
        }
        let resolved = ResolvedSource::new(row, None);
        // The first entry that has any skills is reported. A row lists its
        // cheapest directory first, so a plugin store is only walked when the
        // plain skills directory holds nothing.
        let found = resolved
            .entries
            .iter()
            .find_map(|(path, kind)| match count_skills(path, *kind) {
                0 => None,
                count => Some((count, path)),
            });
        if let Some((count, path)) = found {
            notices.push(format!(
                "Skills: {count} skill(s) in {} not loaded; set {setting}",
                display_path(path)
            ));
        }
    }
    notices
}

/// Write the disabled-source notices to the log once per process.
///
/// This runs on the startup load, so the hints appear beside the rest of the
/// startup lines and a reload does not repeat them.
pub(crate) fn log_disabled_sources_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(log_disabled_sources);
}

/// Write the disabled-source notices to the log.
fn log_disabled_sources() {
    for notice in disabled_source_notices() {
        crate::logging::info(&notice);
    }
}

/// How many skills a source would contribute, without parsing them.
fn count_skills(path: &Path, kind: EntryKind) -> usize {
    match kind {
        EntryKind::SkillsDir => count_skill_dirs(path),
        EntryKind::ClaudePlugins => plugin_skill_dirs_under(path)
            .iter()
            .map(|dir| count_skill_dirs(dir))
            .sum(),
        EntryKind::Managed => count_skill_dirs(path),
    }
}

fn count_skill_dirs(dir: &Path) -> usize {
    // A missing directory is not a read; only an opened one is counted.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    horon_agent_perf_counters::record_skill_dir_read();
    entries
        .flatten()
        .filter(|entry| entry.path().join("SKILL.md").is_file())
        .count()
}

/// `path` with the home directory shortened to `~` for a log line.
fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// Paths of the project-level rows of [`SOURCES`], in table order.
///
/// The skill tool's help text lists these, so the documented workspace
/// locations are the ones the loader actually reads.
pub fn project_skill_paths() -> Vec<&'static str> {
    SOURCES
        .iter()
        .filter(|row| row.level == Level::Project)
        .flat_map(|row| row.entries.iter())
        .filter_map(|entry| match entry.rule {
            PathRule::Project(relative) => Some(relative),
            _ => None,
        })
        .collect()
}

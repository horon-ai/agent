//! Where MCP server definitions come from: one ordered list of sources.
//!
//! Sources are merged highest priority first, and the first definition of a
//! name wins:
//!
//! 1. the workspace (`.horon/mcp.json` > `.mcp.json` > `.claude/mcp.json`),
//!    only when workspace trust allows it;
//! 2. horon-agent's own `~/.horon/mcp.json`;
//! 3. Claude Code's `~/.claude.json` (this project's entry, then the global
//!    servers) and `~/.claude/mcp.json`, when `[mcp] claude_user` is on;
//! 4. Codex CLI's `[mcp_servers.*]` in `~/.codex/config.toml`, when
//!    `[mcp] codex_user` is on. Read live; nothing is copied.
//!
//! One exception keeps issue #653 fixed in both directions: when one source has
//! a local stdio entry and another a remote entry of the same name, the stdio
//! entry is kept whichever source ranks higher, because a remote twin of a
//! working local server usually needs a separate login.
//!
//! User-level files are parsed once per [`FileStamp`] and the result is kept in
//! a small process cache, so a session start that changes nothing reads no
//! file. Project files are not cached: they are parsed from the same single
//! read that the workspace-trust decision covered.

use super::protocol::{McpConfig, McpServerConfig};
use crate::skill::FileStamp;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

type Servers = HashMap<String, McpServerConfig>;

/// Parsed user files kept at once. One generation per path; a handful of
/// files exist, the bound only stops a pathological home from growing it.
const MAX_CACHED_FILES: usize = 16;

/// How a user-level file is parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// `{"mcpServers": ...}` or `{"servers": ...}`.
    Mcp,
    /// `~/.claude.json`: global `mcpServers` plus `projects.<path>.mcpServers`.
    ClaudeJson,
    /// `[mcp_servers.*]` tables in Codex's config.toml.
    CodexToml,
}

/// One parsed user-level file. `~/.claude.json` keeps every project's entry,
/// so the cache is keyed by the file alone and each load picks its own
/// project out of it: a server one repository defines never reaches another.
#[derive(Debug, Default)]
struct Parsed {
    servers: Servers,
    projects: HashMap<String, Servers>,
}

struct Slot {
    stamp: FileStamp,
    parsed: Arc<Parsed>,
    last_used: u64,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Slot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache() -> MutexGuard<'static, HashMap<PathBuf, Slot>> {
    CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn tick() -> u64 {
    static TICK: AtomicU64 = AtomicU64::new(0);
    TICK.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// User files parsed on this test thread. The process-wide
    /// `mcp_config_reads` counter is shared with parallel tests, so a test that
    /// proves "parsed once" counts here instead.
    static PARSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn parses_for_tests() -> u64 {
    PARSES.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(super) fn clear_cache_for_tests() {
    cache().clear();
}

/// Every server the sources define for `project_dir`, merged by priority.
/// Environment expansion and the runnable filter run on the result.
pub(super) fn load_merged(project_dir: Option<&Path>) -> Servers {
    let mut merged = Servers::new();
    if let Some(project_root) = project_dir {
        merge_lower(
            &mut merged,
            McpConfig::load_authorized_project_locals(project_root).servers,
        );
    }
    if let Ok(dir) = crate::storage::horon_agent_dir()
        && let Some(parsed) = cached(&dir.join("mcp.json"), Format::Mcp)
    {
        merge_lower(&mut merged, parsed.servers.clone());
    }

    let switches = crate::config::config().mcp;
    let claude_disabled_by_env = std::env::var_os("HORON_AGENT_DISABLE_CLAUDE_MCP").is_some();
    if switches.claude_user && !claude_disabled_by_env {
        merge_claude(&mut merged, project_dir);
    } else if !claude_disabled_by_env {
        static LOGGED: AtomicBool = AtomicBool::new(false);
        log_waiting_once(
            &LOGGED,
            "claude_user",
            &[(".claude.json", Format::ClaudeJson), (".claude/mcp.json", Format::Mcp)],
        );
    }
    if switches.codex_user {
        if let Some(parsed) = user_file(".codex/config.toml", Format::CodexToml) {
            merge_lower(&mut merged, parsed.servers.clone());
        }
    } else {
        static LOGGED: AtomicBool = AtomicBool::new(false);
        log_waiting_once(&LOGGED, "codex_user", &[(".codex/config.toml", Format::CodexToml)]);
    }
    merged
}

/// Claude Code's sources, in Claude's own order: this project's entry beats
/// the global servers, and `~/.claude.json` beats the legacy `~/.claude/mcp.json`.
fn merge_claude(merged: &mut Servers, project_dir: Option<&Path>) {
    if let Some(parsed) = user_file(".claude.json", Format::ClaudeJson) {
        let project = project_dir.and_then(|dir| project_entry(&parsed, dir));
        let count = parsed.servers.len() + project.map_or(0, HashMap::len);
        if count > 0 {
            crate::logging::info(&live_claude_log_message(count, "~/.claude.json"));
        }
        if let Some(project) = project {
            merge_lower(merged, project.clone());
        }
        merge_lower(merged, parsed.servers.clone());
    }
    if let Some(parsed) = user_file(".claude/mcp.json", Format::Mcp) {
        if !parsed.servers.is_empty() {
            crate::logging::info(&live_claude_log_message(
                parsed.servers.len(),
                "~/.claude/mcp.json (legacy)",
            ));
        }
        merge_lower(merged, parsed.servers.clone());
    }
}

pub(super) fn live_claude_log_message(server_count: usize, source: &str) -> String {
    format!(
        "MCP: Loaded {server_count} server(s) live from Claude Code ({source}); source values were not copied into horon-agent config"
    )
}

/// This project's servers in `~/.claude.json`, which keys them by the
/// absolute project path as Claude saw it: the path as given, then its real
/// path when the two differ (a symlinked checkout).
fn project_entry<'a>(parsed: &'a Parsed, dir: &Path) -> Option<&'a Servers> {
    if let Some(servers) = parsed.projects.get(dir.to_string_lossy().as_ref()) {
        return Some(servers);
    }
    let Ok(real) = dir.canonicalize() else {
        return None;
    };
    parsed.projects.get(real.to_string_lossy().as_ref())
}

/// One line per process naming the servers a switched-off source would add.
fn log_waiting_once(logged: &AtomicBool, switch: &str, files: &[(&str, Format)]) {
    if logged.swap(true, Ordering::Relaxed) {
        return;
    }
    for (relative, format) in files {
        if let Some(parsed) = user_file(relative, *format)
            && !parsed.servers.is_empty()
        {
            crate::logging::info(&format!(
                "MCP: {} server(s) in ~/{relative} are not loaded; set [mcp] {switch} = true to use them",
                parsed.servers.len()
            ));
        }
    }
}

fn user_file(relative: &str, format: Format) -> Option<Arc<Parsed>> {
    let Ok(path) = crate::storage::user_home_path(relative) else {
        return None;
    };
    cached(&path, format)
}

/// Merge a lower-priority source into `merged`: a name already defined keeps
/// its definition, except that a stdio entry replaces a remote one.
pub(super) fn merge_lower(merged: &mut Servers, lower: Servers) {
    for (name, config) in lower {
        match merged.get(&name) {
            None => {
                merged.insert(name, config);
            }
            Some(current) if !current.is_stdio() && config.is_stdio() => {
                crate::logging::info(&format!(
                    "MCP: Keeping stdio server '{name}' over the {} definition from a higher-priority config",
                    current.transport.as_deref().unwrap_or("http")
                ));
                merged.insert(name, config);
            }
            Some(current) if current.is_stdio() && !config.is_stdio() => {
                crate::logging::info(&format!(
                    "MCP: Keeping existing stdio server '{name}'; ignoring {} definition from a lower-priority config",
                    config.transport.as_deref().unwrap_or("http")
                ));
            }
            Some(_) => {}
        }
    }
}

/// The parsed file at `path`, read again only when its stamp changed. `None`
/// when the file does not exist.
fn cached(path: &Path, format: Format) -> Option<Arc<Parsed>> {
    let stamp = match FileStamp::capture(path) {
        Ok(stamp) => stamp,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::logging::warn(&format!("MCP: cannot read {}: {error}", path.display()));
            return None;
        }
    };
    if let Some(slot) = cache().get_mut(path)
        && slot.stamp == stamp
    {
        slot.last_used = tick();
        return Some(Arc::clone(&slot.parsed));
    }

    // Stamped before the read: a file written in between is newer than the
    // stamp kept here, so the next load parses it again.
    let parsed = Arc::new(parse(path, format));
    let mut slots = cache();
    slots.insert(
        path.to_path_buf(),
        Slot {
            stamp,
            parsed: Arc::clone(&parsed),
            last_used: tick(),
        },
    );
    while slots.len() > MAX_CACHED_FILES {
        let Some(oldest) = slots
            .iter()
            .min_by_key(|(_, slot)| slot.last_used)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        slots.remove(&oldest);
    }
    Some(parsed)
}

fn parse(path: &Path, format: Format) -> Parsed {
    #[cfg(test)]
    PARSES.with(|parses| parses.set(parses.get() + 1));
    let result = match format {
        Format::Mcp => McpConfig::load_from_file(path).map(|config| Parsed {
            servers: config.servers,
            projects: HashMap::new(),
        }),
        Format::CodexToml => McpConfig::load_from_codex_toml(path).map(|config| Parsed {
            servers: config.servers,
            projects: HashMap::new(),
        }),
        Format::ClaudeJson => parse_claude_json(path),
    };
    match result {
        Ok(parsed) => parsed,
        Err(error) => {
            crate::logging::warn(&format!("MCP: cannot parse {}: {error}", path.display()));
            Parsed::default()
        }
    }
}

fn parse_claude_json(path: &Path) -> anyhow::Result<Parsed> {
    let content = std::fs::read_to_string(path)?;
    horon_agent_perf_counters::record_mcp_config_read(content.len() as u64);
    let value: serde_json::Value = serde_json::from_str(&content)?;
    let servers_of = |entry: &serde_json::Value| -> Option<Servers> {
        match serde_json::from_value::<Servers>(entry.get("mcpServers")?.clone()) {
            Ok(servers) => Some(servers),
            Err(error) => {
                crate::logging::warn(&format!(
                    "MCP: skipping malformed mcpServers in {}: {error}",
                    path.display()
                ));
                None
            }
        }
    };
    // A file without a global `mcpServers` or a `projects` map simply
    // contributes nothing from that part.
    let mut parsed = Parsed::default();
    if let Some(servers) = servers_of(&value) {
        parsed.servers = servers;
    }
    if let Some(projects) = value.get("projects").and_then(serde_json::Value::as_object) {
        parsed.projects = projects
            .iter()
            .filter_map(|(path, entry)| Some((path.clone(), servers_of(entry)?)))
            .filter(|(_, servers)| !servers.is_empty())
            .collect();
    }
    Ok(parsed)
}

#[cfg(test)]
#[path = "sources_tests.rs"]
mod tests;

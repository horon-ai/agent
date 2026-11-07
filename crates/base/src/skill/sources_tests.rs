//! Tests for the skill source table and its parallel read.

use super::{SkillRegistry, sources};
use std::path::{Path, PathBuf};

/// A sandboxed `HORON_AGENT_HOME` for one test.
///
/// `user_home_path` resolves other tools' directories under
/// `<home>/external`, so a test never reads or writes the real `~/.claude`,
/// `~/.agents`, or `~/.codex`.
struct Sandbox {
    home: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
}

impl Sandbox {
    /// `config` is written as `<home>/config.toml` when it is not empty.
    fn with_config(config: &str) -> Self {
        // The environment and the cached config are process-global.
        let previous_home = std::env::var_os("HORON_AGENT_HOME");
        let home = tempfile::tempdir().expect("sandbox home");
        crate::env::set_var("HORON_AGENT_HOME", home.path());
        if !config.is_empty() {
            std::fs::write(home.path().join("config.toml"), config).expect("write sandbox config");
        }
        crate::config::Config::invalidate_cache();
        Self { home, previous_home }
    }

    /// A path under horon-agent's own home.
    fn agent(&self, relative: &str) -> PathBuf {
        self.home.path().join(relative)
    }

    /// A path under the sandboxed user home (`<home>/external`).
    fn user(&self, relative: &str) -> PathBuf {
        self.home.path().join("external").join(relative)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(previous) => crate::env::set_var("HORON_AGENT_HOME", previous),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

const CLAUDE_ON: &str = "[skills]\nclaude_user = true\n";

/// How many times a load is repeated to prove the list is deterministic.
const MANIFEST_RUNS: usize = 10;

fn write_skill(skills_dir: &Path, name: &str) {
    let dir = skills_dir.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Skill {name}\n---\n\nUse {name}.\n"),
    )
    .expect("write skill");
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

/// Every loaded skill as `name path`, sorted by name.
fn listing(registry: &SkillRegistry) -> Vec<String> {
    registry
        .list()
        .iter()
        .map(|skill| format!("{} {}", skill.name, skill.path.display()))
        .collect()
}

#[test]
fn sources_load_in_priority_order() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    let workspace = tempfile::tempdir().expect("workspace");

    write_skill(&sandbox.agent("skills"), "user-skill");
    write_skill(&sandbox.user(".claude/skills"), "claude-skill");
    write_skill(&workspace.path().join(".horon-agent/skills"), "project-skill");

    let registry = SkillRegistry::load_for_working_dir(Some(workspace.path())).expect("load skills");
    for name in ["user-skill", "claude-skill", "project-skill"] {
        assert!(
            registry.get(name).is_some(),
            "all three sources must be visible, missing {name} in {:?}",
            listing(&registry)
        );
    }
    let first = listing(&registry);
    println!("first listing ({MANIFEST_RUNS}-run comparison starts here): {first:?}");

    // The same directories must always produce the same list: `read_dir` order
    // differs between filesystems and between runs on one machine.
    for run in 0..MANIFEST_RUNS {
        let again = SkillRegistry::load_for_working_dir(Some(workspace.path())).expect("reload skills");
        assert_eq!(listing(&again), first, "run {run} produced a different skill list");
    }
    println!("{MANIFEST_RUNS} further loads produced byte-identical listings");
}

#[test]
fn higher_priority_source_wins_a_name_collision() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config(CLAUDE_ON);

    write_skill(&sandbox.agent("skills"), "shared");
    write_skill(&sandbox.user(".claude/skills"), "shared");
    write_skill(&sandbox.user(".agents/skills"), "shared");

    let registry = SkillRegistry::load_global().expect("load global skills");
    let skill = registry.get("shared").expect("shared skill");
    assert_eq!(
        skill.path,
        sandbox.agent("skills/shared/SKILL.md"),
        "horon-agent's own collection (priority 100) must beat claude (80) and agents (70)"
    );

    // With the same priority, the table order decides, not `read_dir` order.
    std::fs::remove_dir_all(sandbox.agent("skills")).expect("drop the winning source");
    let registry = SkillRegistry::load_global().expect("load global skills");
    assert_eq!(
        registry.get("shared").expect("shared skill").path,
        sandbox.user(".claude/skills/shared/SKILL.md"),
        "claude (80) must beat agents (70)"
    );
}

#[test]
fn every_project_row_outranks_every_user_row() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config("");
    let workspace = tempfile::tempdir().expect("workspace");

    // The lowest project priority is 70, the highest user priority is 100.
    write_skill(&sandbox.agent("skills"), "shared");
    write_skill(&workspace.path().join(".agents/skills"), "shared");

    let registry = SkillRegistry::load_for_working_dir(Some(workspace.path())).expect("load skills");
    assert_eq!(
        registry.get("shared").expect("shared skill").path,
        workspace.path().join(".agents/skills/shared/SKILL.md"),
        "a project row must win over a higher-priority user row"
    );
}

#[test]
fn source_table_lists_the_documented_rows() {
    let rows: Vec<(&str, sources::Level, u8)> = sources::SOURCES
        .iter()
        .map(|row| (row.id, row.level, row.priority))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("horon-agent", sources::Level::Project, 100),
            ("claude", sources::Level::Project, 80),
            ("agents", sources::Level::Project, 70),
            ("horon-agent", sources::Level::User, 100),
            ("claude", sources::Level::User, 80),
            ("agents", sources::Level::User, 70),
            ("codex", sources::Level::User, 70),
            ("managed", sources::Level::User, 5),
        ]
    );
    assert_eq!(
        super::project_skill_paths(),
        vec![".horon-agent/skills", ".claude/skills", ".agents/skills"]
    );
}

#[test]
fn claude_user_off_by_default() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config("");

    write_skill(&sandbox.user(".claude/skills"), "claude-skill");
    write_skill(&sandbox.user(".claude/skills"), "claude-skill-two");
    let plugins_root = sandbox.user(".claude/plugins");
    write_plugin_skill(&plugins_root.join("cache/test-marketplace/one/1.0.0"), "plugin-skill");

    let registry = SkillRegistry::load_global().expect("load global skills");
    assert!(
        registry.get("claude-skill").is_none(),
        "~/.claude/skills must stay unread while [skills] claude_user is off"
    );
    assert!(
        registry.get("plugin-skill").is_none(),
        "~/.claude/plugins must stay unread while [skills] claude_user is off"
    );

    // A skill waiting in the managed directory is horon-agent's own file, not
    // another tool's, so it must not appear as a switch the operator can flip.
    write_skill(&sandbox.agent("managed-skills"), "model-written");

    let notices = sources::disabled_source_notices();
    assert_eq!(
        notices.len(),
        1,
        "one line per disabled external source, got {notices:?}"
    );
    let notice = &notices[0];
    assert!(notice.contains("Skills: 2 skill(s) in"), "unexpected notice: {notice}");
    assert!(
        notice.contains("not loaded; set [skills] claude_user = true"),
        "the notice must name the switch that turns the source on: {notice}"
    );
    assert!(
        notice.contains(".claude/skills"),
        "the notice must name the directory it skipped: {notice}"
    );
}

/// The startup load has to leave a real log line, and the logger's file is
/// fixed when it initialises. The assertion therefore runs in a child of this
/// same test binary, where the logger is fresh and `HORON_AGENT_HOME` is the
/// sandbox that child creates.
#[test]
fn claude_user_off_by_default_writes_a_log_line() {
    const CHILD: &str = "skill::sources_tests::claude_user_off_by_default_writes_a_log_line_child";
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary path"))
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .output()
        .expect("spawn the isolated log-emission child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("--- isolated child {CHILD} ---\n{stdout}");
    if !stderr.trim().is_empty() {
        println!("--- isolated child stderr ---\n{stderr}");
    }
    assert!(
        output.status.success(),
        "the isolated child must pass, got {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains("1 passed"),
        "the child must run exactly one test; a name mismatch reports zero tests\n{stdout}"
    );
}

#[test]
#[ignore = "spawned by claude_user_off_by_default_writes_a_log_line so the process-global logger is fresh"]
fn claude_user_off_by_default_writes_a_log_line_child() {
    let sandbox = Sandbox::with_config("");
    write_skill(&sandbox.user(".claude/skills"), "claude-skill");
    crate::logging::init();
    let log_path = crate::logging::log_path().expect("the sandboxed home resolves a log path");
    assert!(
        log_path.starts_with(sandbox.home.path()),
        "a fresh process must log under the sandbox, got {}",
        log_path.display()
    );

    SkillRegistry::load_global().expect("load global skills");

    let contents = std::fs::read_to_string(&log_path).expect("the logger writes under the sandboxed home");
    let lines: Vec<&str> = contents
        .lines()
        .filter(|line| line.contains("[skills] claude_user = true"))
        .collect();
    for line in &lines {
        println!("logged: {line}");
    }
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one disabled-source line, got {lines:?}"
    );
    assert!(
        lines[0].contains("1 skill(s) in") && lines[0].contains(".claude/skills not loaded"),
        "unexpected line: {}",
        lines[0]
    );
}

#[test]
fn no_first_run_copy() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config("");
    write_skill(&sandbox.user(".claude/skills"), "claude-skill");

    assert!(
        !sandbox.agent("skills").exists(),
        "the sandbox starts with no skills dir"
    );
    SkillRegistry::load_global().expect("load global skills");
    assert!(
        !sandbox.agent("skills").exists(),
        "loading must not create ~/.horon-agent/skills from another tool's directory"
    );
}

#[cfg(unix)]
#[test]
fn unreadable_source_skipped() {
    use std::os::unix::fs::PermissionsExt;

    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    write_skill(&sandbox.agent("skills"), "own-skill");
    write_skill(&sandbox.user(".claude/skills"), "claude-skill");

    let locked = sandbox.user(".agents/skills");
    write_skill(&locked, "hidden-skill");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock the source");

    let registry = SkillRegistry::load_global().expect("an unreadable source must not fail the load");

    // Restore before asserting so a failure cannot leave an unreadable dir for
    // the tempdir cleanup.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock the source");

    // root ignores the mode bits, and a filesystem that cannot represent them
    // makes the setup meaningless: say so instead of passing on a lie.
    if registry.get("hidden-skill").is_some() {
        println!("skipped: this process can read a 0o000 directory, so the mode cannot make a source unreadable");
        return;
    }

    assert!(registry.get("own-skill").is_some());
    assert!(registry.get("claude-skill").is_some());
    assert!(
        registry.get("hidden-skill").is_none(),
        "a directory that cannot be read contributes nothing"
    );
}

#[test]
fn managed_skills_load_only_when_switched_on_and_never_replace_an_authored_skill() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config("");
    write_skill(&sandbox.agent("skills"), "authored");
    write_skill(&sandbox.agent("managed-skills"), "authored");
    write_skill(&sandbox.agent("managed-skills"), "model-written");

    let off = SkillRegistry::load_global().expect("load global skills");
    assert!(off.get("model-written").is_none(), "managed skills are off by default");

    std::fs::write(
        sandbox.home.path().join("config.toml"),
        "[tools]\nmanaged_skills = true\n",
    )
    .expect("switch managed skills on");
    crate::config::Config::invalidate_cache();

    let on = SkillRegistry::load_global().expect("load global skills");
    assert!(
        on.get("model-written").is_some(),
        "the switch loads the managed directory"
    );
    assert_eq!(
        on.get("authored").expect("authored skill").path,
        sandbox.agent("skills/authored/SKILL.md"),
        "a managed skill must never replace an authored one"
    );
}

#[test]
fn plugin_skills_load_from_installed_plugins_manifest() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    let plugins_root = sandbox.user(".claude/plugins");

    // Mirror the real Claude Code layout:
    // cache/<marketplace>/<plugin>/<version>/skills/<skill>/SKILL.md
    let install = plugins_root.join("cache/test-marketplace/vercel/0.40.1");
    write_plugin_skill(&install, "ai-gateway");
    // Nested `.claude/skills` variant inside the same install.
    write_plugin_skill(&install.join(".claude"), "benchmark-agents");
    write_installed_plugins_manifest(&plugins_root, &[&install]);

    let registry = SkillRegistry::load_global().expect("load global skills");
    assert!(registry.contains("ai-gateway"));
    assert!(registry.contains("benchmark-agents"));
}

#[test]
fn plugin_skills_fall_back_to_cache_scan_without_manifest() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config(CLAUDE_ON);

    let install = sandbox.user(".claude/plugins/cache/test-marketplace/my-plugin/1.0.0");
    write_plugin_skill(&install, "cache-skill");

    let registry = SkillRegistry::load_global().expect("load global skills");
    assert!(registry.contains("cache-skill"));
}

#[test]
fn plugin_skills_load_from_repos_layout() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config(CLAUDE_ON);

    let repo = sandbox.user(".claude/plugins/repos/owner/my-plugin");
    write_plugin_skill(&repo, "repo-skill");

    let registry = SkillRegistry::load_global().expect("load global skills");
    assert!(registry.contains("repo-skill"));
}

#[test]
fn plugin_scan_skips_marketplace_catalog_when_manifest_exists() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    let plugins_root = sandbox.user(".claude/plugins");

    // Installed plugin listed in the manifest.
    let install = plugins_root.join("cache/test-marketplace/installed/1.0.0");
    write_plugin_skill(&install, "installed-skill");
    write_installed_plugins_manifest(&plugins_root, &[&install]);

    // Marketplace catalog entry the user never installed.
    write_plugin_skill(
        &plugins_root.join("marketplaces/test-marketplace/plugins/uninstalled"),
        "uninstalled-skill",
    );
    // Cache entry not referenced by the manifest (stale install).
    write_plugin_skill(&plugins_root.join("cache/test-marketplace/stale/0.1.0"), "stale-skill");

    let registry = SkillRegistry::load_global().expect("load global skills");
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
    let sandbox = Sandbox::with_config(CLAUDE_ON);

    // Deeper than the plugin scan bound below the cache root.
    let too_deep = sandbox.user(".claude/plugins/cache/a/b/c/d/e/f");
    write_plugin_skill(&too_deep, "too-deep-skill");

    let registry = SkillRegistry::load_global().expect("load global skills");
    assert!(!registry.contains("too-deep-skill"));
}

#[test]
fn plugin_skill_dirs_empty_for_missing_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing = temp.path().join("does-not-exist");
    assert!(sources::plugin_skill_dirs_under(&missing).is_empty());
    let (skills, stats) = sources::scan_plugins_root(&missing);
    assert!(skills.is_empty());
    assert_eq!(stats.dirs, 0, "a missing root is never enumerated");
}

#[test]
fn explicit_horon_agent_skill_wins_over_plugin_skill_with_same_name() {
    let _env_guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::with_config(CLAUDE_ON);

    let install = sandbox.user(".claude/plugins/cache/test-marketplace/my-plugin/1.0.0");
    write_plugin_skill_with_description(&install, "shared-name", "plugin version");
    write_skill(&sandbox.agent("skills"), "shared-name");

    let registry = SkillRegistry::load_global().expect("load global skills");
    let skill = registry.get("shared-name").expect("skill present");
    assert_eq!(
        skill.description, "Skill shared-name",
        "the explicit horon-agent skill must override the plugin-provided one"
    );
}

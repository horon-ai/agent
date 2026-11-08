//! The MCP source table: priority, switches, the parse cache, and trust.

use super::super::protocol::McpConfig;
use std::path::{Path, PathBuf};

/// An isolated `HORON_AGENT_HOME` with an optional config.toml and a cold
/// parse cache. Other tools' files live under `<home>/external`, which is where
/// `user_home_path` resolves `~` while `HORON_AGENT_HOME` is set.
struct Sandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    home: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
    previous_disable: Option<std::ffi::OsString>,
}

impl Sandbox {
    fn with_config(config: &str) -> Self {
        let guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let previous_home = std::env::var_os("HORON_AGENT_HOME");
        let previous_disable = std::env::var_os("HORON_AGENT_DISABLE_CLAUDE_MCP");
        crate::env::set_var("HORON_AGENT_HOME", home.path());
        crate::env::remove_var("HORON_AGENT_DISABLE_CLAUDE_MCP");
        if !config.is_empty() {
            std::fs::write(home.path().join("config.toml"), config).expect("config");
        }
        crate::config::Config::invalidate_cache();
        crate::workspace_trust::set_trust_file_for_tests(home.path().join("workspace-trust.json"));
        super::clear_cache_for_tests();
        Self {
            _guard: guard,
            home,
            previous_home,
            previous_disable,
        }
    }

    fn agent(&self, relative: &str) -> PathBuf {
        self.home.path().join(relative)
    }

    fn user(&self, relative: &str) -> PathBuf {
        self.home.path().join("external").join(relative)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        match self.previous_disable.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_DISABLE_CLAUDE_MCP", value),
            None => crate::env::remove_var("HORON_AGENT_DISABLE_CLAUDE_MCP"),
        }
        crate::config::Config::invalidate_cache();
    }
}

const CLAUDE_ON: &str = "[mcp]\nclaude_user = true\n";
const CODEX_ON: &str = "[mcp]\ncodex_user = true\n";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(path, text).expect("write");
}

fn command_of(config: &McpConfig, name: &str) -> Option<String> {
    config.servers.get(name).map(|server| server.command.clone())
}

#[test]
fn horon_config_wins_over_claude() {
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    write(
        &sandbox.agent("mcp.json"),
        r#"{"mcpServers":{"github":{"command":"horon-bin"}}}"#,
    );
    write(
        &sandbox.user(".claude.json"),
        r#"{"mcpServers":{"github":{"command":"claude-bin"},"claude-only":{"command":"c"}}}"#,
    );
    let config = McpConfig::load_for_dir(None);
    assert_eq!(command_of(&config, "github").as_deref(), Some("horon-bin"));
    assert_eq!(
        command_of(&config, "claude-only").as_deref(),
        Some("c"),
        "a name only Claude defines still loads"
    );
}

#[test]
fn user_files_parsed_once() {
    let sandbox = Sandbox::with_config("");
    write(
        &sandbox.agent("mcp.json"),
        r#"{"mcpServers":{"steady":{"command":"steady-bin"}}}"#,
    );
    let before = super::parses_for_tests();
    for _ in 0..10 {
        assert_eq!(
            command_of(&McpConfig::load_for_dir(None), "steady").as_deref(),
            Some("steady-bin")
        );
    }
    let parses = super::parses_for_tests() - before;
    println!("loads: 10, user file parses: {parses}");
    assert_eq!(parses, 1, "an unchanged user file is parsed once");
}

#[test]
fn edited_user_file_reparsed() {
    let sandbox = Sandbox::with_config("");
    let path = sandbox.agent("mcp.json");
    write(&path, r#"{"mcpServers":{"alpha":{"command":"first"}}}"#);
    assert_eq!(
        command_of(&McpConfig::load_for_dir(None), "alpha").as_deref(),
        Some("first")
    );
    write(
        &path,
        r#"{"mcpServers":{"alpha":{"command":"second-edit"},"beta":{"command":"b"}}}"#,
    );
    let edited = McpConfig::load_for_dir(None);
    assert_eq!(command_of(&edited, "alpha").as_deref(), Some("second-edit"));
    assert!(edited.servers.contains_key("beta"));
    std::fs::remove_file(&path).expect("delete");
    assert!(
        McpConfig::load_for_dir(None).servers.is_empty(),
        "a deleted file contributes nothing"
    );
}

#[test]
fn claude_user_off_by_default() {
    let sandbox = Sandbox::with_config("");
    write(
        &sandbox.user(".claude.json"),
        r#"{"mcpServers":{"claude-current":{"command":"c"}}}"#,
    );
    write(
        &sandbox.user(".claude/mcp.json"),
        r#"{"mcpServers":{"claude-legacy":{"command":"l"}}}"#,
    );
    write(
        &sandbox.user(".codex/config.toml"),
        "[mcp_servers.codex_only]\ncommand = \"codex-bin\"\n",
    );
    let config = McpConfig::load_for_dir(None);
    assert!(
        config.servers.is_empty(),
        "another host's servers need their switch: {:?}",
        config.servers.keys()
    );
}

#[test]
fn disable_env_overrides_switch() {
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    crate::env::set_var("HORON_AGENT_DISABLE_CLAUDE_MCP", "1");
    write(
        &sandbox.agent("mcp.json"),
        r#"{"mcpServers":{"horon-global":{"command":"h"}}}"#,
    );
    write(
        &sandbox.user(".claude.json"),
        r#"{"mcpServers":{"claude-current":{"command":"c"}}}"#,
    );
    write(
        &sandbox.user(".claude/mcp.json"),
        r#"{"mcpServers":{"claude-legacy":{"command":"l"}}}"#,
    );
    let config = McpConfig::load_for_dir(None);
    assert!(config.servers.contains_key("horon-global"));
    assert!(!config.servers.contains_key("claude-current"));
    assert!(!config.servers.contains_key("claude-legacy"));
}

#[test]
fn untrusted_project_still_skipped() {
    let sandbox = Sandbox::with_config("");
    write(
        &sandbox.agent("mcp.json"),
        r#"{"mcpServers":{"horon-global":{"command":"h"}}}"#,
    );
    let project = tempfile::tempdir().expect("project");
    write(
        &project.path().join(".mcp.json"),
        r#"{"mcpServers":{"untrusted":{"command":"u"}}}"#,
    );
    // Warm the cache first: a warm user-file cache must not open the project.
    for _ in 0..2 {
        let config = McpConfig::load_for_dir(Some(project.path()));
        assert!(config.servers.contains_key("horon-global"));
        assert!(
            !config.servers.contains_key("untrusted"),
            "an untrusted project config is never loaded"
        );
    }
    crate::workspace_trust::trust_for_tests(project.path());
    assert!(
        McpConfig::load_for_dir(Some(project.path()))
            .servers
            .contains_key("untrusted")
    );
}

#[test]
fn project_config_wins_over_user_config() {
    let sandbox = Sandbox::with_config("");
    write(
        &sandbox.agent("mcp.json"),
        r#"{"mcpServers":{"shared":{"command":"user-bin"}}}"#,
    );
    let project = tempfile::tempdir().expect("project");
    write(
        &project.path().join(".mcp.json"),
        r#"{"mcpServers":{"shared":{"command":"project-bin"}}}"#,
    );
    crate::workspace_trust::trust_for_tests(project.path());
    assert_eq!(
        command_of(&McpConfig::load_for_dir(Some(project.path())), "shared").as_deref(),
        Some("project-bin")
    );
}

#[test]
fn claude_project_entries_do_not_leak() {
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    let project_a = tempfile::tempdir().expect("a");
    let project_b = tempfile::tempdir().expect("b");
    let body = serde_json::json!({
        "mcpServers": { "global-srv": { "command": "global-bin" } },
        "projects": {
            project_a.path().to_string_lossy(): {
                "mcpServers": {
                    "a-only": { "command": "a-bin" },
                    "global-srv": { "command": "a-override" }
                }
            },
            project_b.path().to_string_lossy(): { "mcpServers": { "b-only": { "command": "b-bin" } } }
        }
    });
    write(&sandbox.user(".claude.json"), &body.to_string());
    for _ in 0..2 {
        let a = McpConfig::load_for_dir(Some(project_a.path()));
        assert_eq!(command_of(&a, "a-only").as_deref(), Some("a-bin"));
        assert_eq!(
            command_of(&a, "global-srv").as_deref(),
            Some("a-override"),
            "a project's entry wins over Claude's global servers"
        );
        assert!(!a.servers.contains_key("b-only"));
        let b = McpConfig::load_for_dir(Some(project_b.path()));
        assert_eq!(command_of(&b, "b-only").as_deref(), Some("b-bin"));
        assert_eq!(command_of(&b, "global-srv").as_deref(), Some("global-bin"));
        assert!(
            !b.servers.contains_key("a-only"),
            "project A's server must not reach project B"
        );
    }
    let unbound = McpConfig::load_for_dir(None);
    assert!(unbound.servers.contains_key("global-srv"));
    assert!(!unbound.servers.contains_key("a-only") && !unbound.servers.contains_key("b-only"));
}

#[test]
fn stdio_kept_over_remote_both_directions() {
    let remote = r#"{"type":"http","url":"https://example.invalid/mcp"}"#;
    let stdio = r#"{"command":"npx","args":["-y","mcp-remote"]}"#;
    for (horon, claude) in [(stdio, remote), (remote, stdio)] {
        let sandbox = Sandbox::with_config(CLAUDE_ON);
        write(
            &sandbox.agent("mcp.json"),
            &format!(r#"{{"mcpServers":{{"github":{horon}}}}}"#),
        );
        write(
            &sandbox.user(".claude.json"),
            &format!(r#"{{"mcpServers":{{"github":{claude}}}}}"#),
        );
        let config = McpConfig::load_for_dir(None);
        let github = config.servers.get("github").expect("github survives");
        assert!(
            github.is_stdio(),
            "higher {horon} vs lower {claude}: the stdio entry is kept"
        );
        assert_eq!(github.command, "npx");
        drop(sandbox);
    }
}

#[test]
fn codex_live_source_does_not_write_mcp_json() {
    let sandbox = Sandbox::with_config(CODEX_ON);
    let codex = sandbox.user(".codex/config.toml");
    write(
        &codex,
        "[mcp_servers.codex_only]\ncommand = \"codex-bin\"\nenv = { TOKEN = \"codex-inline-secret\" }\n\n\
         [mcp_servers.disabled_one]\ncommand = \"disabled-bin\"\nenabled = false\n",
    );
    let first = McpConfig::load_for_dir(None);
    assert_eq!(command_of(&first, "codex_only").as_deref(), Some("codex-bin"));
    assert!(
        !first.servers["disabled_one"].is_enabled(),
        "enabled = false is carried over"
    );
    assert!(
        !sandbox.agent("mcp.json").exists(),
        "the live Codex source is never copied"
    );

    std::fs::remove_file(&codex).expect("remove Codex config");
    assert!(
        McpConfig::load_for_dir(None).servers.is_empty(),
        "a removed Codex server disappears instead of surviving in a snapshot"
    );
    assert!(!sandbox.agent("mcp.json").exists());
}

#[test]
fn legacy_claude_config_is_live() {
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    let legacy = sandbox.user(".claude/mcp.json");
    write(
        &legacy,
        r#"{"mcpServers":{"alpha":{"command":"legacy-alpha"},"beta":{"command":"legacy-beta"}}}"#,
    );
    let first = McpConfig::load_for_dir(None);
    assert!(first.servers.contains_key("alpha") && first.servers.contains_key("beta"));
    write(&legacy, r#"{"mcpServers":{"alpha":{"command":"legacy-alpha-edited"}}}"#);
    let second = McpConfig::load_for_dir(None);
    assert_eq!(command_of(&second, "alpha").as_deref(), Some("legacy-alpha-edited"));
    assert!(!second.servers.contains_key("beta"), "a deleted server disappears");
    assert!(!sandbox.agent("mcp.json").exists(), "Claude values are never copied");
}

#[test]
fn live_claude_log_names_the_source_without_values() {
    let message = super::live_claude_log_message(2, "~/.claude.json");
    assert!(message.contains("Loaded 2 server(s) live from Claude Code (~/.claude.json)"));
    assert!(message.contains("source values were not copied"));
}

#[test]
fn structural_user_config_rereads() {
    let sandbox = Sandbox::with_config(CLAUDE_ON);
    write(
        &sandbox.agent("mcp.json"),
        r#"{"mcpServers":{"horon-global":{"command":"h"}}}"#,
    );
    write(
        &sandbox.user(".claude.json"),
        r#"{"mcpServers":{"claude-current":{"command":"c"}}}"#,
    );
    write(
        &sandbox.user(".claude/mcp.json"),
        r#"{"mcpServers":{"claude-legacy":{"command":"l"}}}"#,
    );
    let project = tempfile::tempdir().expect("project with no MCP config");

    // Process-wide counters: the gate runs this test alone with
    // `--test-threads=1`, so the deltas belong to it.
    let cold = horon_agent_perf_counters::snapshot();
    let warm_config = McpConfig::load_for_dir(Some(project.path()));
    let warm = horon_agent_perf_counters::snapshot();
    assert_eq!(warm_config.servers.len(), 3, "{:?}", warm_config.servers.keys());
    for _ in 0..9 {
        McpConfig::load_for_dir(Some(project.path()));
    }
    let after = horon_agent_perf_counters::snapshot();
    assert!(after.enabled, "the structural measurement needs the perf counters on");

    println!(
        "STRUCTURAL_PERF section=mcp_config source=mcp-config mcp_config_warmup_reads={} mcp_config_user_rereads={}",
        warm.mcp_config_reads - cold.mcp_config_reads,
        after.mcp_config_reads - warm.mcp_config_reads,
    );
}

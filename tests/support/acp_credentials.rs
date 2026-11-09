//! The credential stores the product reads, and the snapshot that proves an
//! ACP auth round trip creates, removes or changes none of them (IS-15).
//!
//! The lists are copies of private constants in `crates/agent-sdk/src/launch.rs`,
//! because that module does not export them. `credential_lists_match_the_shipped_code`
//! fails the moment the two drift apart.

use super::acp_daemon::AcpDaemon;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `CREDENTIAL_FILES` of crates/agent-sdk/src/launch.rs: the stores the
/// product keeps directly under HORON_HOME. The list is private there, so
/// it is copied and `credential_lists_match_the_shipped_code` fails the moment
/// the two drift apart.
const SHIPPED_HOME_CREDENTIAL_FILES: [&str; 5] = [
    "auth.json",
    "openai-auth.json",
    "antigravity_oauth.json",
    "gemini_oauth.json",
    "config.toml",
];

/// Further stores under HORON_HOME that are not in launch.rs: the MCP
/// OAuth store (`STORE_FILE` of crates/base/src/mcp/oauth.rs).
const EXTRA_HOME_CREDENTIAL_FILES: [&str; 1] = ["mcp-oauth.json"];

/// `EXTERNAL_CREDENTIAL_FILES` of crates/agent-sdk/src/launch.rs, rooted at
/// HOME (the scratch root), copied for the same reason.
const SHIPPED_EXTERNAL_CREDENTIAL_FILES: [&str; 20] = [
    ".claude/.credentials.json",
    ".codex/auth.json",
    ".gemini/oauth_creds.json",
    ".cursor/auth.json",
    ".config/cursor/auth.json",
    "AppData/Roaming/Cursor/auth.json",
    ".config/Cursor/User/globalStorage/state.vscdb",
    ".config/cursor/User/globalStorage/state.vscdb",
    "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
    "Library/Application Support/cursor/User/globalStorage/state.vscdb",
    "AppData/Roaming/Cursor/User/globalStorage/state.vscdb",
    "AppData/Roaming/cursor/User/globalStorage/state.vscdb",
    ".config/github-copilot/hosts.json",
    ".config/github-copilot/apps.json",
    ".copilot/config.json",
    ".hermes/auth.json",
    ".pi/agent/auth.json",
    ".openclaw/agent/auth.json",
    ".openclaw/credentials/oauth.json",
    ".local/share/opencode/auth.json",
];

/// The per-agent OpenClaw store names launch.rs reads one agent-id level deep:
/// `$HOME/.openclaw/agents/<id>/agent/<name>`.
const OPENCLAW_AGENT_CREDENTIAL_FILES: [&str; 2] = ["auth-profiles.json", "auth.json"];

/// Every credential location the product reads or writes for a daemon whose
/// HORON_HOME is `home`, HOME is `user_home` and XDG_CONFIG_HOME is
/// `xdg_config`, with its bytes or `None` when absent, so a credential that
/// appears, disappears or changes alters the snapshot. Two copies of each
/// external store and each provider env file are watched:
/// - where the daemon itself resolves them under HORON_HOME, which the
///   test daemon always sets: `crate::storage::user_home_path(rel)` is
///   `$HORON_HOME/external/<rel>` and `app_config_dir()` is
///   `$HORON_HOME/config/horon-agent` (crates/storage/src/lib.rs), which
///   are also the instance paths launch.rs links the stores to;
/// - where launch.rs reads them from the user's HOME: `$HOME/<rel>`, and
///   `user_app_config_dir()`, which is `$HOME/Library/Application Support/horon-agent`
///   on macOS and `$XDG_CONFIG_HOME/horon-agent` on other unix systems.
///
/// For each root: the 20 external stores, the per-agent OpenClaw stores one
/// agent-id level deep, and every `*.env` provider key file. Under `home`
/// also: the launch.rs `CREDENTIAL_FILES`, the MCP OAuth store and every
/// `*.env` provider key file.
///
/// The set is closed: runtime state such as `active_pids/`, session files or
/// the model catalog caches in the config dir never enters it, whatever its name.
pub(super) fn credential_files(home: &Path, user_home: &Path, xdg_config: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn external_stores(root: &Path, out: &mut Vec<PathBuf>) {
        out.extend(SHIPPED_EXTERNAL_CREDENTIAL_FILES.iter().map(|name| root.join(name)));
        for agent in std::fs::read_dir(root.join(".openclaw/agents"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if agent.path().is_dir() {
                for name in OPENCLAW_AGENT_CREDENTIAL_FILES {
                    out.push(agent.path().join("agent").join(name));
                }
            }
        }
    }
    fn env_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().ends_with(".env") {
                out.push(entry.path());
            }
        }
    }
    let mut paths: Vec<PathBuf> = SHIPPED_HOME_CREDENTIAL_FILES
        .iter()
        .chain(EXTRA_HOME_CREDENTIAL_FILES.iter())
        .map(|name| home.join(name))
        .collect();
    external_stores(&home.join("external"), &mut paths);
    env_files(&home.join("config").join("horon-agent"), &mut paths);
    external_stores(user_home, &mut paths);
    let user_app_config = if cfg!(target_os = "macos") {
        user_home.join("Library/Application Support/horon-agent")
    } else {
        xdg_config.join("horon-agent")
    };
    env_files(&user_app_config, &mut paths);
    paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).ok();
            (path, bytes)
        })
        .collect()
}

/// [`credential_files`] for one test daemon.
pub(super) fn daemon_credentials(daemon: &AcpDaemon) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    credential_files(&daemon.home, &daemon.root, &daemon.root.join("config"))
}

/// The copied credential lists equal the private stores the shipped code reads.
#[test]
fn credential_lists_match_the_shipped_code() {
    fn literals(source: &str, start: &str) -> Vec<String> {
        let begin = source.find(start).unwrap_or_else(|| panic!("{start} is in launch.rs")) + start.len();
        let end = begin + source[begin..].find("];").expect("the list ends");
        source[begin..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }
    let launch = include_str!("../../crates/agent-sdk/src/launch.rs");
    assert_eq!(
        literals(launch, "const CREDENTIAL_FILES: &[&str] = &["),
        SHIPPED_HOME_CREDENTIAL_FILES
    );
    assert_eq!(
        literals(launch, "const EXTERNAL_CREDENTIAL_FILES: &[&str] = &["),
        SHIPPED_EXTERNAL_CREDENTIAL_FILES
    );
    assert!(
        launch.contains(r#"for name in ["auth-profiles.json", "auth.json"] {"#)
            && launch.contains(r#".join("agents")"#),
        "the per-agent OpenClaw store names"
    );
    assert!(
        include_str!("../../crates/platform-macos/src/lib.rs").contains(r#".join("Library/Application Support")"#)
            && include_str!("../../crates/platform/src/paths.rs").contains("home.join(app)")
            && launch.contains(r#"user_config_dir("horon-agent")"#)
            && launch.contains(r#"ends_with(".env")"#),
        "the provider env files of user_app_config_dir()"
    );
    assert!(
        launch.contains(r#"PathBuf::from("external").join(relative)"#)
            && launch.contains(r#"PathBuf::from("external").join(&relative)"#)
            && launch.contains(r#"PathBuf::from("config").join("horon-agent").join(&name)"#),
        "launch.rs links the stores to <instance>/external/<rel> and <instance>/config/horon-agent/<name>.env"
    );
    let storage = include_str!("../../crates/storage/src/lib.rs");
    assert!(
        storage.contains(r#"return Ok(PathBuf::from(path).join("external").join(relative));"#)
            && storage.contains(r#"return Ok(PathBuf::from(path).join("config").join("horon-agent"));"#),
        "under HORON_HOME the daemon resolves user_home_path to $HORON_HOME/external/<rel> and app_config_dir to $HORON_HOME/config/horon-agent"
    );
    let oauth = include_str!("../../crates/base/src/mcp/oauth.rs");
    assert!(
        oauth.contains(r#"const STORE_FILE: &str = "mcp-oauth.json";"#),
        "the MCP OAuth store name"
    );
}

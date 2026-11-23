//! `horon config show|check` and `horon run --plan`, driven through the real binary.

use std::path::Path;
use std::process::{Command, Output};

fn horon(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_horon-agent"))
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("HORON_HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("HORON_AGENT_NON_INTERACTIVE", "1")
        .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
        .env("HORON_AGENT_MEMORY_ENABLED", "0")
        .env("NO_COLOR", "1")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(args)
        .output()
        .expect("run horon-agent")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn config_check_names_the_invalid_section_and_show_never_prints_a_credential() {
    let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
    let home = tempfile::tempdir().unwrap();
    let keyed = "[providers.keyed]\ntype = \"openai-compatible\"\nbase_url = \"https://llm.example.com/v1\"\n";
    std::fs::write(
        home.path().join("config.toml"),
        format!("{keyed}api_key = \"{secret}\"\ndefault_model = \"m\"\n"),
    )
    .unwrap();
    let shown = horon(home.path(), &["config", "show", "--json"]);
    assert!(shown.status.success(), "{}", text(&shown.stderr));
    let report: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let settings = report["settings"].as_array().unwrap();
    assert!(!settings.is_empty());
    for row in settings {
        assert!(row["source"].is_string() && row["key"].is_string());
    }
    assert!(!text(&shown.stdout).contains(secret));
    assert!(horon(home.path(), &["config", "check"]).status.success());

    std::fs::write(home.path().join("config.toml"), "[prewalk]\nenabled = true\n").unwrap();
    let bad = horon(home.path(), &["config", "check", "--json"]);
    assert_eq!(bad.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&bad.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert!(report["error"].as_str().unwrap().contains("[prewalk]"), "{report}");
}

#[test]
fn run_plan_fails_before_any_request_when_plan_mode_is_off() {
    let home = tempfile::tempdir().unwrap();
    // Nothing listens on this port, so only a pre-request refusal can name plan mode.
    let config = "[provider]\ndefault_provider = \"qa\"\ndefault_model = \"m\"\n\n[providers.qa]\ntype = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:1/v1\"\nauth = \"none\"\ndefault_model = \"m\"\n";
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    let result = horon(home.path(), &["run", "make a plan", "--no-update", "--plan"]);
    assert!(!result.status.success());
    let stderr = text(&result.stderr);
    assert!(stderr.contains("Plan mode is off"), "{stderr}");
}

#[test]
fn config_show_reflects_tool_launch_flags_and_isolated_profile_applies_to_offline_commands() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[tools]\nprofile = \"core\"\nmcp_tools = \"eager\"\n\n[provider]\ndefault_provider = \"root_provider\"\ndefault_model = \"m-root\"\n\n[providers.root_provider]\ntype = \"openai-compatible\"\nbase_url = \"https://root.example.com/v1\"\nauth = \"none\"\ndefault_model = \"m-root\"\n",
    )
    .unwrap();

    let plain = horon(home.path(), &["config", "show", "--json"]);
    assert!(plain.status.success(), "{}", text(&plain.stderr));
    let plain_json: serde_json::Value = serde_json::from_slice(&plain.stdout).unwrap();
    let plain_rows = plain_json["settings"].as_array().unwrap();
    let plain_profile = plain_rows.iter().find(|row| row["key"] == "tools.profile").unwrap();
    assert_eq!(plain_profile["value"], "core");
    assert_eq!(plain_profile["source"], "file");

    let flagged = horon(
        home.path(),
        &[
            "--tool-profile",
            "full",
            "--mcp-tools",
            "deferred",
            "config",
            "show",
            "--json",
        ],
    );
    assert!(flagged.status.success(), "{}", text(&flagged.stderr));
    let flagged_json: serde_json::Value = serde_json::from_slice(&flagged.stdout).unwrap();
    let rows = flagged_json["settings"].as_array().unwrap();
    let profile_row = rows.iter().find(|row| row["key"] == "tools.profile").unwrap();
    assert_eq!(profile_row["value"], "full");
    assert_eq!(profile_row["source"], "cli");
    assert_eq!(profile_row["origin"], "--tool-profile");
    let mcp_row = rows.iter().find(|row| row["key"] == "tools.mcp_tools").unwrap();
    assert_eq!(mcp_row["value"], "deferred");
    assert_eq!(mcp_row["source"], "cli");
    assert_eq!(mcp_row["origin"], "--mcp-tools");

    let profile_home = home.path().join("profiles").join("qa");
    std::fs::create_dir_all(&profile_home).unwrap();
    std::fs::write(
        profile_home.join("config.toml"),
        "[provider]\ndefault_provider = \"iso_provider\"\ndefault_model = \"m-iso\"\n\n[providers.iso_provider]\ntype = \"openai-compatible\"\nbase_url = \"https://iso.example.com/v1\"\nauth = \"none\"\ndefault_model = \"m-iso\"\n",
    )
    .unwrap();
    let status = horon(
        home.path(),
        &["--isolated-profile", "qa", "provider", "status", "--json"],
    );
    assert!(status.status.success(), "{}", text(&status.stderr));
    let status_text = text(&status.stdout);
    assert!(status_text.contains("iso_provider"), "{status_text}");
    assert!(!status_text.contains("root_provider"), "{status_text}");
}

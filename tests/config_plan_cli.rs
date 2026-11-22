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

//! Tests for the allow-listed daemon settings report.
//!
//! Each test supplies its own config, file layer, environment and launch
//! record: the process's own would make a row pass for a reason the test did
//! not name.

use super::*;
use crate::config::Config;

/// A config with distinctive non-default values, so a row that fell back to a
/// default would be visible rather than plausible.
fn distinctive_config() -> Config {
    let mut config = Config::default();
    config.provider.openai_reasoning_effort = Some("high".into());
    config.provider.stream_idle_timeout_secs = 240;
    config.provider.max_retries = 3;
    config.tools.profile = "full".into();
    config
}

fn file_layer(text: &str) -> FileLayer {
    FileLayer {
        path: "/home/u/config.toml".into(),
        config: toml::from_str(text).expect("the fixture loads as config"),
        document: toml::from_str(text).expect("the fixture parses as toml"),
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn no_launch(_: &str) -> Option<LaunchOrigin> {
    None
}

fn pick<'a>(rows: &'a [EffectiveSettingRow], key: &str) -> &'a EffectiveSettingRow {
    rows.iter()
        .find(|row| row.key == key)
        .expect("the allow-list names this key")
}

#[test]
fn an_unset_key_reports_the_default_with_no_origin() {
    // The shipped default names an effort ("low"), so the fixture clears it to
    // reach the unset case: the row must say the value is absent instead of
    // printing a default the daemon never had to invent.
    let mut config = Config::default();
    config.provider.openai_reasoning_effort = None;
    let rows = report(&config, None, no_env, no_launch);
    let row = pick(&rows, "provider.openai_reasoning_effort");
    assert_eq!(row.source, EffectiveSource::Default);
    assert_eq!(row.origin, None);
    assert_eq!(row.value, "(unset)", "an absent value is said out loud");
}

#[test]
fn a_key_the_file_declares_names_that_file() {
    let rows = report(
        &distinctive_config(),
        Some(&file_layer("[provider]\nstream_idle_timeout_secs = 240\n")),
        no_env,
        no_launch,
    );
    let row = pick(&rows, "provider.stream_idle_timeout_secs");
    assert_eq!(row.value, "240");
    assert_eq!(row.source, EffectiveSource::File);
    assert_eq!(row.origin.as_deref(), Some("/home/u/config.toml"));
}

#[test]
fn a_key_the_file_does_not_declare_stays_a_default_even_when_it_differs() {
    // The config's value could come from anywhere; only the file it was
    // actually written in may be named as the source.
    let rows = report(
        &distinctive_config(),
        Some(&file_layer("[tools]\nprofile = \"full\"\n")),
        no_env,
        no_launch,
    );
    let row = pick(&rows, "provider.stream_idle_timeout_secs");
    assert_eq!(row.value, "240");
    assert_eq!(
        row.source,
        EffectiveSource::Default,
        "a file that never mentions the key is not its source"
    );
}

#[test]
fn a_file_value_that_differs_from_the_value_in_force_names_no_file() {
    // The file was read after the config was cached, or the value came from a
    // layer this report cannot see. Naming the file would blame a value it
    // does not hold.
    let mut config = Config::default();
    config.provider.stream_idle_timeout_secs = 300;
    let rows = report(
        &config,
        Some(&file_layer("[provider]\nstream_idle_timeout_secs = 240\n")),
        no_env,
        no_launch,
    );
    assert_eq!(
        pick(&rows, "provider.stream_idle_timeout_secs").source,
        EffectiveSource::Default
    );
}

#[test]
fn a_declared_value_the_daemon_would_reject_is_not_the_files_source() {
    // `ApprovalMode` silently falls back to the default for an unrecognized
    // string, so the file parses and the value in force is the default. The
    // file must not be named for a value it never applied.
    let text = "[tools]\napproval_mode = \"nonsense\"\n";
    let rows = report(&Config::default(), Some(&file_layer(text)), no_env, no_launch);
    let row = pick(&rows, "tools.approval_mode");
    assert_eq!(row.value, "yolo");
    assert_eq!(row.source, EffectiveSource::Default);
    assert_eq!(row.origin, None);
}

#[test]
fn an_environment_variable_that_the_daemon_would_apply_wins_over_the_file() {
    let rows = report(
        &distinctive_config(),
        Some(&file_layer("[provider]\nstream_idle_timeout_secs = 240\n")),
        |name| (name == "HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS").then(|| "300".to_string()),
        no_launch,
    );
    let row = pick(&rows, "provider.stream_idle_timeout_secs");
    assert_eq!(row.source, EffectiveSource::Env);
    assert_eq!(row.origin.as_deref(), Some("HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS"));
}

#[test]
fn an_environment_value_the_daemon_ignores_is_not_named_as_the_source() {
    // A zero timeout is filtered out by the daemon, so the file's value is
    // still the one in force and the variable must not be blamed for it.
    let rows = report(
        &distinctive_config(),
        Some(&file_layer("[provider]\nstream_idle_timeout_secs = 240\n")),
        |name| (name == "HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS").then(|| "0".to_string()),
        no_launch,
    );
    assert_eq!(
        pick(&rows, "provider.stream_idle_timeout_secs").source,
        EffectiveSource::File
    );
}

#[test]
fn a_flag_the_launcher_recorded_is_the_source_named_as_the_flag() {
    let env = |name: &str| (name == "HORON_AGENT_MCP_TOOLS").then(|| "eager".to_string());
    let launch = |name: &str| (name == "HORON_AGENT_MCP_TOOLS").then_some(LaunchOrigin::Flag("--mcp-tools"));
    let rows = report(&distinctive_config(), None, env, launch);
    let row = pick(&rows, "tools.mcp_tools");
    assert_eq!(row.source, EffectiveSource::Cli);
    assert_eq!(row.origin.as_deref(), Some("--mcp-tools"));
}

#[test]
fn a_default_the_launcher_chose_is_not_reported_as_the_environment() {
    // A headless launch that selected no profile sets the variable to `core`
    // itself. The variable exists, but the user's environment never set it.
    let mut config = Config::default();
    config.tools.profile = "core".into();
    let env = |name: &str| (name == "HORON_AGENT_TOOL_PROFILE").then(|| "core".to_string());
    let launch = |name: &str| (name == "HORON_AGENT_TOOL_PROFILE").then_some(LaunchOrigin::Default);
    let rows = report(&config, None, env, launch);
    let row = pick(&rows, "tools.profile");
    assert_eq!(row.source, EffectiveSource::Default);
    assert_eq!(row.origin, None);
}

#[test]
fn a_blank_tool_profile_variable_is_the_source_the_daemon_applied() {
    // `HORON_AGENT_TOOL_PROFILE` is assigned without a blank check, so an empty
    // value clears the profile rather than leaving the file's value in force.
    // Naming the file, or the default, would be wrong.
    let mut config = distinctive_config();
    config.tools.profile = String::new();
    let rows = report(
        &config,
        Some(&file_layer("[tools]\nprofile = \"full\"\n")),
        |name| (name == "HORON_AGENT_TOOL_PROFILE").then(String::new),
        no_launch,
    );
    let row = pick(&rows, "tools.profile");
    assert_eq!(row.source, EffectiveSource::Env);
    assert_eq!(row.origin.as_deref(), Some("HORON_AGENT_TOOL_PROFILE"));
    assert_eq!(row.value, "(unset)", "a cleared profile is said out loud");
}

#[test]
fn a_recorded_origin_replaces_the_one_before_it() {
    // The launcher may choose a default and then apply a flag; the public
    // effective_settings() report reflects the flag as the winning origin.
    let _guard = crate::storage::lock_test_env();
    let previous_home = std::env::var_os("HORON_HOME");
    let previous_profile = std::env::var_os("HORON_AGENT_TOOL_PROFILE");
    let dir = tempfile::TempDir::new().expect("tempdir");
    crate::env::set_var("HORON_HOME", dir.path());
    crate::env::set_var("HORON_AGENT_TOOL_PROFILE", "core");
    Config::invalidate_cache();

    record_launch_default("HORON_AGENT_TOOL_PROFILE");
    let first_rows = effective_settings();
    let first = pick(&first_rows, "tools.profile");
    assert_eq!(first.source, EffectiveSource::Default);
    assert_eq!(first.origin, None);

    record_launch_flag("HORON_AGENT_TOOL_PROFILE", "--tool-profile");
    let second_rows = effective_settings();
    let second = pick(&second_rows, "tools.profile");
    assert_eq!(second.value, "core");
    assert_eq!(second.source, EffectiveSource::Cli);
    assert_eq!(second.origin.as_deref(), Some("--tool-profile"));

    match previous_home {
        Some(home) => crate::env::set_var("HORON_HOME", home),
        None => crate::env::remove_var("HORON_HOME"),
    }
    match previous_profile {
        Some(profile) => crate::env::set_var("HORON_AGENT_TOOL_PROFILE", profile),
        None => crate::env::remove_var("HORON_AGENT_TOOL_PROFILE"),
    }
    Config::invalidate_cache();
}

#[test]
fn a_config_file_that_does_not_parse_supplies_no_layer() {
    let _guard = crate::storage::lock_test_env();
    let previous_home = std::env::var_os("HORON_HOME");
    let dir = tempfile::TempDir::new().expect("tempdir");
    crate::env::set_var("HORON_HOME", dir.path());
    Config::invalidate_cache();
    std::fs::write(dir.path().join("config.toml"), "[provider]\nmax_retries = \"many\"\n")
        .expect("write a malformed config");

    assert!(
        FileLayer::read().is_none(),
        "a file the daemon could not apply cannot be named as a source"
    );

    match previous_home {
        Some(home) => crate::env::set_var("HORON_HOME", home),
        None => crate::env::remove_var("HORON_HOME"),
    }
    Config::invalidate_cache();
}

#[test]
fn the_report_holds_only_allow_listed_keys() {
    let rows = report(&distinctive_config(), None, no_env, no_launch);
    let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
    let allowed: Vec<&str> = SPECS.iter().map(|spec| spec.key).collect();
    assert_eq!(keys, allowed, "the report is the allow-list, in its order");
    for forbidden in [
        "provider.api_key_env",
        "providers",
        "stream_rules",
        "hooks",
        "mcp",
        "desktop",
    ] {
        assert!(
            !keys.iter().any(|key| key.starts_with(forbidden)),
            "`{forbidden}` must never be reported"
        );
    }
}

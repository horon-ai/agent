//! `[tools] checkpoint`: the switch is on by default, parses when set, and is
//! documented in the generated default config.

use super::{Config, ToolConfig};

#[test]
fn checkpoint_switch_is_on_by_default() {
    assert!(
        ToolConfig::default().checkpoint,
        "the pair is part of the open default surface"
    );
    assert!(
        Config::default().tools.checkpoint,
        "the whole-config default agrees with the tool config"
    );
}

#[test]
fn checkpoint_switch_parses_when_set() {
    let parsed: Config = toml::from_str("[tools]\ncheckpoint = true\n").expect("the switch parses");
    assert!(parsed.tools.checkpoint);
    let off: Config =
        toml::from_str("[tools]\nprofile = \"core\"\ncheckpoint = false\n").expect("an explicit false parses");
    assert!(!off.tools.checkpoint);
    let allowed = off
        .tools
        .selection_with_ambient(&crate::config::AmbientConfig::default())
        .allowed_tools
        .expect("core is an allow-list");
    for name in crate::config::CHECKPOINT_TOOLS {
        assert!(!allowed.contains(*name), "the switch off removes {name}");
    }
}

#[test]
fn generated_default_config_documents_the_checkpoint_switch() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_HOME");
    let home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("HORON_HOME", home.path());

    let path = Config::create_default_config_file().expect("create default config file");
    let content = std::fs::read_to_string(path).expect("read default config file");

    match previous {
        Some(value) => crate::env::set_var("HORON_HOME", value),
        None => crate::env::remove_var("HORON_HOME"),
    }

    assert!(
        content.contains("checkpoint = true"),
        "the generated default config documents the switch as on"
    );
}

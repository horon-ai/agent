//! Config tests for the `[display]` section loaded from a real config file.
//!
//! Split out of `config_tests.rs` to keep that file under the test-size
//! ratchet. These tests cover the path a user actually takes: a hand-written
//! `~/.horon/config.toml` loaded through the same global cache the running
//! process uses.

use super::Config;

/// The exact `[display]` block from issue #689 must work through a real config
/// file.
///
/// The unit tests in `horon-agent-config-types` cover lenient enum parsing in
/// isolation. This covers the path the reporter actually took: hand-written
/// config loaded through the global cache. The original bug was invisible at the
/// field level precisely because it happened during whole-file parsing, so it
/// needs a file-level test.
#[test]
fn reported_display_config_survives_a_real_config_file_round_trip() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("HORON_HOME");
    let dir = tempfile::TempDir::new().expect("tempdir");
    crate::env::set_var("HORON_HOME", dir.path());
    Config::invalidate_cache();

    let path = Config::path().expect("config path");
    std::fs::write(
        &path,
        concat!(
            "[display]\n",
            "show_thinking = true\n",
            "reasoning_display = \"current\"\n",
        ),
    )
    .expect("write user config");
    Config::invalidate_cache();

    let loaded = crate::config::config();
    assert!(loaded.display.show_thinking, "show_thinking must apply");
    assert_eq!(
        loaded.display.reasoning_display(),
        horon_agent_config_types::ReasoningDisplayMode::Current
    );

    // A genuinely unknown value degrades only itself.
    std::fs::write(
        &path,
        "[display]\nemoji = false\nshow_thinking = true\nreasoning_display = \"nonsense\"\n",
    )
    .expect("write user config");
    Config::invalidate_cache();
    let loaded = crate::config::config();
    assert!(
        !loaded.display.emoji && loaded.display.show_thinking,
        "one unknown enum value must not discard the rest of the file"
    );
    assert!(
        !loaded.display.has_explicit_reasoning_display(),
        "an unparseable value means 'not configured'"
    );

    match prev_home {
        Some(prev) => crate::env::set_var("HORON_HOME", prev),
        None => crate::env::remove_var("HORON_HOME"),
    }
    Config::invalidate_cache();
}

/// Configs written for the terminal UI keep loading: the removed sections and
/// keys are ignored rather than fatal.
#[test]
fn legacy_tui_config_sections_load_without_error() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("HORON_HOME");
    let dir = tempfile::TempDir::new().expect("tempdir");
    crate::env::set_var("HORON_HOME", dir.path());
    Config::invalidate_cache();

    let path = Config::path().expect("config path");
    std::fs::write(
        &path,
        concat!(
            "[keybindings]\n",
            "scroll_up = \"ctrl+k\"\n",
            "auto_poke_toggle = \"ctrl+p\"\n",
            "\n",
            "[dictation]\n",
            "command = \"whisper\"\n",
            "mode = \"send\"\n",
            "\n",
            "[menubar]\n",
            "enabled = true\n",
            "\n",
            "[display]\n",
            "diff_mode = \"inline\"\n",
            "centered = true\n",
            "idle_animation = true\n",
            "emoji = false\n",
            "reasoning_display = \"current\"\n",
            "\n",
            "[features]\n",
            "mermaid = true\n",
        ),
    )
    .expect("write legacy config");

    let loaded = Config::load_strict().expect("a legacy config must still load");
    assert!(!loaded.display.emoji, "surviving settings still apply");
    assert_eq!(
        loaded.display.reasoning_display(),
        horon_agent_config_types::ReasoningDisplayMode::Current
    );

    match prev_home {
        Some(prev) => crate::env::set_var("HORON_HOME", prev),
        None => crate::env::remove_var("HORON_HOME"),
    }
    Config::invalidate_cache();
}

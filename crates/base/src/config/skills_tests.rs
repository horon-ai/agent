//! `[skills]`: the switches for skill collections horon-agent does not own.

use super::super::Config;

#[test]
fn skills_sources_default_off_and_parse_opt_in() {
    let defaults = Config::default();
    assert!(
        !defaults.skills.claude_user,
        "reading ~/.claude/skills must stay a decision the operator makes"
    );
    assert!(
        !defaults.skills.codex_user,
        "reading ~/.codex/skills must stay a decision the operator makes"
    );

    let cfg: Config =
        toml::from_str("[skills]\nclaude_user = true\ncodex_user = true\n").expect("the skills section parses");
    assert!(cfg.skills.claude_user);
    assert!(cfg.skills.codex_user);

    // The shipped template documents both switches without enabling them.
    let template = Config::default_config_file_contents();
    assert!(template.contains("[skills]"));
    assert!(template.contains("claude_user = false"));
    assert!(template.contains("codex_user = false"));
}

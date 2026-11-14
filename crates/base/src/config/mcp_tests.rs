//! `[mcp]`: the source switches and the roots opt-in.

use super::super::Config;

#[test]
fn mcp_sources_default_off_and_parse_opt_in() {
    let defaults = Config::default();
    assert!(
        !defaults.mcp.claude_user,
        "reading ~/.claude.json must stay a decision the operator makes"
    );
    assert!(
        !defaults.mcp.codex_user,
        "reading ~/.codex/config.toml must stay opt-in"
    );
    assert!(
        !defaults.mcp.expose_roots,
        "handing a server the working directory over roots/list must stay opt-in"
    );

    let cfg: Config = toml::from_str("[mcp]\nexpose_roots = true\n").expect("the mcp section parses");
    assert!(cfg.mcp.expose_roots);

    // The shipped template documents the switch without enabling it.
    let template = Config::default_config_file_contents();
    assert!(template.contains("[mcp]"), "the template must document [mcp]");
    assert!(
        template.contains("expose_roots = false"),
        "the template must document the roots opt-in as off"
    );
}

use super::*;

fn no_allowed() -> Option<HashSet<String>> {
    None
}

fn only(names: &[&str]) -> Option<HashSet<String>> {
    Some(names.iter().map(|name| (*name).to_string()).collect())
}

#[test]
fn read_off_shell_idioms_named_by_the_dedicated_tool() {
    for command in [
        "cat Cargo.toml",
        "head -n 5 src/lib.rs",
        "tail src/lib.rs",
        "less src/lib.rs",
    ] {
        let rule = match_rule(command, false, no_allowed().as_ref())
            .unwrap_or_else(|| panic!("{command:?} should be intercepted"));
        assert_eq!(rule.tool, "read", "{command:?}");
        assert_eq!(
            rule.message,
            "Use the `read` tool instead of cat/head/tail; it supports line selectors like path:10-40 and path:-50."
        );
    }
}

#[test]
fn search_idioms_name_agentgrep() {
    assert_eq!(
        match_rule("grep -rn foo .", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("agentgrep")
    );
    assert_eq!(
        match_rule("rg foo src", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("agentgrep")
    );
    assert_eq!(
        match_rule("find . -name '*.rs'", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("agentgrep")
    );
    assert_eq!(
        match_rule("fd --type f src", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("agentgrep")
    );
}

#[test]
fn in_place_edits_name_edit() {
    for command in [
        "sed -i 's/a/b/' src/lib.rs",
        "sed --in-place 's/a/b/' src/lib.rs",
        "perl -pi -e 's/a/b/' src/lib.rs",
        "awk -i inplace '{print}' src/lib.rs",
    ] {
        assert_eq!(
            match_rule(command, false, no_allowed().as_ref()).map(|rule| rule.tool),
            Some("edit"),
            "{command:?}"
        );
    }
}

#[test]
fn redirection_names_write() {
    assert_eq!(
        match_rule("echo hi > note.txt", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("write")
    );
    assert_eq!(
        match_rule("printf 'x' >> note.txt", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("write")
    );
    assert_eq!(
        match_rule("cat <<EOF > note.txt", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("write"),
        "a heredoc that writes a file is a write, not a read"
    );
}

#[test]
fn background_idioms_name_a_named_bash_service() {
    for command in ["nohup ./server", "sleep 1 &"] {
        assert_eq!(
            match_rule(command, false, no_allowed().as_ref()).map(|rule| rule.tool),
            Some("bash"),
            "{command:?}"
        );
    }
    assert!(
        match_rule("nohup ./server", true, no_allowed().as_ref()).is_none(),
        "a call that already carries a service name is not nudged"
    );
}

#[test]
fn a_piped_segment_is_not_intercepted() {
    assert!(
        match_rule("ls | cat", false, no_allowed().as_ref()).is_none(),
        "cat reading a pipe is not something read can replace"
    );
    assert!(
        match_rule("cat", false, no_allowed().as_ref()).is_none(),
        "bare cat reads stdin and has no file to open"
    );
}

#[test]
fn a_leading_env_assignment_does_not_hide_the_idiom() {
    assert_eq!(
        match_rule("FOO=1 cat Cargo.toml", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("read")
    );
    assert_eq!(
        match_rule("A=1 B=2 grep foo f", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("agentgrep")
    );
}

#[test]
fn a_chained_command_is_intercepted_on_its_own_segment() {
    assert_eq!(
        match_rule("cd repo && grep -n foo src/lib.rs", false, no_allowed().as_ref()).map(|rule| rule.tool),
        Some("agentgrep")
    );
}

#[test]
fn a_rule_never_names_an_unreachable_tool() {
    let allowed = only(&["read"]);
    assert_eq!(
        match_rule("cat Cargo.toml", false, allowed.as_ref()).map(|rule| rule.tool),
        Some("read")
    );
    assert!(
        match_rule("grep foo f", false, allowed.as_ref()).is_none(),
        "agentgrep is not reachable under this allow-list"
    );
    assert!(
        match_rule("nohup ./server", false, allowed.as_ref()).is_none(),
        "bash itself is not reachable under this allow-list"
    );

    let wildcard = only(&["*"]);
    assert_eq!(
        match_rule("grep foo f", false, wildcard.as_ref()).map(|rule| rule.tool),
        Some("agentgrep")
    );
}

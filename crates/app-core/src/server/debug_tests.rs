//! Debug socket namespace parsing.

use super::super::debug_help::parse_namespaced_command;

#[test]
fn bare_commands_use_the_server_namespace() {
    assert_eq!(parse_namespaced_command("state"), Ok("state"));
    assert_eq!(parse_namespaced_command("  state  "), Ok("state"));
}

#[test]
fn server_prefix_is_stripped() {
    assert_eq!(parse_namespaced_command("server:info"), Ok("info"));
    assert_eq!(parse_namespaced_command("server:"), Ok(""));
}

#[test]
fn server_side_namespaces_keep_their_prefix() {
    assert_eq!(parse_namespaced_command("clients:map"), Ok("clients:map"));
    assert_eq!(parse_namespaced_command("clients:mapping"), Ok("clients:mapping"));
    assert_eq!(
        parse_namespaced_command("queue_interrupt:next task"),
        Ok("queue_interrupt:next task")
    );
    assert_eq!(parse_namespaced_command("swarm:info:/tmp/p"), Ok("swarm:info:/tmp/p"));
}

#[test]
fn retired_client_and_tester_namespaces_are_rejected() {
    for command in ["client:foo", "tester:list", "client:mermaid:stats"] {
        let error = parse_namespaced_command(command).expect_err("retired namespace must fail");
        assert!(error.contains("Unknown debug namespace"), "{error}");
    }
}

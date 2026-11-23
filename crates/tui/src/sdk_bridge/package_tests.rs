//! What the bridge makes of a package command answer: the rows it lists, the
//! answer it hands the client, and the one failure it reads as an old harness.
//!
//! Nothing here talks to a daemon. Each test feeds one payload or one error to
//! the function that reads it, which is what the worker calls.

use horon_agent_sdk::api::{PackageCommandInfo, PackageCommandKind, PackageCommandList, PackageCommandOutcome};
use horon_agent_sdk::{Error, ErrorKind};

use super::package_commands::{listed, outcome, unsupported};
use crate::commands::PackageCommand;

/// One row of the harness's list.
fn info(name: &str, kind: PackageCommandKind, description: &str, hint: Option<&str>) -> PackageCommandInfo {
    let (package_id, command) = name
        .split_once(':')
        .expect("a typed name names a package and a command");
    PackageCommandInfo {
        name: name.to_string(),
        package_id: package_id.to_string(),
        command: command.to_string(),
        kind,
        description: description.to_string(),
        argument_hint: hint.map(str::to_string),
    }
}

#[test]
fn a_listing_becomes_the_names_the_resolver_keeps() {
    let list = PackageCommandList {
        generation: 7,
        commands: vec![
            info(
                "demo:hello",
                PackageCommandKind::Template,
                "  greet one person\nsecond line  ",
                Some(" <name> "),
            ),
            info("demo:lint", PackageCommandKind::Executable, "", None),
        ],
    };
    assert_eq!(
        listed(&list),
        vec![
            PackageCommand {
                name: "demo:hello".to_string(),
                package_id: "demo".to_string(),
                command: "hello".to_string(),
                argument_hint: Some("<name>".to_string()),
                description: "greet one person".to_string(),
            },
            PackageCommand {
                name: "demo:lint".to_string(),
                package_id: "demo".to_string(),
                command: "lint".to_string(),
                argument_hint: None,
                description: String::new(),
            },
        ]
    );
}

#[test]
fn a_row_that_names_no_command_is_skipped() {
    // A row with no typed name or no declared command is one the client could
    // list but never run, so it never reaches the resolver.
    let list = PackageCommandList {
        generation: 1,
        commands: vec![
            info("demo:hello", PackageCommandKind::Template, "kept", None),
            PackageCommandInfo {
                name: "   ".to_string(),
                package_id: "demo".to_string(),
                command: "nameless".to_string(),
                kind: PackageCommandKind::Template,
                description: "no typed name".to_string(),
                argument_hint: None,
            },
            PackageCommandInfo {
                name: "demo:blank".to_string(),
                package_id: "demo".to_string(),
                command: String::new(),
                kind: PackageCommandKind::Executable,
                description: "no command".to_string(),
                argument_hint: None,
            },
        ],
    };
    let names: Vec<String> = listed(&list).into_iter().map(|command| command.name).collect();
    assert_eq!(names, vec!["demo:hello".to_string()]);
}

#[test]
fn an_answer_carries_the_typed_line_and_which_kind_it_was() {
    let template = outcome(
        "/demo:hello Ada",
        PackageCommandOutcome {
            name: "demo:hello".to_string(),
            package_id: "demo".to_string(),
            command: "hello".to_string(),
            kind: PackageCommandKind::Template,
            text: Some("Hello Ada".to_string()),
            output: None,
            data: None,
        },
    );
    assert_eq!(template.display, "/demo:hello Ada");
    assert_eq!(template.text.as_deref(), Some("Hello Ada"));
    assert_eq!(template.output, None);

    let executable = outcome(
        "/demo:lint",
        PackageCommandOutcome {
            name: "demo:lint".to_string(),
            package_id: "demo".to_string(),
            command: "lint".to_string(),
            kind: PackageCommandKind::Executable,
            text: None,
            output: Some("3 warnings".to_string()),
            data: Some(serde_json::json!({"warnings": 3})),
        },
    );
    assert_eq!(executable.text, None);
    assert_eq!(executable.output.as_deref(), Some("3 warnings"));
}

#[test]
fn only_a_missing_capability_is_read_as_an_old_harness() {
    // The SDK refuses both calls before sending when `package_commands_v1` is
    // absent, so this is the one error that means "no package commands here".
    assert!(unsupported(&Error::new(
        ErrorKind::Unsupported,
        "list_package_commands requires the `package_commands_v1` capability"
    )));
    assert!(!unsupported(&Error::new(
        ErrorKind::Harness(horon_agent_sdk::api::ErrorCode::UnknownRequest),
        "no such command"
    )));
    assert!(!unsupported(&Error::new(
        ErrorKind::Disconnected,
        "the connection closed"
    )));
}

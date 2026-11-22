//! `cloud doctor`: a read-only preflight of a cloud host.
//!
//! The only remote command is `horon-agent cloud receive --help`, which prints
//! usage and returns. No session data is sent and nothing is created remotely.

use anyhow::Result;
use serde::Serialize;

use super::cloud_move::Target;

#[derive(Debug, PartialEq, Eq)]
enum Health {
    Ready,
    Unreachable(String),
    Incompatible(String),
}

#[derive(Serialize)]
struct Report<'a> {
    host: &'a str,
    status: &'static str,
    message: String,
}

pub(crate) fn run(target: &Target, json: bool) -> Result<()> {
    let host = target.resolve_host()?;
    let health = check(target, &host);
    let (status, message) = match &health {
        Health::Ready => ("ready", format!("`{host}` is reachable and supports `cloud move`.")),
        Health::Unreachable(reason) => (
            "unreachable",
            format!("cannot reach `{host}` over ssh ({reason}). Check `ssh {host}` works without a prompt."),
        ),
        Health::Incompatible(reason) => (
            "incompatible",
            format!("`{host}` is reachable but horon-agent does not support `cloud move` ({reason}). Update it there."),
        ),
    };
    if json {
        let report = Report {
            host: &host,
            status,
            message,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{status}: {message}");
    }
    if health != Health::Ready {
        std::process::exit(1);
    }
    Ok(())
}

fn check(target: &Target, host: &str) -> Health {
    match target.run_remote(host, &["cloud", "receive", "--help"], None) {
        Ok(_) => Health::Ready,
        Err(error) => classify(&format!("{error:#}")),
    }
}

fn classify(error: &str) -> Health {
    let reason = error
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("remote `horon-agent"))
        .unwrap_or("no detail")
        .chars()
        .take(160)
        .collect::<String>();
    // ssh itself exits 255 when it cannot connect or authenticate.
    if error.contains("failed to reach") || error.contains("255)") {
        Health::Unreachable(reason)
    } else {
        Health::Incompatible(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fixture(script: &str) -> (tempfile::TempDir, Target) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let target = Target {
            host: Some("fixture".to_owned()),
            // The transport runs the remote command through `sh -c`; the remote
            // binary is a local script standing in for the cloud host.
            remote_binary: Some(path.display().to_string()),
            transport: Some("sh -c".to_owned()),
        };
        (dir, target)
    }

    #[cfg(unix)]
    #[test]
    fn host_states_map_to_one_actionable_result() {
        let (_ready, target) = fixture("exit 0");
        assert_eq!(check(&target, "fixture"), Health::Ready);

        let (_old, target) = fixture("echo \"error: unrecognized subcommand 'cloud'\" >&2; exit 2");
        assert!(matches!(check(&target, "fixture"), Health::Incompatible(_)));

        let (_down, target) = fixture("echo 'Connection refused' >&2; exit 255");
        assert!(matches!(check(&target, "fixture"), Health::Unreachable(_)));
    }

    #[test]
    fn a_missing_transport_program_is_unreachable() {
        let target = Target {
            host: Some("fixture".to_owned()),
            remote_binary: None,
            transport: Some("/nonexistent/horon-transport".to_owned()),
        };
        assert!(matches!(check(&target, "fixture"), Health::Unreachable(_)));
    }
}

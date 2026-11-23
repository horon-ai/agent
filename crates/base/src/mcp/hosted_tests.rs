//! The hosted decision matrix, and the two process choke points it guards.
//!
//! The matrix drives [`decide`] with explicit policies, so no test depends on the
//! process environment. The two choke points start real children, which ends the
//! process's hosting policy once, so each runs in a child of this test binary
//! (`--exact --ignored`): that child owns its environment and its logger.

use super::*;
use crate::hosting::{HostingMode, HostingPolicy, HostingSection, TenantId};

fn tenant() -> TenantId {
    TenantId::parse("acme").expect("valid tenant id")
}

fn hosting(mode: HostingMode, section: HostingSection) -> HostingPolicy {
    let tenant_id = mode.is_hosted().then(tenant);
    HostingPolicy::new(mode, tenant_id, &section).expect("policy")
}

fn no_hosting() -> HostingPolicy {
    HostingPolicy::default()
}

fn per_user_container() -> HostingPolicy {
    hosting(HostingMode::PerUserContainer, HostingSection::default())
}

fn narrowed_container() -> HostingPolicy {
    hosting(
        HostingMode::PerUserContainer,
        HostingSection {
            allow_shared_children: Some(false),
            allowed_mcp_servers: None,
            mode: None,
        },
    )
}

fn listing_container(servers: &[&str]) -> HostingPolicy {
    let servers = servers.iter().map(|server| server.to_string()).collect();
    hosting(
        HostingMode::PerUserContainer,
        HostingSection {
            allow_shared_children: None,
            allowed_mcp_servers: Some(servers),
            mode: None,
        },
    )
}

fn shared_worker() -> HostingPolicy {
    hosting(HostingMode::SharedWorker, HostingSection::default())
}

/// One request per row of the matrix.
fn operations() -> Vec<HostedRequest<'static>> {
    vec![
        HostedRequest::server(HostedOperation::Stdio, "editor"),
        HostedRequest::server(HostedOperation::SessionStdio, "editor"),
        HostedRequest::server(HostedOperation::PooledStdio, "editor"),
        HostedRequest::server(HostedOperation::Remote, "web"),
        HostedRequest::package("weather"),
    ]
}

fn reason(policy: &HostingPolicy, request: &HostedRequest<'_>) -> RefusalReason {
    decide(policy, request)
        .expect_err("the policy refuses this operation")
        .reason()
}

#[test]
fn no_hosting_allows_every_operation() {
    for request in operations() {
        assert!(
            decide(&no_hosting(), &request).is_ok(),
            "{} must be unchanged without hosting",
            request.operation.as_str()
        );
    }
}

#[test]
fn a_per_user_container_allows_every_operation() {
    for request in operations() {
        assert!(
            decide(&per_user_container(), &request).is_ok(),
            "a per-user container runs its own {}",
            request.operation.as_str()
        );
    }
}

#[test]
fn a_narrowed_container_refuses_child_processes_only() {
    let policy = narrowed_container();
    for operation in [
        HostedOperation::Stdio,
        HostedOperation::SessionStdio,
        HostedOperation::PooledStdio,
        HostedOperation::PackageChild,
    ] {
        let request = if operation == HostedOperation::PackageChild {
            HostedRequest::package("weather")
        } else {
            HostedRequest::server(operation, "editor")
        };
        assert_eq!(
            reason(&policy, &request),
            RefusalReason::SharedChildrenDisabled,
            "{}",
            operation.as_str()
        );
    }
    assert!(
        decide(&policy, &HostedRequest::server(HostedOperation::Remote, "web")).is_ok(),
        "a remote connection starts no child, so narrowing children leaves it reachable"
    );
}

#[test]
fn a_shared_worker_refuses_every_operation() {
    for request in operations() {
        assert_eq!(
            reason(&shared_worker(), &request),
            RefusalReason::SharedWorker,
            "{}",
            request.operation.as_str()
        );
    }
}

#[test]
fn an_allowed_list_permits_only_the_names_it_lists() {
    let policy = listing_container(&["editor"]);
    assert!(
        decide(&policy, &HostedRequest::server(HostedOperation::Stdio, "editor")).is_ok(),
        "a listed server is reachable"
    );
    for operation in [
        HostedOperation::Stdio,
        HostedOperation::SessionStdio,
        HostedOperation::PooledStdio,
        HostedOperation::Remote,
    ] {
        assert_eq!(
            reason(&policy, &HostedRequest::server(operation, "other")),
            RefusalReason::NotAllowed,
            "{}",
            operation.as_str()
        );
    }
}

#[test]
fn an_empty_allowed_list_refuses_every_named_server() {
    let policy = listing_container(&[]);
    for operation in [
        HostedOperation::Stdio,
        HostedOperation::SessionStdio,
        HostedOperation::PooledStdio,
        HostedOperation::Remote,
    ] {
        assert_eq!(
            reason(&policy, &HostedRequest::server(operation, "editor")),
            RefusalReason::NotAllowed,
            "{}",
            operation.as_str()
        );
    }
}

#[test]
fn an_allowed_list_never_narrows_a_process_without_hosting() {
    // The mode matrix says no hosting is unchanged; a key left over on a reused
    // volume must not take the operator's own servers away.
    let policy = hosting(
        HostingMode::None,
        HostingSection {
            allow_shared_children: None,
            allowed_mcp_servers: Some(vec!["editor".to_string()]),
            mode: None,
        },
    );
    for request in operations() {
        assert!(
            decide(&policy, &request).is_ok(),
            "{} must be unchanged without hosting",
            request.operation.as_str()
        );
    }
}

#[test]
fn a_package_child_is_not_narrowed_by_the_server_list() {
    // The list names MCP servers; a package id is not one, so a package's
    // extension children keep following the mode alone.
    let policy = listing_container(&["editor"]);
    assert!(decide(&policy, &HostedRequest::package("weather")).is_ok());
}

#[test]
fn an_allowed_list_never_widens_a_shared_worker() {
    let policy = hosting(
        HostingMode::SharedWorker,
        HostingSection {
            allow_shared_children: Some(true),
            allowed_mcp_servers: Some(vec!["editor".to_string()]),
            mode: None,
        },
    );
    assert_eq!(
        reason(&policy, &HostedRequest::server(HostedOperation::Stdio, "editor")),
        RefusalReason::SharedWorker
    );
}

#[test]
fn a_refusal_names_the_policy_the_operation_and_the_subject() {
    let cases = [
        (
            shared_worker(),
            HostedRequest::server(HostedOperation::SessionStdio, "editor"),
            "hosting policy 'shared-worker' refuses the session MCP stdio server 'editor': a shared worker \
             serves several customers from one process and never starts a child process for one of them",
        ),
        (
            shared_worker(),
            HostedRequest::server(HostedOperation::Remote, "web"),
            "hosting policy 'shared-worker' refuses the remote MCP server 'web': a shared worker serves several \
             customers from one process, so its OAuth token store and ${VAR} environment cannot be proven per user",
        ),
        (
            narrowed_container(),
            HostedRequest::package("weather"),
            "hosting policy 'per-user-container' refuses the package child 'weather': [hosting] \
             allow_shared_children = false closes child processes in this container",
        ),
        (
            listing_container(&["editor"]),
            HostedRequest::server(HostedOperation::Stdio, "other"),
            "hosting policy 'per-user-container' refuses the MCP stdio server 'other': it is not listed in \
             [hosting] allowed_mcp_servers",
        ),
    ];
    for (policy, request, expected) in cases {
        let refusal = decide(&policy, &request).expect_err("refused");
        assert_eq!(refusal.to_string(), expected);
        assert_eq!(refusal.subject(), request.subject());
        assert_eq!(refusal.operation(), request.operation);
    }
}

#[test]
fn find_reaches_a_refusal_through_wrapping_context() {
    let refusal = decide(
        &shared_worker(),
        &HostedRequest::server(HostedOperation::Stdio, "editor"),
    )
    .expect_err("refused");
    let error = anyhow::Error::new(refusal).context("start the MCP server");
    let found = find(&error).expect("the refusal is in the error chain");
    assert_eq!(found.operation(), HostedOperation::Stdio);
    assert_eq!(found.reason(), RefusalReason::SharedWorker);
    assert!(
        find(&anyhow::anyhow!("an ordinary failure")).is_none(),
        "a failure without a refusal reports none"
    );
}

// ---------------------------------------------------------------------------
// Real children, one fresh process per hosting mode
// ---------------------------------------------------------------------------

/// Whether a child can drive the `/bin/sh` stub these tests use.
fn shell_supported(test: &str) -> bool {
    let supported = std::env::consts::OS != "windows";
    if !supported {
        eprintln!("SKIP {test}: the /bin/sh stub needs a Unix shell");
    }
    supported
}

/// Run one `#[ignore]`d test of this binary as its own process.
fn run_child(name: &str) {
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary path"))
        .args(["--exact", name, "--ignored", "--nocapture"])
        .output()
        .expect("spawn the isolated child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the isolated child must pass, got {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains("1 passed"),
        "the child must run exactly one test; a name mismatch reports zero tests\n{stdout}"
    );
}

/// A stdio MCP server that records its own start and answers a handshake.
fn stdio_config(marker: &std::path::Path) -> crate::mcp::McpServerConfig {
    let script = format!(
        r#"
printf started > '{marker}'
while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2024-11-05","capabilities":{{}},"serverInfo":{{"name":"fake","version":"0"}}}}}}\n'
      ;;
    *'"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[]}}}}\n'
      ;;
  esac
done
"#,
        marker = marker.display()
    );
    crate::mcp::McpServerConfig {
        command: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script],
        env: std::collections::HashMap::new(),
        shared: false,
        transport: None,
        url: None,
        headers: std::collections::HashMap::new(),
        enabled: None,
        disabled: None,
        timeout_secs: None,
    }
}

fn enter_hosting(home: &std::path::Path, mode: &str) {
    crate::env::set_var("HORON_HOME", home);
    crate::env::set_var("HORON_AGENT_HOSTING_MODE", mode);
    crate::env::set_var("HORON_AGENT_TENANT_ID", "acme");
}

#[test]
fn a_shared_worker_refuses_a_real_stdio_child() {
    if !shell_supported("a_shared_worker_refuses_a_real_stdio_child") {
        return;
    }
    run_child("mcp::hosted::tests::a_shared_worker_refuses_a_real_stdio_child_child");
}

#[tokio::test]
#[ignore = "spawned by a_shared_worker_refuses_a_real_stdio_child so the process policy and the logger are fresh"]
async fn a_shared_worker_refuses_a_real_stdio_child_child() {
    let home = tempfile::tempdir().expect("isolated home");
    enter_hosting(home.path(), "shared-worker");
    crate::logging::init();
    let log_path = crate::logging::log_path().expect("the sandboxed home resolves a log path");
    let marker = home.path().join("started");
    let config = stdio_config(&marker);

    let error = match crate::mcp::McpClient::connect("editor".to_string(), &config).await {
        Ok(_) => panic!("a shared worker must refuse the child"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "hosting policy 'shared-worker' refuses the MCP stdio server 'editor': a shared worker serves several \
         customers from one process and never starts a child process for one of them"
    );
    assert!(!marker.exists(), "the refused child was never started");

    let log = std::fs::read_to_string(&log_path).expect("the audit line is written under the home");
    let line = log
        .lines()
        .find(|line| line.contains("EVENT event=HOSTED_POLICY"))
        .unwrap_or_else(|| panic!("no HOSTED_POLICY line in {log}"));
    for field in [
        "operation=mcp-stdio",
        "policy=shared-worker",
        "reason=shared-worker",
        "subject=editor",
        "tenant=acme",
    ] {
        assert!(line.contains(field), "{field} missing from: {line}");
    }
}

#[test]
fn a_per_user_container_connects_a_real_stdio_child() {
    if !shell_supported("a_per_user_container_connects_a_real_stdio_child") {
        return;
    }
    run_child("mcp::hosted::tests::a_per_user_container_connects_a_real_stdio_child_child");
}

#[tokio::test]
#[ignore = "spawned by a_per_user_container_connects_a_real_stdio_child so the process policy and the logger are fresh"]
async fn a_per_user_container_connects_a_real_stdio_child_child() {
    let home = tempfile::tempdir().expect("isolated home");
    enter_hosting(home.path(), "per-user-container");
    let marker = home.path().join("started");
    let config = stdio_config(&marker);

    let mut client = crate::mcp::McpClient::connect("editor".to_string(), &config)
        .await
        .expect("a per-user container keeps running its own MCP servers");
    assert_eq!(client.tools().len(), 0, "the stub advertises no tools");
    assert!(marker.exists(), "the child ran");
    client.shutdown().await;
}

//! Phase 7 bash-policy behaviours, proven through the public dispatch path.
//!
//! Each case drives `Registry::execute`, so the approval gate, the interceptor,
//! the shared-checkout guard and the background manager run where they actually
//! run. `HORON_HOME` and `HORON_AGENT_RUNTIME_DIR` point at per-case temp
//! directories; the cases hold one mutex because both are process-global.

use async_trait::async_trait;
use horon_agent_app_core::message::{Message, ToolDefinition};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::tool::{Registry, ToolContext, ToolExecutionMode};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

static TEST_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("the stub provider never completes a request"))
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

struct IsolatedEnv {
    home: tempfile::TempDir,
    _runtime: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
    previous_runtime: Option<std::ffi::OsString>,
}

impl IsolatedEnv {
    fn new(label: &str, tools_table: &str) -> Self {
        let home = tempfile::Builder::new()
            .prefix(&format!("horon-bash-policy-{label}-home-"))
            .tempdir()
            .expect("a private home");
        let runtime = tempfile::Builder::new()
            .prefix(&format!("horon-bash-policy-{label}-runtime-"))
            .tempdir()
            .expect("a private runtime directory");
        let previous_home = std::env::var_os("HORON_HOME");
        let previous_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
        horon_agent_app_core::env::set_var("HORON_HOME", home.path());
        horon_agent_app_core::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        let config = if tools_table.is_empty() {
            String::new()
        } else {
            format!("[tools]\n{tools_table}")
        };
        std::fs::write(home.path().join("config.toml"), config).expect("the config is written");
        horon_agent_app_core::config::invalidate_config_cache();
        Self {
            home,
            _runtime: runtime,
            previous_home,
            previous_runtime,
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => horon_agent_app_core::env::set_var("HORON_HOME", value),
            None => horon_agent_app_core::env::remove_var("HORON_HOME"),
        }
        match self.previous_runtime.take() {
            Some(value) => horon_agent_app_core::env::set_var("HORON_AGENT_RUNTIME_DIR", value),
            None => horon_agent_app_core::env::remove_var("HORON_AGENT_RUNTIME_DIR"),
        }
        horon_agent_app_core::config::invalidate_config_cache();
    }
}

async fn registry() -> Registry {
    Registry::new(Arc::new(StubProvider)).await
}

fn context(session_id: &str, working_dir: &Path) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: format!("{session_id}-message"),
        tool_call_id: format!("{session_id}-call"),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

async fn run_bash(registry: &Registry, session: &str, cwd: &Path, input: serde_json::Value) -> anyhow::Result<String> {
    let output = registry.execute("bash", input, context(session, cwd)).await?;
    Ok(output.output)
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("the repository directory");
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["config", "user.email", "t@example.test"]);
    git(&repo, &["config", "user.name", "T"]);
    std::fs::write(repo.join("tracked.txt"), "one\n").expect("a tracked file");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

#[tokio::test]
async fn interceptor_block_refuses_cat_and_names_read() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("interceptor-block", "bash_interceptor = \"block\"\n");
    let registry = registry().await;
    let error = registry
        .execute(
            "bash",
            json!({"command": "cat Cargo.toml"}),
            context("policy-interceptor-block", env.home()),
        )
        .await
        .expect_err("a blocked idiom must not run");
    let message = error.to_string();
    assert!(
        message.contains(
            "Blocked: Use the `read` tool instead of cat/head/tail; it supports line selectors like path:10-40 and path:-50."
        ),
        "the block must name the read tool: {message}"
    );
    assert!(
        message.contains("Original command: cat Cargo.toml"),
        "the refusal must quote the command: {message}"
    );
}

#[tokio::test]
async fn interceptor_nudge_runs_the_command_and_prefixes_the_note() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("interceptor-nudge", "bash_interceptor = \"nudge\"\n");
    let registry = registry().await;
    let file = env.home().join("note.txt");
    std::fs::write(&file, "hello-nudge\n").expect("the file to read");
    let output = run_bash(
        &registry,
        "policy-interceptor-nudge",
        env.home(),
        json!({"command": format!("cat {}", file.display())}),
    )
    .await
    .expect("a nudged command still runs");
    assert!(
        output.starts_with("[Note: Use the `read` tool instead of cat/head/tail"),
        "the note must be the first line: {output}"
    );
    assert!(output.contains("hello-nudge"), "the command must still run: {output}");
}

#[tokio::test]
async fn a_heredoc_redirection_is_the_write_rule_not_the_read_rule() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("interceptor-heredoc", "bash_interceptor = \"block\"\n");
    let registry = registry().await;
    let error = registry
        .execute(
            "bash",
            json!({"command": "cat <<EOF > out.txt\nhi\nEOF"}),
            context("policy-interceptor-heredoc", env.home()),
        )
        .await
        .expect_err("a heredoc that writes a file must be blocked");
    assert!(
        error
            .to_string()
            .contains("Blocked: Use `write` to create or replace files instead of shell redirection."),
        "{error}"
    );
}

#[tokio::test]
async fn a_piped_cat_is_not_blocked_by_the_read_rule() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("interceptor-pipe", "bash_interceptor = \"block\"\n");
    let registry = registry().await;
    let output = run_bash(
        &registry,
        "policy-interceptor-pipe",
        env.home(),
        json!({"command": "printf hi | cat"}),
    )
    .await
    .expect("a piped cat is a filter, not a file reader");
    assert_eq!(output.trim(), "hi");
}

#[tokio::test]
async fn a_deny_bash_pattern_blocks_the_command() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new(
        "bash-pattern",
        "bash_patterns = [{ pattern = \"git push *\", approval = \"deny\" }]\n",
    );
    let registry = registry().await;
    let error = registry
        .execute(
            "bash",
            json!({"command": "git push origin main"}),
            context("policy-bash-pattern", env.home()),
        )
        .await
        .expect_err("a deny pattern must refuse the call");
    assert_eq!(error.to_string(), "Blocked by bash pattern 'git push *' (deny).");
}

#[tokio::test]
async fn a_named_service_reports_readiness_and_is_addressable_by_name() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("service", "");
    let registry = registry().await;
    let session = "policy-service";
    let output = run_bash(
        &registry,
        session,
        env.home(),
        json!({
            "command": "while true; do echo service-ready-line; sleep 1; done",
            "name": "ticker",
            "ready": {"log": "service-ready-line", "timeout_secs": 10.0},
            "timeout": 1000
        }),
    )
    .await
    .expect("a named service starts");

    let first_line = output.lines().next().unwrap_or_default();
    assert!(
        first_line.starts_with("ticker: running pid=") && first_line.ends_with(" ready"),
        "the first line must report the service state: {first_line:?}"
    );
    assert!(
        output.contains("Ignored timeout: services run until cancelled; readiness follows ready.timeout_secs."),
        "a service timeout must be reported as ignored: {output}"
    );

    let status = registry
        .execute(
            "bg",
            json!({"action": "status", "task_id": "ticker"}),
            context(session, env.home()),
        )
        .await
        .expect("bg resolves the service name");
    assert!(
        status.output.contains("running"),
        "bg must find the task by its service name: {}",
        status.output
    );

    registry
        .execute(
            "bg",
            json!({"action": "cancel", "task_id": "ticker"}),
            context(session, env.home()),
        )
        .await
        .expect("cancelling by service name stops the task");
}

#[tokio::test]
async fn a_duplicate_service_name_is_refused() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("service-dup", "");
    let registry = registry().await;
    let session = "policy-service-dup";
    run_bash(
        &registry,
        session,
        env.home(),
        json!({"command": "sleep 30", "name": "worker"}),
    )
    .await
    .expect("the first service starts");

    let error = registry
        .execute(
            "bash",
            json!({"command": "sleep 30", "name": "worker"}),
            context(session, env.home()),
        )
        .await
        .expect_err("a second service with the same name must be refused");
    assert!(
        error
            .to_string()
            .starts_with("Service 'worker' is already running as task "),
        "{error}"
    );
    assert!(
        error.to_string().contains("cancel it first with bg action=cancel."),
        "{error}"
    );

    registry
        .execute(
            "bg",
            json!({"action": "cancel", "task_id": "worker"}),
            context(session, env.home()),
        )
        .await
        .expect("cleanup cancels the service");
}

#[tokio::test]
async fn ready_without_a_name_is_ignored_with_a_note() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("ready-no-name", "");
    let registry = registry().await;
    let output = run_bash(
        &registry,
        "policy-ready-no-name",
        env.home(),
        json!({"command": "echo ready-alone", "ready": {"log": "ready-alone"}}),
    )
    .await
    .expect("the command still runs");
    assert!(output.contains("ready-alone"));
    assert!(
        output.contains("Ignored ready: it only applies together with name."),
        "{output}"
    );
}

#[tokio::test]
async fn an_invalid_service_name_is_refused_before_spawning() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("service-name", "");
    let registry = registry().await;
    let error = registry
        .execute(
            "bash",
            json!({"command": "echo hi", "name": "bad name"}),
            context("policy-service-name", env.home()),
        )
        .await
        .expect_err("a name with a space must be refused");
    assert_eq!(
        error.to_string(),
        "Service name must be 1-48 letters, numbers, dots, underscores, or hyphens"
    );
}

#[tokio::test]
async fn git_guard_refuses_a_stash_in_the_shared_checkout_and_allows_a_linked_worktree() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("git-guard", "git_guard = true\n");
    let repo = init_repo(env.home());
    let registry = registry().await;

    let error = registry
        .execute(
            "bash",
            json!({"command": "git stash"}),
            context("policy-git-guard", &repo),
        )
        .await
        .expect_err("a stash in the shared main checkout must be refused");
    assert!(
        error
            .to_string()
            .starts_with("git stash is blocked in the shared main checkout:"),
        "{error}"
    );

    let linked = env.home().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf-8 worktree path"),
            "-b",
            "linked-branch",
        ],
    );
    let output = run_bash(&registry, "policy-git-guard", &linked, json!({"command": "git stash"}))
        .await
        .expect("a linked worktree owns its own refs, so the stash is allowed");
    assert!(
        !output.contains("blocked in the shared main checkout"),
        "a linked worktree must not be refused: {output}"
    );
    // Prove the command actually ran: stash the change, then confirm the
    // worktree is clean. The message text is locale-dependent, the state is not.
    std::fs::write(linked.join("tracked.txt"), "two\n").expect("a local change");
    run_bash(&registry, "policy-git-guard", &linked, json!({"command": "git stash"}))
        .await
        .expect("the stash runs in the linked worktree");
    let status = Command::new("git")
        .arg("-C")
        .arg(&linked)
        .args(["status", "--porcelain"])
        .output()
        .expect("git status runs");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "the stash must have removed the local change"
    );
}

#[tokio::test]
async fn a_named_service_can_wait_for_a_tcp_port() {
    if Command::new("python3").arg("--version").output().is_err() {
        return;
    }
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("service-port", "");
    let registry = registry().await;
    let session = "policy-service-port";
    // Ask the OS for a free port, then hand it to the server. The tiny race
    // between closing the probe socket and the server binding is acceptable:
    // the readiness wait allows ten seconds for the bind to succeed.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().expect("the bound address").port();
    drop(listener);

    let output = run_bash(
        &registry,
        session,
        env.home(),
        json!({
            "command": format!("python3 -m http.server {port} --bind 127.0.0.1"),
            "name": "web",
            "ready": {"port": port, "timeout_secs": 10.0}
        }),
    )
    .await
    .expect("the web service starts");

    let first_line = output.lines().next().unwrap_or_default();
    assert!(
        first_line.starts_with("web: running pid=") && first_line.ends_with(" ready"),
        "the port check must report readiness: {first_line:?}"
    );

    registry
        .execute(
            "bg",
            json!({"action": "cancel", "task_id": "web"}),
            context(session, env.home()),
        )
        .await
        .expect("cleanup cancels the web service");
}

#[tokio::test]
async fn an_invalid_readiness_regex_is_refused() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("ready-regex", "");
    let registry = registry().await;
    let error = registry
        .execute(
            "bash",
            json!({"command": "echo hi", "name": "svc", "ready": {"log": "("}}),
            context("policy-ready-regex", env.home()),
        )
        .await
        .expect_err("an uncompilable readiness pattern must be refused");
    assert!(error.to_string().starts_with("Invalid readiness regex: "), "{error}");
}

#[tokio::test]
async fn a_ready_spec_without_a_check_is_refused() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("ready-empty", "");
    let registry = registry().await;
    let error = registry
        .execute(
            "bash",
            json!({"command": "echo hi", "name": "svc", "ready": {}}),
            context("policy-ready-empty", env.home()),
        )
        .await
        .expect_err("a readiness spec with nothing to wait on must be refused");
    assert_eq!(error.to_string(), "ready needs a log pattern or a port");
}

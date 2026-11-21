use super::{NAME, forget_session, release_idle_kernel};
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::{Registry, ToolContext, ToolExecutionMode, ToolOutput, ToolVerdict};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

struct OfflineProvider;

#[async_trait]
impl Provider for OfflineProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("unexpected provider request in eval test")
    }

    fn name(&self) -> &str {
        "offline"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

struct Home {
    dir: tempfile::TempDir,
    original: Option<OsString>,
    cwd: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temporary Horon home");
        let cwd = dir.path().join("workspace");
        std::fs::create_dir(&cwd).expect("create test workspace");
        let original = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::config::invalidate_config_cache();
        Self { dir, original, cwd }
    }

    fn config(&self, tools: &str) {
        std::fs::write(self.dir.path().join("config.toml"), format!("[tools]\n{tools}\n")).expect("write test config");
        crate::config::invalidate_config_cache();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(value) = self.original.take() {
            crate::env::set_var("HORON_HOME", value);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
        crate::config::invalidate_config_cache();
    }
}

struct TestSession {
    cwd: PathBuf,
    id: &'static str,
}

impl TestSession {
    fn new(id: &'static str, cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            id,
        }
    }

    fn context(&self, call: &str) -> ToolContext {
        ToolContext {
            session_id: self.id.to_owned(),
            message_id: "eval-test".to_owned(),
            tool_call_id: call.to_owned(),
            working_dir: Some(self.cwd.clone()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    async fn request(&self, registry: &Registry, input: Value) -> String {
        let refusal = registry
            .execute(NAME, input, self.context("request"))
            .await
            .expect_err("eval requires explicit approval")
            .to_string();
        assert!(refusal.contains("needs approval before it runs"), "{refusal}");
        refusal
            .split_once("Request ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .expect("queued request ID in refusal")
            .to_owned()
    }

    async fn approved(&self, registry: &Registry, input: Value) -> ToolOutput {
        let request = self.request(registry, input.clone()).await;
        crate::safety::record_permission_via_file(&request, true, "api_client", None).expect("approve this cell");
        registry
            .execute(NAME, input, self.context("approved"))
            .await
            .expect("call approved eval")
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        crate::plan_mode::clear_state(self.id);
        forget_session(self.id);
    }
}
fn isolated(test: impl AsyncFnOnce(&Home)) {
    let _env = crate::storage::lock_test_env();
    let home = Home::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create Tokio runtime");
    runtime.block_on(test(&home));
}

#[test]
fn eval_opt_in_follows_profile_and_disabled_tools() {
    isolated(async |home| {
        for (tools, available) in [
            ("profile = 'full'", false),
            ("profile = 'full'\neval = true", true),
            ("profile = 'core'\neval = true", false),
            ("profile = 'acp'\neval = true", false),
            ("profile = 'none'\neval = true", false),
            ("eval = true\nenabled = ['eval']", true),
            ("profile = 'full'\neval = true\ndisabled = ['eval']", false),
        ] {
            home.config(tools);
            let registry = Registry::new(Arc::new(OfflineProvider)).await;
            let selection = crate::config::config().tools.selection();
            let definitions = registry
                .definitions_filtered(selection.allowed_tools.as_ref(), &selection.disabled_tools)
                .await;
            assert_eq!(definitions.iter().any(|tool| tool.name == NAME), available, "{tools}");
        }
        home.config("profile = 'full'\neval = true");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-restricted-session", &home.cwd);
        let _policy =
            crate::tool::register_session_tool_policy(session.id, Some(["read".into()].into()), Default::default());
        let denied = registry
            .execute(NAME, json!({"code": "print(42)"}), session.context("restricted"))
            .await;
        assert!(
            denied
                .expect_err("session allow-list excludes eval")
                .to_string()
                .contains("not allowed")
        );
    });
}

#[test]
fn eval_requires_explicit_approval_even_in_permissive_modes() {
    isolated(async |home| {
        for (mode, id) in [("yolo", "eval-yolo"), ("off", "eval-off")] {
            home.config(&format!("profile = 'full'\neval = true\napproval_mode = '{mode}'"));
            let registry = Registry::new(Arc::new(OfflineProvider)).await;
            let session = TestSession::new(id, &home.cwd);
            let input = json!({"code": "from pathlib import Path\nPath('side-effect').write_text('ran')"});
            let request = session.request(&registry, input.clone()).await;
            assert!(!home.cwd.join("side-effect").exists());
            crate::safety::record_permission_via_file(&request, false, "api_client", None).expect("deny the cell");
            let denied = registry
                .execute(NAME, input, session.context("retry"))
                .await
                .expect_err("denied cell stays denied");
            assert!(denied.to_string().contains("answered no"), "{denied:#}");
            assert!(!home.cwd.join("side-effect").exists());

            crate::plan_mode::set_state(id, crate::plan_mode::PlanState::Planning);
            let blocked = registry
                .execute(
                    NAME,
                    json!({"code": "print('must not run')"}),
                    session.context("plan-mode"),
                )
                .await
                .expect_err("eval has a plan-mode barrier");
            assert!(blocked.to_string().to_lowercase().contains("plan"), "{blocked:#}");
        }
    });
}

#[test]
fn eval_retains_state_until_reset_or_idle_reclamation() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let first = TestSession::new("eval-state-1", &home.cwd);
        let second = TestSession::new("eval-state-2", &home.cwd);
        first.approved(&registry, json!({"code": "counter = 17"})).await;
        let same = first.approved(&registry, json!({"code": "counter + 4"})).await;
        assert_eq!(same.output, "21\n");
        let other = second
            .approved(&registry, json!({"code": "'counter' in globals()"}))
            .await;
        assert_eq!(other.output, "False\n");
        let reset = first
            .approved(
                &registry,
                json!({"code": "print('counter' in globals())\ncounter = 42", "reset": true}),
            )
            .await;
        assert!(
            reset.output.contains("previous state is unavailable"),
            "{}",
            reset.output
        );
        assert!(reset.output.ends_with("False\n"), "{}", reset.output);
        release_idle_kernel(first.id);
        let idle = first
            .approved(&registry, json!({"code": "'counter' in globals()"}))
            .await;
        assert!(idle.output.contains("previous state is unavailable"), "{}", idle.output);
        assert!(idle.output.ends_with("False\n"), "{}", idle.output);
    });
}

#[test]
fn eval_bounds_inline_output_and_keeps_a_session_artifact() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true\nsecrets_mode = 'obfuscate'");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let first = TestSession::new("eval-artifact-1", &home.cwd);
        let second = TestSession::new("eval-artifact-2", &home.cwd);
        let _vault = crate::secrets::register_session_vault(first.id);
        let secret = concat!("sk-", "proj-", "abcdefghijklmnopqrstuvwxyz012345");
        let result = first
            .approved(&registry, json!({"code": format!("import os\nprint(os.getenv('HORON_HOME', 'not-inherited'))\nprint('x' * 50000)\nprint('only-in-the-middle')\nprint('OPENAI_API_KEY={secret}')\nprint('y' * 50000)")}))
            .await;
        assert_eq!(result.verdict, Some(ToolVerdict::Truncated));
        assert!(result.output.len() <= 50 * 1024);
        assert!(result.output.starts_with("not-inherited\n"));
        assert!(!result.output.contains("only-in-the-middle"));
        let id = result.output.split_once("artifact://").expect("artifact pointer").1;
        let id = id.split(|c: char| !c.is_ascii_digit()).next().expect("artifact id");
        let request = json!({"file_path": format!("artifact://{id}"), "start_line": 3, "end_line": 4});
        let retained = registry
            .execute("read", request.clone(), first.context("read-own"))
            .await
            .expect("read retained output");
        assert!(retained.output.contains("only-in-the-middle"), "{}", retained.output);
        assert!(
            !retained.output.contains(secret),
            "artifact contained the synthetic key"
        );
        assert!(retained.output.contains("«SECRET:"), "{}", retained.output);
        let other = registry.execute("read", request, second.context("read-other")).await;
        assert!(other.is_err() || other.is_ok_and(|output| !output.output.contains("only-in-the-middle")));
    });
}

#[test]
fn eval_timeout_stops_the_process_and_its_child() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-timeout", &home.cwd);
        let timed_out = session.approved(&registry, json!({
            "timeout": 3,
            "code": "import os, subprocess, sys, time\nfrom pathlib import Path\nchild = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\nPath('pids').write_text(f'{os.getpid()} {child.pid}')\nmarker = 9\ntime.sleep(60)"
        })).await;
        assert_eq!(timed_out.verdict, Some(ToolVerdict::Interrupted));
        assert!(timed_out.output.contains("timed out after 3s"));
        let recovered = session
            .approved(&registry, json!({"code": "'marker' in globals()"}))
            .await;
        assert!(recovered.output.ends_with("False\n"), "{}", recovered.output);
        let pids = std::fs::read_to_string(home.cwd.join("pids")).expect("cell and child started before timeout");
        let pids: Vec<u32> = pids
            .split_whitespace()
            .map(|pid| pid.parse().expect("valid PID"))
            .collect();
        assert_eq!(pids.len(), 2);
        tokio::time::timeout(Duration::from_secs(5), async {
            while pids.iter().any(|&pid| horon_agent_platform::is_process_running(pid)) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the previous Python process and its child are gone");
    });
}

#[test]
fn eval_aborted_call_cleans_up_before_the_next_cell() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true");
        let registry = Arc::new(Registry::new(Arc::new(OfflineProvider)).await);
        let session = TestSession::new("eval-cancel", &home.cwd);
        let input = json!({"code": "import os, time\nfrom pathlib import Path\nPath('ready').write_text(str(os.getpid()))\nmarker = 9\ntime.sleep(60)"});
        let request = session.request(&registry, input.clone()).await;
        crate::safety::record_permission_via_file(&request, true, "api_client", None).expect("approve the cell");
        let parent = Arc::clone(&registry);
        let ctx = session.context("aborted");
        let task = tokio::spawn(async move { parent.execute(NAME, input, ctx).await });
        let ready = home.cwd.join("ready");
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) =
                    std::fs::read_to_string(&ready).and_then(|text| text.parse::<u32>().map_err(std::io::Error::other))
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("running cell announces its PID");
        task.abort();
        assert!(task.await.expect_err("cell was aborted").is_cancelled());
        let result = session
            .approved(&registry, json!({"code": "'marker' in globals()"}))
            .await;
        assert!(result.output.ends_with("False\n"), "{}", result.output);
        tokio::time::timeout(Duration::from_secs(5), async {
            while horon_agent_platform::is_process_running(pid) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the aborted Python process is gone");
    });
}

#[test]
fn eval_oversized_cell_preserves_previous_state() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-size", &home.cwd);
        session.approved(&registry, json!({"code": "saved = 23"})).await;
        let input = json!({"code": "\\".repeat(140_000)});
        let request = session.request(&registry, input.clone()).await;
        crate::safety::record_permission_via_file(&request, true, "api_client", None).expect("approve the cell");
        let error = registry
            .execute(NAME, input, session.context("oversized"))
            .await
            .expect_err("reject oversized cell");
        assert!(error.to_string().contains("256 KiB"));
        let result = session.approved(&registry, json!({"code": "saved"})).await;
        assert_eq!(result.output, "23\n");
    });
}

#[test]
fn eval_accepts_model_tool_arguments() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-model-arguments", &home.cwd);
        let output = session
            .approved(
                &registry,
                json!({
                    "code": "print(37)",
                    "intent": "Show the value",
                    "accept_large_output": null,
                    "reset": null,
                    "timeout": null
                }),
            )
            .await;
        assert_eq!(output.output, "37\n");
    });
}

#[test]
fn eval_daemon_approval_is_consumed_once() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-single-approval", &home.cwd);
        let input = json!({"code": "print('approved cell')"});
        let request = session.request(&registry, input.clone()).await;
        crate::safety::record_permission_via_file(&request, true, "api_client", None).expect("approve the cell");
        let (first, second) = tokio::join!(
            registry.execute(NAME, input.clone(), session.context("retry-1")),
            registry.execute(NAME, input, session.context("retry-2")),
        );
        let runs: Vec<_> = [first, second].into_iter().filter_map(Result::ok).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].output, "approved cell\n");
    });
}

#[test]
fn eval_editor_approves_one_cell_without_releasing_another() {
    use crate::protocol::ServerEvent;
    use crate::tool::{acp_session, permission_bridge};

    isolated(async |home| {
        home.config("profile = 'full'\neval = true\napproval_mode = 'yolo'");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-editor", &home.cwd);
        let connection = "eval-test-editor";
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let acp_token = acp_session::install(session.id, connection, tx.clone());
        let token = permission_bridge::install(session.id, connection, tx);
        let _turn = acp_session::claim_turn(session.id, Some(connection));
        let bridge = permission_bridge::bridge_for_connection(session.id, connection).expect("editor bridge");
        let input = json!({"code": "print('one editor approval')"});
        let first_registry = registry.clone();
        let first_input = input.clone();
        let first_ctx = session.context("editor-1");
        let first = tokio::spawn(async move { first_registry.execute(NAME, first_input, first_ctx).await });
        let second_ctx = session.context("editor-2");
        let second = tokio::spawn(async move { registry.execute(NAME, input, second_ctx).await });

        let mut ask = async || {
            loop {
                if let Some(ServerEvent::ToolPermissionRequested { request_id, .. }) = events.recv().await {
                    return request_id;
                }
            }
        };
        let first_id = tokio::time::timeout(Duration::from_secs(5), ask())
            .await
            .expect("first prompt");
        let second_id = tokio::time::timeout(Duration::from_secs(5), ask())
            .await
            .expect("second prompt");
        assert_ne!(first_id, second_id);
        for (id, allow) in [(&first_id, true), (&second_id, false)] {
            assert_eq!(bridge.begin_editor_answer(id), permission_bridge::EditorAnswer::Pending);
            crate::safety::record_permission_via_file(id, allow, "api_client", None).expect("record editor answer");
            bridge.finish_editor_answer(id, allow, "api_client");
        }
        let runs: Vec<_> = [first.await.expect("first call"), second.await.expect("second call")]
            .into_iter()
            .filter_map(Result::ok)
            .collect();
        permission_bridge::clear(session.id, token);
        acp_session::clear(session.id, acp_token);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].output, "one editor approval\n");
    });
}

#[test]
fn eval_bridge_reads_and_runs_shell_through_the_registry() {
    isolated(async |home| {
        home.config(
            "profile = 'full'\neval = true\n[[tools.bash_patterns]]\npattern = 'echo bridge-denied'\napproval = 'deny'",
        );
        std::fs::write(home.cwd.join("bridge.txt"), "hello from the scoped file\n").expect("write fixture");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-tool-access", &home.cwd);
        let output = session
            .approved(
                &registry,
                json!({"code": r#"
text = await tool.read(file_path='bridge.txt')
print('read verified' if 'hello from the scoped file' in text else text)
shell = await tool.bash(command='echo bridge-allowed')
print('shell verified' if any(line.strip() == 'bridge-allowed' for line in shell.splitlines()) else shell)
try:
    await tool.bash(command='echo bridge-denied')
except RuntimeError as error:
    print('deny verified' if "bash pattern 'echo bridge-denied'" in str(error) else error)
"#}),
            )
            .await;
        assert_eq!(output.output, "read verified\nshell verified\ndeny verified\n");
    });
}

#[test]
fn eval_bridge_requires_separate_approval_for_a_shell_call() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true\napproval_mode = 'all'");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-tool-approval", &home.cwd);
        let output = session
            .approved(
                &registry,
                json!({"code": r#"
try:
    await tool.bash(command='echo side-effect > bridge-approval.txt')
except RuntimeError as error:
    print('approval queued' if 'needs approval before it runs' in str(error) else error)
"#}),
            )
            .await;
        assert_eq!(output.output, "approval queued\n");
        assert!(!home.cwd.join("bridge-approval.txt").exists());
    });
}

#[test]
fn eval_bridge_refuses_off_tools_dispatchers_and_background_shell() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true\n[tools.load_mode]\nread = 'off'");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-tool-policy", &home.cwd);
        let output = session
            .approved(
                &registry,
                json!({"code": r#"
for call in [tool.read(file_path='no-access'), tool.eval(code='42'), tool.toolbox(action='list'),
             tool.bash(command='echo unexpected', run_in_background=True)]:
    try:
        await call
    except RuntimeError as error:
        print(error)
"#}),
            )
            .await;
        assert_eq!(
            output.output,
            concat!(
                "Tool 'read' is not available in this session.\n",
                "Tool 'eval' cannot be called from a Python cell.\n",
                "Tool 'toolbox' cannot be called from a Python cell.\n",
                "Python tool calls cannot start background shell commands or services\n",
            )
        );
    });
}

#[test]
fn eval_bridge_limits_calls_and_keeps_the_next_cell_usable() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true");
        std::fs::write(home.cwd.join("small.txt"), "bridge fixture contents\n").expect("write fixture");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-tool-limit", &home.cwd);
        let output = session
            .approved(
                &registry,
                json!({"code": r#"
count = 0
try:
    for _ in range(65):
        await tool.read(file_path='small.txt')
        count += 1
except RuntimeError as error:
    print(count, error)
"#}),
            )
            .await;
        assert_eq!(output.output, "64 Python eval allows at most 64 tool calls per cell\n");
        let direct = registry
            .execute(
                "read",
                json!({"file_path": "small.txt"}),
                session.context("direct-read"),
            )
            .await
            .expect("read from the model context");
        assert!(direct.output.contains("bridge fixture contents"), "{}", direct.output);
        let next = session
            .approved(
                &registry,
                json!({"code": r#"
text = await tool.read(file_path='small.txt')
print('read verified' if 'bridge fixture contents' in text else text)
"#}),
            )
            .await;
        assert_eq!(next.output, "read verified\n");
    });
}

#[test]
fn eval_bridge_timeout_stops_running_shell() {
    isolated(async |home| {
        home.config("profile = 'full'\neval = true");
        let registry = Registry::new(Arc::new(OfflineProvider)).await;
        let session = TestSession::new("eval-tool-timeout", &home.cwd);
        let output = session.approved(&registry, json!({"timeout": 5, "code": r#"
import sys
command = f'''"{sys.executable}" -c "import os, time; open('bridge-pid', 'w').write(str(os.getpid())); time.sleep(60)"'''
await tool.bash(command=command)
"#})).await;
        assert_eq!(output.verdict, Some(ToolVerdict::Interrupted));
        assert!(output.output.contains("timed out after 5s"), "{}", output.output);
        let pid: u32 = std::fs::read_to_string(home.cwd.join("bridge-pid"))
            .expect("the child of the bridge shell started")
            .parse()
            .expect("valid pid");
        tokio::time::timeout(Duration::from_secs(5), async {
            while horon_agent_platform::is_process_running(pid) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the bridge shell's child was stopped");

        if horon_agent_platform::process::process_groups_supported() {
            let output = session
                .approved(
                    &registry,
                    json!({"code": r#"
await tool.bash(command='sleep 60 >/dev/null 2>&1 & echo $! > bridge-finished-pid')
print('shell returned')
"#}),
                )
                .await;
            assert!(output.output.ends_with("shell returned\n"), "{}", output.output);
            let descendant: u32 = std::fs::read_to_string(home.cwd.join("bridge-finished-pid"))
                .expect("shell wrote the descendant PID")
                .trim()
                .parse()
                .expect("valid pid");
            tokio::time::timeout(Duration::from_secs(5), async {
                // Linux can keep an exited orphan's PID until init reaps it.
                // Check whether the child can still execute, not just whether
                // the kernel still has a process table entry for it.
                loop {
                    let status = tokio::process::Command::new("ps")
                        .args(["-o", "stat=", "-p", &descendant.to_string()])
                        .output()
                        .await
                        .expect("read descendant process status");
                    let state = String::from_utf8_lossy(&status.stdout);
                    let state = state.trim();
                    if state.starts_with('Z') || (status.status.code() == Some(1) && state.is_empty()) {
                        break;
                    }
                    assert!(status.status.success() && !state.is_empty(), "{status:?}");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("the shell's group was stopped after its leader exited");
        }
    });
}

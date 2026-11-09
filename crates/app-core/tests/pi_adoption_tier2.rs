//! The tier-2 behaviours, proven end to end through the public dispatch path.
//!
//! Every case drives `Registry::execute`, the entry point a model's tool call
//! takes, so the plan-mode gate, the approval gate and the bash session
//! directory are exercised where they actually run rather than through a
//! private helper. `HORON_HOME` and `HORON_AGENT_RUNTIME_DIR` point at
//! per-case temp directories because `horon_agent_storage::horon_agent_dir` and
//! `runtime_dir` read them: without isolation a case writes sessions into the
//! developer's real home.

use async_trait::async_trait;
use horon_agent_app_core::config::SecretsMode;
use horon_agent_app_core::message::{ContentBlock, Message, ToolDefinition};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::tool::{Registry, ToolContext, ToolExecutionMode};
use horon_agent_app_core::{plan_mode, secrets};
use horon_agent_message_types::StreamEvent;
use horon_agent_token_estimate::{TokenFamily, estimate_tokens_for_family};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A `tokio` mutex, not a `std` one: the guard is held across `await` points, and
/// a `std` guard held across an await is what `clippy::await_holding_lock` forbids.
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
            .prefix(&format!("horon-tier2-{label}-home-"))
            .tempdir()
            .expect("a private home");
        let runtime = tempfile::Builder::new()
            .prefix(&format!("horon-tier2-{label}-runtime-"))
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

fn real_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn printed_path(output: &str) -> PathBuf {
    let last = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| panic!("the command printed nothing: {output}"));
    real_path(Path::new(last))
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn bash_keeps_its_directory_across_calls() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("bash-cwd", "bash_session_cwd = true\n");
    let session = "tier2-bash-cwd";
    let registry = registry().await;

    let root = env.home().join("cwd-root");
    let entered = root.join("entered");
    std::fs::create_dir_all(&entered).expect("the directory to enter");

    registry
        .execute(
            "bash",
            json!({"command": format!("cd {}", entered.display())}),
            context(session, &root),
        )
        .await
        .expect("the cd call runs");

    let printed = registry
        .execute("bash", json!({"command": "pwd"}), context(session, &root))
        .await
        .expect("the pwd call runs");

    assert_eq!(
        printed_path(&printed.output),
        real_path(&entered),
        "the second call must start where the first one ended, got {:?}",
        printed.output
    );
}

#[tokio::test]
async fn secrets_are_masked_outbound_and_restored_for_tools() {
    let _env = TEST_ENV.lock().await;
    let _isolated = IsolatedEnv::new("secrets", "");
    let session = "tier2-secrets";
    let _registration = secrets::register_session_vault(session);

    // Shaped like a GitHub token for the masking rule (`ghp_` plus 20 or more
    // token characters) but shorter than the 36 the repository secret scan looks
    // for, so the fixture never reads as live credential material.
    let credential = "ghp_fixture0123456789abcdefghij";
    let transcript = vec![Message::user(&format!(
        "deploy with GITHUB_TOKEN={credential} and nothing else"
    ))];

    let outbound = secrets::outbound_messages(&transcript, session, SecretsMode::Obfuscate)
        .expect("obfuscate mode never refuses")
        .expect("obfuscate mode returns a masked copy");
    assert_eq!(outbound.len(), 1, "one message in, one message out");
    let sent = message_text(&outbound[0]);
    assert!(
        sent.contains("«SECRET:"),
        "the outbound copy must carry a placeholder: {sent}"
    );
    assert!(
        !sent.contains(credential),
        "the outbound copy must not carry the credential: {sent}"
    );

    let placeholder = sent
        .split("«SECRET:")
        .nth(1)
        .and_then(|rest| rest.split('»').next())
        .map(|label| format!("«SECRET:{label}»"))
        .expect("a placeholder label");

    let restored = secrets::restore_tool_input(
        json!({"command": format!("export GITHUB_TOKEN={placeholder}")}),
        session,
    );
    let command = restored["command"].as_str().expect("the command string");
    assert!(
        command.contains(credential),
        "the tool must receive the real value, got {command}"
    );
    assert!(
        !command.contains("«SECRET:"),
        "no placeholder may reach the tool, got {command}"
    );

    let history = secrets::mask_for_history(&format!("GITHUB_TOKEN={credential}"), session, SecretsMode::Obfuscate);
    assert!(
        !history.contains(credential),
        "the disk path stays one-way, so history must not keep the credential"
    );
}

const KOREAN_PROSE: &str = "에이전트가 파일을 읽고 편집한 뒤 테스트를 실행합니다. 이 과정에서 컨텍스트가 얼마나 남았는지 정확히 알아야 압축을 제때 시작할 수 있습니다.";

fn unbroken_hangul_run() -> String {
    "가".repeat(1_200)
}

#[test]
fn korean_conversation_estimate_is_not_larger_than_the_old_heuristic() {
    assert_eq!(KOREAN_PROSE.chars().count(), 79, "the measured sample is 79 characters");
    assert_eq!(KOREAN_PROSE.len(), 197, "and 197 bytes");

    let old_heuristic = KOREAN_PROSE.len() / 4;
    let estimate = estimate_tokens_for_family(KOREAN_PROSE, TokenFamily::O200k);

    assert!(
        estimate <= old_heuristic,
        "compaction fires when the estimate crosses a threshold, so a larger estimate compacts \
         EARLIER and discards usable context. o200k estimate {estimate} must not exceed the \
         bytes/4 heuristic {old_heuristic} for this sample (real o200k count is 44)"
    );

    let run = unbroken_hangul_run();
    let run_old = run.len() / 4;
    let run_estimate = estimate_tokens_for_family(&run, TokenFamily::O200k);
    assert!(
        run_estimate > run_old,
        "an unbroken Hangul run carries no spaces or punctuation, so bytes/4 loses the weight it \
         was counting and the estimate must be larger: {run_estimate} vs {run_old}"
    );
}

#[test]
fn cl100k_estimate_is_larger_than_the_old_heuristic() {
    let old_heuristic = KOREAN_PROSE.len() / 4;
    let cl100k = estimate_tokens_for_family(KOREAN_PROSE, TokenFamily::Cl100k);
    let o200k = estimate_tokens_for_family(KOREAN_PROSE, TokenFamily::O200k);

    assert!(
        cl100k > old_heuristic,
        "cl100k spends close to one token per Hangul syllable, so its estimate {cl100k} must \
         exceed the bytes/4 heuristic {old_heuristic} (real cl100k count is 72)"
    );
    assert!(
        cl100k > o200k,
        "the two families must disagree in the measured direction: cl100k {cl100k} > o200k {o200k}"
    );
}

#[tokio::test]
async fn plan_mode_blocks_write_and_submit_plan_unblocks_it() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("plan-mode", "plan_mode = true\n");
    let session = "tier2-plan-mode";
    plan_mode::clear_state(session);
    let registry = registry().await;

    let source = env.home().join("plan-source.txt");
    std::fs::write(&source, "readable\n").expect("the readable file");
    let target = env.home().join("plan-target.txt");
    let write_input = json!({
        "file_path": target.display().to_string(),
        "content": "the approved write\n",
    });

    plan_mode::set_state(session, plan_mode::PlanState::Planning);

    let refusal = registry
        .execute("write", write_input.clone(), context(session, env.home()))
        .await
        .expect_err("plan mode must refuse a write");
    assert_eq!(
        refusal.to_string(),
        plan_mode::block_message("write"),
        "the refusal must be the plan-mode one, not a tool error"
    );
    assert!(!target.exists(), "a refused write must not create the file");

    registry
        .execute(
            "read",
            json!({"file_path": source.display().to_string()}),
            context(session, env.home()),
        )
        .await
        .expect("plan mode must allow a read");

    let submitted = registry
        .execute(
            "submit_plan",
            json!({
                "plan": "# Plan\n\n1. Read the source.\n2. Write the target.",
                "files": [target.display().to_string()],
            }),
            context(session, env.home()),
        )
        .await
        .expect("a valid plan must be queued");
    assert_eq!(
        plan_mode::state_for(session),
        plan_mode::PlanState::PendingApproval,
        "a submission must park the session in pending approval"
    );
    registry
        .execute("write", write_input.clone(), context(session, env.home()))
        .await
        .expect_err("pending approval is still read-only");

    let request_id = submitted
        .metadata
        .as_ref()
        .and_then(|metadata| metadata["request_id"].as_str())
        .expect("the submission carries its request id")
        .to_string();
    assert_eq!(
        plan_mode::observe_decision(&request_id, true).as_deref(),
        Some(session),
        "the approval must move this session"
    );
    assert_eq!(plan_mode::state_for(session), plan_mode::PlanState::Approved);

    registry
        .execute("write", write_input, context(session, env.home()))
        .await
        .expect("an approved plan must reopen the same write");
    assert_eq!(
        std::fs::read_to_string(&target).expect("the approved write landed"),
        "the approved write\n"
    );

    plan_mode::clear_state(session);
}

#[tokio::test]
async fn write_approval_mode_auto_approves_files_and_asks_for_shell() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("approval-write", "approval_mode = \"write\"\n");
    let session = "tier2-approval-write";
    plan_mode::clear_state(session);
    let registry = registry().await;

    let target = env.home().join("approval-target.txt");
    registry
        .execute(
            "write",
            json!({
                "file_path": target.display().to_string(),
                "content": "auto approved\n",
            }),
            context(session, env.home()),
        )
        .await
        .expect("write mode auto-approves a plain file mutation");
    assert_eq!(
        std::fs::read_to_string(&target).expect("the auto-approved write landed"),
        "auto approved\n"
    );

    let refusal = registry
        .execute("bash", json!({"command": "echo hi"}), context(session, env.home()))
        .await
        .expect_err("write mode must ask before a shell command runs");
    let message = refusal.to_string();
    assert!(
        message.contains("needs approval"),
        "the refusal must name the approval gate: {message}"
    );
    assert!(message.contains("bash"), "the refusal must name the tool: {message}");
}

/// The dual-confirm list has to ask at the default `off` setting. Only a
/// dispatch-path test can prove it: `evaluate` is a pure function, and a test
/// that calls it directly never passes through the gate that decides whether
/// evaluation runs at all.
#[tokio::test]
async fn dual_confirm_class_asks_even_when_mode_is_off() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new(
        "dual-confirm-off",
        "approval_mode = \"off\"\ndual_confirm_classes = [\"destructive-shell\"]\n",
    );
    let registry = registry().await;
    let work = tempfile::tempdir().expect("a scratch working directory");

    let result = registry
        .execute(
            "bash",
            json!({"command": "rm -rf /"}),
            context("dual-confirm-off", work.path()),
        )
        .await;

    let error = result.expect_err(
        "a listed dual-confirm class must ask even with approval_mode = off, \
         so the call must not run",
    );
    let message = error.to_string();
    assert!(
        message.contains("needs approval"),
        "the refusal must name the approval requirement, got: {message}"
    );
    assert!(
        message.contains("Request "),
        "the refusal must name the queued request so it can be answered, got: {message}"
    );
    drop(env);
}

/// A refused call has to leave a request behind, or `approval_mode = "write"`
/// turns every shell call into a dead end: refused with nothing to answer.
#[tokio::test]
async fn write_mode_enqueues_a_request_instead_of_only_refusing() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("write-mode-enqueues", "approval_mode = \"write\"\n");
    let registry = registry().await;
    let work = tempfile::tempdir().expect("a scratch working directory");

    let result = registry
        .execute(
            "bash",
            json!({"command": "echo queued"}),
            context("write-mode-enqueues", work.path()),
        )
        .await;

    let error = result.expect_err("write mode must ask before a shell command runs");
    let message = error.to_string();
    assert!(
        message.contains("needs approval"),
        "the refusal must name the approval requirement, got: {message}"
    );

    // The message names the request, and that request is really in the queue.
    let request_id = message
        .split("Request ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the refusal names the queued request id")
        .to_string();
    assert!(
        !request_id.contains("not queued"),
        "the request has to reach the queue, got: {request_id}"
    );
    let queued = horon_agent_app_core::tool::ambient::shared_safety_system()
        .pending_requests()
        .iter()
        .any(|request| request.id == request_id);
    assert!(queued, "request {request_id} must be pending so the user can answer it");
    drop(env);
}

/// The approval audit has to reach the log file a running daemon writes, not
/// just an in-process sink.
///
/// `observe_audit` is a `#[cfg(test)]` recorder: in a release build it is an
/// empty function, so an assertion that reads it proves nothing about what an
/// operator would find on disk. This test initialises the real logger against an
/// isolated home, drives a write-mode auto-approval through `Registry::execute`,
/// and reads the `TOOL_APPROVAL` line back out of the log file.
#[tokio::test]
async fn auto_approval_writes_a_tool_approval_line_to_the_log() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("approval-audit-log", "approval_mode = \"write\"\n");
    horon_agent_app_core::logging::init();

    let registry = registry().await;
    let target = env.home().join("audit-target.txt");
    registry
        .execute(
            "write",
            json!({
                "file_path": target.display().to_string(),
                "content": "audited\n",
            }),
            context("approval-audit-log", env.home()),
        )
        .await
        .expect("write mode auto-approves a plain file mutation");

    let log_path = horon_agent_app_core::logging::log_path().expect("the logger resolved a log file");
    let contents =
        std::fs::read_to_string(&log_path).unwrap_or_else(|error| panic!("read {}: {error}", log_path.display()));
    let line = contents
        .lines()
        .find(|line| line.contains("TOOL_APPROVAL"))
        .unwrap_or_else(|| {
            panic!(
                "no TOOL_APPROVAL line in {}; the log holds:\n{contents}",
                log_path.display()
            )
        });

    // The TOOL_APPROVAL fields are tool, mode, reason and tool_call_id. `context`
    // builds the id as `{session_id}-call`, so this is the real check that the
    // line names the call it recorded. A disjunction over the field name
    // `tool_call_id` would always be true and prove nothing.
    assert!(
        line.contains("approval-audit-log-call"),
        "the audit line must carry the tool call id, got: {line}"
    );
    assert!(
        line.contains("mode=write"),
        "the audit line must name the approval mode, got: {line}"
    );
    drop(env);
}

/// Plan mode has to refuse a process leg through the dispatch path, not only as
/// a predicate.
///
/// `plan_mode_tests` proves `allows_effects` returns false for `process()`, but a
/// predicate that no call site consults would still read as passing. This drives
/// `Registry::execute` with a real shell command while the session is planning,
/// and asserts the command did not run.
#[tokio::test]
async fn plan_mode_refuses_a_shell_command_through_the_dispatch_path() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("plan-mode-process", "plan_mode = true\n");
    let session = "tier2-plan-mode-process";
    plan_mode::clear_state(session);
    plan_mode::set_state(session, plan_mode::PlanState::Planning);

    let marker = env.home().join("shell-ran-marker.txt");
    let registry = registry().await;
    let result = registry
        .execute(
            "bash",
            json!({"command": format!("touch {}", marker.display())}),
            context(session, env.home()),
        )
        .await;

    let error = result.expect_err("plan mode must refuse a process leg");
    let message = error.to_string();
    assert!(
        message.contains("plan"),
        "the refusal must name plan mode so the model knows why, got: {message}"
    );
    assert!(
        !marker.exists(),
        "the refused shell command must not have run: {} exists",
        marker.display()
    );
    drop(env);
}

/// The same refusal has to survive a tool that declares no effects at all: the
/// trait default is a barrier, so an unclassified call is refused rather than
/// allowed to slip through as a read.
#[tokio::test]
async fn plan_mode_refuses_a_write_that_declares_nothing() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("plan-mode-write", "plan_mode = true\n");
    let session = "tier2-plan-mode-write";
    plan_mode::clear_state(session);
    plan_mode::set_state(session, plan_mode::PlanState::Planning);

    let target = env.home().join("declared-nothing.txt");
    let registry = registry().await;
    let result = registry
        .execute(
            "write",
            json!({
                "file_path": target.display().to_string(),
                "content": "should not land\n",
            }),
            context(session, env.home()),
        )
        .await;

    assert!(
        result.is_err(),
        "plan mode must refuse a file write while the session is planning"
    );
    assert!(
        !target.exists(),
        "the refused write must not have created {}",
        target.display()
    );
    drop(env);
}

/// Shaped like a GitHub token for the masking rule (`ghp_` plus 20 or more token
/// characters) but shorter than the 36 the repository secret scan looks for, so
/// the fixture never reads as live credential material.
const REAL_CREDENTIAL: &str = "ghp_fixture0123456789abcdefghij";

/// A provider that records what it was asked and issues one shell call.
///
/// The masking contract is about what a turn sends and what a tool receives, so
/// proving it needs the whole turn: a helper-level assertion on
/// `outbound_messages` shows the string was rewritten, not that the turn used it.
#[derive(Clone)]
struct TurnRecorder {
    seen: Arc<std::sync::Mutex<Vec<String>>>,
    rounds: Arc<std::sync::Mutex<usize>>,
    out_path: PathBuf,
}

impl TurnRecorder {
    fn new(out_path: PathBuf) -> Self {
        Self {
            seen: Arc::new(std::sync::Mutex::new(Vec::new())),
            rounds: Arc::new(std::sync::Mutex::new(0)),
            out_path,
        }
    }
}

fn render_message(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Provider for TurnRecorder {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let rendered = messages.iter().map(render_message).collect::<Vec<_>>().join("\n");
        self.seen.lock().unwrap().push(rendered.clone());
        let mut rounds = self.rounds.lock().unwrap();
        *rounds += 1;
        let round = *rounds;
        drop(rounds);
        if round == 1 {
            let placeholder = rendered
                .find("«SECRET:")
                .and_then(|start| {
                    let rest = &rendered[start..];
                    rest.find('»').map(|end| rest[..end + '»'.len_utf8()].to_string())
                })
                .or_else(|| {
                    rendered.find("ghp_").map(|start| {
                        rendered[start..]
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect::<String>()
                    })
                })
                .unwrap_or_else(|| "NO_CREDENTIAL_SEEN".to_string());
            let call = json!({
                "command": format!("printf '%s' '{placeholder}' > {}", self.out_path.display())
            });
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::ToolUseStart {
                    id: "c1".into(),
                    name: "bash".into(),
                }),
                Ok(StreamEvent::ToolInputDeltaFor {
                    id: "c1".into(),
                    delta: call.to_string(),
                }),
                Ok(StreamEvent::ToolUseEndFor { id: "c1".into() }),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".into()),
                }),
            ])))
        } else {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta("done".into())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                }),
            ])))
        }
    }

    fn name(&self) -> &str {
        "turn-recorder"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn masking_round(mode: &str) -> (String, Option<String>, Option<String>) {
    let env = IsolatedEnv::new("turn-masking", &format!("secrets_mode = \"{mode}\"\n"));
    let out_path = env.home().join("placeholder-seen.txt");
    let recorder = TurnRecorder::new(out_path.clone());
    let provider: Arc<dyn Provider> = Arc::new(recorder.clone());
    let registry = Registry::new(provider.clone()).await;
    let mut agent = horon_agent_app_core::agent::Agent::new(provider, registry);
    let result = agent
        .run_once_capture(&format!("deploy with GITHUB_TOKEN={REAL_CREDENTIAL} and nothing else"))
        .await;
    let outbound = recorder.seen.lock().unwrap().first().cloned().unwrap_or_default();
    let tool_file = std::fs::read_to_string(&out_path).ok();
    let error = result.err().map(|error| error.to_string());
    drop(env);
    (outbound, tool_file, error)
}

/// Masking is a turn-level promise: the model never sees the value, the tool
/// always does.
#[tokio::test]
async fn turn_masking_hides_the_credential_from_the_model_and_restores_it_for_the_tool() {
    let _env = TEST_ENV.lock().await;
    let (outbound, tool_file, error) = masking_round("obfuscate").await;

    assert!(error.is_none(), "obfuscate must not refuse the turn: {error:?}");
    assert!(
        outbound.contains("«SECRET:"),
        "the model must be shown a placeholder, got: {outbound}"
    );
    assert!(
        !outbound.contains(REAL_CREDENTIAL),
        "the model must never see the credential, got: {outbound}"
    );
    let tool_input = tool_file.expect("the bash tool ran and wrote what it received");
    assert!(
        tool_input.contains(REAL_CREDENTIAL),
        "the tool must receive the real value, got: {tool_input}"
    );
    assert!(
        !tool_input.contains("«SECRET:"),
        "no placeholder may reach the tool, got: {tool_input}"
    );
}

/// `off` is the default, so it has to leave the turn byte-for-byte unchanged.
#[tokio::test]
async fn turn_masking_off_sends_the_credential_verbatim() {
    let _env = TEST_ENV.lock().await;
    let (outbound, tool_file, error) = masking_round("off").await;

    assert!(error.is_none(), "off must not refuse the turn: {error:?}");
    assert!(
        outbound.contains(REAL_CREDENTIAL),
        "off sends the credential unchanged, got: {outbound}"
    );
    assert!(
        !outbound.contains("«SECRET:"),
        "off must not mint a placeholder, got: {outbound}"
    );
    assert!(
        tool_file.expect("the bash tool ran").contains(REAL_CREDENTIAL),
        "the tool still gets the real value"
    );
}

/// `block` refuses the send outright and never reaches the provider.
#[tokio::test]
async fn turn_masking_block_refuses_the_send() {
    let _env = TEST_ENV.lock().await;
    let (outbound, tool_file, error) = masking_round("block").await;

    let message = error.expect("block must refuse the turn");
    assert!(
        message.contains("SECRET_BLOCK"),
        "the refusal must be named so it is actionable, got: {message}"
    );
    assert!(outbound.is_empty(), "block must never reach the provider");
    assert!(tool_file.is_none(), "no tool may run when the send is refused");
}

/// An approval mode has to be satisfiable: refusing, then approving, then
/// retrying must let the call run.
///
/// This is the half of B2 the first fix missed. Enqueueing a request proves the
/// user has something to answer; it does not prove the answer changes anything.
/// Here the decision is recorded through the real approval store and the same
/// call is retried, so an implementation that only ever refuses fails.
#[tokio::test]
async fn an_approved_request_lets_the_same_call_run() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("approval-satisfiable", "approval_mode = \"write\"\n");
    let session = "tier2-approval-satisfiable";
    let registry = registry().await;
    let work = tempfile::tempdir().expect("a scratch working directory");

    let call = json!({"command": "echo satisfied"});

    let refusal = registry
        .execute("bash", call.clone(), context(session, work.path()))
        .await
        .expect_err("write mode must ask before a shell command runs");
    let request_id = refusal
        .to_string()
        .split("Request ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the refusal names the queued request")
        .to_string();

    horon_agent_app_core::tool::ambient::shared_safety_system()
        .record_decision(&request_id, true, "test", None)
        .expect("the approval is recorded");

    let output = registry
        .execute("bash", call, context(session, work.path()))
        .await
        .expect("an approved request must let the retried call run");
    assert!(
        output.output.contains("satisfied"),
        "the approved call must actually execute, got: {}",
        output.output
    );
    drop(env);
}

/// Retrying a still-pending refusal must not pile up requests.
///
/// Without a keyed lookup every retry enqueues a fresh request that nobody can
/// answer, so the queue grows on each attempt while the mode stays unsatisfiable.
#[tokio::test]
async fn retrying_a_pending_request_does_not_enqueue_a_second_one() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("approval-no-pileup", "approval_mode = \"write\"\n");
    let session = "tier2-approval-no-pileup";
    let registry = registry().await;
    let work = tempfile::tempdir().expect("a scratch working directory");

    let call = json!({"command": "echo once"});
    let first = registry
        .execute("bash", call.clone(), context(session, work.path()))
        .await
        .expect_err("the first attempt is refused");
    let first_id = first
        .to_string()
        .split("Request ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("names the request")
        .to_string();

    let second = registry
        .execute("bash", call, context(session, work.path()))
        .await
        .expect_err("the retry is still refused while the answer is pending");
    let second_id = second
        .to_string()
        .split("Request ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("names the request")
        .to_string();

    assert_eq!(
        first_id, second_id,
        "a retry of the same call must point at the request already queued, not open a new one"
    );

    // The id comparison above would pass if both refusals named one request while
    // a second sat in the queue under a different id, so count what this session
    // has pending. Filtering on `first_id` would not catch that: a second minted
    // request has its own id and never matches. Counting by session does.
    // Count by tool as well as session, because a duplicate whose `context` is
    // None would slip past a session-only filter: with no `session_id` to compare,
    // a session-only predicate rejects it rather than counting it.
    let for_this_session = horon_agent_app_core::tool::ambient::shared_safety_system()
        .pending_requests()
        .into_iter()
        .filter(|request| request.action == "bash")
        .filter(|request| {
            request
                .context
                .as_ref()
                .and_then(|context| context.get("session_id"))
                .and_then(|value| value.as_str())
                .is_none_or(|id| id == session)
        })
        .count();
    assert_eq!(
        for_this_session, 1,
        "the retried call must leave exactly one pending request for this session, \
         found {for_this_session}"
    );
    drop(env);
}

/// A denied request keeps refusing, and says so.
#[tokio::test]
async fn a_denied_request_refuses_the_retry_with_the_denial() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("approval-denied", "approval_mode = \"write\"\n");
    let session = "tier2-approval-denied";
    let registry = registry().await;
    let work = tempfile::tempdir().expect("a scratch working directory");

    let call = json!({"command": "echo denied"});
    let first = registry
        .execute("bash", call.clone(), context(session, work.path()))
        .await
        .expect_err("the first attempt is refused");
    let request_id = first
        .to_string()
        .split("Request ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("names the request")
        .to_string();

    horon_agent_app_core::tool::ambient::shared_safety_system()
        .record_decision(&request_id, false, "test", None)
        .expect("the denial is recorded");

    let retry = registry
        .execute("bash", call.clone(), context(session, work.path()))
        .await
        .expect_err("a denied request must keep refusing the call");
    assert!(
        retry.to_string().contains("denied"),
        "the refusal must say the request was denied, got: {retry}"
    );

    // The denial has to keep holding on every later attempt, not only the first.
    // Before the fix the second retry forgot the answer and minted a fresh
    // request, which also wrote the denied id back into the queue; the daemon run
    // that caught it is .omo/evidence/F3-third-pi-adoption-tier2-addendum.txt.
    let third = registry
        .execute("bash", call, context(session, work.path()))
        .await
        .expect_err("the denial must still hold on a later retry");
    assert!(
        third.to_string().contains("denied"),
        "the second retry must still report the denial, got: {third}"
    );
    assert!(
        !third.to_string().contains("is queued"),
        "a denied call must not open a new request, got: {third}"
    );
    let reopened = horon_agent_app_core::tool::ambient::shared_safety_system()
        .pending_requests()
        .into_iter()
        .filter(|request| request.id != request_id)
        .filter(|request| {
            request
                .context
                .as_ref()
                .and_then(|context| context.get("session_id"))
                .and_then(|value| value.as_str())
                == Some(session)
        })
        .count();
    assert_eq!(
        reopened, 0,
        "a denied call must not leave a second request for the same session, found {reopened}"
    );
    drop(env);
}

/// A decision written by ANOTHER process must release the call.
///
/// The approval surface, the desktop app and `/plan approve` all answer by
/// writing `history.json`; they are not this process's `SafetySystem` instance.
/// A daemon run found the call still refused after such an answer, so the
/// in-process test above is not enough: this one writes the file directly, which
/// only `decision_for`'s disk fallback can see. The measurement is in
/// .omo/evidence/F3-third-pi-adoption-tier2.txt.
#[tokio::test]
async fn a_decision_written_to_disk_by_another_process_releases_the_call() {
    let _env = TEST_ENV.lock().await;
    let env = IsolatedEnv::new("approval-disk", "approval_mode = \"write\"\n");
    let session = "tier2-approval-disk";
    let registry = registry().await;
    let work = tempfile::tempdir().expect("a scratch working directory");

    let call = json!({"command": "echo from-disk"});
    let refusal = registry
        .execute("bash", call.clone(), context(session, work.path()))
        .await
        .expect_err("write mode must ask before a shell command runs");
    let request_id = refusal
        .to_string()
        .split("Request ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the refusal names the queued request")
        .to_string();

    // The file an external approver writes. `history_path()` is
    // <agent dir>/safety/history.json.
    let safety_dir = env.home().join("safety");
    std::fs::create_dir_all(&safety_dir).expect("the safety directory");
    let history = json!([{
        "request_id": request_id,
        "approved": true,
        "decided_at": chrono::Utc::now().to_rfc3339(),
        "decided_via": "api_client",
    }]);
    std::fs::write(
        safety_dir.join("history.json"),
        serde_json::to_vec(&history).expect("history serialises"),
    )
    .expect("the decision is written");

    let output = registry
        .execute("bash", call, context(session, work.path()))
        .await
        .expect("a decision on disk must release the call");
    assert!(
        output.output.contains("from-disk"),
        "the released call must actually execute, got: {}",
        output.output
    );
    drop(env);
}

/// `lsp` is a config-gated tool: the switch alone decides whether the registry
/// holds it, so `full` must not expose it until `[tools] lsp = true`.
#[tokio::test]
async fn lsp_is_registered_only_when_the_switch_is_on() {
    let _env = TEST_ENV.lock().await;

    let off = IsolatedEnv::new("lsp-off", "profile = \"full\"\n");
    let without = registry().await;
    let names: Vec<String> = without
        .definitions(None)
        .await
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert!(
        !names.iter().any(|name| name == "lsp"),
        "lsp must stay out of the tool list while the switch is off"
    );
    drop(off);

    let on = IsolatedEnv::new("lsp-on", "profile = \"full\"\nlsp = true\n");
    let with = registry().await;
    let names: Vec<String> = with
        .definitions(None)
        .await
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert!(
        names.iter().any(|name| name == "lsp"),
        "lsp must be registered when the switch is on"
    );
    drop(on);
}

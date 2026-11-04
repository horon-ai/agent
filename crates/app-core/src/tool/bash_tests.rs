#![cfg_attr(test, allow(clippy::await_holding_lock))]
use super::*;
use crate::bus::{BackgroundTaskProgressSource, BackgroundTaskStatus};
use crate::tool::StdinInputRequest;
use crate::tool::bash::{BashTool, ProgressLineUpdate, parse_heuristic_progress, parse_progress_line};
use serde_json::json;
use tokio::sync::mpsc;

#[cfg(not(windows))]
#[test]
fn bash_definition_prefers_repository_search_tools() {
    let definition = BashTool::new().to_definition();
    assert_eq!(definition.name, "bash");
    // The always-on description is capped at 20 estimated tokens, so the routing
    // rule and the rg detail are split across the description and the `command`
    // parameter description. Both are model-facing prompt text, so both are
    // asserted: the guidance must survive, not the field it used to sit in.
    let model_facing = format!(
        "{}\n{}",
        definition.description, definition.input_schema["properties"]["command"]["description"]
    );
    assert!(model_facing.contains("prefer agentgrep or rg over grep -r"));
    assert!(model_facing.contains("--hidden --no-ignore"));
    assert!(model_facing.contains("hidden and ignored"));
}

#[test]
fn bash_optional_nulls_use_omitted_defaults() {
    let omitted: BashInput = serde_json::from_value(json!({"command": "printf ok"})).unwrap();
    let nulls: BashInput = serde_json::from_value(json!({
        "command": "printf ok",
        "intent": null,
        "accept_large_output": null,
        "timeout": null,
        "run_in_background": null,
        "notify": null,
        "wake": null,
        "stall_wake_seconds": null,
        "justification": null
    }))
    .unwrap();
    assert!(nulls.notify);
    assert!(!nulls.wake);
    assert_eq!(nulls.notify, omitted.notify);
    assert_eq!(nulls.wake, omitted.wake);
    assert_eq!(nulls.run_in_background, omitted.run_in_background);
    assert_eq!(nulls.timeout, omitted.timeout);
    assert_eq!(nulls.stall_wake_seconds, omitted.stall_wake_seconds);
    assert_eq!(nulls.justification, omitted.justification);
    assert_eq!(nulls.intent, omitted.intent);
}

#[test]
fn bash_optional_booleans_preserve_explicit_values() {
    for value in [false, true] {
        let input: BashInput = serde_json::from_value(json!({
            "command": "printf ok", "notify": value, "wake": value,
            "run_in_background": value
        }))
        .unwrap();
        assert_eq!(input.notify, value);
        assert_eq!(input.wake, value);
        assert_eq!(input.run_in_background, Some(value));
    }
}

#[test]
fn bash_optional_booleans_still_reject_invalid_types() {
    for key in ["notify", "wake", "run_in_background"] {
        for value in [json!("false"), json!(0), json!([]), json!({})] {
            let mut input = json!({"command": "printf ok"});
            input[key] = value;
            assert!(serde_json::from_value::<BashInput>(input).is_err());
        }
    }
    assert!(serde_json::from_value::<BashInput>(json!({"command": null})).is_err());
    assert!(serde_json::from_value::<BashInput>(json!({})).is_err());
}

#[tokio::test]
async fn bash_executes_with_null_optional_arguments() {
    let output = BashTool::new()
        .execute(
            json!({
                "command": "printf null-options-ok",
                "notify": null, "wake": null, "run_in_background": null,
                "timeout": null, "stall_wake_seconds": null, "justification": null,
                "intent": null, "accept_large_output": null
            }),
            make_ctx(None),
        )
        .await
        .unwrap();
    assert!(output.output.contains("null-options-ok"));
}

#[test]
fn repository_commands_export_a_logged_cargo_function() {
    let repo = crate::build::find_repo_in_ancestors(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("test runs inside the horon-agent repository");
    let wrapped = wrap_repo_cargo_commands("cargo test -p demo && echo done", Some(&repo))
        .expect("horon-agent repository has cargo.sh");

    assert!(wrapped.contains("export HORON_AGENT_DEV_CARGO_SCRIPT="));
    assert!(wrapped.contains("HORON_AGENT_IN_DEV_CARGO=1 \"$HORON_AGENT_DEV_CARGO_SCRIPT\" \"$@\""));
    assert!(wrapped.contains("export -f cargo"));
    assert!(wrapped.ends_with("cargo test -p demo && echo done"));
}

#[test]
fn cargo_routing_is_limited_to_the_horon_agent_repository() {
    assert!(wrap_repo_cargo_commands("cargo test", Some(std::path::Path::new("/"))).is_none());
    assert!(wrap_repo_cargo_commands("cargo test", None).is_none());
}

#[test]
fn cargo_wrapper_path_is_shell_quoted() {
    assert_eq!(shell_single_quote("a'b"), "'a'\"'\"'b'");
}

#[tokio::test]
async fn background_command_stdin_is_null() {
    let mut command = build_shell_command("if IFS= read -r _; then printf inherited; else printf eof; fi", false);

    // Start with a readable pipe so this test does not depend on, or modify, the
    // test runner's process-wide stdin. Background configuration must replace it.
    command.stdin(Stdio::piped());
    configure_background_command_stdio(&mut command);

    let child = command.spawn().expect("background command should spawn");
    assert!(
        child.stdin.is_none(),
        "background commands must not retain a writable stdin pipe"
    );

    let output = tokio::time::timeout(Duration::from_secs(2), child.wait_with_output())
        .await
        .expect("background command should observe EOF instead of blocking")
        .expect("background command should exit cleanly");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "eof");
}

#[cfg(unix)]
fn write_pty_stub(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join(name);
    std::fs::write(
        &path,
        "#!/bin/sh\nif test -t 0; then echo TTY-OK; else echo NOT-TTY; fi\n",
    )
    .expect("write pty stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod pty stub");
    path
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_off_keeps_stdin_on_a_pipe() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let output = BashTool::new()
        .execute(
            json!({
                "command": "if test -t 0; then echo TTY-OK; else echo NOT-TTY; fi",
                "pty": "off"
            }),
            make_ctx(Some(tx)),
        )
        .await
        .unwrap();
    assert!(
        output.output.contains("NOT-TTY"),
        "pty=off must leave stdin a pipe: {}",
        output.output
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_always_allocates_a_terminal() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let output = BashTool::new()
        .execute(
            json!({
                "command": "if test -t 0; then echo TTY-OK; else echo NOT-TTY; fi",
                "pty": "always"
            }),
            make_ctx(Some(tx)),
        )
        .await
        .unwrap();
    assert!(
        output.output.contains("TTY-OK"),
        "pty=always must allocate a terminal: {}",
        output.output
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_auto_allocates_for_allowlisted_argv0() {
    let dir = std::env::temp_dir().join(format!("horon-agent-pty-stub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create stub dir");
    write_pty_stub(&dir, "ssh");

    let command = format!("PATH='{}':$PATH ssh probe", dir.display());
    let output = BashTool::new()
        .execute(json!({ "command": command, "pty": "auto" }), make_ctx(None))
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        output.output.contains("TTY-OK"),
        "auto must allocate for an allowlisted argv0: {}",
        output.output
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_auto_skips_plain_command() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let output = BashTool::new()
        .execute(
            json!({
                "command": "echo hi; if test -t 0; then echo TTY-OK; else echo NOT-TTY; fi",
                "pty": "auto"
            }),
            make_ctx(Some(tx)),
        )
        .await
        .unwrap();
    assert!(
        output.output.contains("NOT-TTY"),
        "auto must leave a plain command on pipes: {}",
        output.output
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_timeout_kills_the_process_tree() {
    let result = BashTool::new()
        .execute(
            json!({
                "command": "sleep 30; echo should_not_print",
                "run_in_background": true,
                "pty": "always",
                "timeout": 300,
                "notify": false,
                "wake": false
            }),
            make_ctx(None),
        )
        .await
        .expect("background pty command should start");

    let task_id = result.metadata.expect("expected metadata")["task_id"]
        .as_str()
        .expect("task id should be present")
        .to_string();

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut status = None;
    while std::time::Instant::now() < deadline {
        let current = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if current.status == BackgroundTaskStatus::Failed {
            status = Some(current);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let status = status.expect("pty background task should fail after its timeout");
    assert_eq!(
        status.exit_code,
        Some(124),
        "timeout must kill the pty command: {status:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_child_owns_the_terminal_and_survives_group_interrupt() {
    use std::os::fd::AsRawFd;

    let mut handle = match super::bash_pty::plan(crate::config::BashPtyMode::Always, "sleep 30").unwrap() {
        super::bash_pty::PtyPlan::Terminal(handle) => handle,
        super::bash_pty::PtyPlan::Pipe { .. } => panic!("a terminal must be allocated"),
    };
    let mut cmd = build_shell_command("printf 'READY\n'; sleep 30", true);
    handle.configure(&mut cmd).expect("configure pty stdio");
    let mut child = cmd.spawn().expect("spawn pty command");
    let child_pid = child.id().expect("pty child pid");
    let mut reader = handle.reader().expect("pty reader");
    let probe = handle.reader().expect("pty reader").into_std().await;
    let fd = probe.as_raw_fd();

    let mut lines = BufReader::new(&mut reader).lines();
    let ready = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("child should announce readiness")
        .expect("read readiness line")
        .expect("readiness line should not be EOF");
    assert!(ready.contains("READY"), "unexpected first line: {ready:?}");

    // SAFETY: `probe` owns the live terminal master for the whole call, so `fd`
    // stays an open descriptor of that terminal.
    let foreground = unsafe { libc::tcgetpgrp(fd) };
    assert_eq!(
        foreground,
        i32::try_from(child_pid).unwrap(),
        "the terminal must belong to the child's process group"
    );
    // SAFETY: `libc::termios` is a plain C struct of integer fields, so an all-zero
    // bit pattern is a valid value.
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is live and `term` is a valid, correctly sized, writable
    // out-pointer for `tcgetattr` to fill.
    let rc = unsafe { libc::tcgetattr(fd, &mut term) };
    assert_eq!(rc, 0, "the master must expose terminal attributes");
    assert_ne!(term.c_lflag & libc::ISIG, 0, "interrupt processing must stay on");

    let pid = i32::try_from(child_pid).unwrap();
    // SAFETY: `pid` came from `child.id()` on a live child, and a negative pid
    // targets that child's process group.
    assert_eq!(
        unsafe { libc::kill(-pid, libc::SIGINT) },
        0,
        "the child's process group must accept SIGINT"
    );

    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("the child should exit on a group SIGINT")
        .expect("wait for the pty child");
    assert!(!status.success(), "SIGINT must terminate the child: {status:?}");
    assert!(
        !crate::platform::is_process_running(child_pid),
        "the signalled child must be gone"
    );
    assert!(
        crate::platform::is_process_running(std::process::id()),
        "a group SIGINT for the child must never reach the parent"
    );
}

fn make_ctx(stdin_tx: Option<mpsc::UnboundedSender<StdinInputRequest>>) -> ToolContext {
    ToolContext {
        session_id: "test-session".to_string(),
        message_id: "test-msg".to_string(),
        tool_call_id: "test-call".to_string(),
        working_dir: Some(std::path::PathBuf::from("/tmp")),
        stdin_request_tx: stdin_tx,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

fn make_agent_ctx(signal: horon_agent_agent_runtime::InterruptSignal) -> ToolContext {
    ToolContext {
        session_id: "test-session".to_string(),
        message_id: "test-msg".to_string(),
        tool_call_id: "test-call-agent".to_string(),
        working_dir: Some(std::path::PathBuf::from("/tmp")),
        stdin_request_tx: None,
        graceful_shutdown_signal: Some(signal),
        execution_mode: crate::tool::ToolExecutionMode::AgentTurn,
    }
}

#[tokio::test]
async fn test_basic_command_no_stdin() {
    let tool = BashTool::new();
    let input = json!({"command": "echo hello"});
    let ctx = make_ctx(None);
    let result = tool.execute(input, ctx).await.unwrap();
    assert!(result.output.contains("hello"));
}

#[tokio::test]
async fn test_basic_command_with_unused_stdin_channel() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let tool = BashTool::new();
    let input = json!({"command": "echo world"});
    let ctx = make_ctx(Some(tx));
    let result = tool.execute(input, ctx).await.unwrap();
    assert!(result.output.contains("world"));
}

#[tokio::test]
async fn test_stdin_forwarding_single_line() {
    let (tx, mut rx) = mpsc::unbounded_channel::<StdinInputRequest>();
    let tool = BashTool::new();

    // "head -n1" reads one line from stdin and prints it
    let input = json!({"command": "head -n1", "timeout": 10000});
    let ctx = make_ctx(Some(tx));

    // Spawn the tool execution
    let tool_handle = tokio::spawn(async move { tool.execute(input, ctx).await });

    // Wait for the stdin request to arrive
    let req = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for stdin request")
        .expect("channel closed");

    assert!(req.request_id.starts_with("stdin-test-call-"));
    assert!(!req.is_password);

    // Send the response
    req.response_tx.send("test_input_line".to_string()).unwrap();

    // Wait for tool to finish
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), tool_handle)
        .await
        .expect("tool timed out")
        .expect("tool panicked")
        .expect("tool errored");

    assert!(
        result.output.contains("test_input_line"),
        "output should contain the input we sent: {}",
        result.output
    );
}

#[tokio::test]
async fn test_stdin_forwarding_multiple_lines() {
    let (tx, mut rx) = mpsc::unbounded_channel::<StdinInputRequest>();
    let tool = BashTool::new();

    // "head -n2" reads two lines
    let input = json!({"command": "head -n2", "timeout": 15000});
    let ctx = make_ctx(Some(tx));

    let tool_handle = tokio::spawn(async move { tool.execute(input, ctx).await });

    // First line
    let req1 = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for first stdin request")
        .expect("channel closed");
    assert!(
        req1.request_id.ends_with("-1"),
        "first request should end with -1: {}",
        req1.request_id
    );
    req1.response_tx.send("line_one".to_string()).unwrap();

    // Second line
    let req2 = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for second stdin request")
        .expect("channel closed");
    assert!(
        req2.request_id.ends_with("-2"),
        "second request should end with -2: {}",
        req2.request_id
    );
    req2.response_tx.send("line_two".to_string()).unwrap();

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), tool_handle)
        .await
        .expect("tool timed out")
        .expect("tool panicked")
        .expect("tool errored");

    assert!(
        result.output.contains("line_one"),
        "missing line_one in: {}",
        result.output
    );
    assert!(
        result.output.contains("line_two"),
        "missing line_two in: {}",
        result.output
    );
}

#[tokio::test]
async fn test_stdin_not_triggered_for_non_blocking_command() {
    let (tx, mut rx) = mpsc::unbounded_channel::<StdinInputRequest>();
    let tool = BashTool::new();

    // This command doesn't read stdin at all
    let input = json!({"command": "echo no_stdin_needed", "timeout": 5000});
    let ctx = make_ctx(Some(tx));

    let result = tool.execute(input, ctx).await.unwrap();
    assert!(result.output.contains("no_stdin_needed"));

    // No stdin request should have been sent
    assert!(
        rx.try_recv().is_err(),
        "no stdin request should be sent for a command that doesn't read stdin"
    );
}

#[tokio::test]
async fn test_command_timeout_with_stdin_channel() {
    let (tx, _rx) = mpsc::unbounded_channel::<StdinInputRequest>();
    let tool = BashTool::new();

    // `cat` blocks forever on stdin. With a short timeout and no stdin response,
    // the command should be promoted to the background (kept running), not killed
    // with an error.
    let input = json!({"command": "cat", "timeout": 1000});
    let ctx = make_ctx(Some(tx));

    let result = tool
        .execute(input, ctx)
        .await
        .expect("timeout should promote to background, not error");
    assert!(
        result.output.contains("continuing in background"),
        "output should explain background promotion: {}",
        result.output
    );
    let metadata = result.metadata.expect("expected background metadata");
    assert_eq!(metadata["background"], true);
    assert_eq!(metadata["timeout_promoted"], true);
    assert_eq!(metadata["foreground_timeout_ms"], 1000);

    // Clean up the still-running background task so it does not linger.
    let task_id = metadata["task_id"].as_str().expect("task_id should be present");
    let _ = crate::background::global().cancel(task_id).await;
}

#[tokio::test]
async fn test_foreground_timeout_promotes_and_command_keeps_running() {
    let tool = BashTool::new();
    // No stdin channel and Direct mode -> plain foreground path. The command runs
    // longer than the timeout, so it should be promoted to background and keep
    // running to completion rather than being killed at the timeout.
    let input = json!({"command": "sleep 0.5; echo fg_promote_ok", "timeout": 100});
    let ctx = make_ctx(None);

    let result = tool
        .execute(input, ctx)
        .await
        .expect("timeout should promote the still-running command to background");
    assert!(
        result.output.contains("continuing in background"),
        "output should explain background promotion: {}",
        result.output
    );
    let metadata = result.metadata.expect("expected background metadata");
    assert_eq!(metadata["background"], true);
    assert_eq!(metadata["timeout_promoted"], true);
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task_id should be present")
        .to_string();

    // Promotion must retain an output artifact before returning, even though
    // this in-process path only fills it once the command finishes.
    assert!(
        crate::background::global().output(&task_id).await.is_some(),
        "output artifact should exist immediately after promotion"
    );

    // Wait for the promoted command to finish on its own.
    let mut final_status = None;
    for _ in 0..40 {
        if let Some(status) = crate::background::global().status(&task_id).await
            && status.status != BackgroundTaskStatus::Running
        {
            final_status = Some(status);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let status = final_status.expect("promoted background task should finish");
    assert_eq!(status.status, BackgroundTaskStatus::Completed);

    let output = crate::background::global()
        .output(&task_id)
        .await
        .expect("output should exist");
    assert!(
        output.contains("fg_promote_ok"),
        "command should have continued after foreground timeout: {output}"
    );
}

#[tokio::test]
async fn test_reload_persistable_bash_continues_in_background() {
    let tool = BashTool::new();
    let signal = horon_agent_agent_runtime::InterruptSignal::new();
    let ctx = make_agent_ctx(signal.clone());

    let signal_task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        signal.fire();
    });

    let result = tool
        .execute(
            json!({"command": "sleep 1; echo reload_persist_ok", "timeout": 10000}),
            ctx,
        )
        .await
        .expect("reload-persistable command should succeed");
    signal_task.await.expect("signal task should complete");

    let metadata = result.metadata.expect("expected background metadata");
    assert_eq!(metadata["background"], true);
    assert_eq!(metadata["reload_persisted"], true);
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task_id should be present")
        .to_string();
    let output_file =
        std::path::PathBuf::from(metadata["output_file"].as_str().expect("output_file should be present"));
    let status_file =
        std::path::PathBuf::from(metadata["status_file"].as_str().expect("status_file should be present"));

    tokio::time::sleep(std::time::Duration::from_millis(1400)).await;

    let status = crate::background::global()
        .status(&task_id)
        .await
        .expect("status should exist");
    assert_eq!(status.status, BackgroundTaskStatus::Completed);
    let output = crate::background::global()
        .output(&task_id)
        .await
        .expect("output should exist");
    assert!(output.contains("reload_persist_ok"), "output was: {output}");

    let _ = tokio::fs::remove_file(output_file).await;
    let _ = tokio::fs::remove_file(status_file).await;
}

#[tokio::test]
async fn test_reload_persistable_bash_timeout_promotes_to_background() {
    let tool = BashTool::new();
    let signal = horon_agent_agent_runtime::InterruptSignal::new();
    let ctx = make_agent_ctx(signal);

    let result = tool
        .execute(
            json!({"command": "sleep 0.4; echo timeout_promote_ok", "timeout": 100}),
            ctx,
        )
        .await
        .expect("timeout should promote the still-running command to background");

    assert!(
        result.output.contains("continuing in background"),
        "output should explain background promotion: {}",
        result.output
    );
    assert!(
        result.output.contains("do not rerun"),
        "output should tell the agent not to rerun duplicate work: {}",
        result.output
    );

    let metadata = result.metadata.expect("expected background metadata");
    assert_eq!(metadata["background"], true);
    assert_eq!(metadata["timeout_promoted"], true);
    assert_eq!(metadata["foreground_timeout_ms"], 100);
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task_id should be present")
        .to_string();
    let output_file =
        std::path::PathBuf::from(metadata["output_file"].as_str().expect("output_file should be present"));
    let status_file =
        std::path::PathBuf::from(metadata["status_file"].as_str().expect("status_file should be present"));

    let initial_status = crate::background::global()
        .status(&task_id)
        .await
        .expect("status should exist");
    assert_eq!(initial_status.status, BackgroundTaskStatus::Running);

    let mut final_status = None;
    for _ in 0..40 {
        let status = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if status.status != BackgroundTaskStatus::Running {
            final_status = Some(status);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let status = final_status.expect("promoted background task should finish");
    assert_eq!(status.status, BackgroundTaskStatus::Completed);
    assert_eq!(status.exit_code, Some(0));

    let output = crate::background::global()
        .output(&task_id)
        .await
        .expect("output should exist");
    assert!(
        output.contains("timeout_promote_ok"),
        "command should have continued after foreground timeout: {output}"
    );

    let _ = tokio::fs::remove_file(output_file).await;
    let _ = tokio::fs::remove_file(status_file).await;
}

#[tokio::test]
async fn test_stderr_captured_with_stdin() {
    let (tx, _rx) = mpsc::unbounded_channel::<StdinInputRequest>();
    let tool = BashTool::new();

    let input = json!({"command": "echo stderr_msg >&2", "timeout": 5000});
    let ctx = make_ctx(Some(tx));

    let result = tool.execute(input, ctx).await.unwrap();
    assert!(
        result.output.contains("stderr_msg"),
        "stderr should be captured: {}",
        result.output
    );
}

#[path = "bash_tests/progress_parsing.rs"]
mod progress_parsing;

#[tokio::test]
async fn test_background_command_progress_marker_updates_status_and_stays_out_of_output() {
    let tool = BashTool::new();
    let ctx = make_ctx(None);

    let result = tool
            .execute(
                json!({
                    "command": "printf '%s\n' 'HORON_AGENT_PROGRESS {\"current\":3,\"total\":10,\"unit\":\"steps\",\"message\":\"Building\"}'; sleep 0.1; echo done",
                    "run_in_background": true,
                    "notify": false,
                    "wake": false,
                }),
                ctx,
            )
            .await
            .expect("background command should start");

    let metadata = result.metadata.expect("expected metadata");
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task id should be present")
        .to_string();

    let mut saw_progress = false;
    // Wall-clock deadline: observing emitted progress depends on scheduler
    // latency, so a fixed 50-iteration budget starved under parallel load
    // (issue #593). The assertions inside stay exact.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let status = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if let Some(progress) = status.progress {
            saw_progress = true;
            assert_eq!(progress.current, Some(3));
            assert_eq!(progress.total, Some(10));
            assert_eq!(progress.unit.as_deref(), Some("steps"));
            assert_eq!(progress.message.as_deref(), Some("Building"));
            assert_eq!(progress.percent, Some(30.0));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(saw_progress, "expected progress to be recorded for {task_id}");

    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let output = crate::background::global()
        .output(&task_id)
        .await
        .expect("output should exist");
    assert!(output.contains("done"), "output was: {output}");
    assert!(
        !output.contains("HORON_AGENT_PROGRESS"),
        "progress marker should be hidden from output: {output}"
    );
}

#[tokio::test]
async fn test_background_command_ratio_output_updates_progress() {
    let tool = BashTool::new();
    let ctx = make_ctx(None);

    let result = tool
        .execute(
            json!({
                "command": "printf '%s\n' 'Running test 4/8 tests'; sleep 0.1; echo done",
                "run_in_background": true,
                "notify": false,
                "wake": false,
            }),
            ctx,
        )
        .await
        .expect("background command should start");

    let metadata = result.metadata.expect("expected metadata");
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task id should be present")
        .to_string();

    let mut saw_progress = false;
    // Wall-clock deadline: observing emitted progress depends on scheduler
    // latency, so a fixed 50-iteration budget starved under parallel load
    // (issue #593). The assertions inside stay exact.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let status = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if let Some(progress) = status.progress {
            saw_progress = true;
            assert_eq!(progress.current, Some(4));
            assert_eq!(progress.total, Some(8));
            assert_eq!(progress.percent, Some(50.0));
            assert_eq!(progress.unit.as_deref(), Some("tests"));
            assert_eq!(progress.source, BackgroundTaskProgressSource::ParsedOutput);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    assert!(saw_progress, "expected heuristic progress to be recorded for {task_id}");
}

#[tokio::test]
async fn test_background_command_byte_ratio_output_updates_progress() {
    let tool = BashTool::new();
    let ctx = make_ctx(None);

    let result = tool
        .execute(
            json!({
                "command": "printf '%s\n' 'Downloaded 1.5/3.0 GiB'; sleep 0.1; echo done",
                "run_in_background": true,
                "notify": false,
                "wake": false,
            }),
            ctx,
        )
        .await
        .expect("background command should start");

    let metadata = result.metadata.expect("expected metadata");
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task id should be present")
        .to_string();

    let mut saw_progress = false;
    // Wall-clock deadline: observing emitted progress depends on scheduler
    // latency, so a fixed 50-iteration budget starved under parallel load
    // (issue #593). The assertions inside stay exact.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let status = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if let Some(progress) = status.progress {
            saw_progress = true;
            assert_eq!(progress.percent, Some(50.0));
            assert_eq!(progress.unit.as_deref(), Some("gib"));
            assert_eq!(progress.source, BackgroundTaskProgressSource::ParsedOutput);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    assert!(
        saw_progress,
        "expected byte-ratio progress to be recorded for {task_id}"
    );
}

#[tokio::test]
async fn test_background_command_respects_timeout() {
    let tool = BashTool::new();
    let ctx = make_ctx(None);

    let result = tool
        .execute(
            json!({
                "command": "sleep 5; echo should_not_print",
                "run_in_background": true,
                "timeout": 100,
                "notify": false,
                "wake": false,
            }),
            ctx,
        )
        .await
        .expect("background command should start");

    let metadata = result.metadata.expect("expected metadata");
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task id should be present")
        .to_string();

    let mut final_status = None;
    // Wall-clock deadline rather than a fixed iteration count. The command's own
    // timeout is 100ms, but the *observation* of the resulting Failed status
    // depends on scheduler latency, and a 50 x 50ms budget starved when the full
    // suite runs in parallel on a loaded machine (issue #593). A generous
    // deadline keeps the assertion strict while removing the timing race: a real
    // regression still fails, it just is not reported as a flake.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let status = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if status.status == BackgroundTaskStatus::Failed {
            final_status = Some(status);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let status = final_status.expect("background task should fail after timeout");
    assert_eq!(status.exit_code, Some(124));
    assert!(
        status.error.as_deref().unwrap_or_default().contains("timed out"),
        "timeout failure should be recorded: {status:?}"
    );

    let output = crate::background::global()
        .output(&task_id)
        .await
        .expect("output should exist");
    assert!(output.contains("timed out after 100ms"), "output was: {output}");
    assert!(
        !output.contains("should_not_print"),
        "timed-out command should not complete normally: {output}"
    );
}

#[tokio::test]
async fn test_background_command_without_timeout_keeps_running_past_default_foreground_timeout() {
    let tool = BashTool::new();
    let ctx = make_ctx(None);

    let result = tool
        .execute(
            json!({
                "command": "sleep 0.25; echo background_no_implicit_timeout_ok",
                "run_in_background": true,
                "notify": false,
                "wake": false,
            }),
            ctx,
        )
        .await
        .expect("background command should start");

    let metadata = result.metadata.expect("expected metadata");
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task id should be present")
        .to_string();
    let output_file =
        std::path::PathBuf::from(metadata["output_file"].as_str().expect("output_file should be present"));
    let status_file =
        std::path::PathBuf::from(metadata["status_file"].as_str().expect("status_file should be present"));

    let mut final_status = None;
    for _ in 0..30 {
        let status = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if status.status != BackgroundTaskStatus::Running {
            final_status = Some(status);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    let status = final_status.expect("background task should finish normally");
    assert_eq!(status.status, BackgroundTaskStatus::Completed);
    assert_eq!(status.exit_code, Some(0));

    let output = crate::background::global()
        .output(&task_id)
        .await
        .expect("output should exist");
    assert!(
        output.contains("background_no_implicit_timeout_ok"),
        "output was: {output}"
    );

    let _ = tokio::fs::remove_file(output_file).await;
    let _ = tokio::fs::remove_file(status_file).await;
}

#[cfg(unix)]
#[tokio::test]
async fn process_group_kill_guard_terminates_descendants() {
    let mut cmd = build_shell_command("sleep 60 & echo $!; wait", false);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.kill_on_drop(true).stdout(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn process group probe");
    let mut lines = BufReader::new(child.stdout.take().expect("probe stdout")).lines();
    let descendant_pid = lines
        .next_line()
        .await
        .expect("read descendant pid")
        .expect("descendant pid line")
        .parse::<u32>()
        .expect("numeric descendant pid");

    let guard = ProcessGroupKillGuard::new(child.id());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .expect("shell should exit after process-group kill")
        .expect("wait for shell");

    // Wall-clock deadline: process teardown is asynchronous and 100 x 10ms was
    // too tight under parallel load (issue #593).
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if !crate::platform::is_process_running(descendant_pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("descendant process {descendant_pid} survived process-group cleanup");
}

#[test]
fn test_bash_tool_schema_advertises_background_progress_guidance() {
    let schema = BashTool::new().parameters_schema();
    let command_description = schema["properties"]["command"]["description"]
        .as_str()
        .expect("command description should be a string");
    let background_description = schema["properties"]["run_in_background"]["description"]
        .as_str()
        .expect("run_in_background description should be a string");

    assert!(
        command_description.contains("HORON_AGENT_SCRATCH_DIR"),
        "command description should keep the scratch-dir guidance"
    );
    assert!(
        background_description.contains("HORON_AGENT_PROGRESS"),
        "background description should mention the progress marker format"
    );
}

// Destructive-command gate integration (#604).
//
// The unit-level policy is covered in horon-agent-command-risk. These tests pin the
// wiring: that the gate actually sits in the bash tool's execute path, that it
// refuses before spawning a process, and that it does not disturb normal work.

/// Each gate test passes its own session id. The bash tool remembers the
/// directory a session's last call ended in and starts the next call there, so
/// a session id shared across these parallel tests let one test start in
/// another test's temp dir after that test had deleted it (ENOENT on spawn), or
/// run its commands in the wrong directory.
fn gate_ctx(session_id: &str, working_dir: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "m".to_string(),
        tool_call_id: "c".to_string(),
        working_dir: Some(std::path::PathBuf::from(working_dir)),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

#[tokio::test]
async fn bash_refuses_to_delete_the_home_directory() {
    // The #604 incident, at the real tool boundary.
    //
    // The gate resolves HOME from the process environment, so both this test
    // and `indirect_dispatch_paths_cannot_bypass_the_gate` mutate a global that
    // the other can observe mid-run. Serialize them on the shared env lock, the
    // same way the other env-sensitive tool tests do.
    let _env_guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().to_string_lossy().to_string();
    let previous = std::env::var("HOME").ok();
    // SAFETY: single-threaded test setup; restored below.
    unsafe { std::env::set_var("HOME", &home) };

    let canary = temp.path().join("precious.txt");
    std::fs::write(&canary, "user data").expect("write canary");

    let result = BashTool::new()
        .execute(
            serde_json::json!({ "command": format!("rm -rf {home}") }),
            gate_ctx("gate-test-home", "/tmp"),
        )
        .await;

    match previous {
        Some(value) => unsafe { std::env::set_var("HOME", value) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    let error = result.expect_err("deleting HOME must be refused");
    assert!(
        error.to_string().contains("blocked"),
        "expected an outright block, got: {error}"
    );
    assert!(
        canary.exists(),
        "the gate must refuse before the process runs; the file was deleted"
    );
}

#[tokio::test]
async fn bash_holds_a_risky_delete_until_justified_then_runs_it() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workdir = temp.path().join("work");
    let target = temp.path().join("outside");
    std::fs::create_dir_all(&workdir).expect("workdir");
    std::fs::create_dir_all(&target).expect("target");
    std::fs::write(target.join("f.txt"), "x").expect("file");

    // The concrete outside-workspace directory is allowed by policy. Its glob
    // keeps this test focused on the Confirm path for a statically unknown set
    // of affected files.
    let command = format!("rm -rf {}/* && rmdir {}", target.display(), target.display());
    let tool = BashTool::new();

    // First attempt: no justification, so it is held.
    let held = tool
        .execute(
            serde_json::json!({ "command": command }),
            gate_ctx("gate-test-risky-delete", workdir.to_str().expect("utf8")),
        )
        .await
        .expect_err("first attempt should be held");
    assert!(held.to_string().contains("justification"), "{held}");
    assert!(target.exists(), "nothing should have been deleted yet");

    // A blind retry is held identically: repetition is not consent.
    let retried = tool
        .execute(
            serde_json::json!({ "command": command }),
            gate_ctx("gate-test-risky-delete", workdir.to_str().expect("utf8")),
        )
        .await
        .expect_err("a blind retry should still be held");
    assert!(retried.to_string().contains("justification"));
    assert!(target.exists());

    // With a real justification it proceeds.
    tool.execute(
        serde_json::json!({
            "command": command,
            "justification": "The user asked me to remove the outside/ fixture \
                              directory they created earlier in this session.",
        }),
        gate_ctx("gate-test-risky-delete", workdir.to_str().expect("utf8")),
    )
    .await
    .expect("a justified command should run");
    assert!(!target.exists(), "the justified delete should have run");
}

#[tokio::test]
async fn bash_does_not_interfere_with_ordinary_commands() {
    // If the gate fires on routine work it will be worked around, so this is a
    // load-bearing test, not a formality.
    let temp = tempfile::tempdir().expect("temp dir");
    let workdir = temp.path().to_str().expect("utf8");
    std::fs::create_dir_all(temp.path().join("build")).expect("build dir");

    for command in ["echo hello", "rm -rf build", "ls -la"] {
        BashTool::new()
            .execute(
                serde_json::json!({ "command": command }),
                gate_ctx("gate-test-ordinary", workdir),
            )
            .await
            .unwrap_or_else(|e| panic!("{command:?} should run untouched: {e}"));
    }
}

#[tokio::test]
async fn indirect_dispatch_paths_cannot_bypass_the_gate() {
    // batch, and every other caller, dispatch through Tool::execute rather than
    // reimplementing it, so the gate lives at the only chokepoint. Assert that
    // directly: calling execute for a background job (the one path that returns
    // early) is still gated.
    let _env_guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().to_string_lossy().to_string();
    let previous = std::env::var("HOME").ok();
    // SAFETY: single-threaded test setup; restored below.
    unsafe { std::env::set_var("HOME", &home) };
    let canary = temp.path().join("precious.txt");
    std::fs::write(&canary, "user data").expect("canary");

    let result = BashTool::new()
        .execute(
            serde_json::json!({
                "command": format!("rm -rf {home}"),
                "run_in_background": true,
            }),
            gate_ctx("gate-test-indirect", "/tmp"),
        )
        .await;

    match previous {
        Some(value) => unsafe { std::env::set_var("HOME", value) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    assert!(
        result.is_err(),
        "background dispatch must be gated too, not just foreground"
    );
    assert!(canary.exists(), "the file must survive a backgrounded call");
}

#[test]
fn parse_progress_line_classifies_markers_checkpoints_and_heuristics() {
    let update = parse_progress_line(r#"HORON_AGENT_PROGRESS {"percent":40,"message":"Working"}"#)
        .expect("parser should not fail")
        .expect("progress marker should parse");
    match update {
        ProgressLineUpdate::Progress(progress) => assert_eq!(progress.percent, Some(40.0)),
        other => panic!("expected a progress update, got {other:?}"),
    }

    let update = parse_progress_line(r#"HORON_AGENT_CHECKPOINT {"message":"Tests passed"}"#)
        .expect("parser should not fail")
        .expect("checkpoint marker should parse");
    match update {
        ProgressLineUpdate::Checkpoint(progress) => {
            assert_eq!(progress.message.as_deref(), Some("Tests passed"))
        }
        other => panic!("expected a checkpoint update, got {other:?}"),
    }

    let update = parse_progress_line("Copied 7/10 files")
        .expect("parser should not fail")
        .expect("heuristic ratio should parse");
    match update {
        ProgressLineUpdate::Progress(progress) => {
            assert_eq!(progress.percent, Some(70.0));
            assert_eq!(progress.source, BackgroundTaskProgressSource::ParsedOutput);
        }
        other => panic!("expected a progress update, got {other:?}"),
    }

    assert!(
        parse_progress_line("plain log line with no progress")
            .expect("parser should not fail")
            .is_none(),
        "non-progress output must not produce updates"
    );
}

/// The bug this guards against: a foreground command promoted to background at
/// the timeout showed 0% until it completed, because nothing parsed its output
/// for progress. Both the update emitted *before* promotion and updates
/// emitted *after* promotion must reach the background task's status.
#[tokio::test]
async fn test_timeout_promoted_command_reports_intermediate_progress() {
    let tool = BashTool::new();
    // Emits 10% before the 300ms foreground timeout, then 80% about 2s in.
    let input = json!({
        "command": "echo 'progress 10% done'; sleep 2; echo 'progress 80% done'; sleep 1",
        "timeout": 300,
    });
    let ctx = make_ctx(None);

    let result = tool
        .execute(input, ctx)
        .await
        .expect("timeout should promote to background");
    let metadata = result.metadata.expect("expected background metadata");
    assert_eq!(metadata["timeout_promoted"], true);
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task_id should be present")
        .to_string();

    // The pre-promotion update (10%) must be attached at promotion time, and
    // the post-promotion update (80%) must stream in while still running.
    let mut observed: Vec<f32> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let status = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if let Some(percent) = status.progress.as_ref().and_then(|p| p.percent)
            && observed.last() != Some(&percent)
        {
            observed.push(percent);
        }
        if observed.contains(&80.0) {
            break;
        }
        if status.status != BackgroundTaskStatus::Running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    assert!(
        observed.contains(&80.0),
        "promoted task should reach 80% via parsed output, saw {observed:?}"
    );
    assert!(
        observed.contains(&10.0),
        "the pre-promotion 10% update should be flushed at promotion, saw {observed:?}"
    );

    let _ = crate::background::global().cancel(&task_id).await;
}

/// Same guarantee for the reload-persistable (detached) path: the command
/// writes straight to its output file, so a follower must translate progress
/// lines into status updates while the task is still running.
#[tokio::test]
async fn test_detached_promoted_command_reports_intermediate_progress() {
    let tool = BashTool::new();
    let signal = horon_agent_agent_runtime::InterruptSignal::new();
    let ctx = make_agent_ctx(signal);

    let result = tool
        .execute(
            json!({
                "command": "sleep 0.5; echo 'done 3/10 steps'; sleep 2; echo 'done 8/10 steps'; sleep 1",
                "timeout": 200,
            }),
            ctx,
        )
        .await
        .expect("timeout should promote the detached command to background");
    let metadata = result.metadata.expect("expected background metadata");
    assert_eq!(metadata["timeout_promoted"], true);
    let task_id = metadata["task_id"]
        .as_str()
        .expect("task_id should be present")
        .to_string();

    let mut observed: Vec<f32> = Vec::new();
    let mut saw_intermediate_while_running = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let status = crate::background::global()
            .status(&task_id)
            .await
            .expect("status should exist");
        if let Some(percent) = status.progress.as_ref().and_then(|p| p.percent) {
            if observed.last() != Some(&percent) {
                observed.push(percent);
            }
            if status.status == BackgroundTaskStatus::Running && percent < 100.0 {
                saw_intermediate_while_running = true;
            }
        }
        if observed.contains(&80.0) {
            break;
        }
        if status.status != BackgroundTaskStatus::Running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert!(
        observed.contains(&30.0) && observed.contains(&80.0),
        "detached task should report 30% then 80% from parsed output, saw {observed:?}"
    );
    assert!(
        saw_intermediate_while_running,
        "intermediate progress must be visible while the task is still running"
    );

    let output_file =
        std::path::PathBuf::from(metadata["output_file"].as_str().expect("output_file should be present"));
    let status_file =
        std::path::PathBuf::from(metadata["status_file"].as_str().expect("status_file should be present"));
    let _ = crate::background::global().cancel(&task_id).await;
    let _ = tokio::fs::remove_file(output_file).await;
    let _ = tokio::fs::remove_file(status_file).await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_preserves_quoting_and_non_utf8_arguments() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let output = BashTool::new()
        .execute(
            json!({
                "command": "printf '%s\\n' \"a b\" 'c \"d\"' && printf 'x\\377y\\n' | od -An -c | tr -s ' '",
                "pty": "always"
            }),
            make_ctx(Some(tx)),
        )
        .await
        .unwrap();
    assert!(
        output.output.contains("a b") && output.output.contains("c \"d\""),
        "quoted arguments must reach the command intact: {}",
        output.output
    );
    assert!(
        output.output.contains("377") || output.output.contains("xff") || output.output.contains("y"),
        "a non-UTF8 byte must survive the terminal path: {}",
        output.output
    );
}

// Session working directory between calls ([tools] bash_session_cwd).
//
// The switch and the per-session map are process-wide, so these tests take one
// shared lock and each owns its session id and its own temp tree.

fn cwd_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cwd_ctx(session_id: &str, working_dir: &std::path::Path) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "cwd-message".to_string(),
        tool_call_id: "cwd-call".to_string(),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

fn real_path(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn printed_path(output: &str) -> std::path::PathBuf {
    let last = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| panic!("command printed nothing: {output}"));
    real_path(std::path::Path::new(last))
}

fn cwd_tree() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("temp root");
    let alpha = root.path().join("alpha");
    let beta = root.path().join("beta");
    std::fs::create_dir(&alpha).expect("alpha dir");
    std::fs::create_dir(&beta).expect("beta dir");
    (root, alpha, beta)
}

#[tokio::test]
async fn cwd_change_persists_to_the_next_call() {
    let _lock = cwd_test_lock();
    let session = "cwd-persist-session";
    forget_bash_session_cwd(session);
    let (root, alpha, _beta) = cwd_tree();
    let tool = BashTool::new();

    tool.execute(
        json!({"command": format!("cd {}", alpha.display())}),
        cwd_ctx(session, root.path()),
    )
    .await
    .expect("cd call");

    let printed = tool
        .execute(json!({"command": "pwd"}), cwd_ctx(session, root.path()))
        .await
        .expect("pwd call");
    assert_eq!(printed_path(&printed.output), real_path(&alpha));

    forget_bash_session_cwd(session);
}

#[tokio::test]
async fn failed_cd_does_not_move_the_session_cwd() {
    let _lock = cwd_test_lock();
    let session = "cwd-failed-cd-session";
    forget_bash_session_cwd(session);
    let (root, alpha, _beta) = cwd_tree();
    let tool = BashTool::new();

    tool.execute(
        json!({"command": format!("cd {}", alpha.display())}),
        cwd_ctx(session, root.path()),
    )
    .await
    .expect("cd call");

    let missing = root.path().join("missing");
    tool.execute(
        json!({"command": format!("cd {}", missing.display())}),
        cwd_ctx(session, root.path()),
    )
    .await
    .expect("a failed cd still returns output");

    let printed = tool
        .execute(json!({"command": "pwd"}), cwd_ctx(session, root.path()))
        .await
        .expect("pwd call");
    assert_eq!(printed_path(&printed.output), real_path(&alpha));

    forget_bash_session_cwd(session);
}

/// A failed `cd` leaves the shell where it was, so the directory it reports still
/// exists and the existence guard never decides anything. The case the guard
/// exists for is a call that enters a directory and removes it: bash still
/// prints the dead path, so without the guard the session would remember a
/// directory that no longer exists.
#[tokio::test]
async fn removed_directory_is_not_remembered() {
    let _lock = cwd_test_lock();
    let session = "cwd-removed-dir-session";
    forget_bash_session_cwd(session);
    let (root, _alpha, _beta) = cwd_tree();
    let tool = BashTool::new();

    let doomed = root.path().join("doomed");
    std::fs::create_dir(&doomed).expect("doomed dir");
    let command = format!("cd {} && rmdir {}", doomed.display(), doomed.display());
    tool.execute(json!({"command": command}), cwd_ctx(session, root.path()))
        .await
        .expect("the call itself succeeds");
    assert!(!doomed.exists(), "the command must really remove the directory");

    assert_eq!(
        bash_session_cwd(session),
        None,
        "a directory the same call removed must never be remembered"
    );

    let printed = tool
        .execute(json!({"command": "pwd"}), cwd_ctx(session, root.path()))
        .await
        .expect("the next call still spawns");
    assert_eq!(
        printed_path(&printed.output),
        real_path(root.path()),
        "the session must stay where it was instead of the dead path"
    );

    forget_bash_session_cwd(session);
}

#[tokio::test]
async fn explicit_cwd_argument_overrides_the_remembered_one() {
    let _lock = cwd_test_lock();
    let session = "cwd-explicit-session";
    forget_bash_session_cwd(session);
    let (root, alpha, beta) = cwd_tree();
    let tool = BashTool::new();

    tool.execute(
        json!({"command": format!("cd {}", alpha.display())}),
        cwd_ctx(session, root.path()),
    )
    .await
    .expect("cd call");

    let explicit = tool
        .execute(
            json!({"command": "pwd", "cwd": beta.to_string_lossy()}),
            cwd_ctx(session, root.path()),
        )
        .await
        .expect("pwd call with cwd");
    assert_eq!(printed_path(&explicit.output), real_path(&beta));

    forget_bash_session_cwd(session);
}

#[tokio::test]
async fn disabled_switch_keeps_every_call_independent() {
    let _lock = cwd_test_lock();
    let session = "cwd-disabled-session";
    forget_bash_session_cwd(session);
    let (root, alpha, _beta) = cwd_tree();
    let tool = BashTool::new();

    set_bash_session_cwd_test_override(Some(false));
    let result = async {
        tool.execute(
            json!({"command": format!("cd {} && pwd", alpha.display())}),
            cwd_ctx(session, root.path()),
        )
        .await
        .expect("cd call");

        let printed = tool
            .execute(json!({"command": "pwd"}), cwd_ctx(session, root.path()))
            .await
            .expect("pwd call");
        printed_path(&printed.output)
    }
    .await;
    set_bash_session_cwd_test_override(None);

    assert_eq!(
        result,
        real_path(root.path()),
        "the second call must not see the first call's cd"
    );
    assert_eq!(
        bash_session_cwd(session),
        None,
        "a disabled switch must not remember anything"
    );
}

#[tokio::test]
async fn deleted_directory_is_not_remembered() {
    let _lock = cwd_test_lock();
    let session = "cwd-deleted-session";
    forget_bash_session_cwd(session);
    let (root, alpha, _beta) = cwd_tree();
    let tool = BashTool::new();

    tool.execute(
        json!({"command": format!("cd {}", alpha.display())}),
        cwd_ctx(session, root.path()),
    )
    .await
    .expect("cd call");

    std::fs::remove_dir_all(&alpha).expect("remove alpha");

    let printed = tool
        .execute(json!({"command": "pwd"}), cwd_ctx(session, root.path()))
        .await
        .expect("a removed remembered directory must fall back, not fail to spawn");
    assert_eq!(printed_path(&printed.output), real_path(root.path()));

    forget_bash_session_cwd(session);
}

#[tokio::test]
async fn background_call_does_not_corrupt_the_session_cwd() {
    let _lock = cwd_test_lock();
    let session = "cwd-background-session";
    forget_bash_session_cwd(session);
    let (root, alpha, beta) = cwd_tree();
    let tool = BashTool::new();

    tool.execute(
        json!({"command": format!("cd {}", alpha.display())}),
        cwd_ctx(session, root.path()),
    )
    .await
    .expect("cd call");

    let remembered = bash_session_cwd(session).expect("the first call must be remembered");
    assert_eq!(real_path(&remembered), real_path(&alpha));
    // The background call returns as soon as the task is spawned, so nothing
    // waits for it: the remembered directory must already be final here.
    tool.execute(
        json!({"command": format!("cd {}", beta.display()), "run_in_background": true}),
        cwd_ctx(session, root.path()),
    )
    .await
    .expect("background call");

    assert_eq!(bash_session_cwd(session), Some(remembered));

    forget_bash_session_cwd(session);
}

/// ACP terminal delegation: the daemon hands a foreground call to the editor's
/// terminal instead of spawning it, and every created terminal is released.
mod bash_client_terminal {
    use super::*;
    use crate::protocol::{AcpClientCapabilities, ClientRpcError, ServerEvent};
    use crate::tool::{acp_session, client_bridge};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;

    const CONNECTION: &str = "conn-terminal";

    fn terminal_caps() -> AcpClientCapabilities {
        AcpClientCapabilities {
            terminal: true,
            ..AcpClientCapabilities::default()
        }
    }

    fn fs_only_caps() -> AcpClientCapabilities {
        AcpClientCapabilities {
            fs_read_text_file: true,
            ..AcpClientCapabilities::default()
        }
    }

    #[derive(Clone, Copy, Default)]
    struct Behaviour {
        fail_create: bool,
        never_exit: bool,
    }

    fn terminal_ctx(session: &str, working_dir: &Path) -> ToolContext {
        ToolContext {
            session_id: session.to_string(),
            message_id: "terminal-message".to_string(),
            tool_call_id: "terminal-call".to_string(),
            working_dir: Some(working_dir.to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        }
    }

    /// `terminal_ctx` with the stdin channel a daemon connection always installs.
    fn daemon_ctx(session: &str) -> (ToolContext, mpsc::UnboundedReceiver<StdinInputRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut ctx = terminal_ctx(session, Path::new("/tmp"));
        ctx.stdin_request_tx = Some(tx);
        (ctx, rx)
    }

    /// A real ACP session claiming its turn, plus a receiver for the client
    /// requests the daemon would have written to the editor.
    struct TerminalHarness {
        session: String,
        session_token: u64,
        bridge_token: u64,
        turn: Option<acp_session::TurnOwnership>,
        bridge: Arc<client_bridge::ClientBridge>,
        events: Option<mpsc::UnboundedReceiver<ServerEvent>>,
        methods: Arc<StdMutex<Vec<String>>>,
        params: Arc<StdMutex<Vec<(String, Value)>>>,
        recorded: Arc<tokio::sync::Notify>,
    }

    impl TerminalHarness {
        fn new(session: &str, caps: AcpClientCapabilities) -> Self {
            let (tx, events) = mpsc::unbounded_channel();
            let session_token = acp_session::install(session, CONNECTION, tx.clone());
            let bridge_token = client_bridge::install(session, CONNECTION, caps, tx);
            let turn = acp_session::claim_turn(session, Some(CONNECTION));
            assert!(turn.is_owning(), "the harness must own the session's turn");
            let bridge = client_bridge::for_session(session).expect("the owning turn sees the bridge");
            Self {
                session: session.to_string(),
                session_token,
                bridge_token,
                turn: Some(turn),
                bridge,
                events: Some(events),
                methods: Arc::new(StdMutex::new(Vec::new())),
                params: Arc::new(StdMutex::new(Vec::new())),
                recorded: Arc::new(tokio::sync::Notify::new()),
            }
        }

        fn start_fake_client(&mut self, behaviour: Behaviour) {
            let events = self.events.take().expect("the fake client starts once");
            spawn_fake_client(
                events,
                Arc::clone(&self.bridge),
                Arc::clone(&self.methods),
                Arc::clone(&self.params),
                Arc::clone(&self.recorded),
                behaviour,
            );
        }

        fn methods(&self) -> Vec<String> {
            self.methods.lock().unwrap().clone()
        }

        fn method_names(&self) -> String {
            self.methods().join(", ")
        }

        fn method_count(&self, method: &str) -> usize {
            self.methods
                .lock()
                .unwrap()
                .iter()
                .filter(|name| *name == method)
                .count()
        }

        fn params_for(&self, method: &str) -> Vec<Value> {
            self.params
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| name == method)
                .map(|(_, params)| params.clone())
                .collect()
        }

        /// Wait for the fake client to have recorded `method` at least `count`
        /// times, without polling: the client notifies on every recorded request.
        async fn wait_for_method(&self, method: &str, count: usize) {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let notified = self.recorded.notified();
                    tokio::pin!(notified);
                    if self.method_count(method) >= count {
                        return;
                    }
                    notified.as_mut().await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "the fake client never received {method} {count} time(s): {}",
                    self.method_names()
                )
            });
        }

        fn finish(mut self) {
            drop(self.turn.take());
            client_bridge::clear(&self.session, self.bridge_token);
            acp_session::clear(&self.session, self.session_token);
        }
    }

    fn unknown_terminal() -> ClientRpcError {
        ClientRpcError {
            code: -32000,
            message: "unknown terminal".to_string(),
        }
    }

    struct FakeTerminal {
        child: tokio::process::Child,
        stdout: Arc<StdMutex<String>>,
        stderr: Arc<StdMutex<String>>,
        readers: Vec<tokio::task::JoinHandle<()>>,
        exit: Option<Value>,
    }

    impl FakeTerminal {
        async fn spawn(params: &Value) -> Self {
            let mut command = TokioCommand::new(params["command"].as_str().unwrap_or("bash"));
            if let Some(args) = params["args"].as_array() {
                command.args(args.iter().filter_map(Value::as_str));
            }
            if let Some(cwd) = params["cwd"].as_str() {
                command.current_dir(cwd);
            }
            if let Some(env) = params["env"].as_array() {
                for variable in env {
                    if let (Some(name), Some(value)) = (variable["name"].as_str(), variable["value"].as_str()) {
                        command.env(name, value);
                    }
                }
            }
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = command.spawn().expect("the fake client's terminal must spawn");
            let mut stdout = child.stdout.take().expect("stdout");
            let mut stderr = child.stderr.take().expect("stderr");
            let out = Arc::new(StdMutex::new(String::new()));
            let err = Arc::new(StdMutex::new(String::new()));
            let out_sink = Arc::clone(&out);
            let err_sink = Arc::clone(&err);
            let readers = vec![
                tokio::spawn(async move {
                    let mut text = String::new();
                    let _ = stdout.read_to_string(&mut text).await;
                    out_sink.lock().unwrap().push_str(&text);
                }),
                tokio::spawn(async move {
                    let mut text = String::new();
                    let _ = stderr.read_to_string(&mut text).await;
                    err_sink.lock().unwrap().push_str(&text);
                }),
            ];
            Self {
                child,
                stdout: out,
                stderr: err,
                readers,
                exit: None,
            }
        }

        async fn wait(&mut self) -> Value {
            if self.exit.is_none() {
                let status = self.child.wait().await.expect("the fake terminal's child must exit");
                for reader in self.readers.drain(..) {
                    let _ = reader.await;
                }
                self.exit = Some(json!({"exitCode": status.code(), "signal": null}));
            }
            self.exit.clone().expect("the exit status is known after waiting")
        }

        fn kill(&mut self) {
            let _ = self.child.start_kill();
        }

        /// Compose the two streams the way the local path does: stdout, then
        /// stderr after a separating newline.
        fn output(&self) -> String {
            let stdout = self.stdout.lock().unwrap().clone();
            let stderr = self.stderr.lock().unwrap().clone();
            let mut output = stdout;
            if !stderr.is_empty() {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&stderr);
            }
            output
        }
    }

    fn spawn_fake_client(
        mut events: mpsc::UnboundedReceiver<ServerEvent>,
        bridge: Arc<client_bridge::ClientBridge>,
        methods: Arc<StdMutex<Vec<String>>>,
        params: Arc<StdMutex<Vec<(String, Value)>>>,
        recorded: Arc<tokio::sync::Notify>,
        behaviour: Behaviour,
    ) {
        tokio::spawn(async move {
            let mut terminals: HashMap<String, FakeTerminal> = HashMap::new();
            let mut sequence = 0u32;
            while let Some(event) = events.recv().await {
                let ServerEvent::ClientRequest {
                    request_id,
                    method,
                    params: request_params,
                    ..
                } = event
                else {
                    continue;
                };
                methods.lock().unwrap().push(method.clone());
                params.lock().unwrap().push((method.clone(), request_params.clone()));
                recorded.notify_waiters();
                let terminal_id = request_params["terminalId"].as_str().map(str::to_string);
                let answer = match method.as_str() {
                    "terminal/create" => {
                        if behaviour.fail_create {
                            let _ = bridge.complete(
                                &request_id,
                                Err(ClientRpcError {
                                    code: -32000,
                                    message: "terminals are disabled in this client".to_string(),
                                }),
                            );
                            continue;
                        }
                        sequence += 1;
                        let id = format!("terminal-{sequence}");
                        terminals.insert(id.clone(), FakeTerminal::spawn(&request_params).await);
                        Ok(json!({"terminalId": id}))
                    }
                    // A client that never exits leaves this request unanswered on
                    // purpose, which is what the tool's own deadline must bound.
                    "terminal/wait_for_exit" if behaviour.never_exit => continue,
                    "terminal/wait_for_exit" => match terminal_id.as_deref().and_then(|id| terminals.get_mut(id)) {
                        Some(terminal) => Ok(terminal.wait().await),
                        None => Err(unknown_terminal()),
                    },
                    "terminal/output" => match terminal_id.as_deref().and_then(|id| terminals.get(id)) {
                        Some(terminal) => {
                            let mut answer = json!({
                                "output": terminal.output(),
                                "truncated": false,
                            });
                            if let Some(exit) = terminal.exit.clone() {
                                answer["exitStatus"] = exit;
                            }
                            Ok(answer)
                        }
                        None => Err(unknown_terminal()),
                    },
                    "terminal/kill" => {
                        if let Some(terminal) = terminal_id.as_deref().and_then(|id| terminals.get_mut(id)) {
                            terminal.kill();
                        }
                        Ok(json!({}))
                    }
                    "terminal/release" => {
                        if let Some(id) = terminal_id.as_deref() {
                            terminals.remove(id);
                        }
                        Ok(json!({}))
                    }
                    other => Err(ClientRpcError {
                        code: -32601,
                        message: format!("{other} is not supported by this client"),
                    }),
                };
                let _ = bridge.complete(&request_id, answer);
            }
        });
    }

    #[tokio::test]
    async fn bash_uses_client_terminal_and_releases() {
        let session = "bash-client-terminal-happy";
        let tool = BashTool::new();
        let local = tool
            .execute(json!({"command": "echo hi"}), daemon_ctx(session).0)
            .await
            .expect("the local run formats the reference output");

        let mut harness = TerminalHarness::new(session, terminal_caps());
        harness.start_fake_client(Behaviour::default());
        let (ctx, mut stdin_rx) = daemon_ctx(session);
        let delegated = tool
            .execute(json!({"command": "echo hi"}), ctx)
            .await
            .expect("the delegated run returns the client terminal's output");

        assert_eq!(
            delegated.output, local.output,
            "a delegated run must be formatted exactly like a local one"
        );
        assert_eq!(
            harness.method_names(),
            "terminal/create, terminal/wait_for_exit, terminal/output, terminal/release"
        );
        let create = harness.params_for("terminal/create").remove(0);
        assert_eq!(create["command"], "bash");
        assert!(
            create["args"][1]
                .as_str()
                .expect("the command text is an argument")
                .contains("echo hi")
        );
        assert_eq!(create["outputByteLimit"], super::MAX_OUTPUT_BYTES);
        assert!(
            create["cwd"].as_str().expect("an absolute cwd").starts_with('/'),
            "the client terminal needs an absolute cwd: {create}"
        );
        assert!(
            stdin_rx.try_recv().is_err(),
            "delegation must not ask the daemon side for stdin"
        );
        harness.finish();
    }

    #[tokio::test]
    async fn bash_client_timeout_kills_then_releases() {
        let session = "bash-client-terminal-timeout";
        let mut harness = TerminalHarness::new(session, terminal_caps());
        harness.start_fake_client(Behaviour {
            never_exit: true,
            ..Behaviour::default()
        });
        let tool = BashTool::new();

        let result = tool
            .execute(
                json!({"command": "sleep 30", "timeout": 300}),
                terminal_ctx(session, Path::new("/tmp")),
            )
            .await
            .expect("a timed-out client terminal still returns a result");

        assert!(
            result
                .output
                .starts_with("Command timed out after 300 ms and was killed (client terminal)"),
            "unexpected timeout text: {}",
            result.output
        );
        harness.wait_for_method("terminal/release", 1).await;
        assert_eq!(
            harness.method_names(),
            "terminal/create, terminal/wait_for_exit, terminal/kill, terminal/output, terminal/release"
        );
        harness.finish();
    }

    #[tokio::test]
    async fn bash_background_stays_local() {
        let session = "bash-client-terminal-background";
        let mut harness = TerminalHarness::new(session, terminal_caps());
        let tool = BashTool::new();

        let result = tool
            .execute(
                json!({"command": "true", "run_in_background": true, "notify": false, "wake": false}),
                terminal_ctx(session, Path::new("/tmp")),
            )
            .await
            .expect("a background call starts");

        assert_eq!(result.metadata.expect("background metadata")["background"], true);
        assert!(
            harness.events.as_mut().expect("not started").try_recv().is_err(),
            "a background call must never reach the client terminal"
        );
        assert!(harness.methods().is_empty());
        harness.finish();
    }

    #[tokio::test]
    async fn bash_without_capability_stays_local() {
        let session = "bash-client-terminal-no-capability";
        let mut harness = TerminalHarness::new(session, fs_only_caps());
        let tool = BashTool::new();

        let result = tool
            .execute(
                json!({"command": "echo local-no-terminal"}),
                terminal_ctx(session, Path::new("/tmp")),
            )
            .await
            .expect("without the terminal capability the call runs locally");

        assert!(result.output.contains("local-no-terminal"), "{}", result.output);
        assert!(
            harness.events.as_mut().expect("not started").try_recv().is_err(),
            "a client without the terminal capability must never be asked"
        );
        assert!(harness.methods().is_empty());
        harness.finish();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_pty_call_stays_local() {
        let session = "bash-client-terminal-pty";
        let mut harness = TerminalHarness::new(session, terminal_caps());
        let tool = BashTool::new();

        let result = tool
            .execute(
                json!({
                    "command": "if test -t 0; then echo TTY-OK; else echo NOT-TTY; fi",
                    "pty": "always"
                }),
                terminal_ctx(session, Path::new("/tmp")),
            )
            .await
            .expect("a PTY call runs locally");

        assert!(
            result.output.contains("TTY-OK"),
            "a PTY call must keep its local terminal: {}",
            result.output
        );
        assert!(
            harness.events.as_mut().expect("not started").try_recv().is_err(),
            "a PTY call must never reach the client terminal"
        );
        assert!(harness.methods().is_empty());
        harness.finish();
    }

    #[tokio::test]
    async fn bash_client_create_failure_falls_back_local() {
        let session = "bash-client-terminal-create-failure";
        let mut harness = TerminalHarness::new(session, terminal_caps());
        harness.start_fake_client(Behaviour {
            fail_create: true,
            ..Behaviour::default()
        });
        let tool = BashTool::new();

        let result = tool
            .execute(
                json!({"command": "echo local-fallback"}),
                terminal_ctx(session, Path::new("/tmp")),
            )
            .await
            .expect("a refused terminal falls back to the local spawner");

        assert!(result.output.contains("local-fallback"), "{}", result.output);
        assert_eq!(
            harness.method_names(),
            "terminal/create",
            "a terminal the client refused to create owes no release"
        );
        harness.finish();
    }

    #[tokio::test]
    async fn bash_client_terminal_preserves_session_cwd() {
        // The switch is already on by default; forcing the process-global test
        // override here would enable session cwd for every other test running
        // beside this one, including the gate tests that share one session id and
        // resolve a temp directory another test drops (ENOENT at spawn). The lock
        // only keeps the one test that forces the switch off out of the way.
        let _lock = cwd_test_lock();
        let session = "bash-client-terminal-cwd";
        forget_bash_session_cwd(session);
        let (root, alpha, _beta) = cwd_tree();
        let mut harness = TerminalHarness::new(session, terminal_caps());
        harness.start_fake_client(Behaviour::default());
        let tool = BashTool::new();

        let outcome = tool
            .execute(
                json!({"command": format!("cd {}", alpha.display())}),
                terminal_ctx(session, root.path()),
            )
            .await;
        let remembered = bash_session_cwd(session);

        outcome.expect("the delegated cd succeeds");
        assert_eq!(
            remembered.map(|dir| real_path(&dir)),
            Some(real_path(&alpha)),
            "the client terminal's end directory must move the session cwd"
        );
        harness.finish();
    }

    #[tokio::test]
    async fn bash_client_cancel_during_wait_releases_once() {
        let session = "bash-client-terminal-cancel";
        let mut harness = TerminalHarness::new(session, terminal_caps());
        harness.start_fake_client(Behaviour {
            never_exit: true,
            ..Behaviour::default()
        });
        let ctx = terminal_ctx(session, Path::new("/tmp"));

        let call = tokio::spawn(async move {
            BashTool::new()
                .execute(json!({"command": "sleep 30", "timeout": 600000}), ctx)
                .await
        });
        harness.wait_for_method("terminal/wait_for_exit", 1).await;
        call.abort();
        let cancelled = call.await.expect_err("the turn was cancelled while waiting");
        assert!(cancelled.is_cancelled());

        harness.wait_for_method("terminal/release", 1).await;
        assert_eq!(
            harness.method_count("terminal/release"),
            1,
            "a cancelled turn releases its terminal exactly once: {}",
            harness.method_names()
        );
        harness.finish();
    }
}
/// Run git in `dir` with a fixed identity so a test repository needs no global
/// configuration.
#[cfg(unix)]
fn run_git(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "horon-agent test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "horon-agent test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git should run")
}

/// Without the non-interactive environment, `git commit` without `-m` launches
/// an editor and the call hangs until its timeout instead of aborting.
#[cfg(unix)]
#[tokio::test]
async fn git_commit_without_a_message_aborts_instead_of_waiting_for_an_editor() {
    set_bash_non_interactive_env_test_override(Some(true));
    let dir = std::env::temp_dir().join(format!("horon-agent-git-editor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create repository directory");

    if !run_git(&dir, &["init", "--quiet"]).status.success() {
        set_bash_non_interactive_env_test_override(None);
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("--- manual QA: git is not available, skipping the editor probe ---");
        return;
    }
    // `git commit` refuses to open an editor before it knows the author, so the
    // repository needs an identity of its own.
    assert!(
        run_git(&dir, &["config", "user.name", "horon-agent test"])
            .status
            .success()
    );
    assert!(
        run_git(&dir, &["config", "user.email", "test@example.com"])
            .status
            .success()
    );
    assert!(
        run_git(&dir, &["commit", "--allow-empty", "-m", "initial"])
            .status
            .success(),
        "the fixture repository needs one commit"
    );

    let mut ctx = make_ctx(None);
    // The session directory memory is keyed by session id and shared by every
    // test in this binary; a unique id keeps a remembered directory from another
    // test out of this one.
    ctx.session_id = "git-editor-session".to_string();
    ctx.working_dir = Some(dir.clone());
    forget_bash_session_cwd("git-editor-session");
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        // `LC_ALL=C` pins git's wording: this test is about the editor
        // resolving without a terminal, not about the machine's locale.
        BashTool::new().execute(
            json!({"command": "LC_ALL=C git commit --allow-empty", "timeout": 10000}),
            ctx,
        ),
    )
    .await
    .expect("git commit must not wait for an editor")
    .expect("bash call should return");
    set_bash_non_interactive_env_test_override(None);
    forget_bash_session_cwd("git-editor-session");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        output.output.contains("Aborting commit due to empty commit message"),
        "unexpected output: {}",
        output.output
    );
}

/// A command that never answers a prompt must see a non-interactive
/// environment: pagers disabled, a dumb terminal, and no editor.
#[cfg(unix)]
#[tokio::test]
async fn non_interactive_environment_reaches_the_child() {
    set_bash_non_interactive_env_test_override(Some(true));
    let output = BashTool::new()
        .execute(
            json!({"command": "printf '%s|%s|%s|%s' \"$PAGER\" \"$TERM\" \"$GIT_EDITOR\" \"$AGENT\""}),
            make_ctx(None),
        )
        .await
        .expect("bash call");
    set_bash_non_interactive_env_test_override(None);
    assert_eq!(output.output.trim(), "cat|dumb|true|1");
}

/// The tail of long output carries the result; a head-only cut lost it.
#[cfg(unix)]
#[tokio::test]
async fn long_output_keeps_the_first_and_last_lines() {
    let output = BashTool::new()
        .execute(json!({"command": "seq 1 100000; exit 3"}), make_ctx(None))
        .await
        .expect("bash call");

    assert!(
        output.output.starts_with("1\n2\n"),
        "the first lines must survive: {:?}",
        &output.output[..40.min(output.output.len())]
    );
    assert!(
        output.output.contains("omitted from `bash` output"),
        "the middle must be marked: {}",
        output.output
    );
    assert!(
        output.output.contains("\n100000"),
        "the last line must survive: {}",
        output.output
    );
    assert!(!output.output.contains("50000"), "the middle is elided");
    assert!(output.output.ends_with("Exit code: 3"), "output: {}", output.output);
    assert!(output.output.len() < 31_000, "output stayed near the budget");
}

#[cfg(unix)]
#[tokio::test]
async fn zero_timeout_waits_for_completion_without_promotion() {
    let output = BashTool::new()
        .execute(json!({"command": "sleep 2; echo done", "timeout": 0}), make_ctx(None))
        .await
        .expect("bash call");
    assert!(output.output.contains("done"), "output: {}", output.output);
    assert!(
        !output.output.contains("continuing in background"),
        "a zero timeout must not promote the command: {}",
        output.output
    );
}

/// Cancelling a background task must reach the process group with SIGTERM so
/// the command can clean up, instead of dropping the future and killing the
/// shell outright.
#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_background_command_delivers_sigterm_first() {
    let result = BashTool::new()
        .execute(
            json!({
                "command": "trap 'echo got-term; exit 0' TERM; sleep 30",
                "run_in_background": true,
                "notify": false,
                "wake": false
            }),
            make_ctx(None),
        )
        .await
        .expect("the background command should start");
    let metadata = result.metadata.expect("background metadata");
    let task_id = metadata["task_id"].as_str().expect("task id").to_string();
    let output_file = std::path::PathBuf::from(metadata["output_file"].as_str().expect("output file"));

    // The manager learns the child pid from inside the task, so wait for it
    // before asking for cancellation.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while crate::background::global()
        .status(&task_id)
        .await
        .and_then(|status| status.pid)
        .is_none()
    {
        assert!(std::time::Instant::now() < deadline, "the task pid was never recorded");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Cancel through the tool the agent actually calls, not the manager API.
    super::super::bg::BgTool::new()
        .execute(json!({"action": "cancel", "task_id": task_id}), make_ctx(None))
        .await
        .expect("bg cancel should succeed");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let text = tokio::fs::read_to_string(&output_file).await.unwrap_or_default();
        if text.contains("got-term") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the TERM trap output never reached the task output file: {text:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_cwd_must_exist() {
    let error = BashTool::new()
        .execute(
            json!({"command": "printf should-not-run", "cwd": "/nonexistent"}),
            make_ctx(None),
        )
        .await
        .expect_err("a missing cwd must be refused before spawning");
    assert_eq!(error.to_string(), "Working directory does not exist: /nonexistent");
}

#[cfg(unix)]
#[tokio::test]
async fn long_bash_output_spills_to_a_readable_artifact() {
    let ctx = make_ctx(None);
    let result = BashTool::new()
        .execute(json!({"command": "seq 1 200000"}), ctx.clone())
        .await
        .expect("the long command should succeed");
    assert!(result.output.starts_with("1\n"), "the head survives");
    assert!(result.output.contains("200000"), "the tail survives");
    assert!(
        result.output.contains("omitted from `bash` output"),
        "the cut is marked: {}",
        result.output
    );
    let marker = result
        .output
        .rfind("artifact://")
        .expect("the full output is saved as an artifact");
    let id: String = result.output[marker + "artifact://".len()..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    assert!(!id.is_empty(), "the artifact pointer names an id: {}", result.output);

    let read = crate::tool::read::ReadTool::new()
        .execute(json!({ "file_path": format!("artifact://{id}:-3") }), ctx)
        .await
        .expect("the artifact should be readable");
    for expected in ["199998", "199999", "200000"] {
        assert!(read.output.contains(expected), "missing {expected}: {}", read.output);
    }
    assert!(
        !read.output.contains("199997"),
        "only the last three lines: {}",
        read.output
    );
}

#[cfg(unix)]
#[tokio::test]
async fn minimizer_summarizes_repeats_and_points_at_the_raw_artifact() {
    let _guard = crate::storage::lock_test_env();
    set_bash_minimizer_test_override(Some(true));

    let result = BashTool::new()
        .execute(json!({"command": "printf 'dup\\n%.0s' {1..500}"}), make_ctx(None))
        .await
        .expect("the repeated-output command should succeed");

    set_bash_minimizer_test_override(None);
    assert!(
        result.output.contains("dup (×500)"),
        "repeats collapse: {}",
        result.output
    );
    assert!(result.output.contains("minimized from"), "{}", result.output);
    assert!(
        result.output.contains("raw output: artifact://"),
        "the raw output is preserved: {}",
        result.output
    );
}

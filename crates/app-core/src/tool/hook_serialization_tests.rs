//! Tests for serializing the hook invocations `Registry::execute` awaits.
//!
//! Three hooks are awaited inside `Registry::execute`: the input transformer
//! (`pre_tool_transform`), the `pre_tool` gate, and `post_tool_feedback`. All
//! three are serialized by `HOOK_SERIAL` so a concurrent tool group cannot run
//! the user's scripts at the same time. The transformer is included because it
//! rewrites the input the tool is about to receive. `post_tool` is an observer
//! dispatched through `dispatch_observer`, which spawns a detached child and
//! returns immediately, so it stays concurrent by contract and these tests pin
//! that behavior instead of changing it.
#![cfg(unix)]
#![allow(clippy::await_holding_lock)]

use super::{MockProvider, mcp_test_context};
use crate::provider::Provider;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Restores the hook environment when the test ends, so a failing assertion
/// cannot leak a hook configuration into the rest of the test binary.
struct HookEnvGuard {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl HookEnvGuard {
    fn install(pairs: &[(&'static str, String)]) -> Self {
        let mut previous = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            previous.push((*key, std::env::var_os(key)));
            crate::env::set_var(key, value);
        }
        // horon-agent-base is compiled without cfg(test) here, so the config
        // cache only re-checks the environment on a short throttle; without
        // this the hook can stay invisible for the first calls.
        crate::config::invalidate_config_cache();
        Self { previous }
    }
}

impl Drop for HookEnvGuard {
    fn drop(&mut self) {
        for (key, previous) in self.previous.drain(..) {
            match previous {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
    }
}

#[cfg(unix)]
fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write hook script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod hook script");
    path
}

/// Five concurrent reads must produce five complete, non-interleaved
/// `post_tool_feedback` lines.
///
/// The script writes half a line, sleeps, then writes the other half. Without
/// serialization the five concurrent invocations all write their first half
/// before any second half lands, so the log interleaves and the line shape
/// assertion fails. The `sleep 0.05` is what makes that detectable rather than
/// luck.
#[cfg(unix)]
#[tokio::test]
async fn concurrent_reads_serialize_post_tool_feedback_hooks() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("feedback.log");
    let script = write_script(
        dir.path(),
        "feedback.sh",
        &format!(
            "#!/bin/sh\n\
             cat >/dev/null\n\
             printf 'BEGIN %s ' \"$HORON_AGENT_HOOK_TOOL_CALL_ID\" >> '{log}'\n\
             sleep 0.05\n\
             printf 'END\\n' >> '{log}'\n\
             echo \"feedback $HORON_AGENT_HOOK_TOOL_CALL_ID\"\n",
            log = log.display()
        ),
    );
    let _hooks = HookEnvGuard::install(&[("HORON_AGENT_HOOK_POST_TOOL_FEEDBACK", script.display().to_string())]);

    let target = dir.path().join("prose.md");
    std::fs::write(&target, "hello\n").expect("write read target");

    let registry = Arc::new(crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await);

    let calls = (0..5).map(|index| {
        let registry = registry.clone();
        let target = target.clone();
        let working_dir = dir.path().to_path_buf();
        async move {
            let mut ctx = mcp_test_context(&working_dir);
            ctx.tool_call_id = format!("call_serial_{index}");
            let result = registry
                .execute("read", serde_json::json!({"file_path": target}), ctx)
                .await;
            (index, result)
        }
    });
    let results = futures::future::join_all(calls).await;

    for (index, result) in &results {
        let output = result.as_ref().expect("read should succeed").output.clone();
        assert!(
            output.contains(&format!("feedback call_serial_{index}")),
            "call {index} did not receive its own feedback: {output}"
        );
    }

    let contents = std::fs::read_to_string(&log).expect("read feedback log");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        5,
        "expected five complete feedback lines, got {} in:\n{contents}",
        lines.len()
    );

    let mut seen: Vec<String> = Vec::new();
    for line in &lines {
        let id = line
            .strip_prefix("BEGIN ")
            .and_then(|rest| rest.strip_suffix(" END"))
            .unwrap_or_else(|| panic!("interleaved hook output line {line:?} in:\n{contents}"));
        assert!(
            id.starts_with("call_serial_"),
            "unexpected hook line {line:?} in:\n{contents}"
        );
        seen.push(id.to_string());
    }
    seen.sort();
    let expected: Vec<String> = (0..5).map(|index| format!("call_serial_{index}")).collect();
    assert_eq!(seen, expected, "each call must contribute exactly one line");
}

/// The `pre_tool` gate still blocks an ordered call: exit 2 becomes the tool
/// error and the tool body never runs.
#[cfg(unix)]
#[tokio::test]
async fn ordered_call_is_blocked_by_the_pre_tool_gate() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_script(
        dir.path(),
        "gate.sh",
        "#!/bin/sh\n\
         cat >/dev/null\n\
         case \"$HORON_AGENT_HOOK_TOOL_NAME\" in\n\
         \x20 write) echo 'writes are blocked here' >&2; exit 2 ;;\n\
         esac\n\
         exit 0\n",
    );
    let _hooks = HookEnvGuard::install(&[("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string())]);

    let registry = crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
    let target = dir.path().join("blocked.txt");
    let mut ctx = mcp_test_context(dir.path());
    ctx.tool_call_id = "call_gate".to_string();

    let result = registry
        .execute(
            "write",
            serde_json::json!({"file_path": target.clone(), "content": "must not land"}),
            ctx,
        )
        .await;

    let error = result.expect_err("a pre_tool exit 2 must block the call").to_string();
    assert!(error.contains("blocked by pre_tool hook"), "{error}");
    assert!(error.contains("writes are blocked here"), "{error}");
    assert!(!target.exists(), "a blocked write must not touch the file");
}

/// Five concurrent calls must not overlap inside `transform_tool_input`.
///
/// The transformer MUTATES the tool input, so overlapping invocations would let
/// two rewrites interleave. The script records entry, sleeps, then records exit;
/// without serialization every invocation writes its entry before any exit
/// lands and the line shape assertion fails.
#[cfg(unix)]
#[tokio::test]
async fn concurrent_reads_serialize_the_input_transform_hook() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("transform.log");
    let script = write_script(
        dir.path(),
        "transform.sh",
        &format!(
            "#!/bin/sh\n\
             payload=$(cat)\n\
             target=$(printf '%s' \"$payload\" | sed -n 's/.*\"file_path\":\"\\([^\"]*\\)\".*/\\1/p')\n\
             printf 'BEGIN %s ' \"$target\" >> '{log}'\n\
             sleep 0.05\n\
             printf 'END\\n' >> '{log}'\n\
             printf '%s' \"$payload\"\n",
            log = log.display()
        ),
    );
    let _hooks = HookEnvGuard::install(&[("HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM", script.display().to_string())]);

    let registry = Arc::new(crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await);
    let targets: Vec<std::path::PathBuf> = (0..5)
        .map(|index| {
            let target = dir.path().join(format!("transform-{index}.md"));
            std::fs::write(&target, "hello\n").expect("write read target");
            target
        })
        .collect();

    let calls = targets.iter().enumerate().map(|(index, target)| {
        let registry = registry.clone();
        let target = target.clone();
        let working_dir = dir.path().to_path_buf();
        async move {
            let mut ctx = mcp_test_context(&working_dir);
            ctx.tool_call_id = format!("call_transform_{index}");
            let result = registry
                .execute("read", serde_json::json!({"file_path": target}), ctx)
                .await;
            (index, result)
        }
    });
    let results = futures::future::join_all(calls).await;
    for (index, result) in &results {
        result
            .as_ref()
            .unwrap_or_else(|error| panic!("read {index} should succeed through the transformer: {error}"));
    }

    let contents = std::fs::read_to_string(&log).expect("read transform log");
    // Captured by default; `--nocapture` shows the recorded entry/exit order,
    // which is the evidence that no two invocations overlapped.
    println!("transform hook log:\n{contents}");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        5,
        "expected five complete transformer lines, got {} in:\n{contents}",
        lines.len()
    );

    let mut seen: Vec<String> = Vec::new();
    for line in &lines {
        let target = line
            .strip_prefix("BEGIN ")
            .and_then(|rest| rest.strip_suffix(" END"))
            .unwrap_or_else(|| panic!("overlapping transformer invocations: line {line:?} in:\n{contents}"));
        assert!(
            target.contains("transform-"),
            "unexpected transformer line {line:?} in:\n{contents}"
        );
        seen.push(target.to_string());
    }
    seen.sort();
    let mut expected: Vec<String> = targets.iter().map(|target| target.display().to_string()).collect();
    expected.sort();
    assert_eq!(seen, expected, "each call must transform exactly once");
}

/// `post_tool` stays detached: `Registry::execute` returns without waiting for
/// the observer script, and the script still runs to completion afterwards.
#[cfg(unix)]
#[tokio::test]
async fn post_tool_observer_stays_detached() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("post_tool.log");
    let script = write_script(
        dir.path(),
        "observer.sh",
        &format!(
            "#!/bin/sh\n\
             cat >/dev/null\n\
             sleep 1\n\
             printf 'observed %s\\n' \"$HORON_AGENT_HOOK_TOOL_NAME\" >> '{log}'\n",
            log = log.display()
        ),
    );
    let _hooks = HookEnvGuard::install(&[("HORON_AGENT_HOOK_POST_TOOL", script.display().to_string())]);

    let target = dir.path().join("observed.md");
    std::fs::write(&target, "hello\n").expect("write read target");

    let registry = crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
    let mut ctx = mcp_test_context(dir.path());
    ctx.tool_call_id = "call_detached".to_string();

    let started = Instant::now();
    let output = registry
        .execute("read", serde_json::json!({"file_path": target}), ctx)
        .await
        .expect("read should succeed")
        .output;
    let elapsed = started.elapsed();
    assert!(output.contains("hello"), "{output}");
    assert!(
        elapsed < Duration::from_millis(500),
        "post_tool must not block the call, but execute waited {elapsed:?} for a 1s observer"
    );
    assert!(
        !log.exists(),
        "the detached observer cannot have finished before execute returned"
    );

    // The child is detached, so there is no event to await; poll the artifact
    // it writes with a bounded deadline instead of sleeping a fixed guess.
    let deadline = Duration::from_secs(10);
    tokio::time::timeout(deadline, async {
        loop {
            let written = std::fs::read_to_string(&log)
                .map(|contents| contents.contains("observed read"))
                .unwrap_or(false);
            if written {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the detached post_tool observer should still write its line");
}

/// Five concurrent calls must not overlap inside the `pre_tool` gate.
///
/// The gate is an external policy hook that can block a call, and `HOOK_SERIAL`
/// serializes it with the input transformer and the feedback hook so a
/// concurrent group cannot run the user's scripts at the same time. The script
/// records entry, sleeps, then records exit; without the lock every invocation
/// writes its entry before any exit lands, so the log interleaves and the line
/// shape assertion fails.
///
/// The other gate test only proves the gate still blocks, which stays true with
/// the lock deleted; this one is what makes the lock's removal observable.
#[cfg(unix)]
#[tokio::test]
async fn concurrent_reads_serialize_the_pre_tool_gate() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("gate.log");
    let script = write_script(
        dir.path(),
        "serial_gate.sh",
        &format!(
            r#"#!/bin/sh
payload=$(cat)
target=$(printf '%s' "$payload" | tr -d '{{}}"' | sed 's/^.*file_path://')
printf 'BEGIN %s ' "$target" >> '{log}'
sleep 0.05
printf 'END\n' >> '{log}'
exit 0
"#,
            log = log.display()
        ),
    );
    let _hooks = HookEnvGuard::install(&[("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string())]);

    let registry = Arc::new(crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await);
    let targets: Vec<std::path::PathBuf> = (0..5)
        .map(|index| {
            let target = dir.path().join(format!("gate-{index}.md"));
            std::fs::write(&target, "hello\n").expect("write read target");
            target
        })
        .collect();

    let calls = targets.iter().enumerate().map(|(index, target)| {
        let registry = registry.clone();
        let target = target.clone();
        let working_dir = dir.path().to_path_buf();
        async move {
            let mut ctx = mcp_test_context(&working_dir);
            ctx.tool_call_id = format!("call_gate_{index}");
            let result = registry
                .execute("read", serde_json::json!({"file_path": target}), ctx)
                .await;
            (index, result)
        }
    });
    let results = futures::future::join_all(calls).await;
    for (index, result) in &results {
        result
            .as_ref()
            .unwrap_or_else(|error| panic!("read {index} should pass the gate: {error}"));
    }

    let contents = std::fs::read_to_string(&log).expect("read gate log");
    // Captured by default; `--nocapture` shows the five entry/exit pairs, which
    // is the evidence that no two gate invocations overlapped.
    println!("pre_tool gate log:\n{contents}");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        5,
        "expected five complete gate lines, got {} in:\n{contents}",
        lines.len()
    );

    let mut seen: Vec<String> = Vec::new();
    for line in &lines {
        let target = line
            .strip_prefix("BEGIN ")
            .and_then(|rest| rest.strip_suffix(" END"))
            .unwrap_or_else(|| panic!("overlapping pre_tool gate invocations: line {line:?} in:\n{contents}"));
        assert!(
            target.contains("gate-"),
            "unexpected gate line {line:?} in:\n{contents}"
        );
        seen.push(target.to_string());
    }
    seen.sort();
    let mut expected: Vec<String> = targets.iter().map(|target| target.display().to_string()).collect();
    expected.sort();
    assert_eq!(seen, expected, "each call must pass through the gate exactly once");
}

/// A hook that never finishes must not wedge the process-global gate for other
/// sessions. The second session's gate proceeds once the first hook reaches its
/// own deadline; the guarantee is boundedness, not concurrency.
#[cfg(unix)]
#[tokio::test]
async fn second_session_gate_proceeds_after_the_first_hook_deadline() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let pid_file = dir.path().join("gate.pid");
    let script = write_script(
        dir.path(),
        "wedged_gate.sh",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{pid_file}'\nsleep 30\n",
            pid_file = pid_file.display()
        ),
    );
    let _hooks = HookEnvGuard::install(&[
        ("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string()),
        ("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS", "100".to_string()),
    ]);

    let registry = Arc::new(crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await);
    let first_target = dir.path().join("first.md");
    let second_target = dir.path().join("second.md");
    std::fs::write(&first_target, "first\n").expect("write first target");
    std::fs::write(&second_target, "second\n").expect("write second target");

    let call = |target: std::path::PathBuf, session: &'static str, call_id: &'static str| {
        let registry = registry.clone();
        let working_dir = dir.path().to_path_buf();
        async move {
            let mut ctx = mcp_test_context(&working_dir);
            ctx.session_id = session.to_string();
            ctx.tool_call_id = call_id.to_string();
            let started = Instant::now();
            let result = registry
                .execute("read", serde_json::json!({"file_path": target}), ctx)
                .await;
            (result, started.elapsed())
        }
    };

    let started = Instant::now();
    let (first, second) = tokio::join!(
        call(first_target, "ses_queue_a", "call_a"),
        call(second_target, "ses_queue_b", "call_b")
    );
    let total = started.elapsed();

    for (label, (result, elapsed)) in [("first", &first), ("second", &second)] {
        let output = result
            .as_ref()
            .unwrap_or_else(|error| panic!("{label} session must be released, not blocked: {error}"))
            .output
            .clone();
        assert!(output.contains(label), "{label} session output: {output}");
        println!("{label} session gate wait: {elapsed:?}");
        assert!(
            *elapsed < Duration::from_secs(5),
            "{label} session waited {elapsed:?} behind the wedged hook"
        );
    }
    println!("both sessions released in {total:?}");
    assert!(
        total < Duration::from_secs(5),
        "two sessions behind one wedged hook took {total:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let alive: Vec<String> = std::fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .lines()
            .filter(|pid| {
                std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg(format!("kill -0 {pid} 2>/dev/null"))
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false)
            })
            .map(ToString::to_string)
            .collect();
        if alive.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "hook children still alive after both sessions returned: {alive:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(
        super::super::HOOK_SERIAL.try_lock().is_ok(),
        "the hook gate stayed locked after both sessions returned"
    );
}

/// An expired queue wait releases the session with a fail-open Allow and leaves
/// the gate free. A wedged holder is simulated by taking the gate directly,
/// which is exactly the state a hook stuck past its deadline leaves behind for a
/// queued session.
///
/// This drives the gate helper rather than `Registry::execute`, because the
/// registry also takes the same lock for the input transformer before the gate;
/// holding it here would block that earlier, unbounded acquisition instead of
/// exercising the gate's bounded one.
#[cfg(unix)]
#[tokio::test]
async fn queue_wait_expiry_allows_the_call_and_releases_the_gate() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_script(dir.path(), "allow.sh", "#!/bin/sh\ncat >/dev/null\nexit 0\n");
    let _hooks = HookEnvGuard::install(&[
        ("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string()),
        // All three serialized hooks share the gate, so the queue budget follows
        // the longest of their deadlines; pin every one of them.
        ("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS", "1".to_string()),
        ("HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM_TIMEOUT_MS", "1".to_string()),
        ("HORON_AGENT_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS", "1".to_string()),
    ]);

    let wedged = super::super::HOOK_SERIAL.lock().await;
    let budget = super::super::hook_serial_queue_budget();
    assert!(
        budget >= Duration::from_millis(1),
        "queue budget must stay positive: {budget:?}"
    );

    let started = Instant::now();
    let decision = super::super::run_gated_pre_tool_hook("ses_queue_expiry", None, "read", "{}").await;
    let elapsed = started.elapsed();
    drop(wedged);

    println!("queue wait expired after {elapsed:?} (budget {budget:?})");
    assert_eq!(
        decision,
        crate::hooks::GateDecision::Allow,
        "an expired queue wait must fail open, never block"
    );
    assert!(
        elapsed >= budget,
        "the gate was acquired before its queue budget expired: {elapsed:?} < {budget:?}"
    );
    assert!(
        elapsed < budget + Duration::from_secs(2),
        "the queue wait was not bounded by its budget: {elapsed:?} for budget {budget:?}"
    );
    assert!(
        super::super::HOOK_SERIAL.try_lock().is_ok(),
        "the hook gate stayed locked after a queue-wait expiry"
    );

    // The gate works again immediately after the expiry: the holder's release
    // is what the next session sees, not a poisoned mutex.
    assert_eq!(
        super::super::run_gated_pre_tool_hook("ses_queue_after", None, "read", "{}").await,
        crate::hooks::GateDecision::Allow
    );
}

/// The gate permit is released after normal completion and after a hook
/// execution timeout, so one slow hook cannot starve every later tool call.
#[cfg(unix)]
#[tokio::test]
async fn gate_permit_is_released_after_completion_and_timeout() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().join("permit.md");
    std::fs::write(&target, "permit\n").expect("write read target");

    let allow = write_script(dir.path(), "allow.sh", "#!/bin/sh\ncat >/dev/null\nexit 0\n");
    {
        let _hooks = HookEnvGuard::install(&[
            ("HORON_AGENT_HOOK_PRE_TOOL", allow.display().to_string()),
            ("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS", "5000".to_string()),
        ]);
        let registry = crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        let mut ctx = mcp_test_context(dir.path());
        ctx.session_id = "ses_permit_ok".to_string();
        ctx.tool_call_id = "call_permit_ok".to_string();
        registry
            .execute("read", serde_json::json!({"file_path": target.clone()}), ctx)
            .await
            .expect("read should pass an allowing gate");
        assert!(
            super::super::HOOK_SERIAL.try_lock().is_ok(),
            "the hook gate stayed locked after a normal gate completion"
        );
    }

    let hang = write_script(dir.path(), "hang.sh", "#!/bin/sh\nsleep 30\n");
    {
        let _hooks = HookEnvGuard::install(&[
            ("HORON_AGENT_HOOK_PRE_TOOL", hang.display().to_string()),
            ("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS", "100".to_string()),
        ]);
        let registry = crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
        let mut ctx = mcp_test_context(dir.path());
        ctx.session_id = "ses_permit_timeout".to_string();
        ctx.tool_call_id = "call_permit_timeout".to_string();
        let started = Instant::now();
        let result = registry
            .execute("read", serde_json::json!({"file_path": target}), ctx)
            .await;
        println!("timed-out gate returned after {:?}", started.elapsed());
        assert!(
            result.is_ok(),
            "a timed-out gate must fail open, got: {:?}",
            result.err().map(|error| error.to_string())
        );
        assert!(
            super::super::HOOK_SERIAL.try_lock().is_ok(),
            "the hook gate stayed locked after a gate execution timeout"
        );
    }
}

/// Manual QA runs share one process-global logger, so both QA tests must point
/// `HORON_AGENT_HOME` at the same directory and read the file that logger opened.
/// A per-test temp home would leave the second test reading a log the logger
/// never wrote to.
#[cfg(unix)]
fn manual_qa_log_file() -> std::path::PathBuf {
    use std::sync::OnceLock;
    static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
    let home = HOME.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("horon-agent-manual-qa-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("create manual QA home");
        crate::env::set_var("HORON_AGENT_HOME", &home);
        crate::logging::init();
        home
    });
    crate::env::set_var("HORON_AGENT_HOME", home);
    crate::logging::log_path().expect("manual QA log path")
}

/// Manual QA on the real dispatch path: a configured `pre_tool` hook that never
/// finishes must return a fail-open decision within its deadline. Prints the
/// measured wall time, the decision, and the emitted log line so the artifact
/// carries measurements rather than a bare pass.
#[cfg(unix)]
#[tokio::test]
#[ignore = "manual QA: measures real dispatch wall time against a wedged hook"]
async fn manual_qa_wedged_pre_tool_hook_returns_within_deadline() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    manual_qa_log_file();

    let script = write_script(dir.path(), "wedged.sh", "#!/bin/sh\nsleep 600\n");
    let _hooks = HookEnvGuard::install(&[
        ("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string()),
        ("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS", "300".to_string()),
    ]);

    let target = dir.path().join("qa.md");
    std::fs::write(&target, "qa\n").expect("write read target");

    let registry = crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
    let mut ctx = mcp_test_context(dir.path());
    ctx.session_id = "ses_manual_qa".to_string();
    ctx.tool_call_id = "call_manual_qa".to_string();

    let started = Instant::now();
    let result = registry
        .execute("read", serde_json::json!({"file_path": target}), ctx)
        .await;
    let wall = started.elapsed();

    let decision = match &result {
        Ok(_) => "Allow (tool call proceeded)",
        Err(_) => "Block/Err",
    };
    println!("MANUAL-QA wall_time={wall:?} decision={decision}");
    if let Err(error) = &result {
        println!("MANUAL-QA error={error}");
    }

    let log_file = manual_qa_log_file();
    let contents = std::fs::read_to_string(&log_file).unwrap_or_default();
    let timed_out: Vec<&str> = contents
        .lines()
        .filter(|line| line.contains("pre_tool") && line.contains("timed out after 300ms"))
        .collect();
    println!("MANUAL-QA log_file={}", log_file.display());
    for line in &timed_out {
        println!("MANUAL-QA log_line={line}");
    }

    assert!(result.is_ok(), "a wedged pre_tool hook must fail open, not block");
    assert!(
        wall < Duration::from_secs(3),
        "the wedged hook escaped its deadline: {wall:?}"
    );
    assert!(
        !timed_out.is_empty(),
        "expected a pre_tool timeout log line, log had:\n{contents}"
    );
    assert!(
        timed_out.iter().any(|line| line.contains("allowing tool call")),
        "the timeout log line must state the fail-open outcome"
    );
}

/// Manual QA for the original B0 hang: a `pre_tool` hook that never reads stdin
/// plus a tool input larger than the pipe buffer. Before the fix the gate blocked
/// on `write_all` indefinitely; this measures the real dispatch path and prints
/// the wall time, the decision, and the log line.
#[cfg(unix)]
#[tokio::test]
#[ignore = "manual QA: measures real dispatch wall time for a hook that never reads stdin"]
async fn manual_qa_never_reading_hook_with_oversized_input_returns_within_deadline() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    manual_qa_log_file();

    let script = write_script(dir.path(), "never_reads.sh", "#!/bin/sh\nsleep 600\n");
    let _hooks = HookEnvGuard::install(&[
        ("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string()),
        ("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS", "300".to_string()),
    ]);

    let target = dir.path().join("qa-large.txt");
    let payload = "z".repeat(1024 * 1024);

    let registry = crate::tool::Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await;
    let mut ctx = mcp_test_context(dir.path());
    ctx.session_id = "ses_manual_qa_stdin".to_string();
    ctx.tool_call_id = "call_manual_qa_stdin".to_string();

    let started = Instant::now();
    let result = registry
        .execute(
            "write",
            serde_json::json!({"file_path": target.clone(), "content": payload}),
            ctx,
        )
        .await;
    let wall = started.elapsed();

    let decision = if result.is_ok() {
        "Allow (tool call proceeded)"
    } else {
        "Block/Err"
    };
    println!(
        "MANUAL-QA-STDIN input_bytes={} wall_time={wall:?} decision={decision}",
        payload.len()
    );
    if let Err(error) = &result {
        println!("MANUAL-QA-STDIN error={error}");
    }

    let log_file = manual_qa_log_file();
    let contents = std::fs::read_to_string(&log_file).unwrap_or_default();
    for line in contents
        .lines()
        .filter(|line| line.contains("pre_tool") && line.contains("timed out after 300ms"))
    {
        println!("MANUAL-QA-STDIN log_line={line}");
    }

    assert!(result.is_ok(), "a stdin-blocked hook must fail open, not hang");
    assert!(
        wall < Duration::from_secs(3),
        "an oversized stdin write escaped the gate deadline: {wall:?}"
    );
    assert!(
        contents.contains("timed out after 300ms") && contents.contains("allowing tool call"),
        "expected the fail-open timeout log line, log had:\n{contents}"
    );
}

/// Cancelling a session while it waits for the gate must not leak the permit and
/// must leave the gate usable. The queue wait is the one place a dropped future
/// could strand the lock; this interrupts it and then proves the next session
/// still acquires the gate.
#[cfg(unix)]
#[tokio::test]
async fn cancelling_during_the_queue_wait_leaves_the_gate_usable() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let script = write_script(dir.path(), "allow.sh", "#!/bin/sh\ncat >/dev/null\nexit 0\n");
    let _hooks = HookEnvGuard::install(&[
        ("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string()),
        ("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS", "5000".to_string()),
    ]);

    let wedged = super::super::HOOK_SERIAL.lock().await;
    let waiter =
        tokio::spawn(async { super::super::run_gated_pre_tool_hook("ses_cancel_wait", None, "read", "{}").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !waiter.is_finished(),
        "the waiter should still be queued behind the held gate"
    );
    waiter.abort();
    let joined = waiter.await;
    assert!(joined.is_err(), "an aborted queue wait must not return a decision");
    drop(wedged);

    assert!(
        super::super::HOOK_SERIAL.try_lock().is_ok(),
        "a cancelled queue wait leaked the hook gate"
    );
    assert_eq!(
        super::super::run_gated_pre_tool_hook("ses_after_cancel", None, "read", "{}").await,
        crate::hooks::GateDecision::Allow,
        "the gate must work again after a cancelled queue wait"
    );
}

/// Two sessions in a row behind the same wedged hook must each be released; the
/// first must not leave the second waiting on a poisoned or stranded gate.
#[cfg(unix)]
#[tokio::test]
async fn repeated_sessions_behind_one_wedged_hook_are_each_released() {
    let _env_lock = crate::storage::lock_test_env();
    let dir = tempfile::tempdir().expect("temp dir");
    let pid_file = dir.path().join("wedged.pid");
    let script = write_script(
        dir.path(),
        "wedged.sh",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{pid_file}'\nsleep 30\n",
            pid_file = pid_file.display()
        ),
    );
    let _hooks = HookEnvGuard::install(&[
        ("HORON_AGENT_HOOK_PRE_TOOL", script.display().to_string()),
        ("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS", "100".to_string()),
    ]);

    let mut waits = Vec::new();
    for index in 0..3 {
        let started = Instant::now();
        let decision = super::super::run_gated_pre_tool_hook("ses_repeat", None, "read", "{}").await;
        let elapsed = started.elapsed();
        println!("repeated session {index} released after {elapsed:?}");
        assert_eq!(decision, crate::hooks::GateDecision::Allow);
        assert!(
            elapsed < Duration::from_secs(5),
            "session {index} waited {elapsed:?} behind a wedged hook"
        );
        waits.push(elapsed);
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let alive: Vec<String> = std::fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .lines()
            .filter(|pid| {
                std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg(format!("kill -0 {pid} 2>/dev/null"))
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false)
            })
            .map(ToString::to_string)
            .collect();
        if alive.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "hook children still alive: {alive:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        super::super::HOOK_SERIAL.try_lock().is_ok(),
        "the hook gate stayed locked after repeated sessions"
    );
}

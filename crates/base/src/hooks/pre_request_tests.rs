use super::*;
#[cfg(unix)]
use horon_agent_message_types::ContentBlock;

/// Environment overrides for one test, restored on drop.
struct HookEnv {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl HookEnv {
    fn install(values: &[(&'static str, Option<String>)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect::<Vec<_>>();
        for (key, value) in values {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
        Self { previous }
    }
}

impl Drop for HookEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
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

#[cfg(unix)]
fn hook_env(script: &std::path::Path, timeout_ms: u64) -> HookEnv {
    HookEnv::install(&[
        (
            "HORON_AGENT_HOOK_PRE_REQUEST",
            Some(script.to_string_lossy().into_owned()),
        ),
        ("HORON_AGENT_HOOK_PRE_REQUEST_TIMEOUT_MS", Some(timeout_ms.to_string())),
    ])
}

#[cfg(unix)]
fn user_message(text: &str) -> Message {
    Message {
        role: horon_agent_message_types::Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

#[cfg(unix)]
fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn context<'a>(system: &'a str) -> RequestContext<'a> {
    RequestContext {
        session_id: "ses_pre_request",
        cwd: None,
        turn: 3,
        model: "test-model",
        system,
    }
}

#[tokio::test]
async fn pre_request_hook_is_unset_by_default() {
    let _guard = crate::storage::lock_test_env();
    let _env = HookEnv::install(&[
        ("HORON_AGENT_HOOK_PRE_REQUEST", None),
        ("HORON_AGENT_HOOK_PRE_REQUEST_TIMEOUT_MS", None),
        ("HORON_AGENT_HOOKS_DISABLED", None),
    ]);
    assert!(!configured(), "no hook is configured");
    assert!(
        rewrite(&context("SYS"), &[]).await.is_none(),
        "an unset hook leaves every request alone"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_replaces_the_messages_for_the_request() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = write_script(
        temp.path(),
        "rewrite.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"messages\":[{\"role\":\"system\",\"content\":\"SYS\"},{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"REWRITTEN\"}]}]}'\n",
    );
    let _env = hook_env(&script, 5000);

    let messages = vec![user_message("original"), user_message("second")];
    let rewritten = rewrite(&context("SYS"), &messages)
        .await
        .expect("the hook replaces the messages");

    assert_eq!(rewritten.len(), 1, "the replacement is used as printed");
    assert_eq!(message_text(&rewritten[0]), "REWRITTEN");
    assert_eq!(
        message_text(&messages[0]),
        "original",
        "the messages the request was built from are untouched"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_empty_stdout_leaves_the_messages_alone() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = write_script(temp.path(), "silent.sh", "#!/bin/sh\ncat >/dev/null\nexit 0\n");
    let _env = hook_env(&script, 5000);

    assert!(
        rewrite(&context("SYS"), &[user_message("hello")]).await.is_none(),
        "empty stdout means no change"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_failure_keeps_the_messages() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let messages = vec![user_message("hello")];

    let failing = write_script(temp.path(), "failing.sh", "#!/bin/sh\ncat >/dev/null\nexit 1\n");
    {
        let _env = hook_env(&failing, 5000);
        assert!(
            rewrite(&context("SYS"), &messages).await.is_none(),
            "a non-zero exit fails open"
        );
    }

    let garbage = write_script(
        temp.path(),
        "garbage.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf 'not json'\n",
    );
    {
        let _env = hook_env(&garbage, 5000);
        assert!(
            rewrite(&context("SYS"), &messages).await.is_none(),
            "invalid JSON fails open"
        );
    }

    let hang = write_script(temp.path(), "hang.sh", "#!/bin/sh\nsleep 30\n");
    {
        let _env = hook_env(&hang, 100);
        let started = std::time::Instant::now();
        assert!(
            rewrite(&context("SYS"), &messages).await.is_none(),
            "a timeout fails open"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the deadline covers the whole exchange: {:?}",
            started.elapsed()
        );
    }

    let missing = std::path::Path::new("/nonexistent/pre-request-hook");
    let _env = hook_env(missing, 5000);
    assert!(
        rewrite(&context("SYS"), &messages).await.is_none(),
        "a missing binary fails open"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_rejects_a_replacement_without_the_system_message() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = write_script(
        temp.path(),
        "drop-system.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"messages\":[{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"only user\"}]}]}'\n",
    );
    let _env = hook_env(&script, 5000);

    assert!(
        rewrite(&context("SYS"), &[user_message("hello")]).await.is_none(),
        "a replacement that drops the system prompt is refused whole"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_rejects_a_rewritten_system_message() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = write_script(
        temp.path(),
        "rewrite-system.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"messages\":[{\"role\":\"system\",\"content\":\"OTHER\"},{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"body\"}]}]}'\n",
    );
    let _env = hook_env(&script, 5000);

    assert!(
        rewrite(&context("SYS"), &[user_message("hello")]).await.is_none(),
        "the system prompt is the request's own and cannot be rewritten"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_chains_commands_in_declared_order() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    // The second hook only answers correctly when it was handed the first
    // hook's replacement, so a chain that restarted from the original input
    // fails instead of passing quietly.
    let first = write_script(
        temp.path(),
        "first.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"messages\":[{\"role\":\"system\",\"content\":\"SYS\"},{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}]}'\n",
    );
    let second = write_script(
        temp.path(),
        "second.sh",
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *first*) ;; *) exit 3 ;; esac\nprintf '%s' '{\"messages\":[{\"role\":\"system\",\"content\":\"SYS\"},{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"second\"}]}]}'\n",
    );
    let commands = serde_json::to_string(&vec![
        first.to_string_lossy().into_owned(),
        second.to_string_lossy().into_owned(),
    ])
    .expect("serialize the hook commands");
    let _env = HookEnv::install(&[
        ("HORON_AGENT_HOOK_PRE_REQUEST", Some(commands)),
        ("HORON_AGENT_HOOK_PRE_REQUEST_TIMEOUT_MS", Some("5000".to_string())),
    ]);

    let rewritten = rewrite(&context("SYS"), &[user_message("original")])
        .await
        .expect("the chain replaces the messages");
    assert_eq!(rewritten.len(), 1);
    assert_eq!(message_text(&rewritten[0]), "second");
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_receives_the_request_on_stdin() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let record = temp.path().join("stdin.json");
    let script = write_script(
        temp.path(),
        "record.sh",
        &format!(
            "#!/bin/sh\ncat > {}\n",
            crate::terminal_launch::sh_escape(&record.to_string_lossy())
        ),
    );
    let _env = hook_env(&script, 5000);

    let _ = rewrite(&context("SYS PROMPT"), &[user_message("hello")]).await;
    let recorded = std::fs::read_to_string(&record).expect("the hook records its stdin");
    let payload: serde_json::Value = serde_json::from_str(&recorded).expect("stdin is JSON");
    assert_eq!(payload["session_id"], "ses_pre_request");
    assert_eq!(payload["turn"], 3);
    assert_eq!(payload["model"], "test-model");
    assert_eq!(
        payload["messages"][0]["role"], "system",
        "the system prompt leads the message list"
    );
    assert_eq!(payload["messages"][0]["content"], "SYS PROMPT");
    assert_eq!(payload["messages"][1]["role"], "user");
    assert_eq!(payload["messages"][1]["content"][0]["text"], "hello");
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_is_suppressed_inside_a_hook_process() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = write_script(
        temp.path(),
        "rewrite.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"messages\":[{\"role\":\"system\",\"content\":\"SYS\"}]}'\n",
    );
    let _env = HookEnv::install(&[
        (
            "HORON_AGENT_HOOK_PRE_REQUEST",
            Some(script.to_string_lossy().into_owned()),
        ),
        ("HORON_AGENT_HOOK_PRE_REQUEST_TIMEOUT_MS", Some("5000".to_string())),
        ("HORON_AGENT_HOOKS_DISABLED", Some("1".to_string())),
    ]);

    assert!(!configured(), "the recursion guard suppresses the hook");
    assert!(
        rewrite(&context("SYS"), &[user_message("hello")]).await.is_none(),
        "a hook process does not run hooks"
    );
}

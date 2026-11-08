use super::*;

/// Environment overrides for one test, restored on drop.
#[cfg(unix)]
struct HookEnv {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

#[cfg(unix)]
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

#[cfg(unix)]
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
fn feedback_env(scripts: &[std::path::PathBuf]) -> HookEnv {
    let commands = serde_json::to_string(
        &scripts
            .iter()
            .map(|script| script.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    )
    .expect("serialize the hook commands");
    HookEnv::install(&[
        ("HORON_AGENT_HOOK_POST_TOOL_FEEDBACK", Some(commands)),
        (
            "HORON_AGENT_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS",
            Some("5000".to_string()),
        ),
    ])
}

#[cfg(unix)]
fn event() -> HookEvent {
    HookEvent::new("post_tool_feedback")
        .session_id("ses_feedback")
        .field("TOOL_NAME", "read")
}

#[test]
fn envelope_requires_one_of_its_three_fields() {
    assert!(envelope(r#"{"content":"x"}"#).is_some());
    assert!(envelope(r#"{"is_error":true}"#).is_some());
    assert!(envelope(r#"{"note":"x"}"#).is_some());
    assert!(
        envelope(r#"{"other":1}"#).is_none(),
        "an object with none of the three fields is ordinary text"
    );
    assert!(envelope("plain text").is_none());
    assert!(envelope("[1,2]").is_none());
}

#[test]
fn capped_trims_and_marks_a_long_output() {
    assert_eq!(capped("  hello \n"), "hello");
    let long = "x".repeat(FEEDBACK_LIMIT + 10);
    let capped = capped(&long);
    assert!(
        capped.ends_with("[feedback truncated]"),
        "{}",
        &capped[capped.len() - 40..]
    );
    assert!(capped.len() <= FEEDBACK_LIMIT + "[feedback truncated]".len() + 1);
}

#[test]
fn append_text_keeps_the_existing_header_behavior() {
    assert_eq!(
        append_text("result", "finding"),
        "result\n\n[post_tool_feedback]\nfinding"
    );
    assert_eq!(append_text("", "finding"), "\n[post_tool_feedback]\nfinding");
    assert_eq!(
        append_text("result\n", "finding"),
        "result\n\n[post_tool_feedback]\nfinding"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn post_tool_feedback_hook_applies_content_error_and_note() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = write_script(
        temp.path(),
        "envelope.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"content\":\"redacted\",\"is_error\":true,\"note\":\"be careful\"}'\n",
    );
    let _env = feedback_env(&[script]);

    let feedback = run(&event(), "{}", "secret value").await;

    assert_eq!(feedback.parts(), &[Part::Replace("redacted".to_string())]);
    assert_eq!(feedback.is_error(), Some(true));
    assert_eq!(feedback.notes(), &["be careful".to_string()]);
}

#[cfg(unix)]
#[tokio::test]
async fn post_tool_feedback_hook_plain_text_is_appended() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = write_script(
        temp.path(),
        "plain.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf 'line one\\nline two\\n'\n",
    );
    let _env = feedback_env(&[script]);

    let feedback = run(&event(), "{}", "result").await;

    assert_eq!(feedback.parts(), &[Part::Text("line one\nline two".to_string())]);
    assert_eq!(feedback.is_error(), None);
    assert!(feedback.notes().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn post_tool_feedback_hook_second_sees_the_first_result() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let first = write_script(
        temp.path(),
        "first.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"content\":\"first\"}'\n",
    );
    // The second hook appends only when it was handed the first hook's text.
    let second = write_script(
        temp.path(),
        "second.sh",
        "#!/bin/sh\ncat >/dev/null\ncase \"$HORON_AGENT_HOOK_OUTPUT\" in first) printf 'second' ;; *) printf 'missing-first' ;; esac\n",
    );
    let _env = feedback_env(&[first, second]);

    let feedback = run(&event(), "{}", "original").await;

    assert_eq!(
        feedback.parts(),
        &[Part::Replace("first".to_string()), Part::Text("second".to_string())],
        "each hook sees the result after the previous one"
    );
}

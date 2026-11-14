//! The platform guard around live Claude Code takeover.
//!
//! Kept apart from `import_tests.rs`, which is at its size budget: these cases
//! cover the one direction that file cannot — a host where the takeover is not
//! available at all.

// Both cases here are the off-Linux direction, so the helpers they share with
// `import_tests` exist only there.
#[cfg(not(target_os = "linux"))]
use super::tests::{EnvVarGuard, write_claude_transcript};

#[cfg(not(target_os = "linux"))]
#[test]
fn takeover_is_refused_before_a_session_is_staged_on_an_unsupported_platform() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("HORON_HOME", temp.path());
    let transcript = temp
        .path()
        .join("external/.claude/projects/demo/unsupported-live.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    write_claude_transcript(&transcript, "unsupported-live");
    let target = horon_agent_session_types::ResumeTarget::ClaudeCodeSession {
        session_id: "unsupported-live".to_string(),
        session_path: transcript.to_string_lossy().to_string(),
    };

    let error = super::take_over_live_claude_session(&target).expect_err("takeover must be refused here");

    // The message has to tell the user which platform they are on and why the
    // command cannot work there, not just that something failed.
    let message = error.to_string();
    assert!(
        message.contains("only supported on Linux"),
        "refusal must name the supported platform: {message}"
    );
    assert!(
        message.contains(std::env::consts::OS),
        "refusal must name the current platform: {message}"
    );
    assert!(
        message.contains("pidfd") && message.contains("/proc"),
        "refusal must name the missing capability: {message}"
    );
    // A refusal that prepared a session and rolled it back would leave no trace
    // only by luck; assert nothing was written at all.
    let sessions = temp.path().join("sessions");
    let staged = std::fs::read_dir(&sessions)
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    assert_eq!(staged, 0, "a refused takeover must not stage a session");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_reason_is_none_exactly_where_takeover_works() {
    assert!(!crate::claude_live::live_takeover_supported());
    let reason = crate::claude_live::live_takeover_unsupported_reason().expect("a reason off Linux");
    assert!(reason.contains(std::env::consts::OS));
}

#[cfg(target_os = "linux")]
#[test]
fn takeover_is_available_where_the_interfaces_exist() {
    // The other direction: on Linux the guard must let the call through, so a
    // future platform cannot silently inherit the refusal.
    assert!(crate::claude_live::live_takeover_supported());
    assert_eq!(crate::claude_live::live_takeover_unsupported_reason(), None);
}

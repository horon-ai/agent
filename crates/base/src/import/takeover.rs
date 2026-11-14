//! Live Claude Code takeover: staging a HoronAgent session, stopping the
//! guarded Claude process, and rolling the staging back when the stop fails.
//!
//! This lives apart from the transcript importers because it is the one path
//! that touches a running process, and because `import.rs` is already over its
//! size budget: the refusal a non-Linux host answers with belongs here with the
//! rest of the takeover code.

use crate::session::Session;
use anyhow::{Context, Result};
use std::path::Path;

use super::import_session_from_file_with_target;

/// Roll a staged takeover back.
///
/// Best effort by design: the caller is already returning the error that made
/// the rollback necessary, and a cleanup failure must not replace it. The two
/// removals cover both durable layouts a staged takeover can hold (the event
/// store's directory and the snapshot/journal pair), so whichever is absent is
/// simply not there.
pub(crate) fn remove_prepared_takeover_session(session_id: &str) {
    if let Ok(snapshot) = crate::session::session_path(session_id) {
        let backup = snapshot.with_extension("bak");
        // A missing backup is the normal case for the event-store layout.
        if let Err(error) = std::fs::remove_file(backup)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            report_rollback_failure(session_id, "the backup snapshot", &error);
        }
    }
    if let Err(error) = crate::session::discard_session_original(session_id) {
        report_rollback_failure(session_id, "the staged original", &error);
    }
    crate::session_list_cache::invalidate();
}

/// Cleanup failures are recorded rather than dropped or escalated.
fn report_rollback_failure(session_id: &str, what: &str, error: &dyn std::fmt::Display) {
    eprintln!("takeover rollback for session {session_id} could not remove {what}: {error}");
}

/// Explicitly hand a currently-running Claude Code session over to HoronAgent.
///
/// This is deliberately separate from normal resume. It first imports the
/// current transcript into a fresh durable HoronAgent session, then gracefully stops
/// the exact PID guarded by Claude's process-start token. After Claude exits we
/// refresh the prepared snapshot once to capture any final transcript flush.
/// A stop failure rolls back the staged HoronAgent session and leaves ordinary
/// resume behavior unchanged.
pub fn take_over_live_claude_session(
    target: &horon_agent_session_types::ResumeTarget,
) -> Result<horon_agent_session_types::ResumeTarget> {
    take_over_live_claude_session_with_timeout(target, std::time::Duration::from_secs(10))
}

pub(crate) fn take_over_live_claude_session_with_timeout(
    target: &horon_agent_session_types::ResumeTarget,
    stop_timeout: std::time::Duration,
) -> Result<horon_agent_session_types::ResumeTarget> {
    use horon_agent_session_types::ResumeTarget;

    let ResumeTarget::ClaudeCodeSession {
        session_id,
        session_path,
    } = target
    else {
        anyhow::bail!("live takeover is only available for Claude Code sessions");
    };

    // Refuse before preparing anything. Discovery and the stop call both need
    // Linux, and a staged HoronAgent session that must be rolled back is worse
    // than a refusal the user can act on: the message names the platform.
    if let Some(reason) = crate::claude_live::live_takeover_unsupported_reason() {
        anyhow::bail!(reason);
    }

    let live = crate::claude_live::find_live_claude_session(session_id)?
        .ok_or_else(|| anyhow::anyhow!("Claude Code session {session_id} is no longer live"))?;
    // Use a normal memorable HoronAgent session ID so the handed-off conversation
    // remains visible and resumable later. Imported-prefixed IDs are hidden from
    // the native session list because their external source row represents them.
    let takeover_id = Session::create(None, None).id;
    let path = Path::new(session_path);

    let prepared = import_session_from_file_with_target(path, session_id, takeover_id.clone(), true)
        .with_context(|| format!("failed to prepare Claude Code session {session_id}"))?;
    if prepared.visible_conversation_message_count() == 0 {
        remove_prepared_takeover_session(&takeover_id);
        anyhow::bail!("Claude Code session {session_id} has no complete conversation messages to take over");
    }

    let stop_outcome = match crate::claude_live::stop_live_claude_session(&live, stop_timeout) {
        Ok(outcome) => outcome,
        Err(err) => {
            remove_prepared_takeover_session(&takeover_id);
            return Err(err)
                .with_context(|| format!("prepared Claude Code session {session_id}, but did not take it over"));
        }
    };

    if stop_outcome == crate::claude_live::StopLiveClaudeOutcome::ExitUnconfirmed {
        crate::session_list_cache::invalidate();
        anyhow::bail!(
            "Claude Code process {} was asked to exit, but its exit was not confirmed; prepared HoronAgent session {} was preserved and can be resumed",
            live.pid,
            takeover_id
        );
    }

    // Claude may flush one last complete message while handling SIGTERM. The
    // staged snapshot remains durable if the final refresh repeatedly fails,
    // but do not report a fully successful handoff with an incomplete tail.
    let mut final_refresh_error = None;
    for attempt in 0..5 {
        match import_session_from_file_with_target(path, session_id, takeover_id.clone(), true) {
            Ok(_) => {
                final_refresh_error = None;
                break;
            }
            Err(err) => {
                final_refresh_error = Some(err);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }
    if let Some(err) = final_refresh_error {
        crate::session_list_cache::invalidate();
        return Err(err).with_context(|| {
            format!(
                "Claude Code exited, but its final transcript could not be refreshed; prepared HoronAgent session {takeover_id} was preserved"
            )
        });
    }

    crate::logging::info(&format!(
        "Claude takeover complete: source_session={session_id} source_pid={} horon_agent_session={takeover_id}",
        live.pid
    ));
    crate::session_list_cache::invalidate();
    Ok(ResumeTarget::HoronAgentSession {
        session_id: takeover_id,
    })
}

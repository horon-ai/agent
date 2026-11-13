//! `session delete`: permanently remove one session and its per-session state.
//!
//! This is the destructive counterpart to `session archive`. Archiving appends
//! a record and keeps the transcript loadable; deleting removes the transcript,
//! todos, the goal attachment, artifacts and the runtime journal, and no
//! `restore` brings them back.
//!
//! A running daemon owns the session map, so it performs the delete when one is
//! listening: only the daemon can tell whether a turn is running for the
//! session, another process is streaming it, or a client is attached, and those
//! three states are exactly what makes a delete unsafe. With no daemon nothing
//! can be loaded or streaming, so the command removes the same files directly.

use anyhow::{Result, bail};
use serde::Serialize;
use std::io::{IsTerminal, Write};

use crate::session;

#[derive(Serialize)]
struct SessionDeleteOutput {
    session_id: String,
    status: &'static str,
    deleted_via: &'static str,
}

pub(crate) async fn run_session_delete_command(session_ref: &str, yes: bool, json: bool) -> Result<()> {
    let session_id = resolve_session_id(session_ref)?;

    if !yes {
        confirm_interactively(&session_id)?;
    }

    let deleted_via = if crate::cli::dispatch::server_is_running().await {
        delete_through_daemon(&session_id).await?;
        "daemon"
    } else {
        // No daemon means nothing can be loaded or streaming, so the base purge
        // is the whole operation.
        session::purge_session_state(&session_id)?;
        "offline"
    };

    let output = SessionDeleteOutput {
        session_id,
        status: "deleted",
        deleted_via,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("deleted session {} ({deleted_via})", output.session_id);
    }
    Ok(())
}

/// Resolve a session reference to the id the store uses.
///
/// A full store id is taken as itself. Every other command loads the transcript
/// to resolve a name, but that is exactly what a delete must not require: a
/// corrupt session has to be deletable, and a session that is already gone has
/// to resolve so that deleting it twice succeeds. The store's own parser is the
/// gate, so a reference cannot name a path outside the store.
///
/// Anything else — a short name or a title — goes through the name matcher.
fn resolve_session_id(session_ref: &str) -> Result<String> {
    let trimmed = session_ref.trim();
    if trimmed.is_empty() {
        bail!("a session ID or name is required");
    }
    if session::is_store_session_id(trimmed) {
        return Ok(trimmed.to_string());
    }
    session::find_session_by_name_or_id(trimmed)
}

/// Ask before erasing a transcript.
///
/// Deleting is irreversible, so a non-interactive invocation without `--yes`
/// refuses instead of assuming consent.
fn confirm_interactively(session_id: &str) -> Result<()> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        bail!("deleting a session is permanent; pass --yes to confirm (no terminal to ask on)");
    }
    eprint!(
        "Permanently delete session {session_id}? The transcript, todos, goal attachment, artifacts and runtime journal are removed and cannot be restored. [y/N] "
    );
    std::io::stderr().flush()?;
    let mut answer = String::new();
    stdin.read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        bail!("aborted; the session was not deleted");
    }
    Ok(())
}

/// Ask the daemon to delete the session.
///
/// The daemon answers `Done` on success and `Error` with the reason it refused
/// (unconfirmed, open, streaming, attached, or an I/O failure), so the CLI
/// reports the daemon's own wording rather than guessing.
async fn delete_through_daemon(session_id: &str) -> Result<()> {
    let mut client = crate::server::Client::connect().await?;
    client.delete_session(session_id).await
}

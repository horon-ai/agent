//! Which stored conversations one session may read.
//!
//! A forked consult (`swarm spawn source_session`) and a direct `history://<id>`
//! read both hand one session's transcript to another, so both ask the same
//! question: may this session look at that one? The answer is the spawn tree,
//! the set of sessions that share a top-most ancestor. Nothing outside it is
//! readable, so a tool call cannot walk into an unrelated conversation.

use crate::session::Session;
use anyhow::{Context, Result};

/// Longest parent chain accepted before the tree is treated as broken.
const MAX_DEPTH: usize = 64;

/// Tools a consult child gets.
///
/// A consult answers from a copied transcript, so it may read and search but
/// never write, run a shell, or reach the network. The list is enforced where
/// the child session is created, not only where the spawn request is built,
/// because a wire client can name a source session directly.
pub const CONSULT_TOOLS: [&str; 4] = ["read", "agentgrep", "ls", "yield"];

/// Refuse a target that is not in `caller_id`'s spawn tree.
///
/// The target is passed in already loaded because every caller needs it
/// anyway: a consult forks it and a `history://` read parses it.
pub fn ensure_visible(caller_id: &str, target: &Session) -> Result<()> {
    if target.id == caller_id {
        return Ok(());
    }
    let caller = Session::load(caller_id).with_context(|| format!("session '{caller_id}' could not be read"))?;
    ensure_visible_from(&caller, target)
}

/// [`ensure_visible`] for a caller whose session is already in memory.
///
/// The live caller is passed in because a session that has not saved yet cannot
/// be read back from disk, and an unsaved caller may already be an ancestor of
/// the target.
pub fn ensure_visible_from(caller: &Session, target: &Session) -> Result<()> {
    if target.id == caller.id {
        return Ok(());
    }
    let caller_root = root(caller, Some(target))?;
    let target_root = root(target, Some(caller))?;
    anyhow::ensure!(
        caller_root == target_root,
        "session '{}' is outside this session's spawn tree; only sessions that share a root session can be read",
        target.id
    );
    Ok(())
}

/// The top-most ancestor id, which names the tree a session belongs to.
///
/// `known` is the other session of the comparison. Reaching it ends the walk
/// without a store read, so an unsaved live session still counts as a real
/// ancestor instead of an unreadable id.
fn root(session: &Session, known: Option<&Session>) -> Result<String> {
    let mut current = session.id.clone();
    let mut parent = session.parent_id.clone();
    for _ in 0..MAX_DEPTH {
        let Some(next) = parent else {
            return Ok(current);
        };
        current = next;
        if let Some(known) = known
            && known.id == current
        {
            parent = known.parent_id.clone();
            continue;
        }
        parent = Session::load(&current)
            .with_context(|| {
                format!(
                    "session '{}' is in the spawn tree of '{}' but its ancestor '{current}' could not be read",
                    session.id, session.id
                )
            })?
            .parent_id;
    }
    anyhow::bail!("session '{}' has a parent chain that never ends", session.id)
}

#[cfg(test)]
#[path = "session_scope_tests.rs"]
mod session_scope_tests;

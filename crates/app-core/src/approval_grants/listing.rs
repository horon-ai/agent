//! Read-only view and explicit revocation of the stored grants
//! (`horon approvals grants`).
//!
//! Listing never writes. Revocation removes only the grants it is asked for, under
//! the same lock `store` takes, and refuses to touch a file it cannot read: an
//! unreadable `approvals.json` is the user's data, so it is reported, not replaced.
//! Revoking a grant can only make a call ask again; `[tools] restricted_paths` is
//! checked separately and no grant, present or revoked, changes it.

use super::*;
use sha2::{Digest, Sha256};

/// One stored grant as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantView {
    /// A stable handle for `revoke`: it names the grant's workspace, path, scope and capability.
    pub id: String,
    pub workspace: String,
    pub path: String,
    pub scope: &'static str,
    pub capability: &'static str,
    pub granted_at: String,
    pub expires_at: String,
    /// `false` once the grant has expired; it no longer answers anything.
    pub active: bool,
}

fn identity(grant: &Grant) -> String {
    let mut hasher = Sha256::new();
    for part in [
        grant.workspace.to_string_lossy().as_ref(),
        &grant.path.to_string_lossy(),
        match grant.scope {
            GrantScope::Path => "path",
            GrantScope::Folder => "folder",
        },
        grant.capability.as_str(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Path text with control characters escaped, so a crafted name cannot move the
/// cursor or recolor the terminal.
pub(super) fn printable(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(|ch| -> Vec<char> {
            if ch.is_control() {
                ch.escape_default().collect()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn view(grant: &Grant, now: Timestamp) -> GrantView {
    GrantView {
        id: identity(grant),
        workspace: printable(&grant.workspace),
        path: printable(&grant.path),
        scope: match grant.scope {
            GrantScope::Path => "path",
            GrantScope::Folder => "folder",
        },
        capability: grant.capability.as_str(),
        granted_at: grant.granted_at.to_string(),
        expires_at: grant.expires_at.to_string(),
        active: now < grant.expires_at,
    }
}

fn views(grants: &[Grant], now: Timestamp) -> Vec<GrantView> {
    let mut sorted: Vec<&Grant> = grants.iter().collect();
    sorted.sort_by(|a, b| {
        (&a.workspace, &a.path, a.scope as u8, a.capability as u8).cmp(&(
            &b.workspace,
            &b.path,
            b.scope as u8,
            b.capability as u8,
        ))
    });
    sorted.into_iter().map(|grant| view(grant, now)).collect()
}

/// Every stored grant, expired ones included and marked.
pub fn list() -> Vec<GrantView> {
    views(&load_grants(), Timestamp::now())
}

/// Remove the grants named by `ids`; returns how many were removed.
///
/// # Errors
/// The grant file exists but cannot be read, or is locked by another writer.
pub fn revoke(ids: &[&str]) -> anyhow::Result<usize> {
    rewrite(|grant| !ids.contains(&identity(grant).as_str()))
}

/// Remove every stored grant; returns how many were removed.
pub fn revoke_all() -> anyhow::Result<usize> {
    rewrite(|_| false)
}

fn rewrite(keep: impl Fn(&Grant) -> bool) -> anyhow::Result<usize> {
    let _lock = acquire_grant_lock(100, std::time::Duration::from_millis(2))?;
    let path = grant_path()?;
    if !path.exists() {
        return Ok(0);
    }
    let mut file = crate::storage::read_json::<GrantFile>(&path)
        .map_err(|error| anyhow::anyhow!("{} cannot be read, so no grant was changed: {error:#}", path.display()))?;
    let before = file.grants.len();
    file.grants.retain(|grant| keep(grant));
    let removed = before - file.grants.len();
    if removed > 0 {
        file.version = GRANT_FILE_VERSION;
        crate::storage::write_json_secret(&path, &file)?;
    }
    Ok(removed)
}

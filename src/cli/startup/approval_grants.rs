//! `approvals grants`: report and revoke stored scoped approval grants.
//!
//! Local files only; no daemon starts. Revoking can only make a later call ask
//! again, and `[tools] restricted_paths` is not stored here, so it is untouched.

use anyhow::{Result, bail};
use serde_json::json;

use crate::approval_grants::{self, GrantView};
use crate::cli::args::{GrantsAction, GrantsArgs};

pub(super) fn run(args: &GrantsArgs) -> Result<()> {
    match &args.action {
        GrantsAction::List { json } => list(&approval_grants::list(), *json),
        GrantsAction::Revoke { all: true, yes, .. } => revoke_all(&approval_grants::list(), *yes),
        GrantsAction::Revoke { ids, .. } => revoke(&approval_grants::list(), ids),
    }
}

fn state(grant: &GrantView) -> &'static str {
    if grant.active { "active" } else { "expired" }
}

fn list(grants: &[GrantView], json: bool) -> Result<()> {
    if json {
        let rows: Vec<_> = grants
            .iter()
            .map(|grant| {
                json!({
                    "id": grant.id, "state": state(grant), "capability": grant.capability, "scope": grant.scope,
                    "path": grant.path, "workspace": grant.workspace,
                    "granted_at": grant.granted_at, "expires_at": grant.expires_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "grants": rows }))?);
        return Ok(());
    }
    if grants.is_empty() {
        println!("No stored approval grants.");
    }
    for grant in grants {
        println!(
            "{}\t{}\t{} {}\t{}\tworkspace {}\texpires {}",
            grant.id,
            state(grant),
            grant.capability,
            grant.scope,
            grant.path,
            grant.workspace,
            grant.expires_at
        );
    }
    Ok(())
}

fn revoke(grants: &[GrantView], ids: &[String]) -> Result<()> {
    if let Some(unknown) = ids.iter().find(|id| !grants.iter().any(|grant| &grant.id == *id)) {
        bail!("no stored grant has the id {unknown:?}; run `horon approvals grants list`");
    }
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let removed = approval_grants::revoke(&ids)?;
    println!("Revoked {removed} grant(s). The next matching action asks again.");
    Ok(())
}

fn revoke_all(grants: &[GrantView], yes: bool) -> Result<()> {
    let workspaces: std::collections::BTreeSet<&str> = grants.iter().map(|grant| grant.workspace.as_str()).collect();
    println!(
        "This revokes {} grant(s) in {} workspace(s):",
        grants.len(),
        workspaces.len()
    );
    for workspace in &workspaces {
        println!("  {workspace}");
    }
    if grants.is_empty() {
        return Ok(());
    }
    if !yes {
        bail!("nothing was revoked; pass --yes to revoke them all");
    }
    let removed = approval_grants::revoke_all()?;
    println!("Revoked {removed} grant(s). The next matching actions ask again.");
    Ok(())
}

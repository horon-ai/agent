//! `horon-agent api-credentials`, and the exposure `api-bridge` serves.
//!
//! The store itself lives in `horon-agent-harness-api-server`, next to the
//! listener that verifies it, so there is one implementation of hashing,
//! owner-only writes and revocation. This module is the CLI surface: resolve
//! the exposure, print a token exactly once, and never print a token again.

use crate::cli::args::ApiCredentialsAction;
use crate::harness_api::{self, ExposureInputs, TcpExposure};
use anyhow::Result;
use horon_agent_harness_api_server::credentials::{CredentialStore, IssuedCredential};
use std::path::{Path, PathBuf};

pub(crate) fn run(action: ApiCredentialsAction) -> Result<()> {
    let store = CredentialStore::default_store()?;
    let namespace = credential_namespace();
    match action {
        ApiCredentialsAction::Issue { label } => {
            let issued = store.issue(label.as_deref())?;
            print_issued("issued", &issued, store.path(), namespace.as_deref());
        }
        ApiCredentialsAction::Rotate { credential } => {
            let issued = store.rotate(&credential)?;
            print_issued("rotated", &issued, store.path(), namespace.as_deref());
        }
        ApiCredentialsAction::Revoke { credential } => {
            let removed = store.revoke(&credential)?;
            println!("revoked {} ({})", removed.id, removed.label);
        }
        ApiCredentialsAction::List => list(&store, namespace.as_deref())?,
    }
    Ok(())
}

/// Resolve the exposure the `api-bridge` flags ask for and serve it: the local
/// socket always, plus a TCP listener that authenticates when one is requested.
pub(crate) async fn serve_bridge(
    api_socket: PathBuf,
    daemon_socket: PathBuf,
    bind_flag: Option<String>,
    insecure_behind_proxy_flag: bool,
) -> Result<()> {
    match resolve_exposure(bind_flag, insecure_behind_proxy_flag)? {
        Some(exposure) => {
            horon_agent_harness_api_server::run_bridge_exposed(api_socket, daemon_socket, &exposure).await
        }
        None => horon_agent_harness_api_server::run_bridge(api_socket, daemon_socket).await,
    }
}

/// Resolve `--api-bind` / `HORON_AGENT_API_BIND` / `[harness_api] bind`.
///
/// An unreadable credential store counts as "no credential here": the refusal
/// then points at `api-credentials issue`, which reports the real file problem.
pub(crate) fn resolve_exposure(
    bind_flag: Option<String>,
    insecure_behind_proxy_flag: bool,
) -> Result<Option<TcpExposure>> {
    let mut inputs = ExposureInputs::from_environment(bind_flag, insecure_behind_proxy_flag);
    inputs.credentials_available = credentials_available();
    harness_api::resolve(&inputs).map_err(anyhow::Error::from)
}

fn credentials_available() -> bool {
    match CredentialStore::default_store() {
        Ok(store) => match store.has_any() {
            Ok(present) => present,
            Err(_) => false,
        },
        Err(_) => false,
    }
}

fn credential_namespace() -> Option<String> {
    match crate::hosting::current() {
        Ok(policy) => policy.tenant_id().map(|tenant| tenant.as_str().to_string()),
        Err(_) => None,
    }
}

fn print_issued(verb: &str, issued: &IssuedCredential, store: &Path, namespace: Option<&str>) {
    println!("{verb} credential {} ({})", issued.record.id, issued.record.label);
    println!("token: {}", issued.token);
    println!("the token is printed once and only its SHA-256 digest is stored; save it now");
    match namespace {
        Some(tenant) => println!("namespace: tenant {tenant} (store: {})", store.display()),
        None => println!("store: {}", store.display()),
    }
}

fn list(store: &CredentialStore, namespace: Option<&str>) -> Result<()> {
    let records = store.records()?;
    if records.is_empty() {
        println!("no harness API credentials; issue one with `horon-agent api-credentials issue`");
        return Ok(());
    }
    for record in &records {
        let rotated = record.rotated_at.as_deref().unwrap_or("never");
        println!(
            "{}\t{}\tcreated {}\trotated {rotated}",
            record.id, record.label, record.created_at
        );
    }
    if let Some(tenant) = namespace {
        println!("namespace: tenant {tenant}");
    }
    Ok(())
}
